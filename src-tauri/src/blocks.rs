use crate::sys;
use async_trait::async_trait;
use crate::spec;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
#[cfg(windows)]
use std::process::Stdio;
use tokio_util::sync::CancellationToken;

pub(super) mod extra;
mod extra_more;
mod advanced;
mod nextgen;

#[cfg(windows)]
const MAX_COMMAND_OUTPUT_BYTES: usize = 16 * 1024;

#[cfg(windows)]
async fn read_bounded_output<R>(mut reader: R, limit: usize) -> Result<(Vec<u8>, bool), String>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    use tokio::io::AsyncReadExt;

    let mut output = Vec::with_capacity(limit.min(8192));
    let mut buffer = [0u8; 8192];
    let mut truncated = false;

    loop {
        let read = reader.read(&mut buffer).await.map_err(|e| format!("read command output: {e}"))?;
        if read == 0 {
            break;
        }
        let keep = (limit - output.len()).min(read);
        output.extend_from_slice(&buffer[..keep]);
        if keep < read {
            truncated = true;
        }
    }

    Ok((output, truncated))
}

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
    /// Resolves the Input blocks wired into this block (evaluated fresh each call).
    pub resolve: Resolve,
    pub visible: Flag,
    /// Variables are shared by every block in this automation run and are discarded when it ends.
    pub variables: Arc<Mutex<HashMap<String, Value>>>,
}

impl NodeCx {
    pub fn get_variable(&self, name: &str) -> Option<Value> {
        self.variables.lock().ok()?.get(name).cloned()
    }

    pub fn set_variable(&self, name: String, value: Value) -> Result<(), String> {
        let mut variables = self.variables.lock().map_err(|_| "workflow variable store is unavailable".to_string())?;
        variables.insert(name, value);
        Ok(())
    }

    pub fn remove_variable(&self, name: &str) -> Result<bool, String> {
        let mut variables = self.variables.lock().map_err(|_| "workflow variable store is unavailable".to_string())?;
        Ok(variables.remove(name).is_some())
    }

    pub fn clear_variables(&self) -> Result<(), String> {
        self.variables.lock().map_err(|_| "workflow variable store is unavailable".to_string())?.clear();
        Ok(())
    }

    /// Replaces {{variable_name}} placeholders. Unknown variables fail explicitly instead of becoming empty strings.
    pub fn expand_template(&self, input: &str) -> Result<String, String> {
        let mut output = String::with_capacity(input.len());
        let mut rest = input;
        while let Some(start) = rest.find("{{") {
            output.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else {
                output.push_str(&rest[start..]);
                return Ok(output);
            };
            let name = &after[..end];
            if let Some(secret_name) = name.strip_prefix("secret:") {
                let secret = crate::secrets::get(secret_name)?;
                output.push_str(&secret);
                rest = &after[end + 2..];
                continue;
            }
            if name.is_empty() || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.') {
                output.push_str("{{");
                rest = after;
                continue;
            }
            let value = self.get_variable(name).ok_or_else(|| format!("workflow variable '{{{{{name}}}}}' is not set"))?;
            let rendered = match value {
                Value::String(s) => s,
                other => other.to_string(),
            };
            output.push_str(&rendered);
            rest = &after[end + 2..];
        }
        output.push_str(rest);
        Ok(output)
    }
}

#[async_trait]
pub trait Block: Send + Sync {
    fn title(&self) -> &'static str;
    fn category(&self) -> Cat;
    fn validate(&self, cfg: &Value) -> Result<(), String>;
    /// Input block kind this block requires, e.g. Some("tgt.process").
    fn requires_target(&self) -> Option<&'static str> {
        None
    }
    /// Whether the generic `timeoutSec` setting applies.
    fn supports_timeout(&self) -> bool {
        false
    }
    /// Declarative UI + validation spec (see docs/ADDING_BLOCKS.md). A native block that returns one needs
    /// no definition in ui/blocks.js: the UI is generated from it.
    fn spec(&self) -> Option<Value> {
        None
    }
    /// True for actions with side effects: they get the strict repeat limit (12 runs per 10 s).
    fn risky(&self) -> bool {
        false
    }
    async fn run(&self, _cx: &NodeCx) -> Result<Outcome, String> {
        Err("this block is not executable".into())
    }
    fn resolve(&self, _cfg: &Value) -> Result<Target, String> {
        Err("this block is not an input".into())
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
        if let Target::Process { pid, name } = t { return Ok((pid, name)); }
    }
    Err("no Process Input is connected to this block".into())
}

