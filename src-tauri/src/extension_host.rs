//! General-purpose external extension host.
//!
//! Extensions are packaged as ZIP files with a root-level extension.json. The
//! declared runtime may be any executable/interpreter available on the machine,
//! or an executable included in the package. The host manages their lifecycle;
//! it does not sandbox their code.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::mpsc::UnboundedReceiver,
};
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;

const MAX_ZIP_BYTES: u64 = 20 * 1024 * 1024;
const MAX_ENTRIES: usize = 500;
const MAX_UNPACKED_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeSpec {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionManifest {
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub entrypoint: String,
    pub runtime: RuntimeSpec,
    #[serde(default)]
    pub auto_start: bool,
    /// Optional process-control protocol. Currently only "stdio-jsonl-v1" is supported.
    #[serde(default)]
    pub protocol: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatus {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub runtime: String,
    pub entrypoint: String,
    pub auto_start: bool,
    pub protocol: Option<String>,
    pub state: String,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCatalog {
    pub root: String,
    pub extensions: Vec<ExtensionStatus>,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPreview {
    pub file: String,
    pub path: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub entrypoint: String,
    pub command: String,
    pub args: Vec<String>,
    pub auto_start: bool,
    pub protocol: Option<String>,
}

struct RuntimeRecord {
    status: ExtensionStatus,
    cancel: Option<CancellationToken>,
    control: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

struct PluginArchive {
    manifest: ExtensionManifest,
    files: HashSet<String>,
}

pub struct ExtensionManager {
    root: PathBuf,
    app: AppHandle,
    state: Mutex<HashMap<String, RuntimeRecord>>,
}

impl ExtensionManager {
    pub fn new(root: PathBuf, app: AppHandle) -> Self {
        let _ = fs::create_dir_all(root.join("packages"));
        Self { root, app, state: Mutex::new(HashMap::new()) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn packages_dir(&self) -> PathBuf {
        self.root.join("packages")
    }

    fn package_dir(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) {
            return Err("invalid extension id".into());
        }
        Ok(self.packages_dir().join(id))
    }

    fn emit_status(&self, status: &ExtensionStatus) {
        let _ = self.app.emit("extension-state", status);
    }

    fn emit_log(&self, id: &str, stream: &str, message: &str) {
        let _ = self.app.emit("extension-log", serde_json::json!({
            "id": id,
            "stream": stream,
            "message": message,
            "ts": now_ms()
        }));
    }

    pub fn catalog(&self) -> ExtensionCatalog {
        let mut extensions = Vec::new();
        let mut errors = Vec::new();
        if let Ok(entries) = fs::read_dir(self.packages_dir()) {
            let mut dirs: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path())
                .filter(|p| p.is_dir()).collect();
            dirs.sort();
            for dir in dirs {
                let manifest_path = dir.join("extension.json");
                let display = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let loaded = read_manifest(&manifest_path);
                let manifest = match loaded {
                    Ok(m) => m,
                    Err(e) => { errors.push(format!("{display}: {e}")); continue; }
                };
                if manifest.id != display {
                    errors.push(format!("{display}: directory name does not match extension id '{}'", manifest.id));
                    continue;
                }
                if let Err(e) = validate_manifest(&manifest, Some(&dir), None) {
                    errors.push(format!("{}: {e}", manifest.id));
                    continue;
                }
                let mut status = default_status(&manifest);
                if let Some(record) = self.state.lock().unwrap().get(&manifest.id) {
                    status = record.status.clone();
                }
                extensions.push(status);
            }
        }
        extensions.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        ExtensionCatalog {
            root: self.root.to_string_lossy().to_string(),
            extensions,
            errors,
        }
    }

    pub fn preview_zip(&self, path: &Path) -> Result<ExtensionPreview, String> {
        let archive = read_archive(path)?;
        let dir = PathBuf::from("<extensionDir>");
        validate_manifest(&archive.manifest, None, Some(&archive.files))?;
        let entrypoint = dir.join(&archive.manifest.entrypoint).to_string_lossy().to_string();
        let command = expand_template(&archive.manifest.runtime.command, &dir, &archive.manifest, &entrypoint);
        let args = archive.manifest.runtime.args.iter()
            .map(|a| expand_template(a, &dir, &archive.manifest, &entrypoint)).collect();
        Ok(ExtensionPreview {
            file: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            path: path.to_string_lossy().to_string(),
            id: archive.manifest.id.clone(),
            name: archive.manifest.name.clone(),
            version: archive.manifest.version.clone(),
            description: archive.manifest.description.clone(),
            entrypoint: archive.manifest.entrypoint.clone(),
            command,
            args,
            auto_start: archive.manifest.auto_start,
            protocol: archive.manifest.protocol.clone(),
        })
    }

    pub fn install_zip(&self, path: &Path) -> Result<String, String> {
        let preview = self.preview_zip(path)?;
        let archive = read_archive(path)?;
        let packages = self.packages_dir();
        fs::create_dir_all(&packages).map_err(|e| format!("create extensions folder: {e}"))?;
        let target = packages.join(&archive.manifest.id);
        let stage = packages.join(format!(".stage-{}-{}", archive.manifest.id, std::process::id()));
        let backup = packages.join(format!(".backup-{}-{}", archive.manifest.id, std::process::id()));
        if stage.exists() { fs::remove_dir_all(&stage).map_err(|e| e.to_string())?; }
        if backup.exists() { fs::remove_dir_all(&backup).map_err(|e| e.to_string())?; }
        if let Err(e) = extract_archive(path, &stage) {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
        let manifest_path = stage.join("extension.json");
        let extracted = match read_manifest(&manifest_path) {
            Ok(m) => m,
            Err(e) => {
                let _ = fs::remove_dir_all(&stage);
                return Err(e);
            }
        };
        if extracted.id != archive.manifest.id {
            let _ = fs::remove_dir_all(&stage);
            return Err("extension manifest changed during installation".into());
        }
        if let Err(e) = validate_manifest(&extracted, Some(&stage), None) {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
        if target.exists() {
            if self.is_running(&extracted.id) {
                let _ = fs::remove_dir_all(&stage);
                return Err("stop this extension before installing an update".into());
            }
            fs::rename(&target, &backup).map_err(|e| format!("could not back up installed extension: {e}"))?;
        }
        if let Err(e) = fs::rename(&stage, &target) {
            if backup.exists() { let _ = fs::rename(&backup, &target); }
            let _ = fs::remove_dir_all(&stage);
            return Err(format!("could not install extension: {e}"));
        }
        if backup.exists() { let _ = fs::remove_dir_all(&backup); }
        self.emit_log(&preview.id, "host", &format!("Installed {} {}", preview.name, preview.version));
        Ok(preview.name)
    }

    pub fn set_auto_start(&self, id: &str, enabled: bool) -> Result<(), String> {
        let dir = self.package_dir(id)?;
        let path = dir.join("extension.json");
        let mut manifest = read_manifest(&path)?;
        if manifest.id != id {
            return Err("extension directory id does not match extension.json".into());
        }
        validate_manifest(&manifest, Some(&dir), None)?;
        manifest.auto_start = enabled;
        let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
        fs::write(&path, bytes).map_err(|e| format!("write extension manifest: {e}"))?;
        if let Some(record) = self.state.lock().unwrap().get_mut(id) {
            record.status.auto_start = enabled;
            let status = record.status.clone();
            self.emit_status(&status);
        }
        Ok(())
    }

    pub fn uninstall(&self, id: &str) -> Result<(), String> {
        if self.is_running(id) {
            return Err("stop the extension before uninstalling it".into());
        }
        let dir = self.package_dir(id)?;
        if !dir.exists() {
            return Err("extension is not installed".into());
        }
        fs::remove_dir_all(&dir).map_err(|e| format!("remove extension: {e}"))?;
        self.state.lock().unwrap().remove(id);
        self.emit_log(id, "host", "Uninstalled extension");
        Ok(())
    }

    fn is_running(&self, id: &str) -> bool {
        self.state.lock().unwrap().get(id)
            .map(|r| r.status.state == "running" || r.status.state == "stopping")
            .unwrap_or(false)
    }

    pub async fn start(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let dir = self.package_dir(id)?;
        if self.is_running(id) {
            return Err("extension is already running".into());
        }
        let manifest = read_manifest(&dir.join("extension.json"))?;
        if manifest.id != id {
            return Err("extension directory id does not match extension.json".into());
        }
        validate_manifest(&manifest, Some(&dir), None)?;
        let entrypoint = dir.join(&manifest.entrypoint).to_string_lossy().to_string();
        let command = expand_template(&manifest.runtime.command, &dir, &manifest, &entrypoint);
        if command.trim().is_empty() {
            return Err("extension runtime command is empty".into());
        }
        let args: Vec<String> = manifest.runtime.args.iter()
            .map(|a| expand_template(a, &dir, &manifest, &entrypoint)).collect();
        let mut child_command = Command::new(&command);
        child_command.args(&args)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        for (key, value) in &manifest.runtime.env {
            if !is_reserved_env(key) {
                child_command.env(key, expand_template(value, &dir, &manifest, &entrypoint));
            }
        }
        child_command.env("EXTENSION_ID", &manifest.id)
            .env("EXTENSION_DIR", &dir)
            .env("EXTENSION_ENTRYPOINT", &entrypoint);
        #[cfg(windows)]
        child_command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

        let mut child = child_command.spawn().map_err(|e| {
            self.set_failed(&manifest, format!("could not start '{command}': {e}"));
            format!("could not start '{command}': {e}")
        })?;
        let pid = child.id();
        let cancel = CancellationToken::new();
        let mut status = default_status(&manifest);
        status.state = "running".into();
        status.pid = pid;
        status.started_at = Some(now_ms());
        status.last_error = None;
        {
            let mut states = self.state.lock().unwrap();
            states.insert(manifest.id.clone(), RuntimeRecord {
                status: status.clone(),
                cancel: Some(cancel.clone()),
            });
        }
        self.emit_status(&status);
        self.emit_log(&manifest.id, "host", &format!("Started runtime: {command}"));
        if let Some(stdout) = child.stdout.take() {
            spawn_log_reader(self.app.clone(), manifest.id.clone(), "stdout", stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_log_reader(self.app.clone(), manifest.id.clone(), "stderr", stderr);
        }
        let manager = self.clone();
        let ext_id = manifest.id.clone();
        tokio::spawn(async move {
            let result = loop {
                if cancel.is_cancelled() {
                    let _ = child.start_kill();
                    break child.wait().await;
                }
                match child.try_wait() {
                    Ok(Some(exit)) => break Ok(exit),
                    Ok(None) => tokio::time::sleep(Duration::from_millis(250)).await,
                    Err(e) => break Err(e),
                }
            };
            manager.finish_process(&ext_id, &cancel, result);
        });
        Ok(())
    }

    fn set_failed(&self, manifest: &ExtensionManifest, error: String) {
        let mut status = default_status(manifest);
        status.state = "failed".into();
        status.last_error = Some(error.clone());
        self.state.lock().unwrap().insert(manifest.id.clone(), RuntimeRecord { status: status.clone(), cancel: None, control: None });
        self.emit_status(&status);
        self.emit_log(&manifest.id, "host", &error);
    }

    fn finish_process(&self, id: &str, cancel: &CancellationToken, result: std::io::Result<std::process::ExitStatus>) {
        let mut states = self.state.lock().unwrap();
        let Some(record) = states.get_mut(id) else { return; };
        let stopped_by_user = cancel.is_cancelled();
        record.status.pid = None;
        record.status.started_at = None;
        record.cancel = None;
        match result {
            Ok(exit) if stopped_by_user => {
                record.status.state = "stopped".into();
                record.status.last_error = None;
            }
            Ok(exit) if exit.success() => {
                record.status.state = "stopped".into();
                record.status.last_error = None;
            }
            Ok(exit) => {
                let error = format!("extension exited with code {}", exit.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into()));
                record.status.state = "failed".into();
                record.status.last_error = Some(error.clone());
                self.emit_log(id, "host", &error);
            }
            Err(e) => {
                let error = format!("extension process monitor failed: {e}");
                record.status.state = if stopped_by_user { "stopped" } else { "failed" }.into();
                record.status.last_error = if stopped_by_user { None } else { Some(error.clone()) };
                if !stopped_by_user { self.emit_log(id, "host", &error); }
            }
        }
        let status = record.status.clone();
        drop(states);
        self.emit_status(&status);
    }

    pub fn stop(&self, id: &str) -> Result<(), String> {
        let mut states = self.state.lock().unwrap();
        let record = states.get_mut(id).ok_or("extension is not running")?;
        if record.status.state != "running" {
            return Err("extension is not running".into());
        }
        let token = record.cancel.as_ref().ok_or("extension process is not controllable")?.clone();
        record.status.state = "stopping".into();
        let status = record.status.clone();
        token.cancel();
        drop(states);
        self.emit_status(&status);
        Ok(())
    }

    pub async fn stop_and_wait(&self, id: &str) -> Result<(), String> {
        let should_stop = self.state.lock().unwrap().get(id)
            .map(|r| r.status.state == "running").unwrap_or(false);
        if should_stop {
            self.stop(id)?;
        }
        for _ in 0..40 {
            if !self.is_running(id) { return Ok(()); }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if self.is_running(id) { Err("extension did not stop within 10 seconds".into()) } else { Ok(()) }
    }

    pub async fn restart(self: &Arc<Self>, id: &str) -> Result<(), String> {
        self.stop_and_wait(id).await?;
        self.start(id).await
    }

    pub fn start_autostart(self: &Arc<Self>) {
        let ids: Vec<String> = self.catalog().extensions.into_iter()
            .filter(|e| e.auto_start).map(|e| e.id).collect();
        for id in ids {
            let manager = self.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = manager.start(&id).await {
                    manager.emit_log(&id, "host", &format!("Auto-start failed: {e}"));
                }
            });
        }
    }

    pub async fn shutdown(&self) {
        let ids: Vec<String> = self.state.lock().unwrap().iter()
            .filter(|(_, r)| r.status.state == "running")
            .map(|(id, _)| id.clone()).collect();
        for id in &ids {
            let _ = self.stop(id);
        }
        for _ in 0..40 {
            if ids.iter().all(|id| !self.is_running(id)) { return; }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

fn default_status(manifest: &ExtensionManifest) -> ExtensionStatus {
    ExtensionStatus {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        runtime: manifest.runtime.command.clone(),
        entrypoint: manifest.entrypoint.clone(),
        auto_start: manifest.auto_start,
        protocol: manifest.protocol.clone(),
        state: "stopped".into(),
        pid: None,
        started_at: None,
        last_error: None,
    }
}

fn is_reserved_env(key: &str) -> bool {
    matches!(key, "EXTENSION_ID" | "EXTENSION_DIR" | "EXTENSION_ENTRYPOINT")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && !id.starts_with('.') && !id.ends_with('.')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
}

fn valid_relative_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains(':') || path.contains('\0') {
        return false;
    }
    path.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}

fn validate_manifest(manifest: &ExtensionManifest, dir: Option<&Path>, files: Option<&HashSet<String>>) -> Result<(), String> {
    if manifest.manifest_version != 1 { return Err("manifestVersion must be 1".into()); }
    if !valid_id(&manifest.id) { return Err("id must use lowercase a-z, digits, dots, dashes or underscores".into()); }
    if manifest.name.trim().is_empty() || manifest.name.chars().count() > 100 {
        return Err("name is required (max 100 characters)".into());
    }
    if manifest.version.trim().is_empty() || manifest.version.len() > 40 {
        return Err("version is required (max 40 characters)".into());
    }
    if manifest.description.chars().count() > 1000 { return Err("description exceeds 1000 characters".into()); }
    if manifest.protocol.as_deref().is_some_and(|p| p != "stdio-jsonl-v1") {
        return Err("protocol must be omitted or set to \"stdio-jsonl-v1\"".into());
    }
    if !valid_relative_path(&manifest.entrypoint) { return Err("entrypoint must be a safe relative path inside the package".into()); }
    if manifest.runtime.command.trim().is_empty() || manifest.runtime.command.len() > 2048 {
        return Err("runtime.command is required (max 2048 characters)".into());
    }
    if manifest.runtime.args.len() > 128 || manifest.runtime.args.iter().any(|a| a.len() > 8192 || a.contains('\0')) {
        return Err("runtime.args may contain at most 128 strings, each up to 8192 characters".into());
    }
    if manifest.runtime.env.len() > 128 {
        return Err("runtime.env may contain at most 128 variables".into());
    }
    for (key, value) in &manifest.runtime.env {
        if key.is_empty() || key.len() > 128 || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("invalid environment variable name '{key}'"));
        }
        if value.len() > 8192 || value.contains('\0') { return Err(format!("environment variable '{key}' is too long or contains NUL")); }
    }
    if let Some(files) = files {
        if !files.contains("extension.json") { return Err("extension.json must be at the ZIP root".into()); }
        if !files.contains(&manifest.entrypoint) { return Err(format!("entrypoint '{}' is missing from the ZIP", manifest.entrypoint)); }
    }
    if let Some(dir) = dir {
        if !dir.join(&manifest.entrypoint).is_file() {
            return Err(format!("entrypoint '{}' does not exist", manifest.entrypoint));
        }
    }
    Ok(())
}

fn expand_template(value: &str, dir: &Path, manifest: &ExtensionManifest, entrypoint: &str) -> String {
    value.replace("{extensionDir}", &dir.to_string_lossy())
        .replace("{entrypoint}", entrypoint)
        .replace("{extensionId}", &manifest.id)
}

fn read_manifest(path: &Path) -> Result<ExtensionManifest, String> {
    let meta = fs::metadata(path).map_err(|e| format!("cannot read extension.json: {e}"))?;
    if meta.len() > MAX_MANIFEST_BYTES { return Err("extension.json exceeds 1 MB".into()); }
    let text = fs::read_to_string(path).map_err(|e| format!("cannot decode extension.json as UTF-8: {e}"))?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("invalid extension.json: {e}"))
}

fn read_archive(path: &Path) -> Result<PluginArchive, String> {
    if path.extension().and_then(|x| x.to_str()).map(|x| !x.eq_ignore_ascii_case("zip")).unwrap_or(true) {
        return Err("extension packages must be .zip files".into());
    }
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_ZIP_BYTES { return Err("extension ZIP exceeds 20 MB".into()); }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("invalid ZIP: {e}"))?;
    if archive.len() == 0 || archive.len() > MAX_ENTRIES { return Err(format!("extension ZIP must contain 1 to {MAX_ENTRIES} entries")); }
    let mut files = HashSet::new();
    let mut unpacked = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| format!("cannot read ZIP entry: {e}"))?;
        let name = entry.name().to_string();
        if entry.enclosed_name().is_none() { return Err(format!("unsafe ZIP path: {name}")); }
        if name.contains('\\') || name.starts_with('/') || name.contains(':') {
            return Err(format!("unsafe ZIP path: {name}"));
        }
        let logical_name = if entry.is_dir() { name.trim_end_matches('/').to_string() } else { name.clone() };
        if !valid_relative_path(&logical_name) { return Err(format!("unsafe ZIP path: {name}")); }
        if !files.insert(name.clone()) { return Err(format!("duplicate ZIP entry: {name}")); }
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 { return Err(format!("symbolic links are not allowed: {name}")); }
        }
        unpacked = unpacked.saturating_add(entry.size());
        if unpacked > MAX_UNPACKED_BYTES { return Err("extension ZIP expands beyond 50 MB".into()); }
        if name == "extension.json" && entry.size() > MAX_MANIFEST_BYTES { return Err("extension.json exceeds 1 MB".into()); }
    }
    if !files.contains("extension.json") { return Err("extension.json must be at the ZIP root".into()); }
    let mut text = String::new();
    archive.by_name("extension.json").map_err(|e| e.to_string())?
        .take(MAX_MANIFEST_BYTES + 1).read_to_string(&mut text).map_err(|e| format!("extension.json is not UTF-8: {e}"))?;
    if text.len() as u64 > MAX_MANIFEST_BYTES { return Err("extension.json exceeds 1 MB".into()); }
    let manifest: ExtensionManifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("invalid extension.json: {e}"))?;
    validate_manifest(&manifest, None, Some(&files))?;
    Ok(PluginArchive { manifest, files })
}

