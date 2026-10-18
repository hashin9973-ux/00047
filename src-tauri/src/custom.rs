//! User-defined blocks. The preferred format is a ZIP plugin with a root manifest.json and scripts/assets.
//! A manifest can launch any executable/runtime by command plus separate arguments. Legacy JSON block files
//! remain supported for compatibility. Field values are exposed as FIELD_<KEY> environment variables.
use crate::{
    blocks::{registry, Block, Cat, NodeCx, Outcome},
    spec,
};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Mutex, OnceLock},
};
use zip::ZipArchive;

const MAX_PLUGIN_ZIP: u64 = 20 * 1024 * 1024;
const MAX_PLUGIN_FILES: usize = 500;
const MAX_PLUGIN_UNPACKED: u64 = 50 * 1024 * 1024;
const MAX_PLUGIN_MANIFEST: u64 = 1 * 1024 * 1024;

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
    let run_type = run.get("type").and_then(Value::as_str).unwrap_or("");
    match run_type {
        "command" | "plugin" => {
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
            if run_type == "plugin"
                && run.get("entrypoint").and_then(Value::as_str).unwrap_or("").trim().is_empty()
            {
                return Err(format!("{kind}: plugin entrypoint is required"));
            }
        }
        "powershell" => {
            if run.get("script").and_then(Value::as_str).unwrap_or("").trim().is_empty() {
                return Err(format!("{kind}: run.script is required"));
            }
        }
        _ => return Err(format!("{kind}: run.type must be \"command\", \"powershell\", or an internal plugin runner")),
    }
    for t in texts {
        for p in placeholders(&t) {
            if !keys.contains(&p) && !(run_type == "plugin" && (p == "entrypoint" || p == "pluginDir")) {
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
    plugin_dir: Option<PathBuf>,
}

impl CustomBlock {
    fn new(spec: Value, plugin_dir: Option<PathBuf>) -> Self {
        let title: &'static str = Box::leak(spec["title"].as_str().unwrap_or("Custom").trim().to_string().into_boxed_str());
        let cat = if spec["category"] == "condition" { Cat::Condition } else { Cat::Action };
        let timeout = spec::fields(&spec).iter().any(|f| f["key"] == "timeoutSec");
        CustomBlock { spec, title, cat, timeout, plugin_dir }
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
        let mut vals = spec::values(&self.spec, &cx.cfg);
        if let Some(plugin_dir) = &self.plugin_dir {
            let root = plugin_dir.to_string_lossy().to_string();
            let entrypoint = plugin_dir.join(run["entrypoint"].as_str().unwrap_or(""));
            vals.insert("pluginDir".into(), root.clone());
            vals.insert("entrypoint".into(), entrypoint.to_string_lossy().to_string());
        }
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
            if k != "pluginDir" && k != "entrypoint" {
                cmd.env(format!("FIELD_{}", k.to_uppercase()), v);
            }
        }
        if self.plugin_dir.is_some() {
            if let Some(v) = vals.get("pluginDir") { cmd.env("PLUGIN_DIR", v); }
            if let Some(v) = vals.get("entrypoint") { cmd.env("PLUGIN_ENTRYPOINT", v); }
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
        return Err("legacy block metadata must be a .json file".into());
    }
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_PLUGIN_MANIFEST {
        return Err("the JSON block metadata is larger than 1 MB".into());
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

struct PluginArchive {
    manifest: Value,
    files: HashSet<String>,
}

fn valid_relative_path(value: &str) -> bool {
    if value.is_empty() || value.starts_with('/') || value.contains('\\') || value.contains(':') {
        return false;
    }
    value.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
}

fn read_plugin_zip(path: &Path) -> Result<PluginArchive, String> {
    if path.extension().and_then(|x| x.to_str()).map(|x| !x.eq_ignore_ascii_case("zip")).unwrap_or(true) {
        return Err("script plugins must be a .zip archive containing manifest.json".into());
    }
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_PLUGIN_ZIP {
        return Err("plugin ZIP exceeds the 20 MB compressed-size limit".into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("invalid ZIP archive: {e}"))?;
    if archive.len() == 0 || archive.len() > MAX_PLUGIN_FILES {
        return Err(format!("plugin ZIP must contain between 1 and {MAX_PLUGIN_FILES} entries"));
    }
    let mut files = HashSet::new();
    let mut unpacked = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| format!("cannot read ZIP entry: {e}"))?;
        let name = entry.name().to_string();
        if entry.enclosed_name().is_none() {
            return Err(format!("unsafe ZIP path: {name}"));
        }
        if !files.insert(name.clone()) {
            return Err(format!("duplicate ZIP entry: {name}"));
        }
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                return Err(format!("symbolic links are not allowed in plugin ZIPs: {name}"));
            }
        }
        unpacked = unpacked.saturating_add(entry.size());
        if unpacked > MAX_PLUGIN_UNPACKED {
            return Err("plugin ZIP expands beyond the 50 MB unpacked-size limit".into());
        }
        if name == "manifest.json" && entry.size() > MAX_PLUGIN_MANIFEST {
            return Err("manifest.json exceeds the 1 MB limit".into());
        }
    }
    if !files.contains("manifest.json") {
        return Err("plugin ZIP must contain manifest.json at its root".into());
    }
    let mut text = String::new();
    archive.by_name("manifest.json")
        .map_err(|e| format!("could not read manifest.json: {e}"))?
        .take(MAX_PLUGIN_MANIFEST + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("could not decode manifest.json as UTF-8: {e}"))?;
    if text.len() as u64 > MAX_PLUGIN_MANIFEST {
        return Err("manifest.json exceeds the 1 MB limit".into());
    }
    let manifest: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("invalid manifest.json: {e}"))?;
    validate_manifest_header(&manifest)?;
    Ok(PluginArchive { manifest, files })
}