fn visible_windows_spec() -> Value {
    let mut fields = vec![
        json!({"key":"op","label":"Match","type":"select","default":"at_least",
             "options":[["at_least","At least"],["at_most","At most"],["exactly","Exactly"]]}),
        json!({"key":"count","label":"Window count","type":"number","min":0,"max":10000,"step":1,"default":1}),
    ];
    fields.extend(extra::behavior_fields());
    json!({
        "category": "condition", "title": "Visible Windows", "icon": "window",
        "description": "Counts visible, non-minimized top-level windows owned by the connected process input.",
        "requires": "tgt.process", "fields": fields,
        "summary": "{op} {count} visible windows",
        "help": "Requires a Process Input. Supports check-once, wait-until-satisfied, and state-change behavior, including a polling interval and timeout. Windows hidden behind other windows may still count; this does not measure unobscured pixels."
    })
}

fn expected_window_count(c: &Value) -> Result<usize, String> {
    let n = rng(c, "count", 1.0, 0.0, 10000.0, "Window count")?;
    if n.fract() != 0.0 { return Err("Window count must be a whole number".into()); }
    Ok(n as usize)
}

fn window_count_matches(mode: &str, actual: usize, expected: usize) -> bool {
    match mode { "at_most" => actual <= expected, "exactly" => actual == expected, _ => actual >= expected }
}

struct VisibleWindows;
#[async_trait]
impl Block for VisibleWindows {
    fn title(&self) -> &'static str { "Visible Windows" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn requires_target(&self) -> Option<&'static str> { Some("tgt.process") }
    fn spec(&self) -> Option<Value> { Some(visible_windows_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&visible_windows_spec(), c)?;
        expected_window_count(c)?;
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let (pid, name) = first_process(cx)?;
        let expected = expected_window_count(&cx.cfg)?;
        let mode = text(&cx.cfg, "op");
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let name = name.clone();
            let mode = mode.clone();
            Box::pin(async move {
                let count = tokio::task::spawn_blocking(move || sys::visible_window_count(pid))
                    .await.map_err(|e| e.to_string())??;
                let satisfied = window_count_matches(&mode, count, expected);
                (cx.report)("waiting", None, format!("{name}: {count} visible window(s); needs {} {expected}",
                    match mode.as_str() { "at_most" => "at most", "exactly" => "exactly", _ => "at least" }));
                Ok(satisfied)
            }) as extra::Probe<'_>
        }).await
    }
}

fn process_audio_spec() -> Value {
    let mut fields = vec![json!({"key":"state","label":"Condition","type":"select","default":"outputting",
             "options":[["outputting","Outputting audio"],["not_outputting","Not outputting audio"]]})];
    fields.extend(extra::behavior_fields());
    json!({
        "category": "condition", "title": "Process Outputting Audio", "icon": "bell",
        "requires": "tgt.process",
        "description": "Checks whether the connected process has an active Windows audio-render session.",
        "fields": fields,
        "summary": "Process audio: {state}",
        "help": "Requires a Process Input. Supports check-once, wait-until-satisfied, and state-change behavior, including polling interval, stable duration, and timeout. A muted or silent stream still counts as active."
    })
}
struct ProcessAudio;
#[async_trait]
impl Block for ProcessAudio {
    fn title(&self) -> &'static str { "Process Outputting Audio" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn requires_target(&self) -> Option<&'static str> { Some("tgt.process") }
    fn spec(&self) -> Option<Value> { Some(process_audio_spec()) }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&process_audio_spec(), c)?;
        extra::wait_settings(c).map(|_| ())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let (pid, name) = first_process(cx)?;
        let state = text(&cx.cfg, "state");
        let (behavior, poll, stable, timeout) = extra::wait_settings(&cx.cfg)?;
        extra::poll_condition(cx, &behavior, poll, stable, timeout, || {
            let name = name.clone();
            let state = state.clone();
            Box::pin(async move {
                let outputting = tokio::task::spawn_blocking(move || sys::process_outputting_audio(pid))
                    .await.map_err(|e| e.to_string())??;
                (cx.report)("waiting", None, format!("{name}: {}", if outputting { "audio stream active" } else { "no active audio stream" }));
                Ok((state == "not_outputting") != outputting)
            }) as extra::Probe<'_>
        }).await
    }
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
    fn title(&self) -> &'static str { "RAM Usage" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn validate(&self, c: &Value) -> Result<(), String> {
        rng(c, "thresholdMb", 800.0, 1.0, 1_048_576.0, "Threshold")?;
        let mode = text(c, "mode");
        if !matches!(mode.as_str(), "" | "check" | "wait" | "wait_change") {
            return Err("Unknown RAM usage mode".into());
        }
        if mode != "check" && !mode.is_empty() {
            rng(c, "pollSec", 1.0, 0.25, 3600.0, "Poll interval")?;
            rng(c, "stableForSec", 0.0, 0.0, 3600.0, "Stable duration")?;
            check_timeout(c)?;
        }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let above = text(&cx.cfg, "op") != "below";
        let thr = rng(&cx.cfg, "thresholdMb", 800.0, 1.0, 1_048_576.0, "Threshold")?;
        let mode = text(&cx.cfg, "mode");
        let behavior = match mode.as_str() { "wait" => "wait", "wait_change" => "change", _ => "check" };
        let poll = rng(&cx.cfg, "pollSec", 1.0, 0.25, 3600.0, "Poll interval")?;
        let stable = rng(&cx.cfg, "stableForSec", 0.0, 0.0, 3600.0, "Stable duration")?;
        let timeout = rng(&cx.cfg, "timeoutSec", 0.0, 0.0, 86400.0, "Timeout")?;
        extra::poll_condition(cx, behavior, poll, stable, timeout, || {
            Box::pin(async move {
                let (cur, scope) = read_mb(cx)?;
                let ok = if above { cur > thr } else { cur < thr };
                (cx.report)(if behavior == "check" { "running" } else { "waiting" }, None,
                    format!("{scope}: {cur:.0} MB (needs {} {thr:.0} MB)", if above { "above" } else { "below" }));
                Ok(ok)
            }) as extra::Probe<'_>
        }).await
    }
}

