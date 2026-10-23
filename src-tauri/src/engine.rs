use crate::{
    blocks::{registry, Cat, NodeCx, Outcome, Target},
    model::*,
    store::Store,
};
use crate::logstore::LogStore;
use crate::vault::SecretVault;
use futures::future::BoxFuture;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

const LOG_CAP: usize = 5000;
const HIST_CAP: usize = 50;

struct RunHandle {
    token: CancellationToken,
    run_id: u64,
    debug: Arc<DebugControl>,
}

#[derive(Default)]
struct DebugControl {
    paused: AtomicBool,
    step_budget: AtomicU64,
    step_active: AtomicBool,
    notify: tokio::sync::Notify,
}

impl DebugControl {
    fn pause(&self) {
        self.step_budget.store(0, Ordering::SeqCst);
        self.paused.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    fn resume(&self) {
        self.step_budget.store(0, Ordering::SeqCst);
        self.step_active.store(false, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    fn step(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.step_budget.store(1, Ordering::SeqCst);
        self.notify.notify_waiters();
    }
    fn is_paused(&self) -> bool { self.paused.load(Ordering::SeqCst) }

    async fn before_node(&self, breakpoint: bool, token: &CancellationToken) -> bool {
        if breakpoint {
            self.pause();
        }
        loop {
            if token.is_cancelled() { return false; }
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.paused.load(Ordering::SeqCst) { return true; }
            let mut budget = self.step_budget.load(Ordering::SeqCst);
            while budget > 0 {
                match self.step_budget.compare_exchange(budget, budget - 1, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) => {
                        self.step_active.store(true, Ordering::SeqCst);
                        return true;
                    }
                    Err(now) => budget = now,
                }
            }
            tokio::select! {
                _ = token.cancelled() => return false,
                _ = notified => {}
            }
        }
    }

    fn after_node(&self) {
        if self.step_active.swap(false, Ordering::SeqCst) {
            self.paused.store(true, Ordering::SeqCst);
            self.notify.notify_waiters();
        }
    }
}

#[derive(Serialize)]
pub struct Runtime {
    pub autos: HashMap<String, AutoRuntime>,
    pub nodes: HashMap<String, HashMap<String, NodeState>>,
}

pub struct Engine {
    app: AppHandle,
    pub store: Arc<Store>,
    vault: Arc<SecretVault>,
    runs: Mutex<HashMap<String, RunHandle>>,
    autos: Mutex<HashMap<String, AutoRuntime>>,
    nodes: Mutex<HashMap<String, HashMap<String, NodeState>>>,
    logs: Mutex<VecDeque<LogEntry>>,
    hist: Mutex<HashMap<String, VecDeque<RunRecord>>>,
    seq: AtomicU64,
    run_seq: AtomicU64,
    ui: AtomicBool,
    disk: LogStore,
    vault: Arc<SecretVault>,
}

impl Engine {
    pub fn new(app: AppHandle, store: Arc<Store>, dir: &std::path::Path) -> Self {
        let (disk, old_logs) = LogStore::open(dir, LOG_CAP);
        let hist = LogStore::load_history(dir);
        let max_run = hist.values().flat_map(|q| q.iter().map(|r| r.id)).max().unwrap_or(0);
        let mut seq = 0u64;
        let logs: VecDeque<LogEntry> = old_logs
            .into_iter()
            .map(|mut e| {
                seq += 1;
                e.seq = seq;
                e
            })
            .collect();
        Engine {
            app,
            store,
            vault: Arc::new(SecretVault::new(dir.join("secrets.json"))),
            runs: Default::default(),
            autos: Default::default(),
            nodes: Default::default(),
            logs: Mutex::new(logs),
            hist: Mutex::new(hist),
            seq: AtomicU64::new(seq),
            run_seq: AtomicU64::new(max_run),
            ui: AtomicBool::new(false),
            disk,
        }
    }

    // ----- UI visibility (live events are only sent while the window is visible) -----
    pub fn visible(&self) -> bool {
        self.ui.load(Ordering::Relaxed)
    }
    pub fn set_ui(&self, v: bool) {
        let old = self.ui.swap(v, Ordering::SeqCst);
        if v && !old {
            let _ = self.app.emit("ui-shown", ());
        } else if !v && old {
            let _ = self.app.emit("ui-hidden", ());
        }
    }

    // ----- logging -----
    pub fn log(&self, level: &str, auto_id: &str, auto_name: &str, node: Option<(String, String)>, msg: String) {
        let e = LogEntry {
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            ts: now_ms(),
            level: level.into(),
            auto_id: auto_id.into(),
            auto_name: auto_name.into(),
            node_id: node.as_ref().map(|n| n.0.clone()),
            node_label: node.map(|n| n.1),
            msg,
        };
        {
            let mut l = self.logs.lock().unwrap();
            if l.len() >= LOG_CAP {
                l.pop_front();
            }
            l.push_back(e.clone());
        }
        self.disk.append(&e);
        if self.visible() {
            let _ = self.app.emit("log", &e);
        }
    }

    pub fn app_log(&self, level: &str, msg: String) {
        self.log(level, "", "Application", None, msg);
    }

    pub fn secret_names(&self) -> Vec<String> { self.vault.names() }
    pub fn store_secret(&self, name: &str, value: &str) -> Result<(), String> { self.vault.set(name, value) }
    pub fn delete_secret(&self, name: &str) -> Result<bool, String> { self.vault.delete(name) }
    pub fn logs(&self, since: u64, auto: Option<String>) -> Vec<LogEntry> {
        self.logs
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.seq > since && auto.as_ref().map(|a| &e.auto_id == a).unwrap_or(true))
            .cloned()
            .collect()
    }
    pub fn clear_logs(&self) {
        self.logs.lock().unwrap().clear();
        self.disk.clear();
    }

    // ----- state -----
    fn set_node(&self, auto_id: &str, node_id: &str, state: &str, progress: Option<f64>, detail: String) {
        let st = NodeState { state: state.into(), progress, detail };
        self.nodes.lock().unwrap().entry(auto_id.into()).or_default().insert(node_id.into(), st.clone());
        if self.visible() {
            let _ = self.app.emit("node-state", json!({"autoId": auto_id, "nodeId": node_id, "state": st}));
        }
    }
    fn set_auto(&self, id: &str, f: impl FnOnce(&mut AutoRuntime)) {
        let rt = {
            let mut m = self.autos.lock().unwrap();
            let r = m.entry(id.into()).or_default();
            f(r);
            r.clone()
        };
        let _ = self.app.emit("auto-state", json!({"id": id, "runtime": rt}));
    }
    pub fn runtime(&self) -> Runtime {
        Runtime { autos: self.autos.lock().unwrap().clone(), nodes: self.nodes.lock().unwrap().clone() }
    }
    pub fn history(&self, id: &str) -> Vec<RunRecord> {
        self.hist.lock().unwrap().get(id).map(|h| h.iter().cloned().collect()).unwrap_or_default()
    }

    // ----- control -----
    pub fn start(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let auto = self.store.get(id).ok_or_else(|| "automation not found".to_string())?;
        if let Err(errs) = validate_automation(&auto) {
            for e in &errs {
                self.log("error", &auto.id, &auto.name, None, format!("Cannot start: {e}"));
            }
            return Err(errs.join("\n"));
        }
        let token = CancellationToken::new();
        let debug = Arc::new(DebugControl::default());
        let run_id = self.run_seq.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut r = self.runs.lock().unwrap();
            if r.contains_key(id) {
                return Err("already running".into());
            }
            r.insert(id.to_string(), RunHandle { token: token.clone(), run_id, debug });
        }
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            me.run_automation(auto, token, run_id).await;
        });
        Ok(())
    }

    pub fn stop(&self, id: &str) {
        let handle = self.runs.lock().unwrap().get(id).map(|r| (r.token.clone(), r.debug.clone()));
        if let Some((token, debug)) = handle {
            debug.resume();
            token.cancel();
            self.set_auto(id, |r| r.status = "stopping".into());
        }
    }

    pub fn pause(&self, id: &str) -> Result<(), String> {
        let debug = self.runs.lock().unwrap().get(id).map(|r| r.debug.clone()).ok_or_else(|| "automation is not running".to_string())?;
        debug.pause();
        self.set_auto(id, |r| r.status = "paused".into());
        Ok(())
    }
    pub fn resume(&self, id: &str) -> Result<(), String> {
        let debug = self.runs.lock().unwrap().get(id).map(|r| r.debug.clone()).ok_or_else(|| "automation is not running".to_string())?;
        debug.resume();
        self.set_auto(id, |r| r.status = "running".into());
        Ok(())
    }
    pub fn step(&self, id: &str) -> Result<(), String> {
        let debug = self.runs.lock().unwrap().get(id).map(|r| r.debug.clone()).ok_or_else(|| "automation is not running".to_string())?;
        debug.step();
        self.set_auto(id, |r| r.status = "paused".into());
        Ok(())
    }

    pub async fn stop_and_wait(&self, id: &str) -> Result<(), String> {
        self.stop(id);
        for _ in 0..200 {
            if !self.runs.lock().unwrap().contains_key(id) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.runs.lock().unwrap().contains_key(id) {
            Err(format!("automation {id} did not stop within 10 seconds; it has not been restarted or deleted"))
        } else {
            Ok(())
        }
    }

    pub async fn restart(self: &Arc<Self>, id: &str) -> Result<(), String> {
        self.stop_and_wait(id).await?;
        self.start(id)
    }

    /// Runs one block using its current settings and wired targets, without traversing flow wires.
    pub async fn test_block(self: &Arc<Self>, auto: Automation, node_id: String) -> Result<String, String> {
        let node = auto.nodes.iter().find(|n| n.id == node_id).cloned().ok_or_else(|| "block not found".to_string())?;
        let blk = registry().get(node.kind.as_str()).ok_or_else(|| "unknown block type".to_string())?;
        if blk.category() == Cat::Target {
            return match blk.resolve(&node.cfg)? {
                Target::Process { pid, name } => Ok(format!("resolves to {name} (pid {pid})")),
                Target::File { path } => Ok(format!("file: {path}")),
            };
        }
        blk.validate(&node.cfg)?;
        let auto = Arc::new(auto);
        let short_id = node.id.chars().take(4).collect::<String>();
        let label = format!("{} #{short_id} (test)", blk.title());
        let test_secrets = Arc::new(Mutex::new(Vec::new()));
        let cx = NodeCx {
            cfg: node.cfg.clone(),
            token: CancellationToken::new(),
            visit: 1,
            report: Box::new(|_: &str, _: Option<f64>, _: String| {}),
            log: {
                let me = self.clone();
                let (aid, name, nid, node_label) = (auto.id.clone(), auto.name.clone(), node.id.clone(), label.clone());
                Box::new(move |lv: &str, m: String| me.log(lv, &aid, &name, Some((nid.clone(), node_label.clone())), m))
            },
            resolve: {
                let a = auto.clone();
                let nid = node.id.clone();
                Box::new(move || resolve_targets(&a, &nid))
            },
            visible: Box::new(|| false),
            variables: Arc::new(Mutex::new(HashMap::new())),
            secret_reader: {
                let vault = self.vault.clone();
                Arc::new(move |name: &str| vault.get(name))
            },
            sensitive_values: test_secrets.clone(),
            subflow_return: Arc::new(Mutex::new(None)),
        };
        let out = tokio::time::timeout(Duration::from_secs(60), blk.run(&cx))
            .await
            .map_err(|_| "test timed out after 60 s".to_string())??;
        let txt = match out {
            Outcome::Satisfied => "satisfied (a real run would follow IF + AFTER wires)",
            Outcome::NotSatisfied => "not satisfied (a real run would follow ELSE + AFTER wires)",
        };
        self.log("info", &auto.id, &auto.name, Some((node.id.clone(), label)), format!("Test run: {txt}"));
        Ok(txt.to_string())
    }

    pub async fn shutdown(&self) {
        let ids: Vec<String> = self.runs.lock().unwrap().keys().cloned().collect();
        for id in &ids {
            self.stop(id);
        }
        for _ in 0..200 {
            if self.runs.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if !self.runs.lock().unwrap().is_empty() {
            self.app_log("error", "Shutdown timed out waiting for one or more automations to stop".into());
        }
        self.disk.flush();
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.store.remove(id)?;
        self.autos.lock().unwrap().remove(id);
        self.nodes.lock().unwrap().remove(id);
        self.hist.lock().unwrap().remove(id);
        self.disk.save_history(&self.hist.lock().unwrap());
        Ok(())
    }

    pub fn start_autostart(self: &Arc<Self>) {
        for a in self.store.all() {
            if a.auto_start {
                if let Err(e) = self.start(&a.id) {
                    self.app_log("error", format!("Could not auto-start '{}': {e}", a.name));
                }
            }
        }
    }

    // ----- run -----
    async fn run_automation(self: Arc<Self>, auto: Automation, parent: CancellationToken, run_id: u64) {
        let started = now_ms();
        self.nodes.lock().unwrap().remove(&auto.id);
        self.set_auto(&auto.id, |r| {
            r.status = "running".into();
            r.started_at = Some(started);
            r.last_run = Some(started);
        });
        self.log("info", &auto.id, &auto.name, None, "Automation started".into());
        let entries = entry_nodes(&auto);
        let debug = {
            let runs = self.runs.lock().unwrap();
            runs.get(&auto.id).map(|h| h.debug.clone()).unwrap_or_else(|| Arc::new(DebugControl::default()))
        };
        let root_id = auto.id.clone();
        let ctx = Arc::new(Ctx {
            eng: self.clone(),
            auto: Arc::new(auto),
            token: parent.child_token(),
            visits: Default::default(),
            variables: Arc::new(Mutex::new(HashMap::new())),
            secret_values: Arc::new(Mutex::new(Vec::new())),
            trace: Arc::new(Mutex::new(Vec::new())),
            debug,
            call_stack: vec![root_id],
            returned: AtomicBool::new(false),
            subflow_return: Arc::new(Mutex::new(None)),
            rate: Default::default(),
            handles: Default::default(),
            errors: AtomicU32::new(0),
            aborted: AtomicBool::new(false),
            single: entries.len() == 1,
        });
        let hs: Vec<_> = entries.into_iter().map(|e| tokio::spawn(run_entry(ctx.clone(), e))).collect();
        for h in hs {
            let _ = h.await;
        }
        let stopped = parent.is_cancelled();
        let aborted = ctx.aborted.load(Ordering::SeqCst);
        let errors = ctx.errors.load(Ordering::SeqCst);
        let outcome = if stopped { "stopped" } else if aborted { "failed" } else { "completed" };
        let ended = now_ms();
        let aid = ctx.auto.id.clone();
        if let Some(m) = self.nodes.lock().unwrap().get_mut(&aid) {
            for s in m.values_mut() {
                if s.state == "running" || s.state == "waiting" {
                    s.state = "idle".into();
                    s.progress = None;
                }
            }
        }
        {
            let mut h = self.hist.lock().unwrap();
            let q = h.entry(aid.clone()).or_default();
            let trace = ctx.trace.lock().unwrap().clone();
            q.push_front(RunRecord { id: run_id, started_at: started, ended_at: ended, outcome: outcome.into(), errors, trace });
            q.truncate(HIST_CAP);
            self.disk.save_history(&h);
        }
        {
            let mut r = self.runs.lock().unwrap();
            if r.get(&aid).map(|h| h.run_id == run_id).unwrap_or(false) {
                r.remove(&aid);
            }
        }
        let label = if errors > 0 { format!("{outcome} ({errors} errors)") } else { outcome.to_string() };
        self.set_auto(&aid, |r| {
            r.status = outcome.into();
            r.started_at = None;
            r.last_outcome = Some(label.clone());
        });
        let lvl = if aborted { "error" } else { "info" };
        self.log(lvl, &aid, &ctx.auto.name, None, format!("Automation {label} after {:.1}s", (ended - started) as f64 / 1000.0));
    }
}

struct Ctx {
    eng: Arc<Engine>,
    auto: Arc<Automation>,
    token: CancellationToken,
    visits: Mutex<HashMap<String, u64>>,
    variables: Arc<Mutex<HashMap<String, Value>>>,
    secret_values: Arc<Mutex<Vec<String>>>,
    trace: Arc<Mutex<Vec<NodeTrace>>>,
    debug: Arc<DebugControl>,
    call_stack: Vec<String>,
    returned: AtomicBool,
    subflow_return: Arc<Mutex<Option<Value>>>,
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
    errors: AtomicU32,
    aborted: AtomicBool,
    single: bool,
}

fn redact_secrets(message: &str, values: &Arc<Mutex<Vec<String>>>) -> String {
    let mut secrets = values.lock().map(|v| v.clone()).unwrap_or_default();
    secrets.sort_by_key(|v| std::cmp::Reverse(v.len()));
    secrets.into_iter().filter(|v| !v.is_empty()).fold(message.to_string(), |text, secret| text.replace(&secret, "[REDACTED]"))
}

impl Ctx {
    fn node(&self, id: &str) -> Option<Node> {
        self.auto.nodes.iter().find(|n| n.id == id).cloned()
    }
    fn label(&self, n: &Node) -> String {
        let t = registry().get(n.kind.as_str()).map(|b| b.title()).unwrap_or("Unknown");
        format!("{t} #{}", n.id.chars().take(4).collect::<String>())
    }
    fn log(&self, level: &str, n: Option<&Node>, msg: String) {
        let msg = redact_secrets(&msg, &self.secret_values);
        self.eng.log(level, &self.auto.id, &self.auto.name, n.map(|n| (n.id.clone(), self.label(n))), msg);
    }
    fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        self.token.cancel();
    }
    fn out(&self, from: &str, kinds: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = vec![];
        for e in &self.auto.edges {
            if e.from == from && kinds.contains(&e.kind.as_str()) && !v.contains(&e.to) {
                v.push(e.to.clone());
            }
        }
        v
    }
    fn resolve_for(&self, id: &str) -> Result<Vec<Target>, String> {
        resolve_targets(&self.auto, id)
    }
    /// Runaway guard: too many executions of one block in a short window aborts the automation.
    fn rate_ok(&self, node: &Node) -> bool {
        let risky = registry().get(node.kind.as_str()).map(|b| b.risky()).unwrap_or(false);
        let (win, limit) = if risky || matches!(node.kind.as_str(), "act.trim" | "act.launch" | "act.stop_proc" | "act.notify") { (10, 12) } else { (2, 40) };
        let mut m = self.rate.lock().unwrap();
        let q = m.entry(node.id.clone()).or_default();
        let now = Instant::now();
        q.push_back(now);
        while let Some(f) = q.front() {
            if now.duration_since(*f) > Duration::from_secs(win) {
                q.pop_front();
            } else {
                break;
            }
        }
        q.len() <= limit
    }
}

