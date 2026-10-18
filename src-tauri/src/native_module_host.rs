//! In-process native extensions for 00047.
//!
//! This C ABI loads a trusted DLL into the 00047 process. It is deliberately
//! distinct from external-process extensions and automation block packs.
//! Native module code is not sandboxed and can crash or corrupt the host.

use libloading::Library;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::{c_char, CStr},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter};

const NATIVE_ABI_VERSION: u32 = 1;
const ENTRYPOINT_SYMBOL: &[u8] = b"extension_00047_get_api_v1\0";

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[repr(C)]
pub struct HostApiV1 {
    pub abi_version: u32,
    pub struct_size: u32,
    /// Opaque pointer-sized context owned by the host. Pass it unchanged to callbacks.
    pub context: usize,
    pub log: Option<unsafe extern "C" fn(context: usize, level: u32, message: *const c_char)>,
}

pub type NativeStartV1 = unsafe extern "C" fn(host: *const HostApiV1, user_data: *mut usize) -> i32;
pub type NativeStopV1 = unsafe extern "C" fn(host: *const HostApiV1, user_data: usize) -> i32;

#[repr(C)]
pub struct NativeModuleApiV1 {
    pub abi_version: u32,
    pub struct_size: u32,
    pub id: *const c_char,
    pub name: *const c_char,
    pub version: *const c_char,
    pub description: *const c_char,
    pub start: Option<NativeStartV1>,
    pub stop: Option<NativeStopV1>,
}