struct ProcRunning;
#[async_trait]
impl Block for ProcRunning {
    fn title(&self) -> &'static str { "Process Running" }
    fn category(&self) -> Cat { Cat::Condition }
    fn supports_timeout(&self) -> bool { true }
    fn validate(&self, c: &Value) -> Result<(), String> {
        if text(c, "name").is_empty() { return Err("Process name is required".into()); }
        let mode = text(c, "mode");
        if !matches!(mode.as_str(), "" | "check" | "wait_start" | "wait_stop" | "wait_change") {
            return Err("Unknown mode".into());
        }
        if mode.starts_with("wait") {
            rng(c, "pollSec", 2.0, 0.25, 3600.0, "Poll interval")?;
            rng(c, "stableForSec", 0.0, 0.0, 3600.0, "Stable duration")?;
            check_timeout(c)?;
        }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let name = cx.expand_template(&text(&cx.cfg, "name"))?;
        let mode = text(&cx.cfg, "mode");
        let behavior = match mode.as_str() { "wait_start" | "wait_stop" => "wait", "wait_change" => "change", _ => "check" };
        let poll = rng(&cx.cfg, "pollSec", 2.0, 0.25, 3600.0, "Poll interval")?;
        let stable = rng(&cx.cfg, "stableForSec", 0.0, 0.0, 3600.0, "Stable duration")?;
        let timeout = rng(&cx.cfg, "timeoutSec", 0.0, 0.0, 86400.0, "Timeout")?;
        extra::poll_condition(cx, behavior, poll, stable, timeout, || {
            let name = name.clone();
            let mode = mode.clone();
            Box::pin(async move {
                let running = sys::find_pid_by_name(&name).is_ok();
                (cx.report)(if behavior == "check" { "running" } else { "waiting" }, None,
                    format!("{name}: {}", if running { "running" } else { "not running" }));
                Ok(if mode == "wait_stop" { !running } else { running })
            }) as extra::Probe<'_>
        }).await
    }
}

// Time window: the reference example of a block defined purely by `spec()` + `run()`.
fn schedule_spec() -> Value {
    json!({
        "category": "condition", "title": "Time window", "icon": "clock",
        "description": "Satisfied during a daily time window (PC local time).",
        "fields": [
            {"key":"start","label":"From","type":"time","default":"09:00","required":true},
            {"key":"end","label":"Until","type":"time","default":"17:00","required":true},
            {"key":"days","label":"Days","type":"select","default":"all","options":[["all","Every day"],["weekdays","Mon–Fri"],["weekends","Sat–Sun"]]},
            {"key":"mode","label":"Mode","type":"select","default":"check","options":[["check","Check once (IF / ELSE)"],["wait","Wait until inside the window"],["wait_change","Wait until window state changes"]]},
            {"key":"pollSec","label":"Check every","type":"number","min":1,"max":3600,"step":1,"unit":"s","default":5,"show":{"key":"mode","ne":"check"}},
            {"key":"stableForSec","label":"Require condition to remain satisfied for","type":"number","min":0,"max":3600,"step":0.5,"unit":"s","default":0,"show":{"key":"mode","ne":"check"}},
            {"preset":"timeout","show":{"key":"mode","ne":"check"}},
            {"preset":"onFail"}
        ],
        "summary": "{start}–{end} · {days}",
        "help": "Uses this PC's local time. If Until is earlier than From the window crosses midnight (for example 22:00 to 06:00). From = Until means the whole day. Wait can debounce the inside-window state; Wait for state change notices either entry or exit."
    })
}