fn short(n: &Node) -> String {
    format!("#{}", n.id.chars().take(4).collect::<String>())
}

fn loop_start_id(a: &Automation) -> Result<String, String> {
    let starts: Vec<&Node> = a.nodes.iter()
        .filter(|node| node.kind == "act.start" && !node.disabled)
        .collect();
    match starts.as_slice() {
        [start] => Ok(start.id.clone()),
        [] => Err("Loop requires one enabled Start block in this automation".into()),
        _ => Err("Loop requires exactly one enabled Start block; multiple enabled Start blocks are ambiguous".into()),
    }
}

fn target_source_ids(a: &Automation, id: &str) -> Vec<String> {
    let reg = registry();
    let mut pending = vec![id.to_string()];
    let mut visited = std::collections::HashSet::new();
    let mut found = Vec::new();
    let mut found_set = std::collections::HashSet::new();

    // Walk backward through workflow wires. A teal input wire binds the input
    // to its consumer; consumers farther down the same chain inherit that binding.
    while let Some(current) = pending.pop() {
        if !visited.insert(current.clone()) {
            continue;
        }
        for edge in a.edges.iter().filter(|edge| edge.to == current) {
            let Some(source) = a.nodes.iter().find(|node| node.id == edge.from) else { continue };
            let is_target = reg.get(source.kind.as_str()).map(|block| block.category() == Cat::Target).unwrap_or(false);
            if edge.kind == "target" || (is_target && ["if", "else", "after"].contains(&edge.kind.as_str())) {
                if is_target && found_set.insert(source.id.clone()) {
                    found.push(source.id.clone());
                }
            } else if ["if", "else", "after"].contains(&edge.kind.as_str()) {
                pending.push(source.id.clone());
            }
        }
    }

    // Preserve the direct downstream-input form supported by the editor.
    if found.is_empty() {
        for edge in a.edges.iter().filter(|edge| edge.from == id && edge.kind != "target") {
            if let Some(dst) = a.nodes.iter().find(|node| node.id == edge.to) {
                if reg.get(dst.kind.as_str()).map(|block| block.category() == Cat::Target).unwrap_or(false)
                    && found_set.insert(dst.id.clone())
                {
                    found.push(dst.id.clone());
                }
            }
        }
    }
    found
}

