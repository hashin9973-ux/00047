use crate::sys;
use async_trait::async_trait;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::OnceLock,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cat {
    Condition,
    Action,
    Target,
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Satisfied,
    NotSatisfied,
}
#[derive(Clone, Debug)]
pub enum Target {
    Process { pid: u32, name: String },
    File { path: String },
}

type Report = Box<dyn Fn(&str, Option<f64>, String) + Send + Sync>;
type LogFn = Box<dyn Fn(&str, String) + Send + Sync>;
type Resolve = Box<dyn Fn() -> Result<Vec<Target>, String> + Send + Sync>;
type Flag = Box<dyn Fn() -> bool + Send + Sync>;

/// Everything a block gets while running.
pub struct NodeCx {
    pub cfg: Value,
    pub token: CancellationToken,
    /// How many times this block has been reached in the current run (1-based).
    pub visit: u64,
    /// report(state "running"|"waiting", progress 0..1, detail text)
    pub report: Report,
    pub log: LogFn,
    /// Resolves the Target blocks wired into this block (evaluated fresh each call).
    pub resolve: Resolve,
    pub visible: Flag,
}

#[async_trait]
pub trait Block: Send + Sync {
    fn title(&self) -> &'static str;
    fn category(&self) -> Cat;
    fn validate(&self, cfg: &Value) -> Result<(), String>;
    /// Target block kind this block requires, e.g. Some("tgt.process").
    fn requires_target(&self) -> Option<&'static str> {
        None
    }
    /// Whether the generic `timeoutSec` setting applies.
    fn supports_timeout(&self) -> bool {
        false
    }
    async fn run(&self, _cx: &NodeCx) -> Result<Outcome, String> {
        Err("this block is not executable".into())
    }
    fn resolve(&self, _cfg: &Value) -> Result<Target, String> {
        Err("this block is not a target".into())
    }
}

// ---------- helpers ----------
fn rng(c: &Value, k: &str, d: f64, lo: f64, hi: f64, label: &str) -> Result<f64, String> {
    let v = match c.get(k) {
        None => d,
        Some(x) => x.as_f64().ok_or_else(|| format!("{label} must be a number"))?,
    };
    if v.is_finite() && v >= lo && v <= hi {
        Ok(v)
    } else {
        Err(format!("{label} must be between {lo} and {hi}"))
    }
}
fn text(c: &Value, k: &str) -> String {
    c.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string()
}
fn flag(c: &Value, k: &str, d: bool) -> bool {
    c.get(k).and_then(|v| v.as_bool()).unwrap_or(d)
}
fn check_timeout(c: &Value) -> Result<(), String> {
    rng(c, "timeoutSec", 0.0, 0.0, 86400.0, "Timeout").map(|_| ())
}

/// Sleeps with progress reporting; returns false if cancelled.
async fn sleep_progress(cx: &NodeCx, secs: f64, label: &str) -> bool {
    let total = Duration::from_secs_f64(secs);
    let t0 = Instant::now();
    loop {
        let el = t0.elapsed();
        if el >= total {
            return true;
        }
        (cx.report)("waiting", Some(el.as_secs_f64() / secs), format!("{label} · {:.1}s / {:.1}s", el.as_secs_f64(), secs));
        let step = if (cx.visible)() { Duration::from_millis(250) } else { Duration::from_secs(2) };
        tokio::select! {
            _ = tokio::time::sleep((total - el).min(step)) => {},
            _ = cx.token.cancelled() => return false,
        }
    }
}

fn first_process(cx: &NodeCx) -> Result<(u32, String), String> {
    for t in (cx.resolve)()? {
        if let Target::Process { pid, name } = t {
            return Ok((pid, name));
        }
    }
    Err("no Active process target is connected to this block".into())
}

// ---------- Conditions ----------
struct Interval;
#[async_trait]
impl Block for Interval {
    fn title(&self) -> &'static str {
        "Interval"
    }
    fn category(&self) -> Cat {
        Cat::Condition
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        rng(c, "seconds", 30.0, 0.1, 86400.0, "Interval").map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let s = rng(&cx.cfg, "seconds", 30.0, 0.1, 86400.0, "Interval")?;
        if sleep_progress(cx, s, "Waiting").await {
            Ok(Outcome::Satisfied)
        } else {
            Err("cancelled".into())
        }
    }
}