struct Schedule;
fn in_window(c: &Value) -> Result<bool, String> {
    let get = |k: &str, d: &str| {
        let t = text(c, k);
        if t.is_empty() { d.to_string() } else { t }
    };
    let s = spec::parse_hhmm(&get("start", "09:00")).ok_or("Start time must look like 09:00")?;
    let e = spec::parse_hhmm(&get("end", "17:00")).ok_or("End time must look like 17:00")?;
    let (dow, now) = sys::local_weekday_minutes();
    let day_ok = match text(c, "days").as_str() {
        "weekdays" => (1..=5).contains(&dow),
        "weekends" => dow == 0 || dow == 6,
        _ => true,
    };
    let time_ok = s == e || if s < e { now >= s && now < e } else { now >= s || now < e };
    Ok(day_ok && time_ok)
}
#[async_trait]
impl Block for Schedule {
    fn title(&self) -> &'static str {
        "Time window"
    }
    fn category(&self) -> Cat {
        Cat::Condition
    }
    fn supports_timeout(&self) -> bool {
        true
    }
    fn spec(&self) -> Option<Value> {
        Some(schedule_spec())
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&schedule_spec(), c)
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;
        let mode = text(&cx.cfg, "mode");
        let behavior = match mode.as_str() { "wait" => "wait", "wait_change" => "change", _ => "check" };
        let poll = rng(&cx.cfg, "pollSec", 5.0, 1.0, 3600.0, "Check interval")?;
        let stable = rng(&cx.cfg, "stableForSec", 0.0, 0.0, 3600.0, "Stable duration")?;
        let timeout = rng(&cx.cfg, "timeoutSec", 0.0, 0.0, 86400.0, "Timeout")?;
        extra::poll_condition(cx, behavior, poll, stable, timeout, || {
            Box::pin(async {
                let inside = in_window(&cx.cfg)?;
                (cx.report)(if behavior == "check" { "running" } else { "waiting" }, None,
                    if inside { "inside the window".to_string() } else { "outside the window".to_string() });
                Ok(inside)
            }) as extra::Probe<'_>
        }).await
    }
}
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
    fn title(&self) -> &'static str { "Start" }
    fn category(&self) -> Cat { Cat::Action }
    fn validate(&self, _c: &Value) -> Result<(), String> { Ok(()) }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        (cx.log)("info", "Workflow entry reached".into());
        Ok(Outcome::Satisfied)
    }
}

struct LaunchFile;
#[async_trait]
impl Block for LaunchFile {
    fn title(&self) -> &'static str { "Launch File" }
    fn category(&self) -> Cat { Cat::Action }
    fn requires_target(&self) -> Option<&'static str> { Some("tgt.file") }
    fn supports_timeout(&self) -> bool { true }
    fn validate(&self, c: &Value) -> Result<(), String> { check_timeout(c) }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let path = (cx.resolve)()?.into_iter()
            .find_map(|t| if let Target::File { path } = t { Some(path) } else { None })
            .ok_or("no File Input is connected to Launch File")?;
        if !std::path::Path::new(&path).exists() {
            return Err(format!("file not found: {path}"));
        }
        let args = cx.expand_template(&text(&cx.cfg, "args"))?;
        (cx.report)("running", None, format!("Launching {path}"));
        let p2 = path.clone();
        tokio::task::spawn_blocking(move || sys::start_file(&p2, &args))
            .await.map_err(|e| e.to_string())??;
        (cx.log)("info", format!("launched {path}"));
        Ok(Outcome::Satisfied)
    }
}

fn run_command_spec() -> Value {
    json!({
        "category": "action",
        "title": "Run Command",
        "icon": "proc",
        "description": "Runs a PowerShell command in the background without opening a terminal window.",
        "fields": [
            {"key":"command","label":"PowerShell command","type":"textarea","rows":5,"required":true,"maxLen":8000},
            {"key":"workingDirectory","label":"Working directory (optional)","type":"text","maxLen":1000,"default":""},
            {"key":"saveOutputAs","label":"Save stdout to variable (optional)","type":"text","maxLen":64},
            {"preset":"timeout","default":30},
            {"preset":"onFail"}
        ],
        "summary": "Run PowerShell command",
        "help": "Runs powershell.exe without a console window. Standard output and standard error are captured in workflow logs, up to 16 KiB each. Commands run with the current Windows user's permissions and are not sandboxed. Use {{variable_name}} placeholders to insert variables; saved stdout is capped at 16 KiB. Leave working directory blank to inherit 00047's working directory. Timeout 0 disables the timeout."
    })
}