fn valid_extension_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
}

/// Validates an app-level command or long-lived service with the familiar block field/runtime schema.
fn validate_runtime_extension(ext: &Value, extension_type: &str) -> Result<(), String> {
    let label = format!("{} extension", extension_type);
    let id = ext.get("id").and_then(Value::as_str).unwrap_or("");
    if !valid_extension_id(id) {
        return Err(format!("{label} id must use lowercase a-z, digits, dots, dashes or underscores (max 80 chars)"));
    }
    let title = ext.get("title").and_then(Value::as_str).unwrap_or("").trim();
    if title.is_empty() || title.chars().count() > 100 {
        return Err(format!("{label} '{id}': title is required (max 100 characters)"));
    }
    let entrypoint = ext.get("entrypoint").and_then(Value::as_str).unwrap_or("");
    if !valid_relative_path(entrypoint) {
        return Err(format!("{label} '{id}': entrypoint must be a safe relative path inside the ZIP"));
    }
    let runtime = ext.get("runtime").ok_or_else(|| format!("{label} '{id}': runtime is required"))?;
    let command = runtime.get("command").and_then(Value::as_str).unwrap_or("").trim();
    if command.is_empty() || command.len() > 2048 {
        return Err(format!("{label} '{id}': runtime.command is required (max 2048 characters)"));
    }
    let args = runtime.get("args").and_then(Value::as_array)
        .ok_or_else(|| format!("{label} '{id}': runtime.args must be an array of strings"))?;
    if args.len() > 128 {
        return Err(format!("{label} '{id}': runtime.args may contain at most 128 arguments"));
    }
    if args.iter().any(|arg| arg.as_str().is_none()) {
        return Err(format!("{label} '{id}': runtime.args must contain only strings"));
    }
    let fields = ext.get("fields").cloned().unwrap_or_else(|| json!([]));
    if !fields.is_array() {
        return Err(format!("{label} '{id}': fields must be an array"));
    }
    let fake = json!({
        "kind": "user.extension",
        "category": "action",
        "title": "Extension",
        "fields": fields,
        "run": {
            "type": "plugin",
            "program": command,
            "args": args,
            "entrypoint": entrypoint
        }
    });
    check_spec(&fake).map_err(|e| format!("{label} '{id}': {e}"))?;
    for field in spec::fields(&fake) {
        let key = field.get("key").and_then(Value::as_str).unwrap_or("");
        if matches!(key, "pluginDir" | "entrypoint" | "pluginId" | "extensionId") {
            return Err(format!("{label} '{id}': field key '{key}' is reserved by the plugin host"));
        }
    }
    if let Some(timeout) = ext.get("timeoutSec") {
        let n = timeout.as_u64().ok_or_else(|| format!("{label} '{id}': timeoutSec must be an integer from 1 to 86400"))?;
        if !(1..=86400).contains(&n) {
            return Err(format!("{label} '{id}': timeoutSec must be from 1 to 86400"));
        }
    }
    if let Some(defaults) = ext.get("defaults") {
        if !defaults.is_object() {
            return Err(format!("{label} '{id}': defaults must be a JSON object"));
        }
    }
    if extension_type == "service" && ext.get("autoStart").is_some() && !ext["autoStart"].is_boolean() {
        return Err(format!("{label} '{id}': autoStart must be a boolean"));
    }
    Ok(())
}