fn extract_archive(path: &Path, destination: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES { return Err("extension ZIP expands beyond 50 MB".into()); }
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 { return Err(format!("symbolic links are not allowed: {}", entry.name())); }
        }
        let rel = entry.enclosed_name().ok_or_else(|| format!("unsafe ZIP path: {}", entry.name()))?.to_path_buf();
        let output = destination.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&output).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = output.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
        let mut file = File::create(&output).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
        if copied != entry.size() { return Err(format!("ZIP entry size mismatch: {}", output.display())); }
    }
    Ok(())
}

fn spawn_control_writer<W>(app: AppHandle, id: String, mut writer: W, mut messages: UnboundedReceiver<String>)
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        while let Some(message) = messages.recv().await {
            if let Err(e) = writer.write_all(message.as_bytes()).await {
                let _ = app.emit("extension-log", serde_json::json!({
                    "id": id, "stream": "host", "message": format!("could not send lifecycle message: {e}"), "ts": now_ms()
                }));
                break;
            }
            if let Err(e) = writer.write_all(b"\n").await {
                let _ = app.emit("extension-log", serde_json::json!({
                    "id": id, "stream": "host", "message": format!("could not finish lifecycle message: {e}"), "ts": now_ms()
                }));
                break;
            }
            let _ = writer.flush().await;
        }
    });
}

