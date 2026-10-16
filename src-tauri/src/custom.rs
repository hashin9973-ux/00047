//! User-defined blocks. Each JSON file in `<app data>/blocks/` may define blocks that run a program or a
//! PowerShell script. No shell string building: arguments are passed separately, and PowerShell scripts read
//! field values from environment variables (FIELD_<KEY>), so field text can never be injected as code.
use crate::{
    blocks::{registry, Block, Cat, NodeCx, Outcome},
    spec,
};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Mutex, OnceLock},
};

static DIR: OnceLock<PathBuf> = OnceLock::new();
static ERRORS: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub fn init(dir: PathBuf) {
    let _ = DIR.set(dir);
}
pub fn blocks_dir() -> Option<&'static PathBuf> {
    DIR.get()
}
fn push_err(e: String) {
    ERRORS.lock().unwrap().push(e);
}
pub fn load_errors() -> Vec<String> {
    ERRORS.lock().unwrap().clone()
}

// ---------- templates: "{field}" substitution, "{{" and "}}" are literal braces ----------
fn placeholders(t: &str) -> Vec<String> {
    let b: Vec<char> = t.chars().collect();
    let (mut out, mut i) = (vec![], 0);
    while i < b.len() {
        if b[i] == '{' {
            if i + 1 < b.len() && b[i + 1] == '{' {
                i += 2;
                continue;
            }
            if let Some(j) = b[i + 1..].iter().position(|&c| c == '}') {
                out.push(b[i + 1..i + 1 + j].iter().collect());
                i += j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn subst(t: &str, vals: &HashMap<String, String>) -> String {
    let b: Vec<char> = t.chars().collect();
    let (mut out, mut i) = (String::new(), 0);
    while i < b.len() {
        if b[i] == '{' {
            if i + 1 < b.len() && b[i + 1] == '{' {
                out.push('{');
                i += 2;
                continue;
            }
            if let Some(j) = b[i + 1..].iter().position(|&c| c == '}') {
                let k: String = b[i + 1..i + 1 + j].iter().collect();
                out.push_str(vals.get(&k).map(String::as_str).unwrap_or(""));
                i += j + 2;
                continue;
            }
        }
        if b[i] == '}' && i + 1 < b.len() && b[i + 1] == '}' {
            out.push('}');
            i += 2;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

// ---------- spec checking ----------
fn check_spec(s: &Value) -> Result<String, String> {
    let kind = s.get("kind").and_then(Value::as_str).ok_or("a block has no \"kind\"")?;
    if !kind.starts_with("user.")
        || kind.len() > 60
        || !kind.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    {
        return Err(format!("kind '{kind}' must start with \"user.\" and use only a-z, 0-9, '.' and '_'"));
    }
    match s.get("category").and_then(Value::as_str) {
        Some("condition") | Some("action") => {}
        _ => return Err(format!("{kind}: category must be \"condition\" or \"action\"")),
    }
    let title = s.get("title").and_then(Value::as_str).unwrap_or("").trim();
    if title.is_empty() || title.chars().count() > 40 {
        return Err(format!("{kind}: title is required (max 40 characters)"));
    }
    let mut keys: Vec<String> = vec![];
    for f in spec::fields(s) {
        let key = f.get("key").and_then(Value::as_str).ok_or_else(|| format!("{kind}: a field has no key"))?;
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("{kind}: field key '{key}' may only use letters, digits and _"));
        }
        if keys.iter().any(|k| k == key) {
            return Err(format!("{kind}: duplicate field key '{key}'"));
        }
        match f.get("type").and_then(Value::as_str).unwrap_or("text") {
            "number" => {
                if f.get("min").and_then(Value::as_f64).is_none() || f.get("max").and_then(Value::as_f64).is_none() {
                    return Err(format!("{kind}: number field '{key}' needs numeric min and max"));
                }
            }
            "select" => {
                let ok = f.get("options").and_then(Value::as_array).map(|o| !o.is_empty() && o.iter().all(|x| x.get(0).and_then(Value::as_str).is_some() && x.get(1).and_then(Value::as_str).is_some())).unwrap_or(false);
                if !ok {
                    return Err(format!("{kind}: select field '{key}' needs options like [[\"value\",\"Label\"]]"));
                }
            }
            "text" | "file" | "bool" | "time" => {}
            other => return Err(format!("{kind}: field '{key}' has unknown type '{other}'")),
        }
        keys.push(key.to_string());
    }
    let run = s.get("run").ok_or_else(|| format!("{kind}: missing \"run\""))?;
    let mut texts: Vec<String> = vec![];
    match run.get("type").and_then(Value::as_str) {
        Some("command") => {
            let p = run.get("program").and_then(Value::as_str).unwrap_or("");
            if p.trim().is_empty() {
                return Err(format!("{kind}: run.program is required"));
            }
            texts.push(p.to_string());
            if let Some(a) = run.get("args") {
                for x in a.as_array().ok_or_else(|| format!("{kind}: run.args must be an array of strings"))? {
                    texts.push(x.as_str().ok_or_else(|| format!("{kind}: run.args must be an array of strings"))?.to_string());
                }
            }
            if let Some(c) = run.get("cwd").and_then(Value::as_str) {
                texts.push(c.to_string());
            }
        }
        Some("powershell") => {
            if run.get("script").and_then(Value::as_str).unwrap_or("").trim().is_empty() {
                return Err(format!("{kind}: run.script is required"));
            }
        }
        _ => return Err(format!("{kind}: run.type must be \"command\" or \"powershell\"")),
    }
    for t in texts {
        for p in placeholders(&t) {
            if !keys.contains(&p) {
                return Err(format!("{kind}: placeholder {{{p}}} is not a field of this block"));
            }
        }
    }
    Ok(kind.to_string())
}

// ---------- the block ----------
struct CustomBlock {
    spec: Value,
    title: &'static str,
    cat: Cat,
    timeout: bool,
}

impl CustomBlock {
    fn new(spec: Value) -> Self {
        let title: &'static str = Box::leak(spec["title"].as_str().unwrap_or("Custom").trim().to_string().into_boxed_str());
        let cat = if spec["category"] == "condition" { Cat::Condition } else { Cat::Action };
        let timeout = spec::fields(&spec).iter().any(|f| f["key"] == "timeoutSec");
        CustomBlock { spec, title, cat, timeout }
    }
}

fn tail(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    let s = s.trim();
    let n = s.chars().count();
    if n > 400 {
        format!("…{}", s.chars().skip(n - 400).collect::<String>())
    } else {
        s.to_string()
    }
}

#[async_trait]
impl Block for CustomBlock {
    fn title(&self) -> &'static str {
        self.title
    }
    fn category(&self) -> Cat {
        self.cat
    }
    fn supports_timeout(&self) -> bool {
        self.timeout
    }
    fn risky(&self) -> bool {
        self.cat == Cat::Action
    }
    fn spec(&self) -> Option<Value> {
        Some(self.spec.clone())
    }
    fn validate(&self, cfg: &Value) -> Result<(), String> {
        spec::validate(&self.spec, cfg)
    }
    async fn run(&self, cx: &NodeCx) -> Result<Outcome, String> {
        let run = &self.spec["run"];
        let vals = spec::values(&self.spec, &cx.cfg);
        let (program, args): (String, Vec<String>) = if run["type"] == "powershell" {
            (
                "powershell.exe".into(),
                vec!["-NoProfile".into(), "-NonInteractive".into(), "-Command".into(), run["script"].as_str().unwrap_or("").to_string()],
            )
        } else {
            let skip_empty = run["skipEmptyArgs"].as_bool().unwrap_or(false);
            let args = run["args"]
                .as_array()
                .map(|a| a.iter().filter_map(Value::as_str).map(|t| subst(t, &vals)).filter(|s| !(skip_empty && s.is_empty())).collect())
                .unwrap_or_default();
            (subst(run["program"].as_str().unwrap_or(""), &vals), args)
        };
        if program.trim().is_empty() {
            return Err("the program to run is empty (check this block's settings)".into());
        }
        let mut cmd = tokio::process::Command::new(&program);
        cmd.args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        for (k, v) in &vals {
            cmd.env(format!("FIELD_{}", k.to_uppercase()), v);
        }
        if let Some(cwd) = run["cwd"].as_str() {
            let cwd = subst(cwd, &vals);
            if !cwd.trim().is_empty() {
                cmd.current_dir(cwd);
            }
        }
        #[cfg(windows)]
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        (cx.report)("running", None, format!("Running {}", self.title));
        if !run["wait"].as_bool().unwrap_or(true) {
            cmd.stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(false);
            cmd.spawn().map_err(|e| format!("could not start '{program}': {e}"))?;
            (cx.log)("info", format!("started {program} (not waiting for it)"));
            return Ok(Outcome::Satisfied);
        }
        let child = cmd.spawn().map_err(|e| format!("could not start '{program}': {e}"))?;
        let out = tokio::select! {
            r = child.wait_with_output() => r.map_err(|e| e.to_string())?,
            _ = cx.token.cancelled() => return Err("cancelled".into()),
        };
        let code = out.status.code().unwrap_or(-1) as i64;
        let ok_codes: Vec<i64> = run["okExitCodes"].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_else(|| vec![0]);
        let (stdout, stderr) = (tail(&out.stdout), tail(&out.stderr));
        if run["capture"].as_bool().unwrap_or(true) && !stdout.is_empty() {
            (cx.log)("info", format!("output: {stdout}"));
        }
        let ok = ok_codes.contains(&code);
        if self.cat == Cat::Condition {
            return Ok(if ok { Outcome::Satisfied } else { Outcome::NotSatisfied });
        }
        if ok {
            Ok(Outcome::Satisfied)
        } else {
            Err(format!("exited with code {code}{}", if stderr.is_empty() { String::new() } else { format!(": {stderr}") }))
        }
    }
}

// ---------- loading ----------
fn parse_pack(path: &Path) -> Result<Vec<Value>, String> {
    if path.extension().map(|x| x != "json").unwrap_or(true) {
        return Err("a block pack must be a .json file".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > 1_000_000 {
        return Err("the pack is larger than 1 MB".into());
    }
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("invalid JSON: {e}"))?;
    Ok(if let Some(a) = v.get("blocks").and_then(Value::as_array) {
        a.clone()
    } else if let Value::Array(a) = &v {
        a.clone()
    } else {
        vec![v]
    })
}

/// Called once while the block registry is built.
pub fn load_into(m: &mut HashMap<&'static str, Box<dyn Block>>) {
    let Some(dir) = DIR.get() else { return };
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map(|x| x == "json").unwrap_or(false)).collect();
    files.sort();
    for p in files {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let specs = match parse_pack(&p) {
            Ok(s) => s,
            Err(e) => {
                push_err(format!("{name}: {e}"));
                continue;
            }
        };
        for s in specs {
            match check_spec(&s) {
                Ok(kind) => {
                    if m.contains_key(kind.as_str()) {
                        push_err(format!("{name}: kind '{kind}' is already defined"));
                        continue;
                    }
                    let leaked: &'static str = Box::leak(kind.into_boxed_str());
                    m.insert(leaked, Box::new(CustomBlock::new(s)));
                }
                Err(e) => push_err(format!("{name}: {e}")),
            }
        }
    }
}

/// Specs of every block that provides one (native spec-driven blocks + user blocks) for the UI.
pub fn all_specs() -> Value {
    let mut specs: Vec<Value> = vec![];
    for (kind, b) in registry().iter() {
        if let Some(mut s) = b.spec() {
            s["kind"] = json!(kind);
            s["origin"] = json!(if kind.starts_with("user.") { "custom" } else { "native" });
            specs.push(s);
        }
    }
    specs.sort_by(|a, b| a["title"].as_str().cmp(&b["title"].as_str()));
    json!({
        "specs": specs,
        "errors": load_errors(),
        "dir": blocks_dir().map(|d| d.to_string_lossy().to_string()).unwrap_or_default(),
    })
}

// ---------- install flow ----------
#[derive(Serialize)]
pub struct BlockPreview {
    pub kind: String,
    pub title: String,
    pub category: String,
    pub command: String,
}
#[derive(Serialize)]
pub struct PackPreview {
    pub file: String,
    pub path: String,
    pub blocks: Vec<BlockPreview>,
    pub errors: Vec<String>,
}

fn describe(s: &Value) -> String {
    let run = &s["run"];
    if run["type"] == "powershell" {
        return format!("powershell -Command {}", run["script"].as_str().unwrap_or(""));
    }
    let mut parts = vec![run["program"].as_str().unwrap_or("").to_string()];
    if let Some(a) = run["args"].as_array() {
        parts.extend(a.iter().map(|x| format!("\"{}\"", x.as_str().unwrap_or(""))));
    }
    parts.join(" ")
}

pub fn preview(path: &Path) -> Result<PackPreview, String> {
    let specs = parse_pack(path)?;
    let (mut blocks, mut errors) = (vec![], vec![]);
    for s in &specs {
        match check_spec(s) {
            Ok(kind) => blocks.push(BlockPreview {
                kind,
                title: s["title"].as_str().unwrap_or("").to_string(),
                category: s["category"].as_str().unwrap_or("").to_string(),
                command: describe(s),
            }),
            Err(e) => errors.push(e),
        }
    }
    if blocks.is_empty() {
        return Err(format!("no valid blocks in this file{}", if errors.is_empty() { String::new() } else { format!(": {}", errors.join("; ")) }));
    }
    let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(PackPreview { file, path: path.to_string_lossy().to_string(), blocks, errors })
}

pub fn install(path: &Path) -> Result<String, String> {
    let pv = preview(path)?;
    let dir = DIR.get().ok_or("blocks folder is not initialised")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stem: String = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "pack".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let dest = dir.join(format!("{stem}.json"));
    if dest.exists() && std::fs::canonicalize(&dest).ok() == std::fs::canonicalize(path).ok() {
        return Ok(pv.file);
    }
    std::fs::copy(path, &dest).map_err(|e| format!("copy: {e}"))?;
    Ok(pv.file)
}