fn extension_array<'a>(manifest: &'a Value, key: &str) -> Result<Vec<&'a Value>, String> {
    match manifest.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            if items.len() > 100 {
                return Err(format!("manifest {key} may contain at most 100 entries"));
            }
            Ok(items.iter().collect())
        }
        Some(_) => Err(format!("manifest {key} must be an array")),
    }
}

fn validate_manifest_header(manifest: &Value) -> Result<(), String> {
    if manifest.get("manifestVersion").and_then(Value::as_u64) != Some(1) {
        return Err("manifestVersion must be 1".into());
    }
    let id = manifest.get("id").and_then(Value::as_str).unwrap_or("");
    if !valid_plugin_id(id) {
        return Err("manifest id must use lowercase a-z, digits, dots, dashes or underscores (max 100 chars)".into());
    }
    let name = manifest.get("name").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err("manifest name is required (max 100 characters)".into());
    }
    let version = manifest.get("version").and_then(Value::as_str).unwrap_or("").trim();
    if version.is_empty() || version.len() > 40 {
        return Err("manifest version is required (max 40 characters)".into());
    }

    let blocks = extension_array(manifest, "blocks")?;
    let commands = extension_array(manifest, "commands")?;
    let services = extension_array(manifest, "services")?;
    if blocks.is_empty() && commands.is_empty() && services.is_empty() {
        return Err("manifest must define at least one block, command, or service".into());
    }
    for ext in &commands { validate_runtime_extension(ext, "command")?; }
    for ext in &services { validate_runtime_extension(ext, "service")?; }
    let mut extension_ids = HashSet::new();
    for ext in commands.iter().chain(services.iter()) {
        let ext_id = ext.get("id").and_then(Value::as_str).unwrap_or("");
        if !extension_ids.insert(ext_id.to_string()) {
            return Err(format!("manifest contains duplicate app-level extension id '{ext_id}'"));
        }
    }
    Ok(())
}

fn normalize_plugin_block(block: &Value, plugin_dir: &Path, require_entrypoint: bool) -> Result<Value, String> {
    let kind = block.get("kind").and_then(Value::as_str).unwrap_or("unknown");
    let entrypoint = block.get("entrypoint").and_then(Value::as_str)
        .ok_or_else(|| format!("{kind}: entrypoint is required"))?;
    if !valid_relative_path(entrypoint) {
        return Err(format!("{kind}: entrypoint must be a safe relative path inside the ZIP"));
    }
    let full_entrypoint = plugin_dir.join(entrypoint);
    if require_entrypoint && !full_entrypoint.is_file() {
        return Err(format!("{kind}: entrypoint '{entrypoint}' does not exist in the installed plugin"));
    }
    let runtime = block.get("runtime").ok_or_else(|| format!("{kind}: runtime is required"))?;
    let command = runtime.get("command").and_then(Value::as_str).unwrap_or("").trim();
    if command.is_empty() || command.len() > 2048 {
        return Err(format!("{kind}: runtime.command is required (max 2048 characters)"));
    }
    let args = runtime.get("args").and_then(Value::as_array)
        .ok_or_else(|| format!("{kind}: runtime.args must be an array of strings"))?;
    if args.len() > 128 {
        return Err(format!("{kind}: runtime.args may contain at most 128 arguments"));
    }
    for arg in args {
        if arg.as_str().is_none() {
            return Err(format!("{kind}: runtime.args must contain only strings"));
        }
    }
    let mut normalized = block.clone();
    if let Some(obj) = normalized.as_object_mut() {
        obj.remove("entrypoint");
        obj.remove("runtime");
        obj.insert("run".into(), json!({
            "type": "plugin",
            "program": command,
            "args": args,
            "cwd": "{pluginDir}",
            "entrypoint": entrypoint,
            "wait": true,
            "okExitCodes": [0],
            "capture": true
        }));
    }
    Ok(normalized)
}

