use crate::model::{LogEntry, RunRecord};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

const MAX_BYTES: u64 = 5 * 1024 * 1024;

struct Inner {
    w: BufWriter<File>,
    bytes: u64,
    last_flush: Instant,
}

pub struct LogStore {
    dir: PathBuf,
    inner: Mutex<Option<Inner>>,
}

fn open_inner(dir: &Path) -> Option<Inner> {
    let path = dir.join("logs.jsonl");
    let f = OpenOptions::new().create(true).append(true).open(&path).ok()?;
    let bytes = f.metadata().map(|m| m.len()).unwrap_or(0);
    Some(Inner { w: BufWriter::new(f), bytes, last_flush: Instant::now() })
}

impl LogStore {
    /// Opens the log files and returns the newest entries from earlier sessions.
    pub fn open(dir: &Path, keep: usize) -> (LogStore, Vec<LogEntry>) {
        let mut entries: VecDeque<LogEntry> = VecDeque::new();
        for name in ["logs.old.jsonl", "logs.jsonl"] {
            if let Ok(f) = File::open(dir.join(name)) {
                for line in BufReader::new(f).lines().map_while(Result::ok) {
                    if let Ok(e) = serde_json::from_str::<LogEntry>(&line) {
                        entries.push_back(e);
                        if entries.len() > keep {
                            entries.pop_front();
                        }
                    }
                }
            }
        }
        let store = LogStore { dir: dir.to_path_buf(), inner: Mutex::new(open_inner(dir)) };
        (store, entries.into_iter().collect())
    }

    /// Appends one entry. Warnings/errors flush immediately; info lines flush at most every 5 s.
    pub fn append(&self, e: &LogEntry) {
        let mut g = self.inner.lock().unwrap();
        let Some(i) = g.as_mut() else { return };
        let Ok(mut line) = serde_json::to_string(e) else { return };
        line.push('\n');
        if i.w.write_all(line.as_bytes()).is_err() {
            return;
        }
        i.bytes += line.len() as u64;
        if e.level != "info" || i.last_flush.elapsed() > Duration::from_secs(5) {
            let _ = i.w.flush();
            i.last_flush = Instant::now();
        }
        if i.bytes > MAX_BYTES {
            let _ = i.w.flush();
            *g = None;
            let _ = fs::remove_file(self.dir.join("logs.old.jsonl"));
            let _ = fs::rename(self.dir.join("logs.jsonl"), self.dir.join("logs.old.jsonl"));
            *g = open_inner(&self.dir);
        }
    }

    pub fn flush(&self) {
        if let Some(i) = self.inner.lock().unwrap().as_mut() {
            let _ = i.w.flush();
        }
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap();
        *g = None;
        let _ = fs::remove_file(self.dir.join("logs.old.jsonl"));
        let _ = fs::remove_file(self.dir.join("logs.jsonl"));
        *g = open_inner(&self.dir);
    }

    pub fn save_history(&self, h: &HashMap<String, VecDeque<RunRecord>>) {
        if let Ok(b) = serde_json::to_vec(h) {
            let tmp = self.dir.join("history.json.tmp");
            if fs::write(&tmp, &b).is_ok() {
                // Windows doesn't reliably replace an existing destination with rename.
                let dst = self.dir.join("history.json");
                let _ = fs::remove_file(&dst);
                let _ = fs::rename(&tmp, dst);
            }
        }
    }

    pub fn load_history(dir: &Path) -> HashMap<String, VecDeque<RunRecord>> {
        fs::read(dir.join("history.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }
}