struct Ram;
fn read_mb(cx: &NodeCx) -> Result<(f64, String), String> {
    for t in (cx.resolve)()? {
        if let Target::Process { pid, name } = t {
            let b = sys::working_set_bytes(pid)?;
            return Ok((b as f64 / 1_048_576.0, format!("{name} ({pid})")));
        }
    }
    Ok((sys::system_used_mb()?, "system".into()))
}
#[async_trait]
impl Block for Ram {
    fn title(&self) -> &'static str {
        "RAM Usage"
    }
    fn category(&self) -> Cat {
        Cat::Condition
    }
    fn supports_timeout(&self) -> bool {
        true
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        rng(c, "thresholdMb", 800.0, 1.0, 1_048_576.0, "Threshold")?;
        if text(c, "mode") == "wait" {
            rng(c, "pollSec", 1.0, 0.25, 3600.0, "Poll interval")?;
            check_timeout(c)?;
        }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let above = text(&cx.cfg, "op") != "below";
        let thr = rng(&cx.cfg, "thresholdMb", 800.0, 1.0, 1_048_576.0, "Threshold")?;
        let wait = text(&cx.cfg, "mode") == "wait";
        let poll = rng(&cx.cfg, "pollSec", 1.0, 0.25, 3600.0, "Poll interval")?;
        loop {
            let (cur, scope) = read_mb(cx)?;
            let ok = if above { cur > thr } else { cur < thr };
            (cx.report)(
                if wait { "waiting" } else { "running" },
                None,
                format!("{scope}: {cur:.0} MB (needs {} {thr:.0} MB)", if above { "above" } else { "below" }),
            );
            if ok {
                return Ok(Outcome::Satisfied);
            }
            if !wait {
                return Ok(Outcome::NotSatisfied);
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs_f64(poll)) => {},
                _ = cx.token.cancelled() => return Err("cancelled".into()),
            }
        }
    }
}

// ---------- Actions ----------
struct Trim;
#[async_trait]
impl Block for Trim {
    fn title(&self) -> &'static str {
        "Working-Set Trim"
    }
    fn category(&self) -> Cat {
        Cat::Action
    }
    fn supports_timeout(&self) -> bool {
        true
    }
    fn requires_target(&self) -> Option<&'static str> {
        Some("tgt.process")
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        rng(c, "percent", 50.0, 0.0, 100.0, "Percent")?;
        check_timeout(c)
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let pct = rng(&cx.cfg, "percent", 50.0, 0.0, 100.0, "Percent")?;
        let (pid, name) = first_process(cx)?;
        (cx.report)("running", None, format!("Trimming {name}"));
        let msg = tokio::task::spawn_blocking(move || sys::trim_working_set(pid, pct))
            .await
            .map_err(|e| e.to_string())??;
        (cx.log)("info", format!("{name} ({pid}): {msg}"));
        Ok(Outcome::Satisfied)
    }
}

struct Wait;
#[async_trait]
impl Block for Wait {
    fn title(&self) -> &'static str {
        "Wait"
    }
    fn category(&self) -> Cat {
        Cat::Action
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        rng(c, "seconds", 5.0, 0.0, 86400.0, "Wait").map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let s = rng(&cx.cfg, "seconds", 5.0, 0.0, 86400.0, "Wait")?;
        if s == 0.0 || sleep_progress(cx, s, "Waiting").await {
            Ok(Outcome::Satisfied)
        } else {
            Err("cancelled".into())
        }
    }
}