fn register_spec(m: &mut HashMap<&'static str, Box<dyn Block>>, name: &str, s: Value, plugin_dir: Option<PathBuf>) {
    match check_spec(&s) {
        Ok(kind) => {
            if m.contains_key(kind.as_str()) {
                push_err(format!("{name}: kind '{kind}' is already defined"));
                return;
            }
            let leaked: &'static str = Box::leak(kind.into_boxed_str());
            m.insert(leaked, Box::new(CustomBlock::new(s, plugin_dir)));
        }
        Err(e) => push_err(format!("{name}: {e}")),
    }
}

fn load_plugin_dir(m: &mut HashMap<&'static str, Box<dyn Block>>, dir: &Path) {
    let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let manifest_path = dir.join("manifest.json");
    let text = match fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => { push_err(format!("{name}: cannot read manifest.json: {e}")); return; }
    };
    if text.len() as u64 > MAX_PLUGIN_MANIFEST {
        push_err(format!("{name}: manifest.json exceeds the 1 MB limit")); return;
    }
    let manifest: Value = match serde_json::from_str(text.trim_start_matches('\u{feff}')) {
        Ok(v) => v,
        Err(e) => { push_err(format!("{name}: invalid manifest.json: {e}")); return; }
    };
    if let Err(e) = validate_manifest_header(&manifest) {
        push_err(format!("{name}: {e}")); return;
    }
    let blocks = manifest["blocks"].as_array().cloned().unwrap_or_default();
    for block in &blocks {
        match normalize_plugin_block(block, dir, true) {
            Ok(s) => register_spec(m, &name, s, Some(dir.to_path_buf())),
            Err(e) => push_err(format!("{name}: {e}")),
        }
    }
}

/// Called once while the block registry is built.
pub fn load_into(m: &mut HashMap<&'static str, Box<dyn Block>>) {
    let Some(dir) = DIR.get() else { return };
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut files: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "json").unwrap_or(false)).collect();
    files.sort();
    for p in files {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let specs = match parse_pack(&p) {
            Ok(s) => s,
            Err(e) => { push_err(format!("{name}: {e}")); continue; }
        };
        for s in specs {
            register_spec(m, &name, s, None);
        }
    }
    let packages = dir.join("packages");
    if let Ok(rd) = fs::read_dir(packages) {
        let mut package_dirs: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path())
            .filter(|p| p.is_dir() && p.join("manifest.json").is_file()).collect();
        package_dirs.sort();
        for package_dir in package_dirs { load_plugin_dir(m, &package_dir); }
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
pub struct ExtensionPreview {
    pub extension_type: String,
    pub id: String,
    pub title: String,
    pub entrypoint: String,
    pub command: String,
}
#[derive(Serialize)]
pub struct PackPreview {
    pub file: String,
    pub path: String,
    pub blocks: Vec<BlockPreview>,
    pub extensions: Vec<ExtensionPreview>,
    pub errors: Vec<String>,
}

fn describe(s: &Value) -> String {
    let run = &s["run"];
    if run["type"] == "powershell" {
        return format!("powershell -NoProfile -NonInteractive -Command {}", run["script"].as_str().unwrap_or(""));
    }
    let mut parts = vec![run["program"].as_str().unwrap_or("").to_string()];
    if let Some(a) = run["args"].as_array() {
        parts.extend(a.iter().map(|x| format!("\"{}\"", x.as_str().unwrap_or(""))));
    }
    parts.join(" ")
}

fn describe_runtime(ext: &Value) -> String {
    let runtime = &ext["runtime"];
    let mut parts = vec![runtime["command"].as_str().unwrap_or("").to_string()];
    if let Some(args) = runtime["args"].as_array() {
        parts.extend(args.iter().map(|a| format!("\"{}\"", a.as_str().unwrap_or(""))));
    }
    parts.join(" ")
}

