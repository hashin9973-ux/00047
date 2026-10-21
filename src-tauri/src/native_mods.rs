//! Native, explicitly enabled Windows shell mods for 00047.
//!
//! This loader deliberately supports only an allow-list of interactive Windows shell
//! processes and 64-bit DLLs. It is not a general-purpose arbitrary-process injector.
//! Mods are native code and are not sandboxed; a faulty DLL can crash its target process.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, Mutex},
};
use zip::ZipArchive;

const MAX_ZIP_BYTES: u64 = 20 * 1024 * 1024;
const MAX_FILES: usize = 500;
const MAX_UNPACKED_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ENABLED_STATE_BYTES: u64 = 1024 * 1024;
static STATE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const REQUIRED_EXPORTS: [&str; 3] = [
    "WmodGetApiVersion",
    "WmodInitializeV1",
    "WmodShutdownV1",
];

pub const SUPPORTED_TARGETS: [&str; 4] = [
    "explorer.exe",
    "StartMenuExperienceHost.exe",
    "ShellExperienceHost.exe",
    "SearchHost.exe",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ModManifest {
    manifest_version: u64,
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    architecture: String,
    module: String,
    targets: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState {
    #[serde(default)]
    enabled: HashSet<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModPreview {
    pub file: String,
    pub path: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub architecture: String,
    pub module: String,
    pub targets: Vec<String>,
    pub exports: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeTarget {
    pub pid: u32,
    pub executable: String,
    pub architecture: String,
    pub eligible: bool,
    pub detail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModRecord {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub architecture: String,
    pub module: String,
    pub targets: Vec<String>,
    pub enabled: bool,
    pub status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModList {
    pub dir: String,
    pub mods: Vec<NativeModRecord>,
    pub targets: Vec<NativeTarget>,
    pub errors: Vec<String>,
}

struct ReadPackage {
    manifest: ModManifest,
}

pub struct NativeModManager {
    root: PathBuf,
    state: Mutex<PersistedState>,
    statuses: Mutex<HashMap<String, String>>,
}

impl NativeModManager {
    pub fn new(root: PathBuf) -> Self {
        let _ = fs::create_dir_all(root.join("packages"));
        let state = load_persisted_state(&root.join("enabled.json"));
        Self {
            root,
            state: Mutex::new(state),
            statuses: Mutex::new(HashMap::new()),
        }
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    fn packages_dir(&self) -> PathBuf {
        self.root.join("packages")
    }

    fn package_dir(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) {
            return Err("Invalid native mod ID.".into());
        }
        Ok(self.packages_dir().join(id))
    }

    fn enabled_snapshot(&self) -> HashSet<String> {
        self.state.lock().map(|s| s.enabled.clone()).unwrap_or_default()
    }

    fn persist_state(&self, state: &PersistedState) -> Result<(), String> {
        if state.enabled.iter().any(|id| !valid_id(id)) {
            return Err("Native mod state contains an invalid mod ID.".into());
        }
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| format!("serialize native mod state: {e}"))?;
        if bytes.len() as u64 > MAX_ENABLED_STATE_BYTES {
            return Err("Native mod enabled-state file exceeds the 1 MB limit.".into());
        }
        atomic_write_enabled_state(&self.root, &bytes)
    }

    fn set_status(&self, id: &str, status: String) {
        if let Ok(mut statuses) = self.statuses.lock() {
            statuses.insert(id.to_string(), status);
        }
    }

    pub fn list(&self) -> NativeModList {
        let enabled = self.enabled_snapshot();
        let statuses = self.statuses.lock().map(|s| s.clone()).unwrap_or_default();
        let mut mods = Vec::new();
        let mut errors = Vec::new();

        let mut folders: Vec<PathBuf> = match fs::read_dir(self.packages_dir()) {
            Ok(entries) => entries.filter_map(Result::ok).map(|e| e.path())
                .filter(|p| p.is_dir() && p.file_name().and_then(|n| n.to_str()).map(|s| !s.starts_with('.')).unwrap_or(false))
                .collect(),
            Err(e) => {
                errors.push(format!("Cannot read native mod packages: {e}"));
                Vec::new()
            }
        };
        folders.sort();

        for folder in folders {
            let folder_name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let manifest = match load_manifest(&folder) {
                Ok(m) => m,
                Err(e) => { errors.push(format!("{folder_name}: {e}")); continue; }
            };
            if manifest.id != folder_name {
                errors.push(format!("{folder_name}: mod ID does not match its package directory"));
                continue;
            }
            let module = folder.join(&manifest.module);
            if let Err(e) = validate_pe_file(&module) {
                errors.push(format!("{}: {e}", manifest.id));
                continue;
            }
            let is_enabled = enabled.contains(&manifest.id);
            let status = statuses.get(&manifest.id).cloned().unwrap_or_else(|| {
                if is_enabled { "Enabled; waiting for target process".into() } else { "Disabled".into() }
            });
            mods.push(NativeModRecord {
                id: manifest.id.clone(),
                name: manifest.name,
                version: manifest.version,
                description: manifest.description,
                architecture: manifest.architecture,
                module: manifest.module,
                targets: manifest.targets,
                enabled: is_enabled,
                status,
            });
        }

        let targets = match discover_targets() {
            Ok(targets) => targets,
            Err(e) => { errors.push(e); Vec::new() }
        };

        NativeModList {
            dir: self.root.to_string_lossy().to_string(),
            mods,
            targets,
            errors,
        }
    }

    pub fn preview(&self, path: &Path) -> Result<NativeModPreview, String> {
        let read = read_package(path)?;
        let m = read.manifest;
        Ok(NativeModPreview {
            file: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            path: path.to_string_lossy().to_string(),
            id: m.id,
            name: m.name,
            version: m.version,
            description: m.description,
            architecture: m.architecture,
            module: m.module,
            targets: m.targets,
            exports: REQUIRED_EXPORTS.iter().map(|s| s.to_string()).collect(),
        })
    }

    pub fn install(&self, path: &Path) -> Result<String, String> {
        let read = read_package(path)?;
        let id = read.manifest.id.clone();
        if self.enabled_snapshot().contains(&id) {
            return Err("Disable this native mod before updating its files.".into());
        }

        let packages = self.packages_dir();
        fs::create_dir_all(&packages).map_err(|e| e.to_string())?;
        let stage = packages.join(format!(".stage-{}-{id}", std::process::id()));
        let backup = packages.join(format!(".backup-{}-{id}", std::process::id()));
        let target = self.package_dir(&id)?;

        if stage.exists() { fs::remove_dir_all(&stage).map_err(|e| e.to_string())?; }
        if backup.exists() { fs::remove_dir_all(&backup).map_err(|e| e.to_string())?; }
        if let Err(e) = extract_package(path, &stage) {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }

        let staged = match load_manifest(&stage) {
            Ok(m) => m,
            Err(e) => { let _ = fs::remove_dir_all(&stage); return Err(e); }
        };
        if staged.id != id {
            let _ = fs::remove_dir_all(&stage);
            return Err("Mod ID changed while staging install.".into());
        }
        if let Err(e) = validate_pe_file(&stage.join(&staged.module)) {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }

        if target.exists() {
            fs::rename(&target, &backup).map_err(|e| format!("stage existing mod: {e}"))?;
        }
        if let Err(e) = fs::rename(&stage, &target) {
            if backup.exists() { let _ = fs::rename(&backup, &target); }
            let _ = fs::remove_dir_all(&stage);
            return Err(format!("install mod: {e}"));
        }
        if backup.exists() { let _ = fs::remove_dir_all(&backup); }
        self.set_status(&id, "Installed; disabled".into());
        Ok(id)
    }

    pub fn enable(&self, id: &str) -> Result<String, String> {
        let folder = self.package_dir(id)?;
        let manifest = load_manifest(&folder)?;
        if manifest.id != id {
            return Err("Mod ID does not match its package directory.".into());
        }
        validate_pe_file(&folder.join(&manifest.module))?;

        {
            let mut state = self.state.lock().map_err(|_| "Native mod state is unavailable.")?;
            let was_enabled = state.enabled.contains(id);
            state.enabled.insert(id.to_string());
            if let Err(e) = self.persist_state(&state) {
                if !was_enabled {
                    state.enabled.remove(id);
                }
                return Err(e);
            }
        }

        match self.apply_one(id) {
            Ok(status) => {
                self.set_status(id, status.clone());
                Ok(status)
            }
            Err(e) => {
                self.disable_after_load_failure(id, &e);
                Err(format!("{e}. The mod was disabled and any partial loads were rolled back where possible."))
            }
        }
    }

    pub fn disable(&self, id: &str) -> Result<String, String> {
        let folder = self.package_dir(id)?;
        let manifest = load_manifest(&folder)?;
        {
            let mut state = self.state.lock().map_err(|_| "Native mod state is unavailable.")?;
            let was_enabled = state.enabled.remove(id);
            if let Err(e) = self.persist_state(&state) {
                if was_enabled {
                    state.enabled.insert(id.to_string());
                }
                return Err(e);
            }
        }

        let dll = folder.join(&manifest.module);
        match unload_from_targets(&manifest.targets, &dll) {
            Ok(count) => {
                let status = format!("Disabled; unloaded from {count} process(es)");
                self.set_status(id, status.clone());
                Ok(status)
            }
            Err(e) => {
                let status = format!("Disabled; unload warning: {e}");
                self.set_status(id, status.clone());
                Err(status)
            }
        }
    }

    pub fn uninstall(&self, id: &str) -> Result<(), String> {
        let folder = self.package_dir(id)?;
        if self.enabled_snapshot().contains(id) {
            self.disable(id)?;
        }
        if folder.exists() {
            fs::remove_dir_all(&folder).map_err(|e| format!("remove native mod package: {e}"))?;
        }
        if let Ok(mut statuses) = self.statuses.lock() {
            statuses.remove(id);
        }
        Ok(())
    }

    pub fn apply_enabled_mods(&self) {
        let enabled = self.enabled_snapshot();
        for id in enabled {
            match self.apply_one(&id) {
                Ok(status) => self.set_status(&id, status),
                Err(e) => self.disable_after_load_failure(&id, &e),
            }
        }
    }

    fn disable_after_load_failure(&self, id: &str, reason: &str) {
        let mut persistence_warning = None;
        if let Ok(mut state) = self.state.lock() {
            state.enabled.remove(id);
            if let Err(e) = self.persist_state(&state) {
                persistence_warning = Some(format!("could not persist disabled state: {e}"));
            }
        } else {
            persistence_warning = Some("native mod state lock is unavailable".into());
        }

        let cleanup_warning = self.package_dir(id)
            .and_then(|folder| load_manifest(&folder).map(|manifest| (folder, manifest)))
            .and_then(|(folder, manifest)| unload_from_targets(&manifest.targets, &folder.join(&manifest.module)).map(|_| ()))
            .err();

        let mut status = format!("Disabled after load failure: {reason}");
        if let Some(warning) = cleanup_warning {
            status.push_str(&format!("; rollback warning: {warning}"));
        }
        if let Some(warning) = persistence_warning {
            status.push_str(&format!("; {warning}"));
        }
        self.set_status(id, status);
    }

    fn apply_one(&self, id: &str) -> Result<String, String> {
        let folder = self.package_dir(id)?;
        let manifest = load_manifest(&folder)?;
        validate_pe_file(&folder.join(&manifest.module))?;
        let targets = discover_targets()?;
        let matching: Vec<NativeTarget> = targets.into_iter()
            .filter(|p| manifest.targets.iter().any(|t| t.eq_ignore_ascii_case(&p.executable)))
            .collect();

        if matching.is_empty() {
            return Ok("Enabled; waiting for a supported target process".into());
        }

        let dll = folder.join(&manifest.module);
        let mut successes = 0usize;
        let mut failures = Vec::new();
        for target in matching {
            if !target.eligible {
                failures.push(format!("{} (PID {}): {}", target.executable, target.pid, target.detail));
                continue;
            }
            match inject_into_process(target.pid, &target.executable, &dll) {
                Ok(()) => successes += 1,
                Err(e) => failures.push(format!("{} (PID {}): {e}", target.executable, target.pid)),
            }
        }
        if !failures.is_empty() {
            return Err(format!("loaded into {successes} process(es); {}", failures.join("; ")));
        }
        Ok(format!("Enabled in {successes} process(es); new shell processes are monitored"))
    }
}

fn load_persisted_state(path: &Path) -> PersistedState {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return PersistedState::default(),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_ENABLED_STATE_BYTES
    {
        return PersistedState::default();
    }
    let state = fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<PersistedState>(&text).ok())
        .unwrap_or_default();
    PersistedState {
        enabled: state.enabled.into_iter().filter(|id| valid_id(id)).collect(),
    }
}

fn checked_enabled_state_path(root: &Path) -> Result<PathBuf, String> {
    let root = root.canonicalize()
        .map_err(|e| format!("Cannot resolve native mod data directory: {e}"))?;
    if !root.is_dir() {
        return Err("Native mod data path must be a directory.".into());
    }
    let path = root.join("enabled.json");
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err("Native mod enabled-state file must not be a symlink.".into());
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err("Native mod enabled-state path must be a regular file.".into());
        }
        Ok(metadata) if metadata.len() > MAX_ENABLED_STATE_BYTES => {
            return Err("Native mod enabled-state file exceeds the 1 MB limit.".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Cannot inspect native mod enabled-state file: {error}")),
    }
    Ok(path)
}

fn atomic_write_enabled_state(root: &Path, bytes: &[u8]) -> Result<(), String> {
    let target = checked_enabled_state_path(root)?;
    let parent = target.parent()
        .ok_or_else(|| "Native mod data directory has no parent path.".to_string())?;
    let mut temporary = None;
    for _ in 0..32 {
        let sequence = STATE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".enabled.json.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
                    drop(file);
                    let _ = fs::remove_file(&candidate);
                    return Err(format!("write native mod state temporary file: {error}"));
                }
                drop(file);
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create native mod state temporary file: {error}")),
        }
    }
    let temporary = temporary.ok_or_else(|| {
        "Cannot allocate a unique temporary file for native mod state.".to_string()
    })?;

    // Re-check the destination immediately before replacement. Rename replaces
    // the directory entry rather than following a destination symlink.
    if let Err(error) = checked_enabled_state_path(root) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = fs::rename(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return Err(format!("atomically save native mod state: {error}"));
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && !id.starts_with('.') && !id.ends_with('.')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '-' | '_'))
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty() && !value.starts_with('/') && !value.contains('\\') && !value.contains(':')
        && value.split('/').all(|part| !part.is_empty() && part != "." && part != "..")
}

fn validate_windows_path_hierarchy(paths: &HashMap<String, bool>) -> Result<(), String> {
    for (path, is_dir) in paths {
        let components: Vec<&str> = path.split('/').collect();
        for index in 1..components.len() {
            let parent = components[..index].join("/");
            if paths.get(&parent).is_some_and(|parent_is_dir| !*parent_is_dir) {
                return Err(format!("ZIP file/directory path conflict at '{parent}'"));
            }
        }
        if !is_dir {
            let prefix = format!("{path}/");
            if paths.keys().any(|candidate| candidate.starts_with(&prefix)) {
                return Err(format!("ZIP file/directory path conflict at '{path}'"));
            }
        }
    }
    Ok(())
}

fn register_windows_zip_path(seen: &mut HashSet<String>, path: &str) -> Result<(), String> {
    let normalized = path.trim_end_matches('/');
    if !normalized.is_empty() && !valid_relative_path(normalized) {
        return Err(format!("unsafe ZIP path: {path}"));
    }
    let key = normalized.to_lowercase();
    if !seen.insert(key) {
        return Err(format!("ZIP entries collide on Windows (case-insensitive path): {path}"));
    }
    Ok(())
}

fn validate_manifest(m: &ModManifest, files: Option<&HashSet<String>>) -> Result<(), String> {
    if m.manifest_version != 1 { return Err("manifestVersion must be 1".into()); }
    if !valid_id(&m.id) { return Err("mod id must use lowercase a-z, digits, dots, dashes or underscores".into()); }
    if m.name.trim().is_empty() || m.name.chars().count() > 100 {
        return Err("mod name is required (max 100 characters)".into());
    }
    if m.version.trim().is_empty() || m.version.len() > 40 {
        return Err("mod version is required (max 40 characters)".into());
    }
    if m.description.chars().count() > 1000 { return Err("mod description is too long".into()); }
    if m.architecture != "x64" { return Err("only x64 native mods are supported in this milestone".into()); }
    if !valid_relative_path(&m.module) || !m.module.to_ascii_lowercase().ends_with(".dll") {
        return Err("module must be a safe relative path to a .dll file".into());
    }
    if files.map(|set| !set.contains(&m.module)).unwrap_or(false) {
        return Err(format!("native module '{}' is missing from the ZIP", m.module));
    }
    if m.targets.is_empty() || m.targets.len() > SUPPORTED_TARGETS.len() {
        return Err("mod must declare between 1 and 4 supported target processes".into());
    }
    let mut unique = HashSet::new();
    for target in &m.targets {
        if !SUPPORTED_TARGETS.iter().any(|supported| supported.eq_ignore_ascii_case(target)) {
            return Err(format!("target '{target}' is not supported; allowed targets: {}", SUPPORTED_TARGETS.join(", ")));
        }
        if !unique.insert(target.to_ascii_lowercase()) {
            return Err(format!("duplicate target '{target}'"));
        }
    }
    Ok(())
}

fn read_package(path: &Path) -> Result<ReadPackage, String> {
    if path.extension().and_then(|x| x.to_str()).map(|x| !x.eq_ignore_ascii_case("zip")).unwrap_or(true) {
        return Err("native mod packages must be .zip archives with a root manifest.json".into());
    }
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_ZIP_BYTES { return Err("native mod ZIP exceeds the 20 MB compressed-size limit".into()); }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("invalid native mod ZIP: {e}"))?;
    if archive.len() == 0 || archive.len() > MAX_FILES {
        return Err(format!("native mod ZIP must contain between 1 and {MAX_FILES} entries"));
    }

    let mut files = HashSet::new();
    let mut windows_path_keys = HashSet::new();
    let mut windows_path_types = HashMap::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        if entry.enclosed_name().is_none() || name.contains('\\') || name.contains(':') {
            return Err(format!("unsafe ZIP path: {name}"));
        }
        if !files.insert(name.clone()) { return Err(format!("duplicate ZIP entry: {name}")); }
        // Windows normally resolves package paths without regard to case. Reject
        // collisions such as "Module.dll" vs "module.dll" and "assets" vs "assets/".
        register_windows_zip_path(&mut windows_path_keys, &name)?;
        let normalized = name.trim_end_matches('/');
        if !normalized.is_empty() && !valid_relative_path(normalized) {
            return Err(format!("unsafe ZIP path: {name}"));
        }
        windows_path_types.insert(normalized.to_lowercase(), entry.is_dir());
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 { return Err(format!("symbolic links are not allowed: {name}")); }
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES { return Err("native mod ZIP expands beyond the 50 MB unpacked-size limit".into()); }
        if name.eq_ignore_ascii_case("manifest.json") && entry.size() > MAX_MANIFEST_BYTES {
            return Err("manifest.json exceeds the 1 MB limit".into());
        }
        // Drain every entry so preview verifies decompression and CRC, including
        // payloads that are not the manifest or DLL. The total size is bounded above.
        std::io::copy(&mut entry, &mut std::io::sink())
            .map_err(|e| format!("ZIP entry integrity check failed for '{name}': {e}"))?;
    }
    validate_windows_path_hierarchy(&windows_path_types)?;
    if !files.contains("manifest.json") { return Err("native mod ZIP must have manifest.json at its root".into()); }

    let mut text = String::new();
    archive.by_name("manifest.json").map_err(|e| e.to_string())?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text).map_err(|e| format!("manifest.json is not valid UTF-8: {e}"))?;
    if text.len() as u64 > MAX_MANIFEST_BYTES { return Err("manifest.json exceeds the 1 MB limit".into()); }
    let manifest: ModManifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("invalid native mod manifest: {e}"))?;
    validate_manifest(&manifest, Some(&files))?;

    let mut dll_bytes = Vec::new();
    archive.by_name(&manifest.module).map_err(|e| format!("cannot read {}: {e}", manifest.module))?
        .read_to_end(&mut dll_bytes).map_err(|e| format!("cannot extract {} from ZIP: {e}", manifest.module))?;
    validate_pe_bytes(&dll_bytes)?;
    Ok(ReadPackage { manifest })
}

