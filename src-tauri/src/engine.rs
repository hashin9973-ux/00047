use crate::{
    blocks::{registry, Cat, NodeCx, Outcome, Target},
    model::*,
    store::Store,
};
use crate::logstore::LogStore;
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
}

#[derive(Serialize)]
pub struct Runtime {
    pub autos: HashMap<String, AutoRuntime>,
    pub nodes: HashMap<String, HashMap<String, NodeState>>,
}

pub struct Engine {
    app: AppHandle,
    pub store: Arc<Store>,
    runs: Mutex<HashMap<String, RunHandle>>,
    autos: Mutex<HashMap<String, AutoRuntime>>,
    nodes: Mutex<HashMap<String, HashMap<String, NodeState>>>,
    logs: Mutex<VecDeque<LogEntry>>,
    hist: Mutex<HashMap<String, VecDeque<RunRecord>>>,
    seq: AtomicU64,
    run_seq: AtomicU64,
    ui: AtomicBool,
    disk: LogStore,
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
        let run_id = self.run_seq.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut r = self.runs.lock().unwrap();
            if r.contains_key(id) {
                return Err("already running".into());
            }
            r.insert(id.to_string(), RunHandle { token: token.clone(), run_id });
        }
        let me = self.clone();
        tauri::async_runtime::spawn(async move {
            me.run_automation(auto, token, run_id).await;
        });
        Ok(())
    }

    pub fn stop(&self, id: &str) {
        let t = self.runs.lock().unwrap().get(id).map(|r| r.token.clone());
        if let Some(t) = t {
            t.cancel();
            self.set_auto(id, |r| r.status = "stopping".into());
        }
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
        let ctx = Arc::new(Ctx {
            eng: self.clone(),
            auto: Arc::new(auto),
            token: parent.child_token(),
            visits: Default::default(),
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
            q.push_front(RunRecord { id: run_id, started_at: started, ended_at: ended, outcome: outcome.into(), errors });
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
    rate: Mutex<HashMap<String, VecDeque<Instant>>>,
    handles: Mutex<Vec<JoinHandle<()>>>,
    errors: AtomicU32,
    aborted: AtomicBool,
    single: bool,
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
        let (win, limit) = if node.kind == "act.trim" || node.kind == "act.launch" { (10, 12) } else { (2, 40) };
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

    // Walk backward through workflow wires. A teal target wire binds the target
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

    // Preserve the direct downstream-target form supported by the editor.
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
            let node = a.nodes.iter().find(|node| node.id == target_id).ok_or("a wired File target is missing")?;
            let block = reg.get(node.kind.as_str()).ok_or("unknown target block type")?;
            block.resolve(&node.cfg)
        }).collect();
    }

    target_source_ids(a, id).into_iter().map(|target_id| {
        let node = a.nodes.iter().find(|node| node.id == target_id).ok_or("a wired target block is missing")?;
        let block = reg.get(node.kind.as_str()).ok_or("unknown target block type")?;
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
    let mut flow_incoming: HashMap<&str, usize> = HashMap::new();
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
        if ["if", "else", "after"].contains(&e.kind.as_str()) {
            *flow_incoming.entry(e.to.as_str()).or_default() += 1;
        }
    }
    for (node_id, count) in flow_incoming {
        if count > 1 {
            errs.push(format!("block #{} has {count} incoming flow wires; converging branches are not supported yet", node_id.chars().take(4).collect::<String>()));
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
                errs.push(format!("{} {} needs a connected {} block in its flow", b.title(), short(n), if req == "tgt.file" { "File" } else { "Active process" }));
            }
        }
    }
    if a.nodes.iter().any(|node| node.kind == "act.loop" && !node.disabled) {
        if let Err(message) = loop_start_id(a) {
            errs.push(message);
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
                errs.push(format!("invalid target wire {} -> {}; targets cannot bind to Start or Loop", short(f), short(t)));
            }
        } else if fc == Some(Cat::Target) {
            errs.push(format!("IF/ELSE/AFTER wire cannot start at Target block {} (use its teal output)", short(f)));
        } else if f.kind == "act.loop" {
            errs.push(format!("Loop {} has no output wires; it returns to Start automatically", short(f)));
        } else if t.kind == "act.start" {
            errs.push(format!("Start {} is an entry point and cannot receive a wire; Loop returns automatically", short(t)));
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
            if ctx.token.is_cancelled() {
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
    };

    let timeout = if blk.supports_timeout() { cx.cfg.get("timeoutSec").and_then(Value::as_f64).unwrap_or(0.0) } else { 0.0 };
    let t0 = Instant::now();
    let res = if timeout > 0.0 {
        match tokio::time::timeout(Duration::from_secs_f64(timeout), blk.run(&cx)).await {
            Ok(r) => r,
            Err(_) => Err(format!("timed out after {timeout}s")),
        }
    } else {
        blk.run(&cx).await
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
