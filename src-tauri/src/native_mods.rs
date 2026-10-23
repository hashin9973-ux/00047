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
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};
use zip::ZipArchive;

const MAX_ZIP_BYTES: u64 = 20 * 1024 * 1024;
const MAX_FILES: usize = 500;
const MAX_UNPACKED_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_ENABLED_STATE_BYTES: u64 = 1024 * 1024;
static STATE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const REQUIRED_EXPORTS: [&str; 5] = [
    "WmodGetApiVersion",
    "WmodInitializeV1",
    "WmodShutdownV1",
    "WmodIsInitializedV1",
    "WmodCanUnloadV1",
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
    #[serde(default)]
    require_trusted_signature: bool,
    #[serde(default)]
    pending_signature_unloads: HashSet<String>,
    #[serde(default)]
    pending_safe_unloads: HashSet<String>,
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

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NativeModTargetState {
    Detected,
    Loading,
    Initializing,
    Initialized,
    // Retained as a defensive legacy state for older internal state transitions.
    Running,
    Stopping,
    ReloadPending,
    Ineligible,
    Failed,
    Indeterminate,
    Unloaded,
    NotLoaded,
    RolledBack,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModTargetStatus {
    pub pid: u32,
    pub executable: String,
    pub state: NativeModTargetState,
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
    pub target_statuses: Vec<NativeModTargetStatus>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModList {
    pub dir: String,
    pub mods: Vec<NativeModRecord>,
    pub targets: Vec<NativeTarget>,
    pub errors: Vec<String>,
    pub require_trusted_signature: bool,
    pub pending_signature_unloads: Vec<String>,
    pub pending_safe_unloads: Vec<String>,
}

struct ReadPackage {
    manifest: ModManifest,
}

fn target_has_pending_reload(statuses: &[NativeModTargetStatus], pid: u32) -> bool {
    statuses.iter().any(|status| {
        status.pid == pid
            && (status.state == NativeModTargetState::ReloadPending
                || (status.state == NativeModTargetState::Indeterminate
                    && status.detail.contains("reload pending")))
    })
}

fn should_defer_reload_failure(error: &str, was_reload_pending: bool) -> bool {
    error.starts_with("reload pending:") || was_reload_pending
}

struct NativeUnloadReport {
    unloaded: usize,
    target_statuses: Vec<NativeModTargetStatus>,
    errors: Vec<String>,
}

pub struct NativeModManager {
    root: PathBuf,
    state: Mutex<PersistedState>,
    statuses: Mutex<HashMap<String, String>>,
    target_statuses: Mutex<HashMap<String, Vec<NativeModTargetStatus>>>,
    lifecycle_gate: Mutex<()>,
}

fn merge_rollback_statuses(
    previous: &[NativeModTargetStatus],
    rollback: &[NativeModTargetStatus],
) -> Vec<NativeModTargetStatus> {
    rollback
        .iter()
        .map(|current| {
            let Some(old) = previous.iter().find(|old| old.pid == current.pid) else {
                return current.clone();
            };
            match (&old.state, &current.state) {
                (NativeModTargetState::Failed, NativeModTargetState::Indeterminate)
                | (NativeModTargetState::Indeterminate, NativeModTargetState::Indeterminate) => {
                    current.clone()
                }
                (NativeModTargetState::Failed, _) => old.clone(),
                (
                    NativeModTargetState::Initialized
                    | NativeModTargetState::Running
                    | NativeModTargetState::Loading
                    | NativeModTargetState::Initializing
                    | NativeModTargetState::Stopping,
                    NativeModTargetState::Unloaded | NativeModTargetState::NotLoaded,
                ) => NativeModTargetStatus {
                    pid: current.pid,
                    executable: current.executable.clone(),
                    state: NativeModTargetState::RolledBack,
                    detail: "This target's load was rolled back after another target failed.".into(),
                },
                _ => current.clone(),
            }
        })
        .collect()
}

impl NativeModManager {
    pub fn new(root: PathBuf) -> Self {
        let _ = fs::create_dir_all(root.join("packages"));
        let state = load_persisted_state(&root.join("enabled.json"));
        Self {
            root,
            state: Mutex::new(state),
            statuses: Mutex::new(HashMap::new()),
            target_statuses: Mutex::new(HashMap::new()),
            lifecycle_gate: Mutex::new(()),
        }
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    fn signature_policy_enabled(&self) -> bool {
        // A poisoned security-state mutex must not silently disable enforcement.
        self.state
            .lock()
            .map(|state| state.require_trusted_signature)
            .unwrap_or(true)
    }

    pub fn set_require_trusted_signature(&self, required: bool) -> Result<String, String> {
        let _transition = self.lifecycle_gate
            .lock()
            .map_err(|_| "Native-mod lifecycle gate is unavailable.".to_string())?;

        // Enabling policy removes untrusted mod IDs from persisted enable intent
        // before attempting remote unload. Any uncertain unload is stored as
        // durable cleanup work so subsequent scans and app restarts can retry it.
        let previously_pending = self.pending_signature_unloads_snapshot();
        let mut candidates = self.enabled_snapshot();
        candidates.extend(previously_pending.iter().cloned());
        let mut package_scan_warning = None;
        if required {
            match fs::read_dir(self.packages_dir()) {
                Ok(entries) => {
                    for entry in entries {
                        match entry {
                            Ok(entry) => {
                                let file_type = match entry.file_type() {
                                    Ok(file_type) => file_type,
                                    Err(error) => {
                                        package_scan_warning = Some(format!(
                                            "could not inspect every installed package directory: {error}"
                                        ));
                                        continue;
                                    }
                                };
                                if !file_type.is_dir() || file_type.is_symlink() {
                                    continue;
                                }
                                let package_name = entry.file_name();
                                if let Some(id) = package_name.to_str() {
                                    // Stage/backup directories intentionally use leading dots
                                    // and are excluded by the same ID validation as package APIs.
                                    if valid_id(id) {
                                        candidates.insert(id.to_string());
                                    }
                                }
                            }
                            Err(error) => {
                                package_scan_warning = Some(format!(
                                    "could not enumerate every installed package directory: {error}"
                                ));
                            }
                        }
                    }
                }
                Err(error) => {
                    package_scan_warning = Some(format!(
                        "could not enumerate the installed package directory: {error}"
                    ));
                }
            }
        }

        // Include disabled packages as well: a prior failed/uncertain unload may
        // leave an untrusted DLL mapped even though its enable intent is already off.
        let untrusted: Vec<String> = if required {
            candidates
                .iter()
                .filter(|id| previously_pending.contains(*id) || !self.package_has_trusted_signature(id))
                .cloned()
                .collect()
        } else {
            Vec::new()
        };

        {
            let mut state = self.state.lock()
                .map_err(|_| "Native mod state is unavailable.".to_string())?;
            let previous = state.clone();
            state.require_trusted_signature = required;
            if required {
                for id in &untrusted {
                    state.enabled.remove(id);
                    state.pending_signature_unloads.insert(id.clone());
                }
            }
            // Pending unload is a runtime-safety invariant, not a signature
            // policy choice. Keep it persisted even if enforcement is switched
            // off; the next background scan must still finish safe cleanup.
            if let Err(error) = self.persist_state(&state) {
                *state = previous;
                return Err(error);
            }
        }

        let mut unload_warnings = Vec::new();
        if let Some(warning) = package_scan_warning {
            unload_warnings.push(warning);
        }
        if required {
            for id in &untrusted {
                match self.disable_inner(id) {
                    Ok(status) => {
                        if let Err(error) = self.clear_pending_signature_unload(id) {
                            self.set_status(id, format!(
                                "Untrusted mod was unloaded, but cleanup state could not be cleared and will be retried: {error}"
                            ));
                            unload_warnings.push(format!("{id}: could not persist cleanup completion: {error}"));
                        } else {
                            self.set_status(id, format!(
                                "Signature policy enabled; untrusted mod disabled. {status}"
                            ));
                        }
                    }
                    Err(error) => {
                        self.mark_running_targets_indeterminate(id, &error);
                        self.set_status(id, format!(
                            "Signature policy enabled; unload remains pending and will be retried during later scans: {error}"
                        ));
                        unload_warnings.push(format!("{id}: {error}"));
                    }
                }
            }
        }

        if !unload_warnings.is_empty() {
            Ok(format!(
                "Trusted Authenticode enforcement is ON. The following disabled mod(s) could not be confirmed unloaded; restart the affected shell process before relying on the policy: {}",
                unload_warnings.join("; ")
            ))
        } else if required && !untrusted.is_empty() {
            Ok(format!(
                "Trusted Authenticode enforcement is ON. Checked and disabled {} unsigned, untrusted, or unverifiable native mod(s).",
                untrusted.len()
            ))
        } else if required {
            Ok("Trusted Authenticode signature enforcement is ON.".into())
        } else {
            Ok("Trusted Authenticode signature enforcement is OFF.".into())
        }
    }

    fn package_has_trusted_signature(&self, id: &str) -> bool {
        let Ok(folder) = self.package_dir(id) else { return false; };
        let Ok(manifest) = load_manifest(&folder) else { return false; };
        let Ok(module) = module_file_path(&folder, &manifest.module) else { return false; };
        verify_trusted_authenticode(&module)
    }

    fn packages_dir(&self) -> PathBuf {
        self.root.join("packages")
    }

    fn package_dir(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) {
            return Err("Invalid native mod ID.".into());
        }

        let app_root = self.root.canonicalize()
            .map_err(|e| format!("Cannot resolve native mod data directory: {e}"))?;
        let expected_packages = app_root.join("packages");
        let packages = self.packages_dir();
        let packages_meta = fs::symlink_metadata(&packages)
            .map_err(|e| format!("Cannot inspect native mod package directory: {e}"))?;
        if packages_meta.file_type().is_symlink() || !packages_meta.is_dir() {
            return Err("Native mod package root must be a real directory, not a symlink.".into());
        }
        let packages = packages.canonicalize()
            .map_err(|e| format!("Cannot resolve native mod package root: {e}"))?;
        if packages != expected_packages {
            return Err("Native mod package root resolves outside its managed location.".into());
        }
        let candidate = packages.join(id);
        match fs::symlink_metadata(&candidate) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    return Err("Installed native mod package must be a real directory, not a symlink.".into());
                }
                let resolved = candidate.canonicalize()
                    .map_err(|e| format!("Cannot resolve installed native mod package: {e}"))?;
                if !resolved.starts_with(&packages) {
                    return Err("Native mod package path escapes the managed package directory.".into());
                }
                Ok(resolved)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(candidate),
            Err(e) => Err(format!("Cannot inspect installed native mod package: {e}")),
        }
    }

    fn enabled_snapshot(&self) -> HashSet<String> {
        self.state.lock().map(|s| s.enabled.clone()).unwrap_or_default()
    }

    fn pending_signature_unloads_snapshot(&self) -> HashSet<String> {
        self.state
            .lock()
            .map(|state| state.pending_signature_unloads.clone())
            .unwrap_or_default()
    }

    fn pending_safe_unloads_snapshot(&self) -> HashSet<String> {
        self.state
            .lock()
            .map(|state| state.pending_safe_unloads.clone())
            .unwrap_or_default()
    }

    fn mark_pending_safe_unload(&self, id: &str) -> Result<(), String> {
        if !valid_id(id) {
            return Err("Cannot record safe-unload work for an invalid mod ID.".into());
        }
        let mut state = self.state
            .lock()
            .map_err(|_| "Native mod state is unavailable.".to_string())?;
        if !state.pending_safe_unloads.insert(id.to_string()) {
            return Ok(());
        }
        if let Err(error) = self.persist_state(&state) {
            state.pending_safe_unloads.remove(id);
            return Err(error);
        }
        Ok(())
    }

    fn clear_pending_safe_unload(&self, id: &str) -> Result<(), String> {
        let mut state = self.state
            .lock()
            .map_err(|_| "Native mod state is unavailable.".to_string())?;
        if !state.pending_safe_unloads.remove(id) {
            return Ok(());
        }
        if let Err(error) = self.persist_state(&state) {
            state.pending_safe_unloads.insert(id.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn clear_pending_signature_unload(&self, id: &str) -> Result<(), String> {
        let mut state = self.state
            .lock()
            .map_err(|_| "Native mod state is unavailable.".to_string())?;
        if !state.pending_signature_unloads.remove(id) {
            return Ok(());
        }
        if let Err(error) = self.persist_state(&state) {
            state.pending_signature_unloads.insert(id.to_string());
            return Err(error);
        }
        Ok(())
    }

    fn retry_pending_safe_unloads(&self) {
        // Include the signature-specific set for state files written by earlier
        // builds; all newly detected uncertain unloads use the general safe set.
        let mut pending = self.pending_safe_unloads_snapshot();
        pending.extend(self.pending_signature_unloads_snapshot());
        for id in pending {
            let Ok(_transition) = self.lifecycle_gate.lock() else {
                return;
            };
            if !self.pending_safe_unloads_snapshot().contains(&id)
                && !self.pending_signature_unloads_snapshot().contains(&id) {
                continue;
            }

            match self.disable_inner(&id) {
                Ok(status) => {
                    // Evaluate both writes so a failure clearing one marker
                    // cannot accidentally skip clearing the other.
                    let safe_clear = self.clear_pending_safe_unload(&id);
                    let signature_clear = self.clear_pending_signature_unload(&id);
                    match safe_clear.and(signature_clear) {
                        Ok(()) => self.set_status(&id, format!(
                            "Deferred safe-unload cleanup completed during background scan. {status}"
                        )),
                        Err(error) => self.set_status(&id, format!(
                            "DLL unload completed, but cleanup state remains pending because persistence failed: {error}"
                        )),
                    }
                }
                Err(error) => {
                    let _ = self.mark_pending_safe_unload(&id);
                    self.mark_running_targets_indeterminate(&id, &error);
                    self.set_status(&id, format!(
                        "Safe-unload cleanup is still pending; 00047 will retry during later scans: {error}"
                    ));
                }
            }
        }
    }

    fn persist_state(&self, state: &PersistedState) -> Result<(), String> {
        if state.enabled.iter().any(|id| !valid_id(id))
            || state.pending_signature_unloads.iter().any(|id| !valid_id(id))
            || state.pending_safe_unloads.iter().any(|id| !valid_id(id)) {
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

    fn target_status_snapshot(&self, id: &str) -> Vec<NativeModTargetStatus> {
        self.target_statuses
            .lock()
            .ok()
            .and_then(|statuses| statuses.get(id).cloned())
            .unwrap_or_default()
    }

    fn set_target_statuses(&self, id: &str, statuses: Vec<NativeModTargetStatus>) {
        if let Ok(mut stored) = self.target_statuses.lock() {
            if statuses.is_empty() {
                stored.remove(id);
            } else {
                stored.insert(id.to_string(), statuses);
            }
        }
    }

    fn set_target_status(&self, id: &str, status: NativeModTargetStatus) {
        if let Ok(mut stored) = self.target_statuses.lock() {
            let statuses = stored.entry(id.to_string()).or_default();
            if let Some(existing) = statuses.iter_mut().find(|existing| existing.pid == status.pid) {
                *existing = status;
            } else {
                statuses.push(status);
                statuses.sort_by_key(|entry| entry.pid);
            }
        }
    }

    fn mark_loaded_targets_stopping(&self, id: &str) {
        let statuses = self.target_status_snapshot(id)
            .into_iter()
            .map(|target| {
                if matches!(
                    target.state,
                    NativeModTargetState::Initialized
                        | NativeModTargetState::Running
                        | NativeModTargetState::Loading
                        | NativeModTargetState::Initializing
                ) {
                    NativeModTargetStatus {
                        state: NativeModTargetState::Stopping,
                        detail: "Shutdown and unload are in progress.".into(),
                        ..target
                    }
                } else {
                    target
                }
            })
            .collect();
        self.set_target_statuses(id, statuses);
    }

    fn mark_running_targets_indeterminate(&self, id: &str, reason: &str) {
        let unresolved = self
            .target_status_snapshot(id)
            .into_iter()
            .map(|target| {
                if matches!(
                    target.state,
                    NativeModTargetState::Initialized
                        | NativeModTargetState::Running
                        | NativeModTargetState::Loading
                        | NativeModTargetState::Initializing
                        | NativeModTargetState::Stopping
                        | NativeModTargetState::Failed
                ) {
                    NativeModTargetStatus {
                        state: NativeModTargetState::Indeterminate,
                        detail: format!(
                            "Unload/load completion could not be confirmed: {reason}. The DLL may still be present; restart this target process to guarantee recovery."
                        ),
                        ..target
                    }
                } else {
                    target
                }
            })
            .collect();
        self.set_target_statuses(id, unresolved);
    }

    pub fn list(&self) -> NativeModList {
        let enabled = self.enabled_snapshot();
        let require_trusted_signature = self.signature_policy_enabled();
        let mut pending_signature_unloads: Vec<String> =
            self.pending_signature_unloads_snapshot().into_iter().collect();
        pending_signature_unloads.sort();
        let mut pending_safe_unloads: Vec<String> =
            self.pending_safe_unloads_snapshot().into_iter().collect();
        pending_safe_unloads.sort();
        let statuses = self.statuses.lock().map(|s| s.clone()).unwrap_or_default();
        let mut mods = Vec::new();
        let mut errors = Vec::new();

        // Keep per-process lifecycle state only while the corresponding PID is still
        // present. If process discovery fails, preserve the last known state instead
        // of incorrectly treating every target as exited.
        let (targets, discovery_succeeded) = match discover_targets() {
            Ok(targets) => (targets, true),
            Err(e) => {
                errors.push(e);
                (Vec::new(), false)
            }
        };
        if discovery_succeeded {
            let active_pids: HashSet<u32> = targets.iter().map(|target| target.pid).collect();
            if let Ok(mut stored) = self.target_statuses.lock() {
                stored.retain(|_, per_target| {
                    per_target.retain(|status| active_pids.contains(&status.pid));
                    !per_target.is_empty()
                });
            }
        }

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
            let folder = match self.package_dir(&folder_name) {
                Ok(path) => path,
                Err(e) => { errors.push(format!("{folder_name}: {e}")); continue; }
            };
            let manifest = match load_manifest(&folder) {
                Ok(m) => m,
                Err(e) => { errors.push(format!("{folder_name}: {e}")); continue; }
            };
            if manifest.id != folder_name {
                errors.push(format!("{folder_name}: mod ID does not match its package directory"));
                continue;
            }
            let module = match module_file_path(&folder, &manifest.module) {
                Ok(path) => path,
                Err(e) => { errors.push(format!("{}: {e}", manifest.id)); continue; }
            };
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
                target_statuses: self.target_status_snapshot(&manifest.id),
            });
        }

        NativeModList {
            dir: self.root.to_string_lossy().to_string(),
            mods,
            targets,
            errors,
            require_trusted_signature,
            pending_signature_unloads,
            pending_safe_unloads,
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
        let _transition = self.lifecycle_gate
            .lock()
            .map_err(|_| "Native-mod lifecycle gate is unavailable.".to_string())?;
        let read = read_package(path)?;
        let id = read.manifest.id.clone();
        if self.enabled_snapshot().contains(&id) {
            return Err("Disable this native mod before updating its files.".into());
        }
        if self.pending_signature_unloads_snapshot().contains(&id)
            || self.pending_safe_unloads_snapshot().contains(&id) {
            return Err(
                "This mod has a pending safe-unload operation. Let 00047 complete cleanup before updating its files."
                    .into(),
            );
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
        let staged_module = match module_file_path(&stage, &staged.module) {
            Ok(path) => path,
            Err(e) => { let _ = fs::remove_dir_all(&stage); return Err(e); }
        };
        if let Err(e) = validate_pe_file(&staged_module) {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
        if self.signature_policy_enabled() && !verify_trusted_authenticode(&staged_module) {
            let _ = fs::remove_dir_all(&stage);
            return Err(
                "Trusted Authenticode signature is required; the package DLL is unsigned, untrusted, revoked, or could not be verified using cached trust data."
                    .into(),
            );
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
        self.set_target_statuses(&id, Vec::new());
        self.set_status(&id, "Installed; disabled".into());
        Ok(id)
    }

    pub fn enable(&self, id: &str) -> Result<String, String> {
        let _transition = self.lifecycle_gate
            .lock()
            .map_err(|_| "Native-mod lifecycle gate is unavailable.".to_string())?;
        let folder = self.package_dir(id)?;
        if self.pending_signature_unloads_snapshot().contains(id)
            || self.pending_safe_unloads_snapshot().contains(id) {
            return Err(
                "A previous DLL image is still pending safe unload. Wait for 00047 to confirm cleanup before enabling this mod."
                    .into(),
            );
        }
        let manifest = load_manifest(&folder)?;
        if manifest.id != id {
            return Err("Mod ID does not match its package directory.".into());
        }
        let module = module_file_path(&folder, &manifest.module)?;
        validate_pe_file(&module)?;
        if self.signature_policy_enabled() && !verify_trusted_authenticode(&module) {
            return Err(
                "Trusted Authenticode signature is required, but this DLL is unsigned, untrusted, revoked, or could not be verified using cached trust data."
                    .into(),
            );
        }

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
        let _transition = self.lifecycle_gate
            .lock()
            .map_err(|_| "Native-mod lifecycle gate is unavailable.".to_string())?;
        self.disable_inner(id)
    }

    fn disable_inner(&self, id: &str) -> Result<String, String> {
        if !valid_id(id) {
            return Err("Invalid native mod ID.".into());
        }
        // Persist retry work before removing the enabled intent. If the later
        // state write or remote unload fails, recovery survives an app restart.
        self.mark_pending_safe_unload(id)?;
        // Persist disabled intent before touching package files. A damaged or
        // temporarily inaccessible manifest must not keep the background
        // reconciler repeatedly trying to load a mod the user disabled.
        {
            let mut state = self.state.lock().map_err(|_| "Native mod state is unavailable.")?;
            let was_enabled = state.enabled.remove(id);
            if let Err(error) = self.persist_state(&state) {
                if was_enabled {
                    state.enabled.insert(id.to_string());
                }
                return Err(error);
            }
        }

        let folder = match self.package_dir(id) {
            Ok(folder) => folder,
            Err(error) => {
                let pending_error = self.mark_pending_safe_unload(id).err();
                self.mark_running_targets_indeterminate(id, &error);
                let mut status = format!(
                    "Disabled intent persisted; safe unload remains pending because the package path is unavailable: {error}"
                );
                if let Some(persist_error) = pending_error {
                    status.push_str(&format!("; could not persist pending safe-unload work: {persist_error}"));
                }
                self.set_status(id, status.clone());
                return Err(status);
            }
        };
        let manifest = match load_manifest(&folder) {
            Ok(manifest) => manifest,
            Err(error) => {
                let pending_error = self.mark_pending_safe_unload(id).err();
                self.mark_running_targets_indeterminate(id, &error);
                let mut status = format!("Disabled; cannot confirm unload because manifest validation failed: {error}");
                if let Some(persist_error) = pending_error {
                    status.push_str(&format!("; could not persist pending safe-unload work: {persist_error}"));
                }
                self.set_status(id, status.clone());
                return Err(status);
            }
        };

        let dll = match module_file_path(&folder, &manifest.module) {
            Ok(path) => path,
            Err(error) => {
                let pending_error = self.mark_pending_safe_unload(id).err();
                self.mark_running_targets_indeterminate(id, &error);
                let mut status = format!("Disabled; unload skipped because the module path is unsafe: {error}");
                if let Some(persist_error) = pending_error {
                    status.push_str(&format!("; could not persist pending safe-unload work: {persist_error}"));
                }
                self.set_status(id, status.clone());
                return Err(status);
            }
        };

        self.mark_loaded_targets_stopping(id);
        match unload_from_targets(&manifest.targets, &dll) {
            Ok(report) => {
                self.set_target_statuses(id, report.target_statuses);
                if report.errors.is_empty() {
                    if let Err(error) = self.clear_pending_safe_unload(id) {
                        let status = format!(
                            "Disabled and unloaded from {} process(es), but pending cleanup could not be cleared: {error}",
                            report.unloaded
                        );
                        self.set_status(id, status.clone());
                        return Err(status);
                    }
                    let status = format!("Disabled; unloaded from {} process(es)", report.unloaded);
                    self.set_status(id, status.clone());
                    Ok(status)
                } else {
                    let details = report.errors.join("; ");
                    let pending_error = self.mark_pending_safe_unload(id).err();
                    self.mark_running_targets_indeterminate(id, &details);
                    let mut status = format!(
                        "Disabled; unloaded from {} process(es); unload warning: {}",
                        report.unloaded,
                        details
                    );
                    if let Some(error) = pending_error {
                        status.push_str(&format!("; could not persist pending safe-unload work: {error}"));
                    }
                    self.set_status(id, status.clone());
                    Err(status)
                }
            }
            Err(error) => {
                let pending_error = self.mark_pending_safe_unload(id).err();
                self.mark_running_targets_indeterminate(id, &error);
                let mut status = format!("Disabled; unload warning: {error}");
                if let Some(persist_error) = pending_error {
                    status.push_str(&format!("; could not persist pending safe-unload work: {persist_error}"));
                }
                self.set_status(id, status.clone());
                Err(status)
            }
        }
    }

    pub fn uninstall(&self, id: &str) -> Result<(), String> {
        let _transition = self.lifecycle_gate
            .lock()
            .map_err(|_| "Native-mod lifecycle gate is unavailable.".to_string())?;
        let folder = self.package_dir(id)?;
        if self.enabled_snapshot().contains(id)
            || self.pending_signature_unloads_snapshot().contains(id)
            || self.pending_safe_unloads_snapshot().contains(id) {
            self.disable_inner(id)?;
            self.clear_pending_signature_unload(id)?;
            self.clear_pending_safe_unload(id)?;
        }
        if folder.exists() {
            fs::remove_dir_all(&folder).map_err(|e| format!("remove native mod package: {e}"))?;
        }
        if let Ok(mut statuses) = self.statuses.lock() {
            statuses.remove(id);
        }
        if let Ok(mut statuses) = self.target_statuses.lock() {
            statuses.remove(id);
        }
        Ok(())
    }

    pub fn apply_enabled_mods(&self) {
        self.retry_pending_safe_unloads();
        // Serialize each reconciliation with install/enable/disable/uninstall.
        // Re-check persisted intent after acquiring the gate, so a stale scan
        // cannot load a mod the user disabled while this scan was queued.
        let enabled = self.enabled_snapshot();
        for id in enabled {
            let Ok(_transition) = self.lifecycle_gate.lock() else {
                return;
            };
            if !self.enabled_snapshot().contains(&id) {
                continue;
            }
            match self.apply_one(&id) {
                Ok(status) => self.set_status(&id, status),
                Err(e) => self.disable_after_load_failure(&id, &e),
            }
        }
    }

    fn disable_after_load_failure(&self, id: &str, reason: &str) {
        let queue_signature_cleanup = self.signature_policy_enabled()
            && reason.contains("Trusted Authenticode signature is required");
        let mut persistence_warning = self.mark_pending_safe_unload(id).err()
            .map(|error| format!("could not persist pending safe-unload work: {error}"));
        if let Ok(mut state) = self.state.lock() {
            state.enabled.remove(id);
            if queue_signature_cleanup {
                state.pending_signature_unloads.insert(id.to_string());
            }
            if let Err(error) = self.persist_state(&state) {
                persistence_warning = Some(format!("could not persist disabled state: {error}"));
            }
        } else {
            persistence_warning = Some("native mod state lock is unavailable".into());
        }

        let previous_target_statuses = self.target_status_snapshot(id);
        self.mark_loaded_targets_stopping(id);
        let cleanup_warning = match self.package_dir(id) {
            Ok(folder) => match load_manifest(&folder) {
                Ok(manifest) => match module_file_path(&folder, &manifest.module) {
                    Ok(dll) => match unload_from_targets(&manifest.targets, &dll) {
                        Ok(report) => {
                            self.set_target_statuses(
                                id,
                                merge_rollback_statuses(&previous_target_statuses, &report.target_statuses),
                            );
                            if report.errors.is_empty() {
                                None
                            } else {
                                Some(report.errors.join("; "))
                            }
                        }
                        Err(error) => Some(error),
                    },
                    Err(error) => Some(error),
                },
                Err(error) => Some(error),
            },
            Err(error) => Some(error),
        };

        if let Some(warning) = cleanup_warning.as_deref() {
            if let Err(error) = self.mark_pending_safe_unload(id) {
                persistence_warning = Some(match persistence_warning {
                    Some(previous) => format!("{previous}; could not persist pending safe-unload work: {error}"),
                    None => format!("could not persist pending safe-unload work: {error}"),
                });
            }
            self.mark_running_targets_indeterminate(id, warning);
        } else {
            if let Err(error) = self.clear_pending_safe_unload(id) {
                persistence_warning = Some(match persistence_warning {
                    Some(previous) => format!("{previous}; could not clear pending safe-unload work: {error}"),
                    None => format!("could not clear pending safe-unload work: {error}"),
                });
            }
            if let Err(error) = self.clear_pending_signature_unload(id) {
                persistence_warning = Some(match persistence_warning {
                    Some(previous) => format!("{previous}; could not clear signature-cleanup state: {error}"),
                    None => format!("could not clear signature-cleanup state: {error}"),
                });
            }
        }

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
        let dll = module_file_path(&folder, &manifest.module)?;
        validate_pe_file(&dll)?;
        if self.signature_policy_enabled() && !verify_trusted_authenticode(&dll) {
            return Err(
                "Trusted Authenticode signature is required, but the installed DLL no longer verifies against the system trust store."
                    .into(),
            );
        }
        let previous_target_statuses = self.target_status_snapshot(id);
        let targets = discover_targets()?;
        let matching: Vec<NativeTarget> = targets.into_iter()
            .filter(|p| manifest.targets.iter().any(|t| t.eq_ignore_ascii_case(&p.executable)))
            .collect();

        if matching.is_empty() {
            self.set_target_statuses(id, Vec::new());
            return Ok("Enabled; waiting for a supported target process".into());
        }

        let mut successes = 0usize;
        let mut failures = Vec::new();
        let mut reload_pending = Vec::new();
        let mut per_target = Vec::new();
        for target in matching {
            if !target.eligible {
                failures.push(format!("{} (PID {}): {}", target.executable, target.pid, target.detail));
                per_target.push(NativeModTargetStatus {
                    pid: target.pid,
                    executable: target.executable,
                    state: NativeModTargetState::Ineligible,
                    detail: target.detail,
                });
                continue;
            }
            let pid = target.pid;
            let executable = target.executable.clone();
            let retrying_reload = target_has_pending_reload(&previous_target_statuses, pid);
            self.set_target_status(
                id,
                NativeModTargetStatus {
                    pid,
                    executable: executable.clone(),
                    state: NativeModTargetState::Detected,
                    detail: "Supported target discovered; preparing native mod load.".into(),
                },
            );
            let status_id = id.to_string();
            let status_executable = executable.clone();
            let mut on_phase = |state: NativeModTargetState, detail: String| {
                self.set_target_status(
                    &status_id,
                    NativeModTargetStatus {
                        pid,
                        executable: status_executable.clone(),
                        state,
                        detail,
                    },
                );
            };
            let result = inject_into_process(pid, &executable, &dll, &mut on_phase);
            drop(on_phase);
            match result {
                Ok(()) => {
                    successes += 1;
                    per_target.push(NativeModTargetStatus {
                        pid,
                        executable,
                        state: NativeModTargetState::Initialized,
                        detail: "The DLL is present and the loader completed successfully.".into(),
                    });
                }
                Err(e) => {
                    let indeterminate = target_operation_is_indeterminate(pid);
                    let defer_reload = should_defer_reload_failure(&e, retrying_reload);
                    let detail = if defer_reload && !e.starts_with("reload pending:") {
                        format!("reload pending: waiting for target recovery before retrying: {e}")
                    } else {
                        e
                    };
                    let target_error = format!("{executable} (PID {pid}): {detail}");
                    if defer_reload {
                        reload_pending.push(target_error);
                    } else {
                        failures.push(target_error);
                    }
                    per_target.push(NativeModTargetStatus {
                        pid,
                        executable,
                        state: if indeterminate {
                            NativeModTargetState::Indeterminate
                        } else if defer_reload {
                            NativeModTargetState::ReloadPending
                        } else {
                            NativeModTargetState::Failed
                        },
                        detail,
                    });
                }
            }
        }
        self.set_target_statuses(id, per_target);
        if !failures.is_empty() {
            return Err(format!("loaded into {successes} process(es); {}", failures.join("; ")));
        }
        if !reload_pending.is_empty() {
            return Ok(format!(
                "Enabled in {successes} process(es); safe reload pending on {} target(s), retrying on later scans: {}",
                reload_pending.len(),
                reload_pending.join("; ")
            ));
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
        require_trusted_signature: state.require_trusted_signature,
        pending_signature_unloads: state.pending_signature_unloads
            .into_iter()
            .filter(|id| valid_id(id))
            .collect(),
        pending_safe_unloads: state.pending_safe_unloads
            .into_iter()
            .filter(|id| valid_id(id))
            .collect(),
    }
}

#[cfg(windows)]
unsafe extern "C" {
    fn wmod_verify_trusted_authenticode(path: *const u16) -> i32;
}

#[cfg(windows)]
fn verify_trusted_authenticode(path: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return false;
    }
    wide.push(0);
    unsafe { wmod_verify_trusted_authenticode(wide.as_ptr()) == 1 }
}

#[cfg(not(windows))]
fn verify_trusted_authenticode(_path: &Path) -> bool {
    false
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
    !value.is_empty()
        && !value.starts_with('/')
        && !value.contains('\\')
        && value.split('/').all(valid_windows_path_component)
}

fn valid_windows_path_component(part: &str) -> bool {
    if part.is_empty()
        || part == "."
        || part == ".."
        || part.ends_with(' ')
        || part.ends_with('.')
        || part.chars().any(|ch| {
            ch <= '\u{1f}' || matches!(ch, '<' | '>' | ':' | '"' | '\\' | '|' | '?' | '*')
        })
    {
        return false;
    }

    // Windows device names remain reserved even when followed by an extension.
    let stem = part.split('.').next().unwrap_or(part)
        .trim_end_matches(|ch| ch == ' ' || ch == '.')
        .to_ascii_lowercase();
    if matches!(stem.as_str(), "con" | "prn" | "aux" | "nul" | "conin$" | "conout$") {
        return false;
    }
    for prefix in ["com", "lpt"] {
        if let Some(suffix) = stem.strip_prefix(prefix) {
            if matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³") {
                return false;
            }
        }
    }
    true
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
    // Strip at most one directory separator. Extra separators leave an empty
    // component and are rejected instead of being silently normalized.
    let normalized = path.strip_suffix('/').unwrap_or(path);
    if !valid_relative_path(normalized) {
        return Err(format!("unsafe or Windows-ambiguous ZIP path: {path}"));
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
    let folder_meta = fs::symlink_metadata(folder)
        .map_err(|e| format!("cannot inspect native mod package directory: {e}"))?;
    if folder_meta.file_type().is_symlink() || !folder_meta.is_dir() {
        return Err("native mod package must be a real directory, not a symlink".into());
    }
    let folder = folder.canonicalize()
        .map_err(|e| format!("cannot resolve native mod package directory: {e}"))?;
    let path = folder.join("manifest.json");
    let meta = fs::symlink_metadata(&path)
        .map_err(|e| format!("cannot inspect manifest.json: {e}"))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("manifest.json must be a regular file, not a symlink".into());
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err("manifest.json exceeds the 1 MB limit".into());
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read manifest.json: {e}"))?;
    let manifest: ModManifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("invalid native mod manifest: {e}"))?;
    validate_manifest(&manifest, None)?;
    Ok(manifest)
}

fn module_file_path(folder: &Path, module: &str) -> Result<PathBuf, String> {
    if !valid_relative_path(module) {
        return Err("native module path is not a safe relative path".into());
    }
    let folder = folder.canonicalize()
        .map_err(|e| format!("cannot resolve native mod package directory: {e}"))?;
    let candidate = folder.join(module);
    let meta = fs::symlink_metadata(&candidate)
        .map_err(|e| format!("cannot inspect native module {}: {e}", candidate.display()))?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err("native module must be a regular file, not a symlink".into());
    }
    let resolved = candidate.canonicalize()
        .map_err(|e| format!("cannot resolve native module {}: {e}", candidate.display()))?;
    if !resolved.starts_with(&folder) {
        return Err("native module path escapes the managed package directory".into());
    }
    Ok(resolved)
}

fn extract_package(path: &Path, destination: &Path) -> Result<(), String> {
    let package_meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if package_meta.len() > MAX_ZIP_BYTES {
        return Err("native mod ZIP exceeds the 20 MB compressed-size limit".into());
    }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;
    if archive.len() == 0 || archive.len() > MAX_FILES {
        return Err(format!("native mod ZIP must contain between 1 and {MAX_FILES} entries"));
    }
    // The stage path must be new. `create_dir_all` would follow an already-planted
    // directory symlink/junction and could extract package contents outside the store.
    fs::create_dir(destination).map_err(|e| format!("cannot create native mod staging directory: {e}"))?;
    let mut total = 0u64;
    let mut windows_path_keys = HashSet::new();
    let mut windows_path_types = HashMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        let relative = entry.enclosed_name()
            .ok_or_else(|| format!("unsafe ZIP path: {name}"))?
            .to_path_buf();
        register_windows_zip_path(&mut windows_path_keys, &name)?;
        let normalized = name.strip_suffix('/').unwrap_or(&name);
        windows_path_types.insert(normalized.to_lowercase(), entry.is_dir());
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES { return Err("native mod ZIP expands beyond the 50 MB unpacked-size limit".into()); }
        if entry.size() > 0 && entry.compressed_size() == 0 {
            return Err(format!("invalid compressed ZIP entry size: {name}"));
        }
        if entry.compressed_size() > 0
            && entry.size() / entry.compressed_size() > 1000
        {
            return Err(format!("suspicious ZIP compression ratio: {name}"));
        }
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
    validate_windows_path_hierarchy(&windows_path_types)?;
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
fn inject_into_process(
    pid: u32,
    executable: &str,
    module: &Path,
    on_phase: &mut dyn FnMut(NativeModTargetState, String),
) -> Result<(), String> {
    win::inject(pid, executable, module, on_phase)
}
#[cfg(not(windows))]
fn inject_into_process(
    _pid: u32,
    _executable: &str,
    _module: &Path,
    _on_phase: &mut dyn FnMut(NativeModTargetState, String),
) -> Result<(), String> {
    Err("native shell mods can only be loaded on Windows".into())
}

fn target_operation_is_indeterminate(pid: u32) -> bool {
    #[cfg(windows)]
    {
        win::target_operation_is_indeterminate(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

fn unload_from_targets(targets: &[String], module: &Path) -> Result<NativeUnloadReport, String> {
    #[cfg(windows)]
    {
        let running = win::discover_targets()?;
        let mut report = NativeUnloadReport {
            unloaded: 0,
            target_statuses: Vec::new(),
            errors: Vec::new(),
        };
        for target in running
            .into_iter()
            .filter(|p| targets.iter().any(|t| t.eq_ignore_ascii_case(&p.executable)))
        {
            if !target.eligible {
                report.target_statuses.push(NativeModTargetStatus {
                    pid: target.pid,
                    executable: target.executable,
                    state: NativeModTargetState::Ineligible,
                    detail: target.detail,
                });
                continue;
            }
            match win::unload(target.pid, &target.executable, module) {
                Ok(true) => {
                    report.unloaded += 1;
                    report.target_statuses.push(NativeModTargetStatus {
                        pid: target.pid,
                        executable: target.executable,
                        state: NativeModTargetState::Unloaded,
                        detail: "Shutdown completed and the DLL was unloaded.".into(),
                    });
                }
                Ok(false) => report.target_statuses.push(NativeModTargetStatus {
                    pid: target.pid,
                    executable: target.executable,
                    state: NativeModTargetState::NotLoaded,
                    detail: "The DLL was not loaded in this process.".into(),
                }),
                Err(error) => {
                    let indeterminate = target_operation_is_indeterminate(target.pid);
                    report.errors.push(format!(
                        "{} (PID {}): {error}",
                        target.executable, target.pid
                    ));
                    report.target_statuses.push(NativeModTargetStatus {
                        pid: target.pid,
                        executable: target.executable,
                        state: if indeterminate {
                            NativeModTargetState::Indeterminate
                        } else {
                            NativeModTargetState::Failed
                        },
                        detail: error,
                    });
                }
            }
        }
        Ok(report)
    }
    #[cfg(not(windows))]
    {
        let _ = (targets, module);
        Err("native shell mods can only be unloaded on Windows".into())
    }
}

#[cfg(windows)]
mod win {
    use super::{validate_pe_file, NativeModTargetState, NativeTarget, SUPPORTED_TARGETS};
    use std::{
        collections::HashSet,
        ffi::{c_char, c_void, OsStr, OsString},
        os::windows::ffi::{OsStrExt, OsStringExt},
        path::Path,
        ptr,
        sync::{Mutex, OnceLock},
    };

    type Handle = *mut c_void;

    const INVALID_HANDLE_VALUE: isize = -1;
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
    const TH32CS_SNAPMODULE: u32 = 0x0000_0008;
    const TH32CS_SNAPMODULE32: u32 = 0x0000_0010;
    const PROCESS_CREATE_THREAD: u32 = 0x0002;
    const SYNCHRONIZE: u32 = 0x0010_0000;
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

    static INDETERMINATE_PIDS: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    static TARGET_OPERATION_GATE: OnceLock<Mutex<()>> = OnceLock::new();

    fn indeterminate_pids() -> &'static Mutex<HashSet<u32>> {
        INDETERMINATE_PIDS.get_or_init(|| Mutex::new(HashSet::new()))
    }

    fn target_operation_gate() -> &'static Mutex<()> {
        TARGET_OPERATION_GATE.get_or_init(|| Mutex::new(()))
    }

    pub fn target_operation_is_indeterminate(pid: u32) -> bool {
        // Fail closed if the registry is poisoned: overlapping remote operations
        // are riskier than requiring a target-process restart.
        indeterminate_pids()
            .lock()
            .map(|pids| pids.contains(&pid))
            .unwrap_or(true)
    }

    fn quarantine_until_target_exit(pid: u32, process: Handle) {
        // The target may have exited between the failed wait and quarantine setup.
        // The original process handle identifies that exact process object and lets
        // us avoid retaining a stale quarantine for a PID that may later be reused.
        if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
            return;
        }

        let inserted = match indeterminate_pids().lock() {
            Ok(mut pids) => pids.insert(pid),
            Err(_) => return,
        };
        if !inserted {
            return;
        }

        // Keep the quarantine until this exact process object exits. If opening a
        // watcher or spawning its thread fails, leave this PID quarantined for the
        // rest of the app session rather than risking another operation.
        let watcher = unsafe { OpenProcess(SYNCHRONIZE, 0, pid) };
        if is_invalid(watcher) {
            if unsafe { WaitForSingleObject(process, 0) } == WAIT_OBJECT_0 {
                if let Ok(mut pids) = indeterminate_pids().lock() {
                    pids.remove(&pid);
                }
            }
            return;
        }
        let watcher_value = watcher as isize;
        let spawned = std::thread::Builder::new()
            .name(format!("00047-native-target-watch-{pid}"))
            .spawn(move || {
                let watcher = watcher_value as Handle;
                let outcome = unsafe { WaitForSingleObject(watcher, INFINITE) };
                unsafe { CloseHandle(watcher); }
                if outcome == WAIT_OBJECT_0 {
                    if let Ok(mut pids) = indeterminate_pids().lock() {
                        pids.remove(&pid);
                    }
                }
            });
        if spawned.is_err() {
            unsafe { CloseHandle(watcher); }
        }
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
            let access = PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ | SYNCHRONIZE;
            let h = OpenProcess(access, 0, pid);
            if is_invalid(h) { Err(last_error("OpenProcess(modification)")) } else { Ok(h) }
        }
    }

    fn remote_call(pid: u32, process: Handle, start: *mut c_void, parameter: *mut c_void) -> Result<u32, String> {
        if target_operation_is_indeterminate(pid) {
            return Err(format!(
                "PID {pid} is quarantined after an earlier remote-operation timeout; restart the target process before retrying."
            ));
        }
        unsafe {
            let thread = CreateRemoteThread(process, ptr::null_mut(), 0, start, parameter, 0, ptr::null_mut());
            if is_invalid(thread) { return Err(last_error("CreateRemoteThread")); }
            let wait = WaitForSingleObject(thread, 10_000);
            if wait != WAIT_OBJECT_0 {
                // A timeout or wait failure does not prove that the remote thread
                // stopped. Do not issue rollback calls against a possibly active
                // initializer/unloader. Quarantine this target until its process
                // exits; process exit also reclaims any remote path buffer.
                let wait_error = if wait == WAIT_TIMEOUT {
                    "remote thread exceeded the 10-second wait limit".to_string()
                } else {
                    last_error("WaitForSingleObject(remote thread)")
                };
                quarantine_until_target_exit(pid, process);
                CloseHandle(thread);
                return Err(format!(
                    "{wait_error}; the operation outcome is indeterminate. PID {pid} is quarantined and no further mod operations will run in it until the process exits. Restart the target process to recover."
                ));
            }
            let mut code = 0;
            let ok = GetExitCodeThread(thread, &mut code);
            CloseHandle(thread);
            if ok == 0 { return Err(last_error("GetExitCodeThread")); }
            Ok(code)
        }
    }

    fn wait_for_safe_unload(
        pid: u32,
        process: Handle,
        module_base: usize,
        can_unload_rva: usize,
        timeout: std::time::Duration,
    ) -> Result<(), String> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match remote_call(
                pid,
                process,
                (module_base + can_unload_rva) as *mut c_void,
                ptr::null_mut(),
            ) {
                Ok(code) if code != 0 => return Ok(()),
                Ok(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Ok(_) => {
                    return Err(
                        "native mod was shut down but still has in-flight module frames; it remains mapped to avoid unsafe DLL unload".into()
                    );
                }
                Err(error) => {
                    return Err(format!(
                        "could not prove native mod unload is safe; DLL was left mapped: {error}"
                    ));
                }
            }
        }
    }

    fn cleanup_after_failed_init(
        pid: u32,
        process: Handle,
        module_base: usize,
        shutdown_rva: usize,
        can_unload_rva: usize,
    ) -> Result<(), String> {
        remote_call(
            pid,
            process,
            (module_base + shutdown_rva) as *mut c_void,
            ptr::null_mut(),
        ).map_err(|error| format!("WmodShutdownV1 failed: {error}"))?;

        wait_for_safe_unload(
            pid,
            process,
            module_base,
            can_unload_rva,
            std::time::Duration::from_secs(3),
        )?;

        let free_library = remote_system_proc(pid, b"FreeLibrary\0")?;
        match remote_call(pid, process, free_library, module_base as *mut c_void) {
            Ok(code) if code != 0 => Ok(()),
            Ok(_) => Err("FreeLibrary returned FALSE in the target process".into()),
            Err(error) => Err(format!("could not free failed native mod: {error}")),
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
        if target_operation_is_indeterminate(pid) {
            return Err(format!(
                "PID {pid} has an indeterminate remote operation; further native-mod actions are blocked until that process exits. Restart the target process to recover."
            ));
        }
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

    pub fn inject(
        pid: u32,
        executable: &str,
        module_path: &Path,
        on_phase: &mut dyn FnMut(NativeModTargetState, String),
    ) -> Result<(), String> {
        // Serialize the full load/validate/initialize sequence so user actions and
        // periodic discovery cannot interleave lifecycle calls for native modules.
        let _operation = target_operation_gate()
            .lock()
            .map_err(|_| "Native-mod operation gate is unavailable.".to_string())?;
        verify_target_pid(pid, executable)?;
        if process_architecture(pid)? != "x64" {
            return Err("target process is not x64; only x64 native mods are supported".into());
        }
        validate_pe_file(module_path)?;

        let canonical_module_path = module_path
            .canonicalize()
            .map_err(|error| format!("Cannot resolve native mod DLL before loading: {error}"))?;

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
            let initialized_rva = match exported_rva(local_module, b"WmodIsInitializedV1\0") {
                Ok(v) => v, Err(e) => { free_local_map(local_module); return Err(e); }
            };
            let can_unload_rva = match exported_rva(local_module, b"WmodCanUnloadV1\0") {
                Ok(v) => v, Err(e) => { free_local_map(local_module); return Err(e); }
            };
            // Optional ABI-v2 adapter export. Older modules remain compatible and
            // simply cannot request a settings-triggered full-image reload.
            let reload_requested_rva =
                exported_rva(local_module, b"WmodIsReloadRequestedV1\0").ok();

            // A previously shut-down DLL can remain mapped if a hook callback
            // was still active. Do not misreport it as initialized or re-enter
            // its initializer until the module confirms it is quiescent.
            let existing = match module_for_path(pid, module_path) {
                Ok(value) => value,
                Err(error) => { free_local_map(local_module); return Err(error); }
            };
            if let Some(existing) = existing {
                let process = match open_modifiable_process(pid) {
                    Ok(value) => value,
                    Err(error) => { free_local_map(local_module); return Err(error); }
                };
                let base = existing.mod_base_addr as usize;
                let reload_requested = if let Some(rva) = reload_requested_rva {
                    remote_call(pid, process, (base + rva) as *mut c_void, ptr::null_mut())
                } else {
                    Ok(0)
                };

                match reload_requested {
                    Err(error) => {
                        CloseHandle(process);
                        free_local_map(local_module);
                        return Err(format!("reload pending: could not query the mod's reload request: {error}"));
                    }
                    Ok(code) if code != 0 => {
                        on_phase(
                            NativeModTargetState::Stopping,
                            "The mod requested a reload after a settings change.".into(),
                        );
                        if let Err(error) = remote_call(
                            pid, process,
                            (base + shutdown_rva) as *mut c_void,
                            ptr::null_mut(),
                        ) {
                            CloseHandle(process);
                            free_local_map(local_module);
                            return Err(format!(
                                "reload pending: reload was requested but shutdown failed; DLL remains mapped: {error}"
                            ));
                        }
                        if let Err(error) = wait_for_safe_unload(
                            pid, process, base, can_unload_rva,
                            std::time::Duration::from_secs(3),
                        ) {
                            CloseHandle(process);
                            free_local_map(local_module);
                            return Err(format!(
                                "reload pending: reload was requested but the DLL was retained for safety: {error}"
                            ));
                        }
                        let free_library = match remote_system_proc(pid, b"FreeLibrary\0") {
                            Ok(value) => value,
                            Err(error) => {
                                CloseHandle(process);
                                free_local_map(local_module);
                                return Err(format!(
                                    "reload pending: FreeLibrary could not be resolved: {error}"
                                ));
                            }
                        };
                        match remote_call(pid, process, free_library, base as *mut c_void) {
                            Ok(freed) if freed != 0 => {}
                            Ok(_) => {
                                CloseHandle(process);
                                free_local_map(local_module);
                                return Err("reload pending: FreeLibrary returned FALSE; DLL remains mapped".into());
                            }
                            Err(error) => {
                                CloseHandle(process);
                                free_local_map(local_module);
                                return Err(format!("reload pending: DLL unload was indeterminate: {error}"));
                            }
                        }
                        match module_for_path(pid, module_path) {
                            Ok(None) => {}
                            Ok(Some(_)) => {
                                // FreeLibrary succeeded but another reference keeps
                                // the same image mapped. Reinitialize that image
                                // instead of decrementing another owner's reference.
                                let init = remote_call(
                                    pid, process,
                                    (base + init_rva) as *mut c_void,
                                    ptr::null_mut(),
                                );
                                CloseHandle(process);
                                free_local_map(local_module);
                                if matches!(init, Ok(code) if code != 0) {
                                    on_phase(
                                        NativeModTargetState::Initialized,
                                        "The DLL remained mapped by another reference and was safely reinitialized in place.".into(),
                                    );
                                    return Ok(());
                                }
                                return Err(format!(
                                    "reload pending: the DLL remained mapped by another reference and reinitialization failed: {}",
                                    init.map(|code| code.to_string()).unwrap_or_else(|error| error)
                                ));
                            }
                            Err(error) => {
                                // FreeLibrary already returned success, but the
                                // image's remaining state is ambiguous. Quarantine
                                // the PID so another scan cannot decrement a
                                // reference owned by another component.
                                quarantine_until_target_exit(pid, process);
                                CloseHandle(process);
                                free_local_map(local_module);
                                return Err(format!(
                                    "reload pending: FreeLibrary completed but the module state could not be verified; target quarantined until exit: {error}"
                                ));
                            }
                        }
                        CloseHandle(process);
                        on_phase(
                            NativeModTargetState::Loading,
                            "Old DLL unloaded safely; loading a fresh image for the requested reload.".into(),
                        );
                    }
                    Ok(_) => {
                        // A previously shut-down DLL can remain mapped if a hook callback
                        // was still active. Never re-enter its initializer until the module
                        // explicitly confirms a safe quiescent state.
                        let initialized = remote_call(
                            pid, process,
                            (base + initialized_rva) as *mut c_void,
                            ptr::null_mut(),
                        );
                        match initialized {
                            Ok(code) if code != 0 => {
                                CloseHandle(process);
                                free_local_map(local_module);
                                on_phase(
                                    NativeModTargetState::Initialized,
                                    "The DLL is already initialized in this target.".into(),
                                );
                                return Ok(());
                            }
                            Ok(_) => {}
                            Err(error) => {
                                CloseHandle(process);
                                free_local_map(local_module);
                                return Err(format!("could not query the already-mapped DLL lifecycle: {error}"));
                            }
                        }

                        let safe_to_reinitialize = remote_call(
                            pid, process,
                            (base + can_unload_rva) as *mut c_void,
                            ptr::null_mut(),
                        );
                        if !matches!(safe_to_reinitialize, Ok(code) if code != 0) {
                            CloseHandle(process);
                            free_local_map(local_module);
                            let detail = match safe_to_reinitialize {
                                Ok(_) => "the previous shutdown has not reached a safe quiescent state".to_string(),
                                Err(error) => format!("the quiescence check failed: {error}"),
                            };
                            return Err(format!(
                                "DLL remains mapped and will not be reinitialized until safe: {detail}"
                            ));
                        }

                        let init = remote_call(
                            pid, process,
                            (base + init_rva) as *mut c_void,
                            ptr::null_mut(),
                        );
                        CloseHandle(process);
                        free_local_map(local_module);
                        if matches!(init, Ok(code) if code != 0) {
                            on_phase(
                                NativeModTargetState::Initialized,
                                "The previously mapped DLL was safely reinitialized.".into(),
                            );
                            return Ok(());
                        }
                        return Err(format!(
                            "the previously mapped DLL could not be reinitialized: {}",
                            init.map(|code| code.to_string()).unwrap_or_else(|error| error)
                        ));
                    }
                }
            }
            let process = match open_modifiable_process(pid) {
                Ok(h) => h, Err(e) => { free_local_map(local_module); return Err(e); }
            };
            let remote_path = match write_remote_utf16(process, &canonical_module_path.to_string_lossy()) {
                Ok(p) => p,
                Err(e) => { CloseHandle(process); free_local_map(local_module); return Err(e); }
            };
            let load_library = match remote_system_proc(pid, b"LoadLibraryW\0") {
                Ok(p) => p,
                Err(e) => { VirtualFreeEx(process, remote_path, 0, MEM_RELEASE); CloseHandle(process); free_local_map(local_module); return Err(e); }
            };
            on_phase(
                NativeModTargetState::Loading,
                "Loading the DLL in the target process.".into(),
            );
            let loaded = remote_call(pid, process, load_library, remote_path);
            if loaded.is_ok() { VirtualFreeEx(process, remote_path, 0, MEM_RELEASE); }
            let result = match loaded {
                Ok(code) if code != 0 => Ok(()),
                Ok(_) => Err("LoadLibraryW returned NULL in the target process".into()),
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                CloseHandle(process); free_local_map(local_module); return Err(e);
            }

            let remote_module = match module_for_path(pid, module_path) {
                Ok(Some(module)) => module.mod_base_addr as usize,
                Ok(None) => {
                    CloseHandle(process);
                    free_local_map(local_module);
                    return Err("DLL loaded but was not found in the target module list".into());
                }
                Err(error) => {
                    CloseHandle(process);
                    free_local_map(local_module);
                    return Err(format!("DLL loaded but its target module could not be verified: {error}"));
                }
            };
            on_phase(
                NativeModTargetState::Initializing,
                "DLL loaded; checking its ABI and running initialization.".into(),
            );
            let api_version = remote_call(pid, process, (remote_module + api_rva) as *mut c_void, ptr::null_mut());
            if api_version != Ok(2) {
                // A completed ABI check can be rolled back only through the
                // same shutdown + quiescence protocol as a normal unload.
                let cleanup = if target_operation_is_indeterminate(pid) {
                    Err("target was quarantined after an indeterminate remote call".to_string())
                } else {
                    cleanup_after_failed_init(
                        pid, process, remote_module, shutdown_rva, can_unload_rva
                    )
                };
                CloseHandle(process);
                free_local_map(local_module);
                let cleanup_note = cleanup.err()
                    .map(|error| format!("; DLL may remain mapped: {error}"))
                    .unwrap_or_default();
                return Err(format!(
                    "mod ABI mismatch: expected version 2, got {}{}",
                    api_version.map(|value| value.to_string()).unwrap_or_else(|error| error),
                    cleanup_note
                ));
            }

            let init = remote_call(pid, process, (remote_module + init_rva) as *mut c_void, ptr::null_mut());
            if !matches!(init, Ok(code) if code != 0) {
                // The initializer's thread has completed (unless quarantined),
                // but it may have started work of its own. Shutdown alone is not
                // permission to unmap the DLL: require the same quiescence proof.
                let cleanup = if target_operation_is_indeterminate(pid) {
                    Err("target was quarantined after an indeterminate initializer".to_string())
                } else {
                    cleanup_after_failed_init(
                        pid, process, remote_module, shutdown_rva, can_unload_rva
                    )
                };
                CloseHandle(process);
                free_local_map(local_module);
                let cleanup_note = cleanup.err()
                    .map(|error| format!("; DLL may remain mapped: {error}"))
                    .unwrap_or_default();
                return Err(format!(
                    "WmodInitializeV1 failed: {}{}",
                    init.map(|value| value.to_string()).unwrap_or_else(|error| error),
                    cleanup_note
                ));
            }

            CloseHandle(process);
            free_local_map(local_module);
            on_phase(
                NativeModTargetState::Initialized,
                "ABI validation and mod initialization completed.".into(),
            );
            Ok(())
        }
    }

    pub fn unload(pid: u32, executable: &str, module_path: &Path) -> Result<bool, String> {
        // Keep shutdown and FreeLibrary together; another operation must not race
        // the target between those two stages.
        let _operation = target_operation_gate()
            .lock()
            .map_err(|_| "Native-mod operation gate is unavailable.".to_string())?;
        verify_target_pid(pid, executable)?;
        let Some(module) = module_for_path(pid, module_path)? else { return Ok(false) };
        unsafe {
            let local_module = local_map_module(module_path)?;
            let shutdown_rva = exported_rva(local_module, b"WmodShutdownV1\0")?;
            let can_unload_rva = exported_rva(local_module, b"WmodCanUnloadV1\0")?;
            let base = module.mod_base_addr as usize;
            let process = open_modifiable_process(pid)?;
            let shutdown_result = remote_call(pid, process,
                (base + shutdown_rva) as *mut c_void, ptr::null_mut());
            if let Err(error) = shutdown_result {
                CloseHandle(process);
                free_local_map(local_module);
                return Err(format!("WmodShutdownV1 failed; DLL was left mapped: {error}"));
            }

            // Only unmap after the target proves no thread instruction pointer
            // or detectable stack return address can enter the DLL image.
            if let Err(error) = wait_for_safe_unload(
                pid, process, base, can_unload_rva, std::time::Duration::from_secs(3)
            ) {
                CloseHandle(process);
                free_local_map(local_module);
                return Err(error);
            }

            let free_library = match remote_system_proc(pid, b"FreeLibrary\0") {
                Ok(pointer) => pointer,
                Err(error) => {
                    CloseHandle(process);
                    free_local_map(local_module);
                    return Err(error);
                }
            };
            let free_result = remote_call(pid, process, free_library, base as *mut c_void);
            CloseHandle(process);
            free_local_map(local_module);
            match free_result {
                Ok(code) if code != 0 => Ok(true),
                Ok(_) => Err("FreeLibrary returned FALSE in the target process".into()),
                Err(error) => Err(format!("could not unload native mod: {error}")),
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
        assert!(!valid_relative_path("bin/CON.dll"));
        assert!(!valid_relative_path("bin/NUL.txt"));
        assert!(!valid_relative_path("bin/COM1.log"));
        assert!(!valid_relative_path("bin/trailing."));
        assert!(!valid_relative_path("bin/trailing "));
        assert!(!valid_relative_path("bin/bad?.dll"));
        let control_path = format!("bin/line{}break.dll", char::from_u32(0x1f).unwrap());
        assert!(!valid_relative_path(&control_path));

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
        assert!(register_windows_zip_path(&mut HashSet::new(), "CON.txt")
            .unwrap_err().contains("unsafe"));
        assert!(register_windows_zip_path(&mut HashSet::new(), "file.")
            .unwrap_err().contains("unsafe"));
        assert!(register_windows_zip_path(&mut HashSet::new(), "folder//")
            .unwrap_err().contains("unsafe"));
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

    #[cfg(unix)]
    #[test]
    fn native_mod_extraction_rejects_symlinked_destination() {
        use std::io::Write;
        use std::os::unix::fs::symlink;
        use zip::write::FileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let root = test_root("extract-symlink-destination");
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("valid.zip");
        let options = FileOptions::default()
            .compression_method(CompressionMethod::Stored);
        {
            let file = File::create(&archive_path).unwrap();
            let mut writer = ZipWriter::new(file);
            writer.start_file("payload.txt", options).unwrap();
            writer.write_all(b"must not escape the staging directory").unwrap();
            writer.finish().unwrap();
        }

        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let destination = root.join("stage");
        symlink(&outside, &destination).unwrap();
        let result = extract_package(&archive_path, &destination);
        assert!(result.unwrap_err().contains("staging directory"));
        assert!(!outside.join("payload.txt").exists());
        assert!(fs::symlink_metadata(&destination).unwrap().file_type().is_symlink());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mod_extraction_rejects_windows_reserved_paths_independently() {
        use std::io::Write;
        use zip::write::FileOptions;
        use zip::{CompressionMethod, ZipWriter};

        let root = test_root("extract-reserved-path");
        fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("changed-after-validation.zip");
        let options = FileOptions::default()
            .compression_method(CompressionMethod::Stored);
        {
            let file = File::create(&archive_path).unwrap();
            let mut writer = ZipWriter::new(file);
            writer.start_file("CON.txt", options).unwrap();
            writer.write_all(b"must not extract").unwrap();
            writer.finish().unwrap();
        }

        let destination = root.join("stage");
        let result = extract_package(&archive_path, &destination);
        assert!(result.unwrap_err().contains("unsafe or Windows-ambiguous"));
        assert!(!destination.join("CON.txt").exists());

        fs::remove_dir_all(root).unwrap();
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

    fn test_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "00047-native-mod-paths-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn native_mod_package_directory_resolves_inside_managed_root() {
        let root = test_root("package-dir");
        let package = root.join("packages").join("com.example.test");
        fs::create_dir_all(&package).unwrap();
        let manager = NativeModManager::new(root.clone());

        let resolved = manager.package_dir("com.example.test").unwrap();
        assert_eq!(resolved, package.canonicalize().unwrap());
        assert!(manager.package_dir("../outside").is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mod_module_path_must_be_a_safe_file_inside_package() {
        let root = test_root("module-path");
        let package = root.join("packages").join("com.example.test");
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::write(package.join("bin/mod.dll"), b"fixture").unwrap();

        let resolved = module_file_path(&package, "bin/mod.dll").unwrap();
        assert_eq!(resolved, package.join("bin/mod.dll").canonicalize().unwrap());
        assert!(module_file_path(&package, "../outside.dll").is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_mod_package_root_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let root = test_root("package-root-symlink");
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("packages")).unwrap();

        let manager = NativeModManager::new(root.clone());
        assert!(manager.package_dir("com.example.test").unwrap_err().contains("symlink"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_mod_package_directory_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let root = test_root("package-symlink");
        let packages = root.join("packages");
        let outside = root.join("outside");
        fs::create_dir_all(&packages).unwrap();
        fs::create_dir_all(&outside).unwrap();
        symlink(&outside, packages.join("com.example.link")).unwrap();

        let manager = NativeModManager::new(root.clone());
        assert!(manager.package_dir("com.example.link").unwrap_err().contains("symlink"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_mod_module_path_rejects_symlinked_parent_escape() {
        use std::os::unix::fs::symlink;

        let root = test_root("module-parent-symlink");
        let package = root.join("packages").join("com.example.test");
        let outside = root.join("outside");
        fs::create_dir_all(&package).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("mod.dll"), b"fixture").unwrap();
        symlink(&outside, package.join("bin")).unwrap();

        assert!(module_file_path(&package, "bin/mod.dll").unwrap_err().contains("escapes"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_mod_manifest_file_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let root = test_root("manifest-symlink");
        let package = root.join("packages").join("com.example.test");
        fs::create_dir_all(&package).unwrap();
        let outside = root.join("outside.json");
        fs::write(&outside, b"{}").unwrap();
        symlink(&outside, package.join("manifest.json")).unwrap();

        assert!(load_manifest(&package).unwrap_err().contains("symlink"));

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_mod_module_file_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let root = test_root("module-symlink");
        let package = root.join("packages").join("com.example.test");
        fs::create_dir_all(package.join("bin")).unwrap();
        let outside = root.join("outside.dll");
        fs::write(&outside, b"fixture").unwrap();
        symlink(&outside, package.join("bin/mod.dll")).unwrap();

        assert!(module_file_path(&package, "bin/mod.dll").unwrap_err().contains("symlink"));

        fs::remove_dir_all(root).unwrap();
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
        state.require_trusted_signature = true;
        state.pending_signature_unloads.insert("com.example.pending".into());
        manager.persist_state(&state).unwrap();

        let path = root.join("enabled.json");
        let first_load = load_persisted_state(&path);
        assert!(first_load.enabled.contains("com.example.first"));
        assert!(first_load.require_trusted_signature);
        assert!(first_load.pending_signature_unloads.contains("com.example.pending"));

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
    fn disabling_signature_policy_does_not_forget_pending_safe_unload() {
        let root = state_test_root("pending-cleanup-policy-off");
        let manager = NativeModManager::new(root.clone());
        {
            let mut state = manager.state.lock().unwrap();
            state.require_trusted_signature = true;
            state.pending_signature_unloads.insert("com.example.pending".into());
            manager.persist_state(&state).unwrap();
        }

        manager.set_require_trusted_signature(false).unwrap();

        let state = load_persisted_state(&root.join("enabled.json"));
        assert!(!state.require_trusted_signature);
        assert!(!state.enabled.contains("com.example.pending"));
        assert!(state.pending_signature_unloads.contains("com.example.pending"));
        assert!(manager.pending_signature_unloads_snapshot().contains("com.example.pending"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mod_pending_signature_cleanup_is_durable_until_cleared() {
        let root = state_test_root("pending-signature-cleanup");
        let manager = NativeModManager::new(root.clone());
        {
            let mut state = manager.state.lock().unwrap();
            state.require_trusted_signature = true;
            state.pending_signature_unloads.insert("com.example.pending".into());
            manager.persist_state(&state).unwrap();
        }

        assert!(manager.pending_signature_unloads_snapshot().contains("com.example.pending"));
        assert!(load_persisted_state(&root.join("enabled.json"))
            .pending_signature_unloads.contains("com.example.pending"));

        manager.clear_pending_signature_unload("com.example.pending").unwrap();
        assert!(!manager.pending_signature_unloads_snapshot().contains("com.example.pending"));
        assert!(!load_persisted_state(&root.join("enabled.json"))
            .pending_signature_unloads.contains("com.example.pending"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mod_safe_unload_queue_is_durable_until_cleared() {
        let root = state_test_root("pending-safe-unload");
        let manager = NativeModManager::new(root.clone());

        assert!(manager.mark_pending_safe_unload("../invalid").is_err());
        manager.mark_pending_safe_unload("com.example.pending").unwrap();
        assert!(manager.pending_safe_unloads_snapshot().contains("com.example.pending"));
        assert!(load_persisted_state(&root.join("enabled.json"))
            .pending_safe_unloads.contains("com.example.pending"));

        // New state fields must preserve signature-specific cleanup markers too.
        {
            let mut state = manager.state.lock().unwrap();
            state.pending_signature_unloads.insert("com.example.signature-pending".into());
            manager.persist_state(&state).unwrap();
        }
        let reloaded = load_persisted_state(&root.join("enabled.json"));
        assert!(reloaded.pending_safe_unloads.contains("com.example.pending"));
        assert!(reloaded.pending_signature_unloads.contains("com.example.signature-pending"));

        manager.clear_pending_safe_unload("com.example.pending").unwrap();
        assert!(!manager.pending_safe_unloads_snapshot().contains("com.example.pending"));
        assert!(!load_persisted_state(&root.join("enabled.json"))
            .pending_safe_unloads.contains("com.example.pending"));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mod_signature_policy_defaults_off_for_legacy_state_files() {
        let root = state_test_root("legacy-signature-policy");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("enabled.json");
        fs::write(&path, r#"{"enabled":["com.example.valid"]}"#).unwrap();

        let state = load_persisted_state(&path);
        assert!(state.enabled.contains("com.example.valid"));
        assert!(!state.require_trusted_signature);
        assert!(state.pending_signature_unloads.is_empty());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_mod_signature_policy_persists_without_installed_mods() {
        let root = state_test_root("signature-policy-toggle");
        let manager = NativeModManager::new(root.clone());
        assert!(!manager.list().require_trusted_signature);

        let enabled = manager.set_require_trusted_signature(true).unwrap();
        assert!(enabled.contains("ON"));
        assert!(manager.list().require_trusted_signature);
        assert!(load_persisted_state(&root.join("enabled.json")).require_trusted_signature);

        let disabled = manager.set_require_trusted_signature(false).unwrap();
        assert!(disabled.contains("OFF"));
        assert!(!manager.list().require_trusted_signature);
        assert!(!load_persisted_state(&root.join("enabled.json")).require_trusted_signature);

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

        // A corrupt/oversized regular file can be repaired safely by replacement.
        atomic_write_enabled_state(&root, br#"{\"enabled\":[]}"#).unwrap();
        assert!(load_persisted_state(&path).enabled.is_empty());
        assert!(fs::metadata(&path).unwrap().len() < MAX_ENABLED_STATE_BYTES);

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
        let result = atomic_write_enabled_state(&root, br#"{"enabled":[]}"#);
        assert!(result.unwrap_err().contains("symlink"));
        assert_eq!(fs::read(&outside).unwrap(), b"do not overwrite");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lifecycle_rollback_marks_successes_and_preserves_target_failures() {
        let previous = vec![
            NativeModTargetStatus {
                pid: 10,
                executable: "explorer.exe".into(),
                state: NativeModTargetState::Initialized,
                detail: "initialized".into(),
            },
            NativeModTargetStatus {
                pid: 11,
                executable: "SearchHost.exe".into(),
                state: NativeModTargetState::Failed,
                detail: "initialization failed".into(),
            },
            NativeModTargetStatus {
                pid: 12,
                executable: "ShellExperienceHost.exe".into(),
                state: NativeModTargetState::Indeterminate,
                detail: "remote call timed out".into(),
            },
        ];
        let rollback = vec![
            NativeModTargetStatus {
                pid: 10,
                executable: "explorer.exe".into(),
                state: NativeModTargetState::Unloaded,
                detail: "unloaded".into(),
            },
            NativeModTargetStatus {
                pid: 11,
                executable: "SearchHost.exe".into(),
                state: NativeModTargetState::NotLoaded,
                detail: "not loaded".into(),
            },
            NativeModTargetStatus {
                pid: 12,
                executable: "ShellExperienceHost.exe".into(),
                state: NativeModTargetState::Indeterminate,
                detail: "still quarantined".into(),
            },
        ];

        let merged = merge_rollback_statuses(&previous, &rollback);
        assert_eq!(merged[0].state, NativeModTargetState::RolledBack);
        assert!(merged[0].detail.contains("rolled back"));
        assert_eq!(merged[1].state, NativeModTargetState::Failed);
        assert_eq!(merged[1].detail, "initialization failed");
        assert_eq!(merged[2].state, NativeModTargetState::Indeterminate);
        assert_eq!(merged[2].detail, "still quarantined");
    }

    #[test]
    fn native_mod_reload_pending_survives_a_quarantined_target_scan() {
        let previous = vec![
            NativeModTargetStatus {
                pid: 42,
                executable: "explorer.exe".into(),
                state: NativeModTargetState::ReloadPending,
                detail: "reload pending: quiescence check timed out".into(),
            },
            NativeModTargetStatus {
                pid: 43,
                executable: "SearchHost.exe".into(),
                state: NativeModTargetState::Indeterminate,
                detail: "reload pending: PID quarantined until exit".into(),
            },
            NativeModTargetStatus {
                pid: 44,
                executable: "ShellExperienceHost.exe".into(),
                state: NativeModTargetState::Indeterminate,
                detail: "remote initializer timed out".into(),
            },
        ];

        assert!(target_has_pending_reload(&previous, 42));
        assert!(target_has_pending_reload(&previous, 43));
        assert!(!target_has_pending_reload(&previous, 44));
        assert!(!target_has_pending_reload(&previous, 99));
        assert!(should_defer_reload_failure("reload pending: retry later", false));
        assert!(should_defer_reload_failure("PID quarantined until exit", true));
        assert!(!should_defer_reload_failure("native mod initialization failed", false));
    }

    #[test]
    fn native_mod_target_lifecycle_states_serialize_as_stable_lower_camel_case() {
        for (state, expected) in [
            (NativeModTargetState::Detected, "\"detected\""),
            (NativeModTargetState::Loading, "\"loading\""),
            (NativeModTargetState::Initializing, "\"initializing\""),
            (NativeModTargetState::Initialized, "\"initialized\""),
            (NativeModTargetState::Stopping, "\"stopping\""),
            (NativeModTargetState::Indeterminate, "\"indeterminate\""),
            (NativeModTargetState::NotLoaded, "\"notLoaded\""),
        ] {
            assert_eq!(serde_json::to_string(&state).unwrap(), expected);
        }
    }

    #[test]
    fn native_mod_manifest_requires_the_module_file_in_package() {
        let m = manifest(vec!["explorer.exe"], "bin/mod.dll");
        let files = HashSet::from(["manifest.json".to_string()]);
        assert!(validate_manifest(&m, Some(&files)).unwrap_err().contains("missing from the ZIP"));
    }
}