fn spawn_log_reader<R>(app: AppHandle, id: String, stream: &'static str, reader: R)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) if !line.trim().is_empty() => {
                    let _ = app.emit("extension-log", serde_json::json!({
                        "id": id, "stream": stream, "message": line, "ts": now_ms()
                    }));
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    let _ = app.emit("extension-log", serde_json::json!({
                        "id": id, "stream": "host", "message": format!("could not read {stream}: {e}"), "ts": now_ms()
                    }));
                    break;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use zip::write::FileOptions;

    fn sample() -> ExtensionManifest {
        serde_json::from_value(json!({
            "manifestVersion": 1,
            "id": "org.example.hello",
            "name": "Hello extension",
            "version": "1.0.0",
            "description": "Test",
            "entrypoint": "scripts/main.py",
            "runtime": { "command": "python", "args": ["{entrypoint}"], "env": {"MODE":"test"} },
            "autoStart": false
        })).unwrap()
    }

    #[test]
    fn accepts_language_agnostic_manifest() {
        let manifest = sample();
        let files = HashSet::from(["extension.json".to_string(), "scripts/main.py".to_string()]);
        assert!(validate_manifest(&manifest, None, Some(&files)).is_ok());
    }

    #[test]
    fn rejects_path_traversal_and_bad_ids() {
        let mut manifest = sample();
        manifest.id = "../escape".into();
        assert!(validate_manifest(&manifest, None, None).is_err());
        manifest.id = "org.example.hello".into();
        manifest.entrypoint = "../outside.py".into();
        assert!(validate_manifest(&manifest, None, None).is_err());
    }

    #[test]
    fn expands_runtime_templates_without_shell_concatenation() {
        let manifest = sample();
        let dir = PathBuf::from("C:/extensions/org.example.hello");
        let out = expand_template("{extensionDir}\\bin\\runner.exe {entrypoint}", &dir, &manifest, "C:/extensions/org.example.hello/scripts/main.py");
        assert!(out.contains("C:/extensions/org.example.hello"));
        assert!(out.contains("scripts/main.py"));
    }

    #[test]
    fn rejects_unknown_manifest_version() {
        let mut manifest = sample();
        manifest.manifest_version = 2;
        assert!(validate_manifest(&manifest, None, None).is_err());
    }

    #[test]
    fn reads_a_zip_with_root_manifest_and_entrypoint() {
        let path = std::env::temp_dir().join(format!("00047-extension-test-{}.zip", std::process::id()));
        let _ = fs::remove_file(&path);
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let manifest = serde_json::to_vec(&sample()).unwrap();
        zip.start_file("extension.json", FileOptions::default()).unwrap();
        zip.write_all(&manifest).unwrap();
        zip.start_file("scripts/main.py", FileOptions::default()).unwrap();
        zip.write_all(b"print('ok')").unwrap();
        zip.finish().unwrap();

        let archive = read_archive(&path).unwrap();
        assert_eq!(archive.manifest.id, "org.example.hello");
        assert!(archive.files.contains("scripts/main.py"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_a_zip_when_its_entrypoint_is_missing() {
        let path = std::env::temp_dir().join(format!("00047-extension-invalid-{}.zip", std::process::id()));
        let _ = fs::remove_file(&path);
        let file = File::create(&path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let manifest = serde_json::to_vec(&sample()).unwrap();
        zip.start_file("extension.json", FileOptions::default()).unwrap();
        zip.write_all(&manifest).unwrap();
        zip.finish().unwrap();

        assert!(read_archive(&path).is_err());
        let _ = fs::remove_file(path);
    }
}