fn preview_plugin(path: &Path) -> Result<PackPreview, String> {
    let archive = read_plugin_zip(path)?;
    let manifest = &archive.manifest;
    let mut blocks = Vec::new();
    let mut extensions = Vec::new();
    let mut errors = Vec::new();
    let preview_dir = PathBuf::from("__00047_plugin_preview__");
    let manifest_blocks = manifest["blocks"].as_array().cloned().unwrap_or_default();
    for block in &manifest_blocks {
        let kind = block["kind"].as_str().unwrap_or("unknown");
        let entrypoint = block.get("entrypoint").and_then(Value::as_str).unwrap_or("");
        if !valid_relative_path(entrypoint) || !archive.files.contains(entrypoint) {
            errors.push(format!("{kind}: entrypoint '{entrypoint}' is missing or unsafe in the ZIP"));
            continue;
        }
        match normalize_plugin_block(block, &preview_dir, false).and_then(|s| {
            check_spec(&s)?;
            Ok(s)
        }) {
            Ok(s) => blocks.push(BlockPreview {
                kind: s["kind"].as_str().unwrap_or("").to_string(),
                title: s["title"].as_str().unwrap_or("").to_string(),
                category: s["category"].as_str().unwrap_or("").to_string(),
                command: describe(&s),
            }),
            Err(e) => errors.push(e),
        }
    }
    for (extension_type, key) in [("command", "commands"), ("service", "services")] {
        if let Some(items) = manifest.get(key).and_then(Value::as_array) {
            for ext in items {
                let id = ext["id"].as_str().unwrap_or("unknown");
                let entrypoint = ext["entrypoint"].as_str().unwrap_or("");
                if let Err(e) = validate_runtime_extension(ext, extension_type) {
                    errors.push(e);
                    continue;
                }
                if !valid_relative_path(entrypoint) || !archive.files.contains(entrypoint) {
                    errors.push(format!("{extension_type} '{id}': entrypoint '{entrypoint}' is missing or unsafe in the ZIP"));
                    continue;
                }
                extensions.push(ExtensionPreview {
                    extension_type: extension_type.into(),
                    id: id.into(),
                    title: ext["title"].as_str().unwrap_or("").into(),
                    entrypoint: entrypoint.into(),
                    command: describe_runtime(ext),
                });
            }
        }
    }
    if blocks.is_empty() && extensions.is_empty() {
        return Err(format!("no valid plugin components in this ZIP{}", if errors.is_empty() { String::new() } else { format!(": {}", errors.join("; ")) }));
    }
    let file = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    Ok(PackPreview { file, path: path.to_string_lossy().to_string(), blocks, extensions, errors })
}
pub fn preview(path: &Path) -> Result<PackPreview, String> {
    if path.extension().and_then(|x| x.to_str()).map(|x| x.eq_ignore_ascii_case("zip")).unwrap_or(false) {
        return preview_plugin(path);
    }
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
    Ok(PackPreview { file, path: path.to_string_lossy().to_string(), blocks, extensions: vec![], errors })
}

fn extract_plugin(path: &Path, destination: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("invalid ZIP archive: {e}"))?;
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let mut total_written = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| format!("cannot read ZIP entry: {e}"))?;
        let declared_size = entry.size();
        total_written = total_written.saturating_add(declared_size);
        if total_written > MAX_PLUGIN_UNPACKED {
            return Err("plugin ZIP expands beyond the 50 MB unpacked-size limit".into());
        }
        let relative = entry.enclosed_name().ok_or_else(|| format!("unsafe ZIP path: {}", entry.name()))?.to_path_buf();
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                return Err(format!("symbolic links are not allowed in plugin ZIPs: {}", entry.name()));
            }
        }
        let out = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
        let mut target = File::create(&out).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry, &mut target).map_err(|e| format!("extract failed: {e}"))?;
        if copied != declared_size {
            return Err(format!("ZIP entry size mismatch: {}", relative.display()));
        }
    }
    Ok(())
}