struct RunCommand;
#[async_trait]
impl Block for RunCommand {
    fn title(&self) -> &'static str { "Run Command" }
    fn category(&self) -> Cat { Cat::Action }
    fn supports_timeout(&self) -> bool { true }
    fn risky(&self) -> bool { true }
    fn spec(&self) -> Option<Value> { Some(run_command_spec()) }

    fn validate(&self, c: &Value) -> Result<(), String> {
        spec::validate(&run_command_spec(), c)?;
        if text(c, "command").contains('\0') {
            return Err("PowerShell command must not contain NUL characters.".into());
        }
        if text(c, "workingDirectory").contains('\0') {
            return Err("Working directory must not contain NUL characters.".into());
        }
        if text(c, "saveOutputAs").contains('\0') {
            return Err("Output variable name must not contain NUL characters.".into());
        }
        if !text(c, "saveOutputAs").is_empty() {
            let name = text(c, "saveOutputAs");
            if name.len() > 64 || !name.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
                || !name.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')) {
                return Err("Output variable name is invalid".into());
            }
        }
        Ok(())
    }

    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        self.validate(&cx.cfg)?;

        #[cfg(not(windows))]
        {
            let _ = cx;
            return Err("Run Command is supported only on Windows.".into());
        }

        #[cfg(windows)]
        {
            let command = cx.expand_template(&text(&cx.cfg, "command"))?;
            let script = format!(
                "$ErrorActionPreference = 'Stop'; try {{\n{}\n}} catch {{ [Console]::Error.WriteLine($_.ToString()); exit 1 }}; if ($null -ne $LASTEXITCODE) {{ exit $LASTEXITCODE }}",
                command
            );

            let mut cmd = tokio::process::Command::new("powershell.exe");
            cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script.as_str()])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);

            let working_directory = cx.expand_template(&text(&cx.cfg, "workingDirectory"))?;
            if !working_directory.is_empty() {
                cmd.current_dir(&working_directory);
            }

            {
                use std::os::windows::process::CommandExt;
                cmd.as_std_mut().creation_flags(0x0800_0000); // CREATE_NO_WINDOW
            }

            (cx.report)("running", None, "Executing PowerShell command (console hidden)".into());
            let mut child = cmd.spawn().map_err(|e| format!("could not start PowerShell command: {e}"))?;
            let stdout = child.stdout.take().ok_or_else(|| "PowerShell stdout pipe was unavailable".to_string())?;
            let stderr = child.stderr.take().ok_or_else(|| "PowerShell stderr pipe was unavailable".to_string())?;
            let stdout_reader = tokio::spawn(read_bounded_output(stdout, MAX_COMMAND_OUTPUT_BYTES));
            let stderr_reader = tokio::spawn(read_bounded_output(stderr, MAX_COMMAND_OUTPUT_BYTES));

            let status = tokio::select! {
                result = child.wait() => result.map_err(|e| format!("could not wait for PowerShell command: {e}"))?,
                _ = cx.token.cancelled() => return Err("cancelled".into()),
            };

            let (stdout_bytes, stdout_truncated) = stdout_reader
                .await.map_err(|e| format!("stdout capture task failed: {e}"))??;
            let (stderr_bytes, stderr_truncated) = stderr_reader
                .await.map_err(|e| format!("stderr capture task failed: {e}"))??;
            let stdout = String::from_utf8_lossy(&stdout_bytes).trim_end().to_string();
            let stderr = String::from_utf8_lossy(&stderr_bytes).trim_end().to_string();

            if !stdout.is_empty() {
                (cx.log)("info", format!("stdout:\n{stdout}"));
            }
            if !stderr.is_empty() {
                (cx.log)(if status.success() { "warn" } else { "error" }, format!("stderr:\n{stderr}"));
            }
            if stdout_truncated {
                (cx.log)("warn", format!("stdout capture truncated at {MAX_COMMAND_OUTPUT_BYTES} bytes"));
            }
            if stderr_truncated {
                (cx.log)("warn", format!("stderr capture truncated at {MAX_COMMAND_OUTPUT_BYTES} bytes"));
            }

            if status.success() {
                let save_as = text(&cx.cfg, "saveOutputAs");
                if !save_as.is_empty() {
                    cx.set_variable(save_as, Value::String(stdout.clone()))?;
                }
                Ok(Outcome::Satisfied)
            } else {
                let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "terminated".into());
                let detail: String = stderr.chars().take(512).collect();
                Err(format!(
                    "PowerShell exited with code {code}{}",
                    if detail.is_empty() { String::new() } else { format!(": {detail}") }
                ))
            }
        }
    }
}