fn resolve_targets(a: &Automation, id: &str) -> Result<Vec<Target>, String> {
    let reg = registry();

    // Start only needs to resolve a File target that is explicitly attached to it
    // or directly after it. Do not fail a Start trigger because a later process
    // target is not running yet.
    if a.nodes.iter().any(|node| node.id == id && node.kind == "act.start") {
        let mut ids = Vec::new();
        for edge in &a.edges {
            if edge.kind == "target" && edge.to == id {
                if a.nodes.iter().any(|node| node.id == edge.from && node.kind == "tgt.file") {
                    ids.push(edge.from.clone());
                }
            } else if edge.from == id && edge.kind != "target" {
                if a.nodes.iter().any(|node| node.id == edge.to && node.kind == "tgt.file") {
                    ids.push(edge.to.clone());
                }
            }
        }
        ids.sort();
        ids.dedup();
        return ids.into_iter().map(|target_id| {
            let node = a.nodes.iter().find(|node| node.id == target_id).ok_or("a wired File Input is missing")?;
            let block = reg.get(node.kind.as_str()).ok_or("unknown input block type")?;
            block.resolve(&node.cfg)
        }).collect();
    }

    target_source_ids(a, id).into_iter().map(|target_id| {
        let node = a.nodes.iter().find(|node| node.id == target_id).ok_or("a wired input block is missing")?;
        let block = reg.get(node.kind.as_str()).ok_or("unknown input block type")?;
        block.resolve(&node.cfg)
    }).collect()
}