fn install_plugin(path: &Path, file_name: &str) -> Result<String, String> {
    let archive = read_plugin_zip(path)?;
    let id = archive.manifest["id"].as_str().ok_or("manifest id is missing")?;
    let dir = DIR.get().ok_or("blocks folder is not initialised")?;
    let packages = dir.join("packages");
    fs::create_dir_all(&packages).map_err(|e| e.to_string())?;
    let target = packages.join(id);
    let stage = packages.join(format!(".stage-{}", std::process::id()));
    let backup = packages.join(format!(".backup-{}", std::process::id()));
    if stage.exists() { fs::remove_dir_all(&stage).map_err(|e| e.to_string())?; }
    if backup.exists() { fs::remove_dir_all(&backup).map_err(|e| e.to_string())?; }
    if let Err(e) = extract_plugin(path, &stage) {
        let _ = fs::remove_dir_all(&stage);
        return Err(e);
    }
    // Validate all executable entrypoints after extraction before replacing an installed version.
    let manifest_blocks = archive.manifest["blocks"].as_array().cloned().unwrap_or_default();
    let mut valid_components = 0usize;
    for block in &manifest_blocks {
        if let Ok(spec) = normalize_plugin_block(block, &stage, true) {
            if check_spec(&spec).is_ok() {
                valid_components += 1;
            }
        }
    }
    for (extension_type, key) in [("command", "commands"), ("service", "services")] {
        if let Some(items) = archive.manifest.get(key).and_then(Value::as_array) {
            for ext in items {
                if validate_runtime_extension(ext, extension_type).is_ok() {
                    let entrypoint = ext["entrypoint"].as_str().unwrap_or("");
                    if valid_relative_path(entrypoint) && stage.join(entrypoint).is_file() {
                        valid_components += 1;
                    }
                }
            }
        }
    }
    if valid_components == 0 {
        let _ = fs::remove_dir_all(&stage);
        return Err("plugin contained no valid blocks, commands, or services after extraction".into());
    }
    if target.exists() {
        fs::rename(&target, &backup).map_err(|e| format!("could not stage previous plugin version: {e}"))?;
    }
    if let Err(e) = fs::rename(&stage, &target) {
        if backup.exists() { let _ = fs::rename(&backup, &target); }
        let _ = fs::remove_dir_all(&stage);
        return Err(format!("could not install plugin: {e}"));
    }
    if backup.exists() { let _ = fs::remove_dir_all(&backup); }
    Ok(file_name.to_string())
}

pub fn install(path: &Path) -> Result<String, String> {
    let pv = preview(path)?;
    if path.extension().and_then(|x| x.to_str()).map(|x| x.eq_ignore_ascii_case("zip")).unwrap_or(false) {
        return install_plugin(path, &pv.file);
    }
    let dir = DIR.get().ok_or("blocks folder is not initialised")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let stem: String = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "pack".into())
        .chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let dest = dir.join(format!("{stem}.json"));
    if dest.exists() && fs::canonicalize(&dest).ok() == fs::canonicalize(path).ok() {
        return Ok(pv.file);
    }
    fs::copy(path, &dest).map_err(|e| format!("copy: {e}"))?;
    Ok(pv.file)
}