struct StopProc;
#[async_trait]
impl Block for StopProc {
    fn title(&self) -> &'static str { "Stop Process" }
    fn category(&self) -> Cat { Cat::Action }
    fn supports_timeout(&self) -> bool { true }
    fn validate(&self, c: &Value) -> Result<(), String> {
        if text(c, "mode") != "target" && text(c, "name").is_empty() {
            return Err("Process name is required".into());
        }
        check_timeout(c)
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let force = flag(&cx.cfg, "force", false);
        let (pid, name) = if text(&cx.cfg, "mode") == "target" {
            first_process(cx)?
        } else {
            let n = cx.expand_template(&text(&cx.cfg, "name"))?;
            (sys::find_pid_by_name(&n)?, n)
        };
        if pid == sys::own_pid() { return Err("refusing to stop 00047 itself".into()); }
        (cx.report)("running", None, format!("Stopping {name} ({pid})"));
        let msg = tokio::task::spawn_blocking(move || sys::stop_process(pid, force))
            .await.map_err(|e| e.to_string())??;
        (cx.log)("info", format!("{name} ({pid}): {msg}"));
        Ok(Outcome::Satisfied)
    }
}

struct Notify;
#[async_trait]
impl Block for Notify {
    fn title(&self) -> &'static str { "Notification" }
    fn category(&self) -> Cat { Cat::Action }
    fn validate(&self, c: &Value) -> Result<(), String> {
        let t = text(c, "title");
        let b = text(c, "body");
        if t.is_empty() || b.is_empty() { return Err("Title and message are required".into()); }
        if t.chars().count() > 64 || b.chars().count() > 256 {
            return Err("Title max 64 and message max 256 characters".into());
        }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let t = cx.expand_template(&text(&cx.cfg, "title"))?;
        let b = cx.expand_template(&text(&cx.cfg, "body"))?;
        let s = flag(&cx.cfg, "sound", true);
        if t.is_empty() || b.is_empty() { return Err("Title and message are required after variable expansion".into()); }
        if t.chars().count() > 64 || b.chars().count() > 256 {
            return Err("Expanded title max 64 and message max 256 characters".into());
        }
        tokio::task::spawn_blocking(move || sys::show_notification(&t, &b, s))
            .await.map_err(|e| e.to_string())??;
        Ok(Outcome::Satisfied)
    }
}

/// Loop: each time it is reached it counts a visit. While visit <= iterations (or iterations = 0)
/// it waits `delaySec` and is Satisfied (wire IF to the loop body, and the body back to this block).
/// After the last iteration it is NotSatisfied (wire ELSE to whatever runs after the loop).
struct Loop;
#[async_trait]
impl Block for Loop {
    fn title(&self) -> &'static str { "Loop" }
    fn category(&self) -> Cat { Cat::Action }
    fn validate(&self, _c: &Value) -> Result<(), String> { Ok(()) }
    async fn run(&self, _cx: &NodeCx) -> Result<Outcome, String> { Ok(Outcome::Satisfied) }
}

struct LogMsg;
#[async_trait]
impl Block for LogMsg {
    fn title(&self) -> &'static str {
        "Log message"
    }
    fn category(&self) -> Cat {
        Cat::Action
    }
    fn validate(&self, c: &Value) -> Result<(), String> {
        let m = text(c, "message");
        if m.is_empty() {
            return Err("Message is required".into());
        }
        if m.chars().count() > 500 {
            return Err("Message must be 500 characters or fewer".into());
        }
        Ok(())
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let lvl = match text(&cx.cfg, "level").as_str() {
            "warn" => "warn",
            "error" => "error",
            _ => "info",
        };
        let message = cx.expand_template(&text(&cx.cfg, "message"))?;
        if message.is_empty() { return Err("Message is required after variable expansion".into()); }
        if message.chars().count() > 500 { return Err("Expanded message must be 500 characters or fewer".into()); }
        (cx.log)(lvl, message);
        Ok(Outcome::Satisfied)
    }
}

// ---------- Targets ----------
struct ProcessTarget;
#[async_trait]
impl Block for ProcessTarget {
    fn title(&self) -> &'static str {
        "Process Input"
    }
    fn category(&self) -> Cat {
        Cat::Target
    }
    async fn run(&self, _cx: &NodeCx) -> Result<Outcome, String> {
        // A Target is a pass-through workflow step; consumers resolve its value when they run.
        Ok(Outcome::Satisfied)
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
#[async_trait]
impl Block for FileTarget {
    fn title(&self) -> &'static str {
        "File Input"
    }
    fn category(&self) -> Cat {
        Cat::Target
    }
    async fn run(&self, _cx: &NodeCx) -> Result<Outcome, String> {
        // A Target is a pass-through workflow step; consumers resolve its value when they run.
        Ok(Outcome::Satisfied)
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
        m.insert("cond.proc_running", Box::new(ProcRunning));
        m.insert("cond.visible_windows", Box::new(VisibleWindows));
        m.insert("cond.process_audio", Box::new(ProcessAudio));
        m.insert("act.trim", Box::new(Trim));
        m.insert("act.stop_proc", Box::new(StopProc));
        m.insert("act.wait", Box::new(Wait));
        m.insert("act.start", Box::new(Start));
        m.insert("act.launch", Box::new(LaunchFile));
        m.insert("act.command", Box::new(RunCommand));
        m.insert("act.loop", Box::new(Loop));
        m.insert("act.log", Box::new(LogMsg));
        m.insert("act.notify", Box::new(Notify));
        m.insert("tgt.process", Box::new(ProcessTarget));
        m.insert("tgt.file", Box::new(FileTarget));
        m.insert("cond.schedule", Box::new(Schedule));
        extra::register_into(&mut m);
        extra_more::register_into(&mut m);
        advanced::register_into(&mut m);
        nextgen::register_into(&mut m);
        crate::custom::load_into(&mut m);
        m
    })
}