fn load_manifest(folder: &Path) -> Result<ModManifest, String> {
    let path = folder.join("manifest.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read manifest.json: {e}"))?;
    if text.len() as u64 > MAX_MANIFEST_BYTES { return Err("manifest.json exceeds the 1 MB limit".into()); }
    let manifest: ModManifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("invalid native mod manifest: {e}"))?;
    validate_manifest(&manifest, None)?;
    Ok(manifest)
}

fn extract_package(path: &Path, destination: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES { return Err("native mod ZIP expands beyond the 50 MB unpacked-size limit".into()); }
        let relative = entry.enclosed_name().ok_or_else(|| format!("unsafe ZIP path: {}", entry.name()))?.to_path_buf();
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 { return Err(format!("symbolic links are not allowed: {}", entry.name())); }
        }
        let out = destination.join(&relative);
        if entry.is_dir() { fs::create_dir_all(&out).map_err(|e| e.to_string())?; continue; }
        if let Some(parent) = out.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
        let declared_size = entry.size();
        let mut target = File::create(&out).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry, &mut target).map_err(|e| format!("extract failed: {e}"))?;
        if copied != declared_size { return Err(format!("ZIP entry size mismatch: {}", relative.display())); }
    }
    Ok(())
}

fn validate_pe_file(path: &Path) -> Result<(), String> {
    let data = fs::read(path).map_err(|e| format!("cannot read native DLL {}: {e}", path.display()))?;
    validate_pe_bytes(&data)
}