type GetNativeModuleApiV1 = unsafe extern "C" fn(requested_abi: u32) -> *const NativeModuleApiV1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModuleMetadata {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    pub file_name: String,
    #[serde(default)]
    pub auto_start: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModuleStatus {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub path: String,
    pub auto_start: bool,
    pub state: String,
    pub last_error: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeModuleCatalog {
    pub root: String,
    pub modules: Vec<NativeModuleStatus>,
    pub errors: Vec<String>,
}

struct HostContext {
    app: AppHandle,
    id: String,
}

struct NativeModuleRecord {
    status: NativeModuleStatus,
    _library: Library,
    host: Box<HostApiV1>,
    _context: Box<HostContext>,
    stop: NativeStopV1,
    user_data: usize,
}

pub struct NativeModuleManager {
    root: PathBuf,
    app: AppHandle,
    state: Mutex<HashMap<String, NativeModuleRecord>>,
    errors: Mutex<Vec<String>>,
}

impl NativeModuleManager {
    pub fn new(root: PathBuf, app: AppHandle) -> Self {
        let _ = fs::create_dir_all(&root);
        Self { root, app, state: Mutex::new(HashMap::new()), errors: Mutex::new(Vec::new()) }
    }

    pub fn root(&self) -> &Path { &self.root }

    fn dll_path(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) { return Err("invalid native module id".into()); }
        Ok(self.root.join(format!("{id}.dll")))
    }

    fn metadata_path(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) { return Err("invalid native module id".into()); }
        Ok(self.root.join(format!("{id}.json")))
    }

    fn disabled_path(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) { return Err("invalid native module id".into()); }
        Ok(self.root.join(format!("{id}.disabled")))
    }

    fn emit_status(&self, status: &NativeModuleStatus) {
        let _ = self.app.emit("native-module-state", status);
    }

    fn log(&self, id: &str, level: &str, message: &str) {
        let _ = self.app.emit("native-module-log", serde_json::json!({
            "id": id, "level": level, "message": message, "ts": now_ms()
        }));
    }

    fn record_error(&self, message: String) {
        let mut errors = self.errors.lock().unwrap();
        errors.push(message);
        if errors.len() > 100 {
            let excess = errors.len() - 100;
            errors.drain(0..excess);
        }
    }

    pub fn catalog(&self) -> NativeModuleCatalog {
        let mut modules = Vec::new();
        let mut errors = self.errors.lock().unwrap().clone();
        if let Ok(entries) = fs::read_dir(&self.root) {
            let mut manifests: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json")).collect();
            manifests.sort();
            for path in manifests {
                let parsed = fs::read(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| serde_json::from_slice::<NativeModuleMetadata>(&bytes).map_err(|e| e.to_string()));
                let meta = match parsed {
                    Ok(m) => m,
                    Err(e) => { errors.push(format!("{}: invalid metadata: {e}", path.display())); continue; }
                };
                if !valid_id(&meta.id) || meta.file_name != format!("{}.dll", meta.id) || path != self.root.join(format!("{}.json", meta.id)) {
                    errors.push(format!("{}: metadata id/path mismatch", path.display()));
                    continue;
                }
                let dll = self.dll_path(&meta.id).unwrap();
                if !dll.is_file() {
                    errors.push(format!("{}: native DLL is missing", meta.id));
                    continue;
                }
                let status = if let Some(record) = self.state.lock().unwrap().get(&meta.id) {
                    record.status.clone()
                } else {
                    NativeModuleStatus {
                        id: meta.id.clone(), name: meta.name.clone(), version: meta.version.clone(),
                        description: meta.description.clone(), path: dll.to_string_lossy().to_string(),
                        auto_start: meta.auto_start,
                        state: if self.disabled_path(&meta.id).map(|p| p.exists()).unwrap_or(false) { "unloaded" } else { "stopped" }.into(),
                        last_error: None,
                    }
                };
                modules.push(status);
            }
        }
        modules.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        NativeModuleCatalog { root: self.root.to_string_lossy().to_string(), modules, errors }
    }

    /// Load a user-selected DLL, copy it into the managed folder, and start it.
    /// Loading the module executes native code in this process; call only after UI confirmation.
    pub fn install_and_start(&self, source: &Path) -> Result<NativeModuleStatus, String> {
        #[cfg(not(windows))]
        { let _ = source; return Err("in-process native modules are supported only on Windows".into()); }

        let source = fs::canonicalize(source).map_err(|e| format!("could not access DLL: {e}"))?;
        if source.extension().and_then(|x| x.to_str()).map(|x| !x.eq_ignore_ascii_case("dll")).unwrap_or(true) {
            return Err("select a Windows .dll native module".into());
        }
        let size = fs::metadata(&source).map_err(|e| e.to_string())?.len();
        if size == 0 || size > 100 * 1024 * 1024 {
            return Err("native module must be between 1 byte and 100 MB".into());
        }

        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let stage = self.root.join(format!(".incoming-{}-{}.dll", std::process::id(), now_ms()));
        fs::copy(&source, &stage).map_err(|e| format!("copy native module: {e}"))?;
        let probe = match inspect_module(&stage) {
            Ok(m) => m,
            Err(e) => { let _ = fs::remove_file(&stage); return Err(e); }
        };
        if self.state.lock().unwrap().contains_key(&probe.id) {
            let _ = fs::remove_file(&stage);
            return Err("this native module is already loaded".into());
        }
        let target = self.dll_path(&probe.id)?;
        if target.exists() {
            let _ = fs::remove_file(&stage);
            return Err("a module with this id is already installed; load it from the manager or uninstall it first".into());
        }
        if let Err(e) = fs::rename(&stage, &target) {
            let _ = fs::remove_file(&stage);
            return Err(format!("could not install native module: {e}"));
        }

        match self.load_installed_module(&probe.id) {
            Ok(status) => Ok(status),
            Err(e) => {
                let _ = fs::remove_file(&target);
                let _ = fs::remove_file(self.metadata_path(&probe.id).unwrap());
                self.record_error(format!("{}: {e}", probe.id));
                Err(e)
            }
        }
    }

    fn load_installed_module(&self, id: &str) -> Result<NativeModuleStatus, String> {
        #[cfg(not(windows))]
        { let _ = id; return Err("in-process native modules are supported only on Windows".into()); }

        let path = self.dll_path(id)?;
        let meta_path = self.metadata_path(id)?;
        let metadata = match fs::read(&meta_path) {
            Ok(bytes) => serde_json::from_slice::<NativeModuleMetadata>(&bytes).ok(),
            Err(_) => None,
        };
        // Initial installation has no sidecar yet, so inspect the DLL to obtain metadata.
        let probe = inspect_module(&path)?;
        if probe.id != id { return Err("native module id does not match installed filename".into()); }
        let mut metadata = metadata.unwrap_or_else(|| probe.clone());
        if metadata.file_name.is_empty() {
            // First load: the descriptor does not yet have a sidecar manifest.
            metadata.file_name = format!("{id}.dll");
        }
        if metadata.id != id || metadata.file_name != format!("{id}.dll") {
            return Err("native module metadata does not match its installed filename".into());
        }
        let metadata_bytes = serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?;
        fs::write(&meta_path, metadata_bytes).map_err(|e| format!("write native module metadata: {e}"))?;

        let context = Box::new(HostContext { app: self.app.clone(), id: probe.id.clone() });
        // Keep the host table and callback context at stable heap addresses for
        // the entire loaded-module lifetime. Extensions may retain these pointers
        // until their stop callback returns.
        let host = Box::new(HostApiV1 {
            abi_version: NATIVE_ABI_VERSION,
            struct_size: std::mem::size_of::<HostApiV1>() as u32,
            context: (&*context as *const HostContext) as usize,
            log: Some(native_log),
        });
        let (library, metadata_live, start, stop) = open_module(&path)?;
        if metadata_live.id != probe.id || metadata_live.name != probe.name || metadata_live.version != probe.version {
            return Err("native module metadata changed while loading".into());
        }
        let mut user_data = 0usize;
        let rc = unsafe { start(&*host, &mut user_data) };
        if rc != 0 {
            // Give the module a chance to undo partial initialization before unloading it.
            unsafe { let _ = stop(&*host, user_data); }
            return Err(format!("native module start returned error code {rc}"));
        }
        let status = NativeModuleStatus {
            id: metadata.id.clone(), name: metadata.name.clone(), version: metadata.version.clone(),
            description: metadata.description.clone(), path: path.to_string_lossy().to_string(),
            auto_start: metadata.auto_start, state: "running".into(), last_error: None,
        };
        let _ = fs::remove_file(self.disabled_path(id)?);
        let record = NativeModuleRecord { status: status.clone(), _library: library, host, _context: context, stop, user_data };
        self.state.lock().unwrap().insert(id.to_string(), record);
        self.emit_status(&status);
        self.log(id, "info", &format!("Loaded native module {} v{}", status.name, status.version));
        Ok(status)
    }

    pub fn start(&self, id: &str) -> Result<NativeModuleStatus, String> {
        if self.state.lock().unwrap().contains_key(id) { return Err("native module is already running".into()); }
        if !self.metadata_path(id)?.is_file() || !self.dll_path(id)?.is_file() {
            return Err("native module is not installed".into());
        }
        self.load_installed_module(id)
    }

    pub fn unload(&self, id: &str) -> Result<(), String> {
        let mut record = {
            self.state.lock().unwrap().remove(id).ok_or("native module is not running")?
        };
        let rc = unsafe { (record.stop)(&*record.host, record.user_data) };
        if rc != 0 {
            record.status.state = "failed".into();
            record.status.last_error = Some(format!("native module stop returned error code {rc}; module remains loaded"));
            self.state.lock().unwrap().insert(id.to_string(), record);
            let status = self.state.lock().unwrap().get(id).map(|r| r.status.clone());
            if let Some(status) = status { self.emit_status(&status); }
            return Err(format!("native module stop returned error code {rc}; it was kept loaded to avoid unloading active native code"));
        }
        let mut status = record.status.clone();
        status.state = "unloaded".into();
        drop(record);
        fs::write(self.disabled_path(id)?, b"").map_err(|e| format!("module was unloaded but its disabled marker could not be written: {e}"))?;
        self.emit_status(&status);
        self.log(id, "info", "Unloaded native module");
        Ok(())
    }

    pub fn uninstall(&self, id: &str) -> Result<(), String> {
        if self.state.lock().unwrap().contains_key(id) { self.unload(id)?; }
        let dll = self.dll_path(id)?;
        let meta = self.metadata_path(id)?;
        let disabled = self.disabled_path(id)?;
        if !dll.is_file() { return Err("native module is not installed".into()); }
        fs::remove_file(&dll).map_err(|e| format!("remove native module DLL: {e}"))?;
        let _ = fs::remove_file(meta);
        let _ = fs::remove_file(disabled);
        self.log(id, "info", "Uninstalled native module");
        Ok(())
    }

    pub fn set_auto_start(&self, id: &str, enabled: bool) -> Result<(), String> {
        let path = self.metadata_path(id)?;
        let mut metadata: NativeModuleMetadata = serde_json::from_slice(&fs::read(&path).map_err(|e| e.to_string())?)
            .map_err(|e| format!("invalid native module metadata: {e}"))?;
        metadata.auto_start = enabled;
        fs::write(&path, serde_json::to_vec_pretty(&metadata).map_err(|e| e.to_string())?)
            .map_err(|e| format!("write native module settings: {e}"))?;
        if enabled {
            let _ = fs::remove_file(self.disabled_path(id)?);
        }
        let status = {
            let mut states = self.state.lock().unwrap();
            if let Some(record) = states.get_mut(id) {
                record.status.auto_start = enabled;
                Some(record.status.clone())
            } else { None }
        };
        if let Some(status) = status { self.emit_status(&status); }
        Ok(())
    }

    pub fn start_autostart(&self) {
        let candidates: Vec<String> = self.catalog().modules.into_iter()
            .filter(|m| m.auto_start && m.state != "unloaded")
            .map(|m| m.id).collect();
        for id in candidates {
            if let Err(e) = self.start(&id) {
                let msg = format!("{id}: autostart failed: {e}");
                self.record_error(msg.clone());
                self.log(&id, "error", &msg);
            }
        }
    }

    pub fn shutdown(&self) {
        let ids: Vec<String> = self.state.lock().unwrap().keys().cloned().collect();
        for id in ids {
            let record = self.state.lock().unwrap().remove(&id);
            if let Some(record) = record {
                let rc = unsafe { (record.stop)(&*record.host, record.user_data) };
                if rc != 0 {
                    self.log(&id, "error", &format!("native module stop returned {rc} during shutdown"));
                } else {
                    self.log(&id, "info", "Native module stopped during shutdown");
                }
                drop(record);
            }
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && !id.starts_with('.') && !id.ends_with('.')
        && !id.contains("..")
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_')
}

fn read_api_string(value: *const c_char, label: &str, max_len: usize, optional: bool) -> Result<String, String> {
    if value.is_null() {
        return if optional { Ok(String::new()) } else { Err(format!("native module descriptor has no {label}")) };
    }
    let text = unsafe { CStr::from_ptr(value) }.to_str()
        .map_err(|_| format!("native module {label} is not valid UTF-8"))?;
    if text.len() > max_len { return Err(format!("native module {label} exceeds {max_len} bytes")); }
    Ok(text.to_string())
}

fn read_descriptor(library: &Library) -> Result<(NativeModuleMetadata, NativeStartV1, NativeStopV1), String> {
    let get_api: libloading::Symbol<GetNativeModuleApiV1> = unsafe {
        library.get(ENTRYPOINT_SYMBOL).map_err(|e| format!("missing extension_00047_get_api_v1 export: {e}"))?
    };
    let api_ptr = unsafe { get_api(NATIVE_ABI_VERSION) };
    if api_ptr.is_null() { return Err("native module does not support ABI v1".into()); }
    let api = unsafe { &*api_ptr };
    if api.abi_version != NATIVE_ABI_VERSION { return Err(format!("native module ABI {} is not supported", api.abi_version)); }
    if (api.struct_size as usize) < std::mem::size_of::<NativeModuleApiV1>() {
        return Err("native module API descriptor is smaller than ABI v1 requires".into());
    }
    let id = read_api_string(api.id, "id", 100, false)?;
    if !valid_id(&id) { return Err("native module id must use lowercase a-z, digits, dots, dashes or underscores".into()); }
    let name = read_api_string(api.name, "name", 100, false)?;
    if name.trim().is_empty() { return Err("native module name is required".into()); }
    let version = read_api_string(api.version, "version", 40, false)?;
    if version.trim().is_empty() { return Err("native module version is required".into()); }
    let description = read_api_string(api.description, "description", 1000, true)?;
    let start = api.start.ok_or("native module has no start callback")?;
    let stop = api.stop.ok_or("native module has no stop callback")?;
    Ok((NativeModuleMetadata { id, name, version, description, file_name: String::new(), auto_start: false }, start, stop))
}

fn inspect_module(path: &Path) -> Result<NativeModuleMetadata, String> {
    let library = unsafe { Library::new(path) }.map_err(|e| format!("could not load native module for inspection: {e}"))?;
    let (metadata, _, _) = read_descriptor(&library)?;
    drop(library);
    Ok(metadata)
}

fn open_module(path: &Path) -> Result<(Library, NativeModuleMetadata, NativeStartV1, NativeStopV1), String> {
    let library = unsafe { Library::new(path) }.map_err(|e| format!("could not load native module: {e}"))?;
    let (metadata, start, stop) = read_descriptor(&library)?;
    Ok((library, metadata, start, stop))
}

unsafe extern "C" fn native_log(context: usize, level: u32, message: *const c_char) {
    if context == 0 || message.is_null() { return; }
    let ctx = &*(context as *const HostContext);
    let message = match CStr::from_ptr(message).to_str() {
        Ok(s) => s,
        Err(_) => "<native module log was not valid UTF-8>",
    };
    let level = match level { 0 => "debug", 1 => "info", 2 => "warn", _ => "error" };
    let _ = ctx.app.emit("native-module-log", serde_json::json!({
        "id": ctx.id, "level": level, "message": message, "ts": now_ms()
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_native_module_ids() {
        assert!(valid_id("org.example.taskbar"));
        assert!(!valid_id("../taskbar"));
        assert!(!valid_id("UpperCase"));
        assert!(!valid_id("org..bad"));
    }

    #[test]
    fn native_manifest_string_sizes_are_bounded() {
        assert!(read_api_string(std::ptr::null(), "id", 100, false).is_err());
        assert_eq!(read_api_string(std::ptr::null(), "description", 1000, true).unwrap(), "");
    }
}