#[cfg(test)]
mod input_condition_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn run_command_is_registered_as_a_risky_timed_action() {
        let command = registry().get("act.command").unwrap();
        assert_eq!(command.title(), "Run Command");
        assert_eq!(command.category(), Cat::Action);
        assert!(command.supports_timeout());
        assert!(command.risky());
        assert!(command.validate(&json!({
            "command": "Get-Date",
            "workingDirectory": "",
            "timeoutSec": 30,
            "onFail": "stop"
        })).is_ok());
        assert!(command.validate(&json!({
            "command": "   ",
            "workingDirectory": "",
            "timeoutSec": 30,
            "onFail": "stop"
        })).is_err());

        let nul_command = format!("Get-Date{}", '\0');
        assert!(command.validate(&json!({
            "command": nul_command,
            "workingDirectory": "",
            "timeoutSec": 30,
            "onFail": "stop"
        })).is_err());

        let too_long = "x".repeat(8001);
        assert!(command.validate(&json!({
            "command": too_long,
            "workingDirectory": "",
            "timeoutSec": 30,
            "onFail": "stop"
        })).is_err());
    }

    #[test]
    fn visible_window_comparisons_cover_all_modes() {
        assert!(window_count_matches("at_least", 3, 2));
        assert!(!window_count_matches("at_least", 1, 2));
        assert!(window_count_matches("at_most", 1, 2));
        assert!(!window_count_matches("at_most", 3, 2));
        assert!(window_count_matches("exactly", 2, 2));
        assert!(!window_count_matches("exactly", 3, 2));
    }

    #[test]
    fn visible_window_count_rejects_fractional_values() {
        assert_eq!(expected_window_count(&json!({"count": 4})).unwrap(), 4);
        assert!(expected_window_count(&json!({"count": 4.5})).is_err());
    }

    #[test]
    fn new_conditions_require_process_input_and_validate_specs() {
        let visible = registry().get("cond.visible_windows").unwrap();
        assert_eq!(visible.category(), Cat::Condition);
        assert_eq!(visible.requires_target(), Some("tgt.process"));
        assert!(visible.validate(&json!({"op":"at_least","count":2,"onFail":"stop"})).is_ok());
        assert!(visible.validate(&json!({"op":"unknown","count":2,"onFail":"stop"})).is_err());

        let audio = registry().get("cond.process_audio").unwrap();
        assert_eq!(audio.category(), Cat::Condition);
        assert_eq!(audio.requires_target(), Some("tgt.process"));
        assert!(audio.validate(&json!({"state":"outputting","onFail":"stop"})).is_ok());
        assert!(audio.validate(&json!({"state":"unknown","onFail":"stop"})).is_err());
    }
}