fn validate_pe_bytes(data: &[u8]) -> Result<(), String> {
    fn u16_at(b: &[u8], o: usize) -> Option<u16> { Some(u16::from_le_bytes([*b.get(o)?, *b.get(o + 1)?])) }
    fn u32_at(b: &[u8], o: usize) -> Option<u32> { Some(u32::from_le_bytes([*b.get(o)?, *b.get(o + 1)?, *b.get(o + 2)?, *b.get(o + 3)?])) }
    #[derive(Clone, Copy)]
    struct Section { virtual_size: u32, virtual_address: u32, raw_size: u32, raw_ptr: u32 }
    fn rva_offset(rva: u32, sections: &[Section], headers_size: u32) -> Option<usize> {
        if rva < headers_size { return Some(rva as usize); }
        sections.iter().find_map(|s| {
            let span = s.virtual_size.max(s.raw_size);
            let end = s.virtual_address.checked_add(span)?;
            if rva < s.virtual_address || rva >= end {
                return None;
            }
            s.raw_ptr.checked_add(rva - s.virtual_address).map(|offset| offset as usize)
        })
    }
    fn c_string(data: &[u8], offset: usize) -> Option<&str> {
        let tail = data.get(offset..)?;
        let end = tail.iter().position(|b| *b == 0)?;
        std::str::from_utf8(&tail[..end]).ok()
    }

    if u16_at(data, 0) != Some(0x5a4d) { return Err("native module is not a PE DLL (missing MZ header)".into()); }
    let pe = u32_at(data, 0x3c).ok_or("native DLL has a truncated PE header")? as usize;
    if data.get(pe..pe + 4) != Some(b"PE\0\0") { return Err("native DLL has an invalid PE signature".into()); }
    let coff = pe + 4;
    if u16_at(data, coff) != Some(0x8664) {
        return Err("native DLL must be x64 (IMAGE_FILE_MACHINE_AMD64)".into());
    }
    let section_count = u16_at(data, coff + 2).ok_or("native DLL has a truncated section table")? as usize;
    let optional_size = u16_at(data, coff + 16).ok_or("native DLL has a truncated optional header")? as usize;
    let optional = coff + 20;
    if u16_at(data, optional) != Some(0x20b) { return Err("native DLL must use the PE32+ x64 format".into()); }
    let headers_size = u32_at(data, optional + 60).ok_or("native DLL has a truncated optional header")?;
    let export_rva = u32_at(data, optional + 112).ok_or("native DLL has no export directory")?;
    if export_rva == 0 { return Err("native DLL has no export table".into()); }

    let section_table = optional.checked_add(optional_size).ok_or("invalid PE section offset")?;
    let mut sections = Vec::new();
    if section_count > 96 { return Err("native DLL has an unreasonable PE section count".into()); }
    for i in 0..section_count {
        let off = section_table + i * 40;
        sections.push(Section {
            virtual_size: u32_at(data, off + 8).ok_or("native DLL section table is truncated")?,
            virtual_address: u32_at(data, off + 12).ok_or("native DLL section table is truncated")?,
            raw_size: u32_at(data, off + 16).ok_or("native DLL section table is truncated")?,
            raw_ptr: u32_at(data, off + 20).ok_or("native DLL section table is truncated")?,
        });
    }
    let export_off = rva_offset(export_rva, &sections, headers_size).ok_or("cannot map native DLL export directory")?;
    let export_size = u32_at(data, optional + 116).ok_or("native DLL export directory is truncated")?;
    let export_end = export_rva.checked_add(export_size).ok_or("native DLL export range overflows")?;
    let function_count = u32_at(data, export_off + 20).ok_or("native DLL export directory is truncated")? as usize;
    let name_count = u32_at(data, export_off + 24).ok_or("native DLL export directory is truncated")? as usize;
    let functions_rva = u32_at(data, export_off + 28).ok_or("native DLL export directory is truncated")?;
    let names_rva = u32_at(data, export_off + 32).ok_or("native DLL export directory is truncated")?;
    let ordinals_rva = u32_at(data, export_off + 36).ok_or("native DLL export directory is truncated")?;
    let functions_off = rva_offset(functions_rva, &sections, headers_size).ok_or("cannot map native DLL export functions")?;
    let names_off = rva_offset(names_rva, &sections, headers_size).ok_or("cannot map native DLL export names")?;
    let ordinals_off = rva_offset(ordinals_rva, &sections, headers_size).ok_or("cannot map native DLL export ordinals")?;
    if name_count > 100_000 || function_count > 100_000 {
        return Err("native DLL export count is unreasonable".into());
    }

    let mut exports = HashMap::new();
    for i in 0..name_count {
        let name_rva = u32_at(data, names_off + i * 4).ok_or("native DLL export name array is truncated")?;
        let name_off = rva_offset(name_rva, &sections, headers_size).ok_or("cannot map native DLL export name")?;
        if let Some(name) = c_string(data, name_off) {
            let ordinal = u16_at(data, ordinals_off + i * 2).ok_or("native DLL export ordinal array is truncated")? as usize;
            if ordinal >= function_count {
                return Err("native DLL export ordinal is out of range".into());
            }
            let function_rva = u32_at(data, functions_off + ordinal * 4).ok_or("native DLL export function array is truncated")?;
            if function_rva == 0 || (function_rva >= export_rva && function_rva < export_end) {
                return Err(format!("native DLL export '{name}' is a forwarder or has no code RVA"));
            }
            if rva_offset(function_rva, &sections, headers_size).is_none() {
                return Err(format!("native DLL export '{name}' points outside the image"));
            }
            exports.insert(name.to_string(), function_rva);
        }
    }
    for export in REQUIRED_EXPORTS {
        if !exports.contains_key(export) {
            return Err(format!("native DLL is missing required SDK export '{export}'"));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn discover_targets() -> Result<Vec<NativeTarget>, String> {
    win::discover_targets()
}
#[cfg(not(windows))]
fn discover_targets() -> Result<Vec<NativeTarget>, String> {
    Ok(Vec::new())
}

#[cfg(windows)]
fn inject_into_process(pid: u32, executable: &str, module: &Path) -> Result<(), String> {
    win::inject(pid, executable, module)
}
#[cfg(not(windows))]
fn inject_into_process(_pid: u32, _executable: &str, _module: &Path) -> Result<(), String> {
    Err("native shell mods can only be loaded on Windows".into())
}

fn unload_from_targets(targets: &[String], module: &Path) -> Result<usize, String> {
    #[cfg(windows)]
    {
        let running = win::discover_targets()?;
        let mut count = 0;
        let mut errors = Vec::new();
        for p in running.into_iter().filter(|p| targets.iter().any(|t| t.eq_ignore_ascii_case(&p.executable))) {
            match win::unload(p.pid, &p.executable, module) {
                Ok(true) => count += 1,
                Ok(false) => {}
                Err(e) => errors.push(format!("{} (PID {}): {e}", p.executable, p.pid)),
            }
        }
        if errors.is_empty() { Ok(count) } else { Err(errors.join("; ")) }
    }
    #[cfg(not(windows))]
    {
        let _ = (targets, module);
        Err("native shell mods can only be unloaded on Windows".into())
    }
}

#[cfg(windows)]
mod win {
    use super::{validate_pe_file, NativeTarget, SUPPORTED_TARGETS};
    use std::{
        ffi::{c_char, c_void, OsStr, OsString},
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::Path,
        ptr,
    };

    type Handle = *mut c_void;

    const INVALID_HANDLE_VALUE: isize = -1;
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const TH32CS_SNAPMODULE: u32 = 0x0000_0008;
    const TH32CS_SNAPMODULE32: u32 = 0x0000_0010;
    const PROCESS_CREATE_THREAD: u32 = 0x0002;
    const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_VM_OPERATION: u32 = 0x0008;
    const PROCESS_VM_READ: u32 = 0x0010;
    const PROCESS_VM_WRITE: u32 = 0x0020;
    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_READWRITE: u32 = 0x04;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 0x102;
    const INFINITE: u32 = 0xffff_ffff;
    const IMAGE_FILE_MACHINE_I386: u16 = 0x014c;
    const IMAGE_FILE_MACHINE_AMD64: u16 = 0x8664;
    const IMAGE_FILE_MACHINE_UNKNOWN: u16 = 0;
    const DONT_RESOLVE_DLL_REFERENCES: u32 = 0x0000_0001;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct ModuleEntry32W {
        dw_size: u32,
        th32_module_id: u32,
        th32_process_id: u32,
        glbl_cnt_usage: u32,
        proc_cnt_usage: u32,
        mod_base_addr: *mut u8,
        mod_base_size: u32,
        h_module: Handle,
        sz_module: [u16; 256],
        sz_exe_path: [u16; 260],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> Handle;
        fn CloseHandle(handle: Handle) -> i32;
        fn GetLastError() -> u32;
        fn GetCurrentProcessId() -> u32;
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Process32FirstW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snapshot: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Module32FirstW(snapshot: Handle, entry: *mut ModuleEntry32W) -> i32;
        fn Module32NextW(snapshot: Handle, entry: *mut ModuleEntry32W) -> i32;
        fn IsWow64Process2(process: Handle, process_machine: *mut u16, native_machine: *mut u16) -> i32;
        fn VirtualAllocEx(process: Handle, address: *mut c_void, size: usize, alloc_type: u32, protect: u32) -> *mut c_void;
        fn VirtualFreeEx(process: Handle, address: *mut c_void, size: usize, free_type: u32) -> i32;
        fn WriteProcessMemory(process: Handle, address: *mut c_void, buffer: *const c_void, size: usize, written: *mut usize) -> i32;
        fn CreateRemoteThread(process: Handle, attributes: *mut c_void, stack_size: usize, start: *mut c_void, parameter: *mut c_void, flags: u32, thread_id: *mut u32) -> Handle;
        fn WaitForSingleObject(handle: Handle, milliseconds: u32) -> u32;
        fn GetExitCodeThread(handle: Handle, exit_code: *mut u32) -> i32;
        fn GetModuleHandleW(name: *const u16) -> Handle;
        fn GetProcAddress(module: Handle, name: *const c_char) -> *mut c_void;
        fn LoadLibraryExW(path: *const u16, file: Handle, flags: u32) -> Handle;
        fn FreeLibrary(module: Handle) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s).encode_wide().chain(Some(0)).collect()
    }
    fn from_wide(s: &[u16]) -> String {
        let end = s.iter().position(|c| *c == 0).unwrap_or(s.len());
        OsString::from_wide(&s[..end]).to_string_lossy().to_string()
    }
    fn last_error(operation: &str) -> String {
        format!("{operation} failed (Win32 error {})", unsafe { GetLastError() })
    }
    fn is_invalid(h: Handle) -> bool {
        h.is_null() || h as isize == INVALID_HANDLE_VALUE
    }

    fn processes() -> Result<Vec<(u32, String)>, String> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if is_invalid(snap) { return Err(last_error("CreateToolhelp32Snapshot(processes)")); }
            let mut entry: ProcessEntry32W = std::mem::zeroed();
            entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as u32;
            let mut out = Vec::new();
            if Process32FirstW(snap, &mut entry) != 0 {
                loop {
                    let name = from_wide(&entry.sz_exe_file);
                    out.push((entry.th32_process_id, name));
                    if Process32NextW(snap, &mut entry) == 0 { break; }
                }
            }
            CloseHandle(snap);
            Ok(out)
        }
    }

    fn process_architecture(pid: u32) -> Result<String, String> {
        unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if is_invalid(process) { return Err(last_error("OpenProcess(query architecture)")); }
            let mut process_machine = IMAGE_FILE_MACHINE_UNKNOWN;
            let mut native_machine = IMAGE_FILE_MACHINE_UNKNOWN;
            let ok = IsWow64Process2(process, &mut process_machine, &mut native_machine);
            CloseHandle(process);
            if ok == 0 { return Err(last_error("IsWow64Process2")); }
            Ok(match process_machine {
                IMAGE_FILE_MACHINE_I386 => "x86".into(),
                IMAGE_FILE_MACHINE_AMD64 => "x64".into(),
                IMAGE_FILE_MACHINE_UNKNOWN if native_machine == IMAGE_FILE_MACHINE_AMD64 => "x64".into(),
                IMAGE_FILE_MACHINE_UNKNOWN => "native/unsupported".into(),
                _ => "unknown".into(),
            })
        }
    }

    pub fn discover_targets() -> Result<Vec<NativeTarget>, String> {
        let mut out = Vec::new();
        for (pid, name) in processes()? {
            if !SUPPORTED_TARGETS.iter().any(|t| t.eq_ignore_ascii_case(&name)) { continue; }
            match process_architecture(pid) {
                Ok(architecture) => {
                    let eligible = architecture == "x64";
                    out.push(NativeTarget {
                        pid,
                        executable: name,
                        architecture: architecture.clone(),
                        eligible,
                        detail: if eligible { "Supported shell process".into() } else { "Native mods in this build require x64 targets".into() },
                    });
                }
                Err(e) => out.push(NativeTarget {
                    pid, executable: name, architecture: "unknown".into(), eligible: false, detail: e,
                }),
            }
        }
        out.sort_by(|a, b| a.executable.to_ascii_lowercase().cmp(&b.executable.to_ascii_lowercase()).then(a.pid.cmp(&b.pid)));
        Ok(out)
    }

    fn module_snapshot(pid: u32) -> Result<Vec<ModuleEntry32W>, String> {
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
            if is_invalid(snap) { return Err(last_error("CreateToolhelp32Snapshot(modules)")); }
            let mut entry: ModuleEntry32W = std::mem::zeroed();
            entry.dw_size = std::mem::size_of::<ModuleEntry32W>() as u32;
            let mut out = Vec::new();
            if Module32FirstW(snap, &mut entry) != 0 {
                loop {
                    out.push(entry);
                    if Module32NextW(snap, &mut entry) == 0 { break; }
                    entry.dw_size = std::mem::size_of::<ModuleEntry32W>() as u32;
                }
            }
            CloseHandle(snap);
            Ok(out)
        }
    }

    fn normalized_path(path: &str) -> String {
        let mut p = path.replace('/', "\\").to_ascii_lowercase();
        if p.starts_with("\\\\?\\") { p = p[4..].to_string(); }
        p
    }

    fn module_for_path(pid: u32, path: &Path) -> Result<Option<ModuleEntry32W>, String> {
        let target = path.canonicalize().map_err(|e| format!("canonicalize {}: {e}", path.display()))?;
        let target = normalized_path(&target.to_string_lossy());
        for module in module_snapshot(pid)? {
            if normalized_path(&from_wide(&module.sz_exe_path)) == target {
                return Ok(Some(module));
            }
        }
        Ok(None)
    }

    fn module_for_name(pid: u32, name: &str) -> Result<Option<ModuleEntry32W>, String> {
        for module in module_snapshot(pid)? {
            if from_wide(&module.sz_module).eq_ignore_ascii_case(name) {
                return Ok(Some(module));
            }
        }
        Ok(None)
    }

    fn remote_system_proc(pid: u32, export_name: &'static [u8]) -> Result<*mut c_void, String> {
        unsafe {
            let kernel = wide("kernel32.dll");
            let local_kernel = GetModuleHandleW(kernel.as_ptr());
            if is_invalid(local_kernel) { return Err(last_error("GetModuleHandleW(kernel32)")); }
            let local_proc = GetProcAddress(local_kernel, export_name.as_ptr().cast());
            if local_proc.is_null() { return Err(last_error("GetProcAddress(kernel32 export)")); }

            // Kernel32 exports may be forwarded to KernelBase. Resolve the module that
            // actually contains the function locally, then use the same module RVA remotely.
            let local_modules = module_snapshot(GetCurrentProcessId())?;
            let address = local_proc as usize;
            let containing = local_modules.iter().find(|m| {
                let start = m.mod_base_addr as usize;
                address >= start && address < start.saturating_add(m.mod_base_size as usize)
            }).ok_or("could not locate local module for a Windows API function")?;
            let module_name = from_wide(&containing.sz_module);
            let remote = module_for_name(pid, &module_name)?
                .ok_or_else(|| format!("{module_name} is not loaded in target process"))?;
            let offset = address - containing.mod_base_addr as usize;
            Ok((remote.mod_base_addr as usize + offset) as *mut c_void)
        }
    }

    fn open_modifiable_process(pid: u32) -> Result<Handle, String> {
        unsafe {
            let access = PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ;
            let h = OpenProcess(access, 0, pid);
            if is_invalid(h) { Err(last_error("OpenProcess(modification)")) } else { Ok(h) }
        }
    }

    fn remote_call(process: Handle, start: *mut c_void, parameter: *mut c_void) -> Result<u32, String> {
        unsafe {
            let thread = CreateRemoteThread(process, ptr::null_mut(), 0, start, parameter, 0, ptr::null_mut());
            if is_invalid(thread) { return Err(last_error("CreateRemoteThread")); }
            let wait = WaitForSingleObject(thread, 10_000);
            if wait == WAIT_TIMEOUT {
                CloseHandle(thread);
                return Err("remote thread timed out; the target process was not terminated".into());
            }
            if wait != WAIT_OBJECT_0 {
                CloseHandle(thread);
                return Err(last_error("WaitForSingleObject(remote thread)"));
            }
            let mut code = 0;
            let ok = GetExitCodeThread(thread, &mut code);
            CloseHandle(thread);
            if ok == 0 { return Err(last_error("GetExitCodeThread")); }
            Ok(code)
        }
    }

    fn write_remote_utf16(process: Handle, text: &str) -> Result<*mut c_void, String> {
        unsafe {
            let value: Vec<u16> = OsStr::new(text).encode_wide().chain(Some(0)).collect();
            let bytes = value.len() * std::mem::size_of::<u16>();
            let remote = VirtualAllocEx(process, ptr::null_mut(), bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
            if remote.is_null() { return Err(last_error("VirtualAllocEx(path)")); }
            let mut written = 0usize;
            if WriteProcessMemory(process, remote, value.as_ptr().cast(), bytes, &mut written) == 0 || written != bytes {
                VirtualFreeEx(process, remote, 0, MEM_RELEASE);
                return Err(last_error("WriteProcessMemory(path)"));
            }
            Ok(remote)
        }
    }

    unsafe fn local_map_module(path: &Path) -> Result<Handle, String> {
        let full = path.canonicalize().map_err(|e| format!("canonicalize {}: {e}", path.display()))?;
        let name: Vec<u16> = full.as_os_str().encode_wide().chain(Some(0)).collect();
        let module = LoadLibraryExW(name.as_ptr(), ptr::null_mut(), DONT_RESOLVE_DLL_REFERENCES);
        if is_invalid(module) { return Err(last_error("LoadLibraryExW(DONT_RESOLVE_DLL_REFERENCES)")); }
        Ok(module)
    }

    unsafe fn exported_rva(module: Handle, name: &'static [u8]) -> Result<usize, String> {
        let proc = GetProcAddress(module, name.as_ptr().cast());
        if proc.is_null() { return Err(format!("mod is missing export {}", String::from_utf8_lossy(&name[..name.len()-1]))); }
        let offset = (proc as usize).checked_sub(module as usize).ok_or("native DLL export address is invalid")?;
        Ok(offset)
    }

    unsafe fn free_local_map(module: Handle) {
        if !module.is_null() { FreeLibrary(module); }
    }

    fn verify_target_pid(pid: u32, requested_name: &str) -> Result<(), String> {
        if !SUPPORTED_TARGETS.iter().any(|t| t.eq_ignore_ascii_case(requested_name)) {
            return Err("target executable is not on the native shell allow-list".into());
        }
        let actual = processes()?.into_iter().find(|(found_pid, _)| *found_pid == pid)
            .map(|(_, name)| name)
            .ok_or_else(|| format!("target process {pid} has exited"))?;
        if !actual.eq_ignore_ascii_case(requested_name)
            || !SUPPORTED_TARGETS.iter().any(|t| t.eq_ignore_ascii_case(&actual))
        {
            return Err(format!("PID {pid} is no longer the expected shell process"));
        }
        Ok(())
    }

    pub fn inject(pid: u32, executable: &str, module_path: &Path) -> Result<(), String> {
        verify_target_pid(pid, executable)?;
        if process_architecture(pid)? != "x64" {
            return Err("target process is not x64; only x64 native mods are supported".into());
        }
        validate_pe_file(module_path)?;

        if module_for_path(pid, module_path)?.is_some() {
            return Ok(());
        }

        unsafe {
            let local_module = local_map_module(module_path)?;
            let api_rva = match exported_rva(local_module, b"WmodGetApiVersion\0") {
                Ok(v) => v, Err(e) => { free_local_map(local_module); return Err(e); }
            };
            let init_rva = match exported_rva(local_module, b"WmodInitializeV1\0") {
                Ok(v) => v, Err(e) => { free_local_map(local_module); return Err(e); }
            };
            let shutdown_rva = match exported_rva(local_module, b"WmodShutdownV1\0") {
                Ok(v) => v, Err(e) => { free_local_map(local_module); return Err(e); }
            };

            let process = match open_modifiable_process(pid) {
                Ok(h) => h, Err(e) => { free_local_map(local_module); return Err(e); }
            };
            let remote_path = match write_remote_utf16(process, &module_path.canonicalize().map_err(|e| e.to_string())?.to_string_lossy()) {
                Ok(p) => p,
                Err(e) => { CloseHandle(process); free_local_map(local_module); return Err(e); }
            };
            let load_library = match remote_system_proc(pid, b"LoadLibraryW\0") {
                Ok(p) => p,
                Err(e) => { VirtualFreeEx(process, remote_path, 0, MEM_RELEASE); CloseHandle(process); free_local_map(local_module); return Err(e); }
            };
            let loaded = remote_call(process, load_library, remote_path);
            if loaded.is_ok() { VirtualFreeEx(process, remote_path, 0, MEM_RELEASE); }
            let result = match loaded {
                Ok(code) if code != 0 => Ok(()),
                Ok(_) => Err("LoadLibraryW returned NULL in the target process".into()),
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                CloseHandle(process); free_local_map(local_module); return Err(e);
            }

            let remote_module = match module_for_path(pid, module_path)? {
                Some(m) => m.mod_base_addr as usize,
                None => { CloseHandle(process); free_local_map(local_module); return Err("DLL loaded but was not found in the target module list".into()); }
            };
            let api_version = remote_call(process, (remote_module + api_rva) as *mut c_void, ptr::null_mut());
            if api_version != Ok(1) {
                let _ = remote_call(process, (remote_module + shutdown_rva) as *mut c_void, ptr::null_mut());
                if let Ok(free_library) = remote_system_proc(pid, b"FreeLibrary\0") {
                    let _ = remote_call(process, free_library, remote_module as *mut c_void);
                }
                CloseHandle(process); free_local_map(local_module);
                return Err(format!("mod ABI mismatch: expected version 1, got {}", api_version.map(|v| v.to_string()).unwrap_or_else(|e| e)));
            }

            let init = remote_call(process, (remote_module + init_rva) as *mut c_void, ptr::null_mut());
            if !matches!(init, Ok(code) if code != 0) {
                let _ = remote_call(process, (remote_module + shutdown_rva) as *mut c_void, ptr::null_mut());
                if let Ok(free_library) = remote_system_proc(pid, b"FreeLibrary\0") {
                    let _ = remote_call(process, free_library, remote_module as *mut c_void);
                }
                CloseHandle(process); free_local_map(local_module);
                return Err(format!("WmodInitializeV1 failed: {}", init.map(|v| v.to_string()).unwrap_or_else(|e| e)));
            }

            CloseHandle(process);
            free_local_map(local_module);
            Ok(())
        }
    }

    pub fn unload(pid: u32, executable: &str, module_path: &Path) -> Result<bool, String> {
        verify_target_pid(pid, executable)?;
        let Some(module) = module_for_path(pid, module_path)? else { return Ok(false) };
        unsafe {
            let local_module = local_map_module(module_path)?;
            let shutdown_rva = exported_rva(local_module, b"WmodShutdownV1\0")?;
            let base = module.mod_base_addr as usize;
            let process = open_modifiable_process(pid)?;
            let shutdown_result = remote_call(process, (base + shutdown_rva) as *mut c_void, ptr::null_mut());
            if let Err(e) = shutdown_result {
                CloseHandle(process); free_local_map(local_module);
                return Err(format!("WmodShutdownV1 failed: {e}"));
            }
            let free_library = match remote_system_proc(pid, b"FreeLibrary\0") {
                Ok(p) => p,
                Err(e) => { CloseHandle(process); free_local_map(local_module); return Err(e); }
            };
            let free_result = remote_call(process, free_library, base as *mut c_void);
            CloseHandle(process);
            free_local_map(local_module);
            match free_result {
                Ok(code) if code != 0 => Ok(true),
                Ok(_) => Err("FreeLibrary returned FALSE in the target process".into()),
                Err(e) => Err(format!("could not unload native mod: {e}")),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(targets: Vec<&str>, module: &str) -> ModManifest {
        ModManifest {
            manifest_version: 1,
            id: "com.example.taskbar-mod".into(),
            name: "Taskbar Mod".into(),
            version: "1.0.0".into(),
            description: String::new(),
            architecture: "x64".into(),
            module: module.into(),
            targets: targets.into_iter().map(str::to_string).collect(),
        }
    }

    #[test]
    fn native_mod_targets_are_restricted_to_shell_processes() {
        let m = manifest(vec!["explorer.exe", "ShellExperienceHost.exe"], "bin/mod.dll");
        assert!(validate_manifest(&m, None).is_ok());
        let arbitrary = manifest(vec!["notepad.exe"], "bin/mod.dll");
        assert!(validate_manifest(&arbitrary, None).unwrap_err().contains("not supported"));
        let duplicate = manifest(vec!["explorer.exe", "EXPLORER.EXE"], "bin/mod.dll");
        assert!(validate_manifest(&duplicate, None).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn native_mod_paths_and_architectures_are_validated() {
        assert!(valid_relative_path("bin/mod.dll"));
        assert!(!valid_relative_path("../mod.dll"));
        assert!(!valid_relative_path("C:/temp/mod.dll"));
        assert!(!valid_relative_path("bin\\mod.dll"));

        let mut wrong_arch = manifest(vec!["explorer.exe"], "bin/mod.dll");
        wrong_arch.architecture = "x86".into();
        assert!(validate_manifest(&wrong_arch, None).unwrap_err().contains("x64"));
    }

    #[test]
    fn native_pe_validator_rejects_non_pe_input() {
        assert!(validate_pe_bytes(b"").unwrap_err().contains("MZ"));
        assert!(validate_pe_bytes(b"MZ").unwrap_err().contains("truncated PE header"));
    }

    #[test]
    fn native_mod_zip_paths_that_alias_on_windows_are_rejected() {
        let mut seen = HashSet::new();
        assert!(register_windows_zip_path(&mut seen, "bin/Module.dll").is_ok());
        assert!(register_windows_zip_path(&mut seen, "BIN/module.DLL")
            .unwrap_err().contains("collide on Windows"));

        let mut seen_directories = HashSet::new();
        assert!(register_windows_zip_path(&mut seen_directories, "assets").is_ok());
        assert!(register_windows_zip_path(&mut seen_directories, "assets/")
            .unwrap_err().contains("collide on Windows"));
    }

    #[test]
    fn native_mod_zip_file_directory_conflicts_are_rejected() {
        let mut file_parent = HashMap::new();
        file_parent.insert("assets".to_string(), false);
        file_parent.insert("assets/icon.png".to_string(), false);
        assert!(validate_windows_path_hierarchy(&file_parent)
            .unwrap_err().contains("file/directory path conflict"));

        let mut file_child = HashMap::new();
        file_child.insert("assets/icon.png".to_string(), false);
        file_child.insert("assets".to_string(), false);
        assert!(validate_windows_path_hierarchy(&file_child)
            .unwrap_err().contains("file/directory path conflict"));

        let mut valid_folder = HashMap::new();
        valid_folder.insert("assets".to_string(), true);
        valid_folder.insert("assets/icon.png".to_string(), false);
        assert!(validate_windows_path_hierarchy(&valid_folder).is_ok());
    }

    #[test]
    fn native_mod_preview_rejects_corrupt_zip_entry_crc() {
        use std::io::Write;
        use zip::write::FileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let path = std::env::temp_dir().join(format!(
            "00047-native-mod-crc-test-{}-{}.zip",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let options = FileOptions::default()
            .compression_method(CompressionMethod::Stored);
        {
            let file = File::create(&path).unwrap();
            let mut writer = ZipWriter::new(file);
            writer.start_file("manifest.json", options).unwrap();
            writer.write_all(b"not valid json").unwrap();
            writer.start_file("payload.bin", options).unwrap();
            writer.write_all(b"unique-native-mod-payload-corrupt-me").unwrap();
            writer.finish().unwrap();
        }

        let mut bytes = fs::read(&path).unwrap();
        let payload = b"unique-native-mod-payload-corrupt-me";
        let offset = bytes.windows(payload.len())
            .position(|window| window == payload)
            .expect("stored payload should appear verbatim in the ZIP");
        bytes[offset] ^= 1;
        fs::write(&path, bytes).unwrap();

        let result = read_package(&path);
        let _ = fs::remove_file(&path);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("ZIP entry integrity check failed"));
    }

    fn state_test_root(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "00047-native-mod-state-{label}-{}-{nonce}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn native_mod_enabled_state_round_trips_using_atomic_replacement() {
        let root = state_test_root("roundtrip");
        let manager = NativeModManager::new(root.clone());
        let mut state = PersistedState::default();
        state.enabled.insert("com.example.first".into());
        manager.persist_state(&state).unwrap();

        let path = root.join("enabled.json");
        assert!(load_persisted_state(&path).enabled.contains("com.example.first"));

        state.enabled.clear();
        state.enabled.insert("com.example.second".into());
        manager.persist_state(&state).unwrap();
        let reloaded = load_persisted_state(&path);
        assert_eq!(reloaded.enabled, HashSet::from(["com.example.second".to_string()]));
        assert!(!fs::read_dir(&root).unwrap().filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp")));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mod_enabled_state_load_ignores_invalid_ids_and_oversized_file() {
        let root = state_test_root("invalid-state");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("enabled.json");
        fs::write(
            &path,
            r#"{"enabled":["com.example.valid","../outside","UPPERCASE"]}"#,
        ).unwrap();
        assert_eq!(
            load_persisted_state(&path).enabled,
            HashSet::from(["com.example.valid".to_string()])
        );

        fs::write(&path, vec![b' '; MAX_ENABLED_STATE_BYTES as usize + 1]).unwrap();
        assert!(load_persisted_state(&path).enabled.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_mod_enabled_state_rejects_symlink_and_preserves_target() {
        use std::os::unix::fs::symlink;

        let root = state_test_root("state-symlink");
        fs::create_dir_all(&root).unwrap();
        let outside = root.join("outside.json");
        fs::write(&outside, b"do not overwrite").unwrap();
        symlink(&outside, root.join("enabled.json")).unwrap();

        assert!(load_persisted_state(&root.join("enabled.json")).enabled.is_empty());
        let state = PersistedState::default();
        let result = atomic_write_enabled_state(&root, br#"{"enabled":[]}"#);
        assert!(result.unwrap_err().contains("symlink"));
        assert_eq!(fs::read(&outside).unwrap(), b"do not overwrite");
        let _ = state;

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mod_manifest_requires_the_module_file_in_package() {
        let m = manifest(vec!["explorer.exe"], "bin/mod.dll");
        let files = HashSet::from(["manifest.json".to_string()]);
        assert!(validate_manifest(&m, Some(&files)).unwrap_err().contains("missing from the ZIP"));
    }
}
