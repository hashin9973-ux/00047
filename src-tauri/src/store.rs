use crate::model::Automation;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub struct Store {
    path: PathBuf,
    autos: Mutex<Vec<Automation>>,
}

impl Store {
    /// Loads automations.json. Returns the store plus an optional warning to log.
    pub fn load(dir: &Path) -> (Store, Option<String>) {
        let path = dir.join("automations.json");
        let mut warn = None;
        let autos = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Vec<Automation>>(&bytes) {
                Ok(v) => v,
                Err(e) => {
                    let bad = dir.join(format!("automations.corrupt-{}.json", crate::engine::now_ms()));
                    let _ = fs::rename(&path, &bad);
                    warn = Some(format!(
                        "automations.json could not be parsed ({e}). It was moved to {} and a fresh file was started.",
                        bad.display()
                    ));
                    vec![]
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![sample()],
            Err(e) => {
                warn = Some(format!("automations.json could not be read: {e}"));
                vec![]
            }
        };
        (Store { path, autos: Mutex::new(autos) }, warn)
    }

    pub fn all(&self) -> Vec<Automation> {
        self.autos.lock().unwrap().clone()
    }

    pub fn get(&self, id: &str) -> Option<Automation> {
        self.autos.lock().unwrap().iter().find(|a| a.id == id).cloned()
    }

    pub fn upsert(&self, a: Automation) -> Result<(), String> {
        let mut current = self.autos.lock().unwrap();
        // Persist a candidate snapshot before publishing it in memory. If disk I/O
        // fails, callers and the next read still see the last successfully saved data.
        let mut candidate = current.clone();
        match candidate.iter_mut().find(|x| x.id == a.id) {
            Some(slot) => *slot = a,
            None => candidate.push(a),
        }
        self.flush(&candidate)?;
        *current = candidate;
        Ok(())
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        let mut current = self.autos.lock().unwrap();
        let mut candidate = current.clone();
        candidate.retain(|a| a.id != id);
        self.flush(&candidate)?;
        *current = candidate;
        Ok(())
    }

    /// Atomic write: temp file, fsync, rename over the real file.
    fn flush(&self, list: &[Automation]) -> Result<(), String> {
        let data = serde_json::to_vec_pretty(list).map_err(|e| format!("serialize: {e}"))?;
        let tmp = self.path.with_extension("json.tmp");
        let mut f = fs::File::create(&tmp).map_err(|e| format!("create temp file: {e}"))?;
        f.write_all(&data).map_err(|e| format!("write: {e}"))?;
        f.sync_all().map_err(|e| format!("flush to disk: {e}"))?;
        drop(f);
        fs::rename(&tmp, &self.path).map_err(|e| format!("replace automations.json: {e}"))
    }
}

fn sample() -> Automation {
    serde_json::from_value(json!({
        "id": "sample-trim",
        "name": "Trim memory when RAM is high",
        "nodes": [
            {"id":"s-int","kind":"cond.interval","x":60.0,"y":80.0,"cfg":{"seconds":30,"repeat":true}},
            {"id":"s-ram","kind":"cond.ram","x":400.0,"y":80.0,"cfg":{"op":"above","thresholdMb":800,"mode":"check","pollSec":1,"timeoutSec":0,"onFail":"stop"}},
            {"id":"s-trim","kind":"act.trim","x":740.0,"y":40.0,"cfg":{"percent":60,"timeoutSec":15,"onFail":"stop"}},
            {"id":"s-tgt","kind":"tgt.process","x":400.0,"y":360.0,"cfg":{"mode":"foreground","name":"","ignoreSelf":true}}
        ],
        "edges": [
            {"id":"e1","from":"s-int","to":"s-ram","kind":"after"},
            {"id":"e2","from":"s-ram","to":"s-trim","kind":"if"},
            {"id":"e3","from":"s-tgt","to":"s-ram","kind":"target"},
            {"id":"e4","from":"s-tgt","to":"s-trim","kind":"target"}
        ],
        "viewport": {"x":0.0,"y":0.0,"k":1.0},
        "autoStart": false
    }))
    .unwrap()
}