/// Lists installed package metadata, including app-level extensions that do not register blocks.
pub fn list_external_plugins() -> Value {
    let Some(root) = DIR.get() else {
        return json!({"plugins": [], "errors": ["plugin directory is not initialised"]});
    };
    let packages = root.join("packages");
    let mut plugins = Vec::new();
    let mut errors = Vec::new();
    let entries = match fs::read_dir(&packages) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return json!({"plugins": [], "errors": []}),
        Err(e) => return json!({"plugins": [], "errors": [format!("cannot read packages directory: {e}")]}),
    };
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path())
        .filter(|p| p.is_dir() && p.join("manifest.json").is_file()).collect();
    dirs.sort();
    for dir in dirs {
        let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let content = match fs::read_to_string(dir.join("manifest.json")) {
            Ok(v) => v,
            Err(e) => { errors.push(format!("{name}: cannot read manifest.json: {e}")); continue; }
        };
        let manifest: Value = match serde_json::from_str(content.trim_start_matches('\u{feff}')) {
            Ok(v) => v,
            Err(e) => { errors.push(format!("{name}: invalid manifest.json: {e}")); continue; }
        };
        if let Err(e) = validate_manifest_header(&manifest) {
            errors.push(format!("{name}: {e}")); continue;
        }
        let id = manifest["id"].as_str().unwrap_or("");
        if id != name {
            errors.push(format!("{name}: package directory does not match manifest id '{id}'"));
            continue;
        }
        let commands: Vec<Value> = manifest["commands"].as_array().into_iter().flatten().map(|ext| json!({
            "id": ext["id"], "title": ext["title"], "description": ext["description"],
            "entrypoint": ext["entrypoint"], "runtime": ext["runtime"], "fields": spec::fields(ext),
            "defaults": ext["defaults"], "timeoutSec": ext["timeoutSec"]
        })).collect();
        let services: Vec<Value> = manifest["services"].as_array().into_iter().flatten().map(|ext| json!({
            "id": ext["id"], "title": ext["title"], "description": ext["description"],
            "entrypoint": ext["entrypoint"], "runtime": ext["runtime"], "fields": spec::fields(ext),
            "defaults": ext["defaults"], "autoStart": ext["autoStart"]
        })).collect();
        plugins.push(json!({
            "id": id, "name": manifest["name"], "version": manifest["version"],
            "description": manifest["description"],
            "blockCount": manifest["blocks"].as_array().map(|b| b.len()).unwrap_or(0),
            "commands": commands, "services": services
        }));
    }
    json!({"plugins": plugins, "errors": errors})
}

/// Resolves a declared app-level command/service. The entrypoint must resolve to a file
/// below the installed package directory; the UI never supplies executable paths.
pub fn runtime_extension(plugin_id: &str, extension_type: &str, extension_id: &str) -> Result<(PathBuf, Value), String> {
    if !valid_plugin_id(plugin_id) { return Err("invalid plugin id".into()); }
    if !valid_extension_id(extension_id) { return Err("invalid extension id".into()); }
    let key = match extension_type {
        "command" => "commands",
        "service" => "services",
        _ => return Err("extension type must be command or service".into()),
    };
    let root = DIR.get().ok_or("plugin directory is not initialised")?;
    let package = fs::canonicalize(root.join("packages").join(plugin_id))
        .map_err(|e| format!("plugin package not found: {e}"))?;
    let manifest_text = fs::read_to_string(package.join("manifest.json")).map_err(|e| format!("cannot read manifest.json: {e}"))?;
    if manifest_text.len() as u64 > MAX_PLUGIN_MANIFEST {
        return Err("manifest.json exceeds the 1 MB limit".into());
    }
    let manifest: Value = serde_json::from_str(manifest_text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("invalid manifest.json: {e}"))?;
    validate_manifest_header(&manifest)?;
    if manifest["id"].as_str() != Some(plugin_id) {
        return Err("package id does not match the installed directory".into());
    }
    let ext = manifest.get(key).and_then(Value::as_array)
        .and_then(|items| items.iter().find(|item| item["id"].as_str() == Some(extension_id)))
        .cloned().ok_or_else(|| format!("{extension_type} '{extension_id}' is not declared by plugin '{plugin_id}'"))?;
    validate_runtime_extension(&ext, extension_type)?;
    let entrypoint = ext["entrypoint"].as_str().unwrap_or("");
    if !valid_relative_path(entrypoint) { return Err("extension entrypoint is not a safe relative path".into()); }
    let resolved = fs::canonicalize(package.join(entrypoint))
        .map_err(|e| format!("extension entrypoint could not be resolved: {e}"))?;
    if !resolved.starts_with(&package) || !resolved.is_file() {
        return Err("extension entrypoint must be a file inside its package directory".into());
    }
    Ok((package, ext))
}

#[cfg(test)]
mod script_plugin_tests {
    use super::*;
    use std::io::Write;
    use zip::write::FileOptions;

    #[test]
    fn package_identifiers_and_entrypoints_reject_path_traversal() {
        assert!(valid_plugin_id("com.example.file-tools"));
        assert!(!valid_plugin_id("../escape"));
        assert!(!valid_plugin_id("UPPERCASE"));
        assert!(valid_relative_path("scripts/main.py"));
        assert!(!valid_relative_path("../main.py"));
        assert!(!valid_relative_path("scripts\\\\main.py"));
        assert!(!valid_relative_path("/absolute/main.py"));
    }