#[cfg(test)]
mod expanded_blocks_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expanded_default_blocks_are_registered_with_specs() {
        let expected: [(&str, Cat); 24] = [
            ("cond.network", Cat::Condition),
            ("cond.battery_power", Cat::Condition),
            ("cond.cpu_usage", Cat::Condition),
            ("cond.disk_space", Cat::Condition),
            ("cond.file_folder", Cat::Condition),
            ("cond.active_window", Cat::Condition),
            ("cond.bluetooth", Cat::Condition),
            ("cond.usb_device", Cat::Condition),
            ("cond.audio_device", Cat::Condition),
            ("cond.workstation_lock", Cat::Condition),
            ("cond.user_idle", Cat::Condition),
            ("cond.app_responsive", Cat::Condition),
            ("act.open_url", Cat::Action),
            ("act.set_volume", Cat::Action),
            ("act.process_priority", Cat::Action),
            ("act.control_window", Cat::Action),
            ("act.copy_move_file", Cat::Action),
            ("act.clipboard", Cat::Action),
            ("act.http", Cat::Action),
            ("act.media", Cat::Action),
            ("act.power", Cat::Action),
            ("act.delete_file", Cat::Action),
            ("act.play_sound", Cat::Action),
            ("act.service", Cat::Action),
        ];
        for (id, category) in expected {
            let block = registry().get(id).unwrap_or_else(|| panic!("missing built-in block {id}"));
            assert_eq!(block.category(), category, "wrong category for {id}");
            assert!(block.spec().is_some(), "missing declarative spec for {id}");
        }
    }

    #[test]
    fn expanded_condition_specs_validate_defaults_and_reject_bad_inputs() {
        let r = registry();
        assert!(r["cond.network"].validate(&json!({"mode":"wifi_connected","behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.network"].validate(&json!({"mode":"bogus","behavior":"check","onFail":"stop"})).is_err());
        assert!(r["cond.battery_power"].validate(&json!({"mode":"charging","behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.cpu_usage"].validate(&json!({"scope":"system","op":"above","thresholdPct":80,"behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.disk_space"].validate(&json!({"drive":"C:","metric":"free_pct","op":"below","threshold":15,"behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.file_folder"].validate(&json!({"path":"C:\\temp\\done.txt","state":"exists","behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.active_window"].validate(&json!({"match":"process","pattern":"notepad","state":"active","behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.bluetooth"].validate(&json!({"deviceName":"headset","state":"present","behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.usb_device"].validate(&json!({"deviceName":"flash","state":"present","behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.audio_device"].validate(&json!({"deviceName":"speaker","state":"present","behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.workstation_lock"].validate(&json!({"state":"locked","behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.user_idle"].validate(&json!({"state":"idle","seconds":300,"behavior":"check","onFail":"stop"})).is_ok());
        assert!(r["cond.app_responsive"].validate(&json!({"processName":"notepad","state":"responsive","behavior":"check","onFail":"stop"})).is_ok());
    }

    #[test]
    fn legacy_conditions_validate_wait_and_state_change_modes() {
        let r = registry();
        assert!(r["cond.ram"].validate(&json!({"op":"above","thresholdMb":800,"mode":"wait_change","pollSec":1,"stableForSec":0.5,"timeoutSec":10})).is_ok());
        assert!(r["cond.proc_running"].validate(&json!({"name":"notepad.exe","mode":"wait_change","pollSec":1,"stableForSec":0.5,"timeoutSec":10})).is_ok());
        assert!(r["cond.schedule"].validate(&json!({"start":"09:00","end":"17:00","days":"all","mode":"wait_change","pollSec":5,"stableForSec":1,"timeoutSec":30,"onFail":"stop"})).is_ok());
    }

    #[test]
    fn expanded_action_specs_validate_safe_baselines() {
        let r=registry();
        assert!(r["act.open_url"].validate(&json!({"url":"https://example.com","onFail":"stop"})).is_ok());
        assert!(r["act.open_url"].validate(&json!({"url":"file:///C:/Windows","onFail":"stop"})).is_err());
        assert!(r["act.set_volume"].validate(&json!({"mode":"set","percent":50,"onFail":"stop"})).is_ok());
        assert!(r["act.process_priority"].validate(&json!({"processName":"notepad","priority":"Normal","onFail":"stop"})).is_ok());
        assert!(r["act.control_window"].validate(&json!({"processName":"notepad","operation":"restore","onFail":"stop"})).is_ok());
        assert!(r["act.copy_move_file"].validate(&json!({"operation":"copy","source":"C:\\a.txt","destination":"C:\\b.txt","collision":"fail","onFail":"stop"})).is_ok());
        assert!(r["act.clipboard"].validate(&json!({"text":"hello","onFail":"stop"})).is_ok());
        assert!(r["act.http"].validate(&json!({"url":"https://example.com","method":"GET","headersJson":"","timeoutSec":15,"onFail":"stop"})).is_ok());
        assert!(r["act.http"].validate(&json!({"url":"file:///C:/secret","method":"GET","headersJson":"","timeoutSec":15,"onFail":"stop"})).is_err());
        assert!(r["act.media"].validate(&json!({"operation":"play_pause","onFail":"stop"})).is_ok());
        assert!(r["act.power"].validate(&json!({"operation":"lock","delaySec":60,"onFail":"stop"})).is_ok());
        assert!(r["act.delete_file"].validate(&json!({"path":"C:\\temp\\old.txt","mode":"recycle","onFail":"stop"})).is_ok());
        assert!(r["act.play_sound"].validate(&json!({"mode":"speak","text":"hello","onFail":"stop"})).is_ok());
        assert!(r["act.service"].validate(&json!({"serviceName":"Spooler","operation":"stop","onFail":"stop","timeoutSec":0})).is_ok());
    }
}