struct Start;
#[async_trait]
impl Block for Start {
    fn title(&self) -> &'static str {
        "Start"
    }
    fn category(&self) -> Cat {
        Cat::Action
    }
    fn supports_timeout(&self) -> bool {
        true
    }
    fn requires_target(&self) -> Option<&'static str> {
        Some("tgt.file")
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        check_timeout(c)
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let path = (cx.resolve)()?
            .into_iter()
            .find_map(|t| if let Target::File { path } = t { Some(path) } else { None })
            .ok_or("no File target is connected to this block")?;
        if !std::path::Path::new(&path).exists() {
            return Err(format!("file not found: {path}"));
        }
        let args = text(&cx.cfg, "args");
        (cx.report)("running", None, format!("Launching {path}"));
        let p2 = path.clone();
        tokio::task::spawn_blocking(move || sys::start_file(&p2, &args)).await.map_err(|e| e.to_string())??;
        (cx.log)("info", format!("launched {path}"));
        Ok(Outcome::Satisfied)
    }
}

/// Loop: each time it is reached it counts a visit. While visit <= iterations (or iterations = 0)
/// it waits `delaySec` and is Satisfied (wire IF to the loop body, and the body back to this block).
/// After the last iteration it is NotSatisfied (wire ELSE to whatever runs after the loop).
struct Loop;
#[async_trait]
impl Block for Loop {
    fn title(&self) -> &'static str {
        "Loop"
    }
    fn category(&self) -> Cat {
        Cat::Action
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        rng(c, "iterations", 3.0, 0.0, 1_000_000.0, "Iterations")?;
        rng(c, "delaySec", 1.0, 0.1, 86400.0, "Delay").map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let it = rng(&cx.cfg, "iterations", 3.0, 0.0, 1_000_000.0, "Iterations")? as u64;
        let delay = rng(&cx.cfg, "delaySec", 1.0, 0.1, 86400.0, "Delay")?;
        if it == 0 || cx.visit <= it {
            let label = if it > 0 { format!("Iteration {}/{it}", cx.visit) } else { format!("Iteration {}", cx.visit) };
            if !sleep_progress(cx, delay, &label).await {
                return Err("cancelled".into());
            }
            Ok(Outcome::Satisfied)
        } else {
            Ok(Outcome::NotSatisfied)
        }
    }
}

// ---------- Targets ----------
struct ProcessTarget;
impl Block for ProcessTarget {
    fn title(&self) -> &'static str {
        "Active process"
    }
    fn category(&self) -> Cat {
        Cat::Target
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        if text(c, "mode") == "name" && text(c, "name").is_empty() {
            return Err("Process name is required".into());
        }
        Ok(())
    }
    fn resolve(&self, c: &Value) -> Result<Target, String> {
        if text(c, "mode") == "name" {
            let n = text(c, "name");
            let pid = sys::find_pid_by_name(&n)?;
            return Ok(Target::Process { pid, name: n });
        }
        let pid = sys::foreground_pid()?;
        if flag(c, "ignoreSelf", true) && pid == sys::own_pid() {
            return Err("the foreground window belongs to 00047 itself".into());
        }
        Ok(Target::Process { pid, name: sys::process_name(pid) })
    }
}

struct FileTarget;
impl Block for FileTarget {
    fn title(&self) -> &'static str {
        "File"
    }
    fn category(&self) -> Cat {
        Cat::Target
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        if text(c, "path").is_empty() {
            return Err("File path is required".into());
        }
        Ok(())
    }
    fn resolve(&self, c: &Value) -> Result<Target, String> {
        self.validate(c)?;
        Ok(Target::File { path: text(c, "path") })
    }
}

pub fn registry() -> &'static HashMap<&'static str, Box<dyn Block>> {
    static R: OnceLock<HashMap<&'static str, Box<dyn Block>>> = OnceLock::new();
    R.get_or_init(|| {
        let mut m: HashMap<&'static str, Box<dyn Block>> = HashMap::new();
        m.insert("cond.interval", Box::new(Interval));
        m.insert("cond.ram", Box::new(Ram));
        m.insert("act.trim", Box::new(Trim));
        m.insert("act.wait", Box::new(Wait));
        m.insert("act.start", Box::new(Start));
        m.insert("act.loop", Box::new(Loop));
        m.insert("tgt.process", Box::new(ProcessTarget));
        m.insert("tgt.file", Box::new(FileTarget));
        m
    })
}