    #[test]
    fn manifest_block_normalizes_to_a_generic_runtime() {
        let plugin_dir = PathBuf::from("example-plugin");
        let block = json!({
            "kind": "user.example",
            "category": "action",
            "title": "Example",
            "entrypoint": "scripts/main.py",
            "runtime": { "command": "python", "args": ["{entrypoint}"] },
            "fields": []
        });
        let normalized = normalize_plugin_block(&block, &plugin_dir, false).unwrap();
        assert_eq!(check_spec(&normalized).unwrap(), "user.example");
        assert_eq!(normalized["run"]["type"], "plugin");
        assert_eq!(normalized["run"]["program"], "python");
        assert_eq!(normalized["run"]["args"][0], "{entrypoint}");
    }

    #[test]
    fn runtime_only_manifests_are_valid_without_automation_blocks() {
        let manifest = json!({
            "manifestVersion": 1,
            "id": "com.example.runtime-only",
            "name": "Runtime-only package",
            "version": "1.0.0",
            "commands": [{
                "id": "inspect", "title": "Inspect", "entrypoint": "scripts/inspect.py",
                "runtime": { "command": "python", "args": ["{entrypoint}"] }, "fields": []
            }],
            "services": [{
                "id": "watcher", "title": "Watcher", "entrypoint": "scripts/watch.py",
                "runtime": { "command": "python", "args": ["{entrypoint}"] }, "autoStart": false
            }]
        });
        assert!(validate_manifest_header(&manifest).is_ok());
    }

    #[test]
    fn runtime_extensions_reject_unsafe_entrypoints_and_reserved_fields() {
        let unsafe_path = json!({
            "id": "runner", "title": "Runner", "entrypoint": "../runner.py",
            "runtime": { "command": "python", "args": ["{entrypoint}"] }
        });
        assert!(validate_runtime_extension(&unsafe_path, "command").is_err());
        let reserved_field = json!({
            "id": "runner", "title": "Runner", "entrypoint": "scripts/runner.py",
            "runtime": { "command": "python", "args": ["{entrypoint}"] },
            "fields": [{"key":"pluginDir","label":"Directory","type":"text"}]
        });
        assert!(validate_runtime_extension(&reserved_field, "command").is_err());
    }

    #[test]
    fn zip_preview_accepts_a_command_only_package() {
        let path = std::env::temp_dir().join(format!("00047-command-plugin-test-{}.zip", std::process::id()));
        let _ = fs::remove_file(&path);
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let manifest = r#"{"manifestVersion":1,"id":"com.example.command-only","name":"Command Only","version":"1.0.0","commands":[{"id":"hello","title":"Hello","entrypoint":"scripts/hello.py","runtime":{"command":"python","args":["{entrypoint}"]},"fields":[]} ]}"#;
        zip.start_file("manifest.json", FileOptions::default()).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("scripts/hello.py", FileOptions::default()).unwrap();
        zip.write_all(b"print('hello')").unwrap();
        zip.finish().unwrap();

        let result = preview_plugin(&path).unwrap();
        assert!(result.blocks.is_empty());
        assert_eq!(result.extensions.len(), 1);
        assert_eq!(result.extensions[0].extension_type, "command");
        assert_eq!(result.extensions[0].id, "hello");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn zip_reader_requires_and_parses_root_manifest() {
        let path = std::env::temp_dir().join(format!("00047-plugin-test-{}.zip", std::process::id()));
        let _ = fs::remove_file(&path);
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let manifest = r#"{"manifestVersion":1,"id":"com.example.test","name":"Test","version":"1.0.0","blocks":[{"kind":"user.test","category":"action","title":"Test","entrypoint":"scripts/main.py","runtime":{"command":"python","args":["{entrypoint}"]},"fields":[]}]}"#;
        zip.start_file("manifest.json", FileOptions::default()).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("scripts/main.py", FileOptions::default()).unwrap();
        zip.write_all(b"print('ok')").unwrap();
        zip.finish().unwrap();

        let result = read_plugin_zip(&path).unwrap();
        assert_eq!(result.manifest["id"], "com.example.test");
        assert!(result.files.contains("manifest.json"));
        assert!(result.files.contains("scripts/main.py"));
        let _ = fs::remove_file(path);
    }
}