pub fn entry_nodes(a: &Automation) -> Vec<String> {
    let reg = registry();
    let entries: Vec<String> = a.nodes
        .iter()
        .filter(|n| {
            let is_target = reg.get(n.kind.as_str()).map(|b| b.category() == Cat::Target).unwrap_or(false);
            !is_target || a.edges.iter().any(|e| e.from == n.id && e.kind == "target")
        })
        .filter(|n| !a.edges.iter().any(|e| e.to == n.id))
        .map(|n| n.id.clone())
        .collect();
    if !entries.is_empty() {
        return entries;
    }

    // Closed loop graphs need an explicit execution anchor. Start is that anchor
    // when every node already has an incoming wire.
    a.nodes.iter().filter(|node| node.kind == "act.start").map(|node| node.id.clone()).collect()
}

pub fn validate_automation(a: &Automation) -> Result<(), Vec<String>> {
    let reg = registry();
    let mut errs: Vec<String> = vec![];
    let find = |id: &str| a.nodes.iter().find(|n| n.id == id);
    let mut node_ids = std::collections::HashSet::new();
    let mut edge_ids = std::collections::HashSet::new();
    let mut edge_shapes = std::collections::HashSet::new();
    if !a.viewport.x.is_finite() || !a.viewport.y.is_finite()
        || !a.viewport.k.is_finite() || !(0.05..=8.0).contains(&a.viewport.k)
    {
        errs.push("canvas viewport is invalid; zoom must be between 0.05 and 8".into());
    }
    for n in &a.nodes {
        if n.id.trim().is_empty() {
            errs.push("a block has an empty ID".into());
        } else if !node_ids.insert(n.id.as_str()) {
            errs.push(format!("duplicate block ID '{}'", n.id));
        }
        if !n.x.is_finite() || !n.y.is_finite() {
            errs.push(format!("{} has an invalid canvas position", short(n)));
        }
    }
    for e in &a.edges {
        if e.id.trim().is_empty() {
            errs.push("a wire has an empty ID".into());
        } else if !edge_ids.insert(e.id.as_str()) {
            errs.push(format!("duplicate wire ID '{}'", e.id));
        }
        if e.from == e.to {
            errs.push(format!("wire {} connects a block to itself", e.id));
        }
        if !e.bend.is_finite() {
            errs.push(format!("wire {} has an invalid bend position", e.id));
        }
        if !edge_shapes.insert((e.from.as_str(), e.to.as_str(), e.kind.as_str())) {
            errs.push(format!("duplicate connection {} -> {} ({})", e.from, e.to, e.kind));
        }
    }
    for n in &a.nodes {
        if n.disabled {
            continue;
        }
        let Some(b) = reg.get(n.kind.as_str()) else {
            errs.push(format!("{} has unknown block type '{}'", short(n), n.kind));
            continue;
        };
        if let Err(e) = b.validate(&n.cfg) {
            errs.push(format!("{} {}: {e}", b.title(), short(n)));
        }
        if let Some(req) = b.requires_target() {
            let ok = target_source_ids(a, &n.id).iter().any(|id| find(id).map(|source| source.kind == req).unwrap_or(false));
            if !ok {
                errs.push(format!("{} {} needs a connected {} Input in its flow", b.title(), short(n), if req == "tgt.file" { "File" } else { "Process" }));
            }
        }
    }
    for node in a.nodes.iter().filter(|node| node.kind == "act.loop" && !node.disabled) {
        if let Err(message) = loop_start_id(a) {
            errs.push(message);
            break;
        }
        let has_incoming_flow = a.edges.iter().any(|edge| {
            edge.to == node.id && ["if", "else", "after"].contains(&edge.kind.as_str())
        });
        if !has_incoming_flow {
            errs.push(format!("Loop {} needs an incoming flow wire from the last block; it then returns to Start automatically", short(node)));
        }
    }
    for e in &a.edges {
        let (Some(f), Some(t)) = (find(&e.from), find(&e.to)) else {
            errs.push(format!("wire {} points at a missing block", e.id));
            continue;
        };
        let fc = reg.get(f.kind.as_str()).map(|b| b.category());
        let tc = reg.get(t.kind.as_str()).map(|b| b.category());
        if e.kind == "target" {
            if fc != Some(Cat::Target) || tc == Some(Cat::Target) || t.kind == "act.start" || t.kind == "act.loop" {
                errs.push(format!("invalid target wire {} -> {}; inputs cannot connect to Start or Loop", short(f), short(t)));
            }
        } else if fc == Some(Cat::Target) {
            errs.push(format!("IF/ELSE/AFTER wire cannot start at Target block {} (use its teal output)", short(f)));
        } else if f.kind == "act.loop" {
            errs.push(format!("Loop {} has no output wires; it returns to Start automatically", short(f)));
        } else if !["if", "else", "after"].contains(&e.kind.as_str()) {
            errs.push(format!("unknown wire type '{}'", e.kind));
        }
    }
    if entry_nodes(a).is_empty() {
        errs.push("no entry point: add a Condition/Action with no incoming wire, or add a Start block to anchor a closed loop".into());
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

async fn run_entry(ctx: Arc<Ctx>, id: String) {
    let repeat = ctx
        .node(&id)
        .map(|n| n.kind == "cond.interval" && n.cfg.get("repeat").and_then(Value::as_bool).unwrap_or(true))
        .unwrap_or(false);
    loop {
        if ctx.single {
            ctx.visits.lock().unwrap().clear();
        }
        branch(ctx.clone(), id.clone()).await;
        loop {
            let h = ctx.handles.lock().unwrap().pop();
            match h {
                Some(h) => {
                    let _ = h.await;
                }
                None => break,
            }
        }
        if !repeat || ctx.token.is_cancelled() {
            break;
        }
    }
}

/// Follows one path. A single successor continues in this loop (no recursion, so long loops use no extra
/// memory); extra successors become independent concurrent branches.
fn branch(ctx: Arc<Ctx>, start: String) -> BoxFuture<'static, ()> {
    Box::pin(async move {
        let mut cur = Some(start);
        while let Some(id) = cur.take() {
            if ctx.token.is_cancelled() || ctx.returned.load(Ordering::SeqCst) {
                break;
            }
            let Some(node) = ctx.node(&id) else { break };
            let next = exec_node(&ctx, &node).await;
            let mut it = next.into_iter();
            cur = it.next();
            for extra in it {
                let h = tokio::spawn(branch(ctx.clone(), extra));
                ctx.handles.lock().unwrap().push(h);
            }
        }
    })
}

async fn exec_node(ctx: &Arc<Ctx>, node: &Node) -> Vec<String> {
    if node.breakpoint {
        ctx.eng.set_auto(&ctx.auto.id, |r| r.status = "paused".into());
    } else if ctx.debug.is_paused() {
        ctx.eng.set_auto(&ctx.auto.id, |r| r.status = "paused".into());
    }
    if !ctx.debug.before_node(node.breakpoint, &ctx.token).await {
        return vec![];
    }
    ctx.eng.set_auto(&ctx.auto.id, |r| r.status = "running".into());
    let started_at = now_ms();
    let next = exec_node_inner(ctx, node).await;
    ctx.debug.after_node();
    if ctx.debug.is_paused() {
        ctx.eng.set_auto(&ctx.auto.id, |r| r.status = "paused".into());
    }
    let ended_at = now_ms();
    let state = ctx.eng.nodes.lock().ok().and_then(|states| states.get(&ctx.auto.id).and_then(|map| map.get(&node.id)).cloned()).unwrap_or_default();
    let title = registry().get(node.kind.as_str()).map(|b| b.title()).unwrap_or("Unknown").to_string();
    if let Ok(mut trace) = ctx.trace.lock() {
        trace.push(NodeTrace {
            automation_id: ctx.auto.id.clone(),
            node_id: node.id.clone(),
            block_title: title,
            started_at,
            ended_at,
            duration_ms: ended_at.saturating_sub(started_at),
            state: state.state,
            detail: state.detail,
        });
    }
    next
}

async fn exec_node_inner(ctx: &Arc<Ctx>, node: &Node) -> Vec<String> {
    let eng = &ctx.eng;
    let aid = ctx.auto.id.clone();
    let Some(blk) = registry().get(node.kind.as_str()) else {
        ctx.errors.fetch_add(1, Ordering::SeqCst);
        ctx.log("error", Some(node), format!("unknown block type '{}'", node.kind));
        ctx.abort();
        return vec![];
    };
    if !ctx.rate_ok(node) {
        ctx.errors.fetch_add(1, Ordering::SeqCst);
        eng.set_node(&aid, &node.id, "failed", None, "runaway guard".into());
        ctx.log("error", Some(node), "Runaway guard tripped: this block ran too often in a short time. Automation aborted. Add a Wait or raise the loop delay / interval.".into());
        ctx.abort();
        return vec![];
    }
    if node.disabled {
        eng.set_node(&aid, &node.id, "done", None, "disabled: skipped".into());
        ctx.log("info", Some(node), "skipped (block is disabled); following AFTER wires only".into());
        return ctx.out(&node.id, &["after"]);
    }
    let visit = {
        let mut v = ctx.visits.lock().unwrap();
        let c = v.entry(node.id.clone()).or_insert(0);
        *c += 1;
        *c
    };
    eng.set_node(&aid, &node.id, "running", None, String::new());

    let cx = NodeCx {
        cfg: node.cfg.clone(),
        token: ctx.token.clone(),
        visit,
        report: {
            let c = ctx.clone();
            let nid = node.id.clone();
            Box::new(move |st: &str, p: Option<f64>, d: String| c.eng.set_node(&c.auto.id, &nid, st, p, d))
        },
        log: {
            let c = ctx.clone();
            let n = node.clone();
            Box::new(move |lv: &str, m: String| c.log(lv, Some(&n), m))
        },
        resolve: {
            let c = ctx.clone();
            let nid = node.id.clone();
            Box::new(move || c.resolve_for(&nid))
        },
        visible: {
            let c = ctx.clone();
            Box::new(move || c.eng.visible())
        },
        variables: ctx.variables.clone(),
        secret_reader: {
            let vault = ctx.eng.vault.clone();
            Arc::new(move |name: &str| vault.get(name))
        },
        sensitive_values: ctx.secret_values.clone(),
        subflow_return: ctx.subflow_return.clone(),
    };

    if node.kind == "act.for_each" {
        return advance_for_each(ctx, node, &cx);
    }
    let timeout = if blk.supports_timeout() { cx.cfg.get("timeoutSec").and_then(Value::as_f64).unwrap_or(0.0) } else { 0.0 };
    let t0 = Instant::now();
    let run_future = async {
        if node.kind == "act.call_subflow" {
            execute_subflow(ctx, node, &cx).await
        } else {
            blk.run(&cx).await
        }
    };
    let res = if timeout > 0.0 {
        match tokio::time::timeout(Duration::from_secs_f64(timeout), run_future).await {
            Ok(r) => r,
            Err(_) => Err(format!("timed out after {timeout}s")),
        }
    } else {
        run_future.await
    };
    if ctx.token.is_cancelled() {
        eng.set_node(&aid, &node.id, "idle", None, String::new());
        return vec![];
    }
    let dt = t0.elapsed().as_secs_f64();
    match res {
        Ok(o) => {
            let (txt, kinds): (&str, &[&str]) = if blk.category() == Cat::Target {
                ("passed through", &["target"])
            } else if o == Outcome::Satisfied {
                ("satisfied", &["if", "after"])
            } else {
                ("not satisfied", &["else", "after"])
            };
            if node.kind == "act.return_value" && ctx.call_stack.len() > 1 {
                ctx.returned.store(true, Ordering::SeqCst);
                eng.set_node(&aid, &node.id, "done", None, format!("returned from subflow · {dt:.1}s"));
                ctx.log("info", Some(node), "Returned a value to the calling subflow block".into());
                return vec![];
            }
            if node.kind == "act.loop" {
                match loop_start_id(&ctx.auto) {
                    Ok(start_id) => {
                        eng.set_node(&aid, &node.id, "done", None, format!("jumping to Start · {dt:.1}s"));
                        ctx.log("info", Some(node), "Loop redirected execution to Start (implicit connection)".into());
                        return vec![start_id];
                    }
                    Err(message) => {
                        ctx.errors.fetch_add(1, Ordering::SeqCst);
                        eng.set_node(&aid, &node.id, "failed", None, message.clone());
                        ctx.log("error", Some(node), message);
                        ctx.abort();
                        return vec![];
                    }
                }
            }
            eng.set_node(&aid, &node.id, "done", None, format!("{txt} · {dt:.1}s"));
            ctx.log("info", Some(node), format!("{txt} ({dt:.2}s)"));
            ctx.out(&node.id, kinds)
        }
        Err(e) => {
            ctx.errors.fetch_add(1, Ordering::SeqCst);
            eng.set_node(&aid, &node.id, "failed", None, e.clone());
            ctx.log("error", Some(node), format!("failed: {e}"));
            match node.cfg.get("onFail").and_then(Value::as_str).unwrap_or("stop") {
                "else" => ctx.out(&node.id, &["else"]),
                "after" => ctx.out(&node.id, &["after"]),
                "abort" => {
                    ctx.log("warn", Some(node), "Aborting automation (On failure = Abort)".into());
                    ctx.abort();
                    vec![]
                }
                _ => vec![],
            }
        }
    }
}


async fn advance_for_each(ctx: &Arc<Ctx>, node: &Node, cx: &NodeCx) -> Vec<String> {
    if let Some(blk) = registry().get(node.kind.as_str()) {
        if let Err(error) = blk.validate(&node.cfg) {
            ctx.errors.fetch_add(1, Ordering::SeqCst);
            ctx.eng.set_node(&ctx.auto.id, &node.id, "failed", None, error.clone());
            ctx.log("error", Some(node), error);
            return match node.cfg.get("onFail").and_then(Value::as_str).unwrap_or("stop") {
                "else" => ctx.out(&node.id, &["else"]),
                "after" => ctx.out(&node.id, &["after"]),
                "abort" => { ctx.abort(); vec![] },
                _ => vec![],
            };
        }
    }
    let source = node.cfg.get("sourceVariable").and_then(Value::as_str).unwrap_or("");
    let item_name = node.cfg.get("itemVariable").and_then(Value::as_str).unwrap_or("item");
    let index_name = node.cfg.get("indexVariable").and_then(Value::as_str).unwrap_or("index");
    let items_key = format!("__foreach_items:{}:{}", ctx.auto.id, node.id);
    let index_key = format!("__foreach_index:{}:{}", ctx.auto.id, node.id);
    let mut variables = ctx.variables.lock().unwrap();
    if !variables.contains_key(&items_key) {
        let items = variables.get(source).cloned().and_then(|v| v.as_array().cloned());
        let Some(items) = items else {
            drop(variables);
            ctx.errors.fetch_add(1, Ordering::SeqCst);
            let message = format!("For Each source variable '{source}' is missing or is not an array");
            ctx.eng.set_node(&ctx.auto.id, &node.id, "failed", None, message.clone());
            ctx.log("error", Some(node), message);
            return match node.cfg.get("onFail").and_then(Value::as_str).unwrap_or("stop") {
                "else" => ctx.out(&node.id, &["else"]),
                "after" => ctx.out(&node.id, &["after"]),
                "abort" => { ctx.abort(); vec![] },
                _ => vec![],
            };
        };
        if items.len() > 10000 {
            drop(variables);
            ctx.errors.fetch_add(1, Ordering::SeqCst);
            let message = "For Each source array exceeds 10,000 items".to_string();
            ctx.eng.set_node(&ctx.auto.id, &node.id, "failed", None, message.clone());
            ctx.log("error", Some(node), message);
            return match node.cfg.get("onFail").and_then(Value::as_str).unwrap_or("stop") {
                "else" => ctx.out(&node.id, &["else"]),
                "after" => ctx.out(&node.id, &["after"]),
                "abort" => { ctx.abort(); vec![] },
                _ => vec![],
            };
        }
        variables.insert(items_key.clone(), Value::Array(items));
        variables.insert(index_key.clone(), json!(0));
    }
    let items = variables.get(&items_key).and_then(Value::as_array).cloned().unwrap_or_default();
    let index = variables.get(&index_key).and_then(Value::as_u64).unwrap_or(0) as usize;
    if index >= items.len() {
        variables.remove(&items_key);
        variables.remove(&index_key);
        variables.remove(item_name);
        variables.remove(index_name);
        drop(variables);
        ctx.eng.set_node(&ctx.auto.id, &node.id, "done", None, format!("completed {} items", items.len()));
        ctx.log("info", Some(node), format!("For Each finished after {} items", items.len()));
        return ctx.out(&node.id, &["after"]);
    }
    variables.insert(item_name.to_string(), items[index].clone());
    variables.insert(index_name.to_string(), json!(index));
    variables.insert(index_key, json!(index + 1));
    drop(variables);
    ctx.eng.set_node(&ctx.auto.id, &node.id, "done", None, format!("item {} of {}", index + 1, items.len()));
    ctx.log("info", Some(node), format!("For Each item {} of {}", index + 1, items.len()));
    let outgoing = ctx.out(&node.id, &["if"]);
    if outgoing.len() != 1 {
        ctx.errors.fetch_add(1, Ordering::SeqCst);
        let message = "For Each requires exactly one IF wire to its loop body".to_string();
        ctx.eng.set_node(&ctx.auto.id, &node.id, "failed", None, message.clone());
        ctx.log("error", Some(node), message);
        return vec![];
    }
    outgoing
}

async fn execute_subflow(ctx: &Arc<Ctx>, node: &Node, cx: &NodeCx) -> Result<Outcome, String> {
    let id = cx.expand_template(node.cfg.get("automationId").and_then(Value::as_str).unwrap_or(""))?;
    if ctx.call_stack.contains(&id) { return Err(format!("Recursive subflow call blocked: {id}")); }
    if ctx.call_stack.len() >= 8 { return Err("Maximum subflow nesting depth (8) reached".into()); }
    let automation = ctx.eng.store.get(&id).ok_or_else(|| format!("Subflow automation '{id}' was not found"))?;
    validate_automation(&automation).map_err(|e| format!("Subflow is invalid: {}", e.join("; ")))?;
    let entries = entry_nodes(&automation);
    if entries.is_empty() { return Err("Subflow has no entry point".into()); }
    *cx.subflow_return.lock().map_err(|_| "subflow return slot is unavailable".to_string())? = None;
    let mut stack = ctx.call_stack.clone();
    stack.push(id.clone());
    let child_token = ctx.token.child_token();
    let child = Arc::new(Ctx {
        eng: ctx.eng.clone(),
        auto: Arc::new(automation.clone()),
        token: child_token.clone(),
        visits: Default::default(),
        variables: ctx.variables.clone(),
        secret_values: ctx.secret_values.clone(),
        trace: ctx.trace.clone(),
        debug: ctx.debug.clone(),
        call_stack: stack,
        returned: AtomicBool::new(false),
        subflow_return: Arc::new(Mutex::new(None)),
        rate: Default::default(),
        handles: Default::default(),
        errors: AtomicU32::new(0),
        aborted: AtomicBool::new(false),
        single: entries.len() == 1,
    });
    let execute = async {
        let handles: Vec<_> = entries.into_iter().map(|entry| tokio::spawn(run_entry(child.clone(), entry))).collect();
        for handle in handles { let _ = handle.await; }
        loop {
            let handle = child.handles.lock().unwrap().pop();
            match handle { Some(handle) => { let _ = handle.await; }, None => break }
        }
        if child_token.is_cancelled() && !ctx.token.is_cancelled() {
            return Err("Subflow was cancelled".to_string());
        }
        if child.aborted.load(Ordering::SeqCst) {
            return Err(format!("Subflow '{}' aborted after {} error(s)", automation.name, child.errors.load(Ordering::SeqCst)));
        }
        Ok(())
    };
    let timeout = node.cfg.get("timeoutSec").and_then(Value::as_f64).unwrap_or(0.0);
    if timeout > 0.0 {
        if tokio::time::timeout(Duration::from_secs_f64(timeout), execute).await.is_err() {
            child_token.cancel();
            return Err(format!("Subflow '{}' timed out after {timeout} seconds", automation.name));
        }
    } else {
        execute.await?;
    }
    if ctx.token.is_cancelled() { return Err("cancelled".into()); }
    if let Some(output) = node.cfg.get("returnAs").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        let value = child.subflow_return.lock().ok().and_then(|v| v.clone()).unwrap_or(Value::Null);
        ctx.variables.lock().map_err(|_| "workflow variable store is unavailable".to_string())?.insert(output.to_string(), value);
    }
    ctx.log("info", Some(node), format!("Subflow '{}' returned", automation.name));
    Ok(Outcome::Satisfied)
}

#[cfg(test)]
mod loop_semantics_tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, kind: &str, cfg: Value) -> Node {
        Node {
            id: id.into(),
            kind: kind.into(),
            x: 0.0,
            y: 0.0,
            collapsed: false,
            disabled: false,
            breakpoint: false,
            cfg,
        }
    }

    fn edge(id: &str, from: &str, to: &str, kind: &str) -> Edge {
        Edge { id: id.into(), from: from.into(), to: to.into(), kind: kind.into(), bend: 0.0 }
    }

    fn automation(nodes: Vec<Node>, edges: Vec<Edge>) -> Automation {
        Automation {
            id: "test".into(),
            name: "test".into(),
            nodes,
            edges,
            viewport: Viewport { x: 0.0, y: 0.0, k: 1.0 },
            auto_start: false,
        }
    }

    #[test]
    fn loop_returns_to_the_single_enabled_start() {
        let a = automation(
            vec![
                node("start", "act.start", json!({})),
                node("wait", "act.wait", json!({"seconds": 0.1})),
                node("loop", "act.loop", json!({})),
            ],
            vec![
                edge("e1", "start", "wait", "after"),
                edge("e2", "wait", "loop", "after"),
            ],
        );
        assert_eq!(loop_start_id(&a).unwrap(), "start");
        assert!(validate_automation(&a).is_ok());
        assert_eq!(entry_nodes(&a), vec!["start".to_string()]);
    }

    #[test]
    fn loop_rejects_missing_or_ambiguous_start() {
        let missing = automation(vec![node("loop", "act.loop", json!({}))], vec![]);
        assert!(validate_automation(&missing).unwrap_err().join(" ").contains("Loop requires one enabled Start"));

        let multiple = automation(
            vec![
                node("start1", "act.start", json!({})),
                node("start2", "act.start", json!({})),
                node("loop", "act.loop", json!({})),
            ],
            vec![],
        );
        assert!(validate_automation(&multiple).unwrap_err().join(" ").contains("exactly one enabled Start"));
    }

    #[test]
    fn loop_rejects_old_outgoing_wires() {
        let a = automation(
            vec![
                node("start", "act.start", json!({})),
                node("loop", "act.loop", json!({})),
                node("wait", "act.wait", json!({})),
            ],
            vec![
                edge("e1", "start", "loop", "after"),
                edge("e2", "loop", "wait", "if"),
            ],
        );
        assert!(validate_automation(&a).unwrap_err().join(" ").contains("Loop"));
    }

    #[test]
    fn start_accepts_an_incoming_flow_wire_and_converging_inputs_validate() {
        let a = automation(
            vec![
                node("start", "act.start", json!({})),
                node("left", "act.wait", json!({"seconds": 0.1})),
                node("right", "act.wait", json!({"seconds": 0.1})),
                node("join", "act.wait", json!({"seconds": 0.1})),
            ],
            vec![
                edge("e1", "start", "left", "if"),
                edge("e2", "start", "right", "if"),
                edge("e3", "left", "join", "after"),
                edge("e4", "right", "join", "after"),
                edge("e5", "join", "start", "after"),
            ],
        );
        assert!(validate_automation(&a).is_ok());
        assert_eq!(entry_nodes(&a), vec!["start".to_string()]);
        assert_eq!(loop_start_id(&a).unwrap(), "start");
    }

    #[test]
    fn launch_file_is_a_separate_action_and_requires_a_file_target() {
        let a = automation(
            vec![
                node("start", "act.start", json!({})),
                node("file", "tgt.file", json!({"path": "C:\\\\test.exe"})),
                node("launch", "act.launch", json!({})),
            ],
            vec![
                edge("e1", "start", "file", "after"),
                edge("e2", "file", "launch", "target"),
            ],
        );
        assert!(validate_automation(&a).is_ok());
    }
}
