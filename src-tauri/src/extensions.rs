//! Independent extension packages for 00047.
//!
//! Unlike workflow block packs, these components are managed directly by the app.
//! They can launch applications, long-running services, or bounded one-shot commands.
//! Extension code is not sandboxed and runs with the current user's permissions.

use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Mutex,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    task::JoinHandle,
    time::{timeout, Duration, Instant},
};
use zip::ZipArchive;

const MAX_ZIP_BYTES: u64 = 20 * 1024 * 1024;
const MAX_FILES: usize = 500;
const MAX_UNPACKED_BYTES: u64 = 50 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const DEFAULT_COMMAND_TIMEOUT_MS: u64 = 15_000;
const MIN_COMMAND_TIMEOUT_MS: u64 = 100;
const MAX_COMMAND_TIMEOUT_MS: u64 = 120_000;
const MAX_COMMAND_OUTPUT_BYTES: usize = 256 * 1024;
const OUTPUT_READER_GRACE_MS: u64 = 2_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    manifest_version: u64,
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    description: String,
    components: Vec<Component>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Component {
    id: String,
    name: String,
    kind: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    entrypoint: Option<String>,
    runtime: RuntimeSpec,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeSpec {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    working_directory: Option<String>,
    #[serde(default)]
    environment: HashMap<String, String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackagePreview {
    pub file: String,
    pub path: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub components: Vec<ComponentPreview>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentPreview {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub command: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCommandResult {
    pub package_id: String,
    pub component_id: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub output_truncated: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionList {
    pub dir: String,
    pub packages: Vec<InstalledPackage>,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledPackage {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub components: Vec<InstalledComponent>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledComponent {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub description: String,
    pub command: String,
    pub running: bool,
    pub status: String,
}

struct RunningProcess {
    child: Child,
}

pub struct ExtensionManager {
    root: PathBuf,
    children: Mutex<HashMap<String, RunningProcess>>,
}

struct ReadPackage {
    manifest: Manifest,
}

impl ExtensionManager {
    pub fn new(root: PathBuf) -> Self {
        let _ = fs::create_dir_all(root.join("packages"));
        Self { root, children: Mutex::new(HashMap::new()) }
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    fn packages_dir(&self) -> PathBuf {
        self.root.join("packages")
    }

    fn package_dir(&self, id: &str) -> Result<PathBuf, String> {
        if !valid_id(id) {
            return Err("Invalid extension package ID.".into());
        }
        Ok(self.packages_dir().join(id))
    }

    pub fn list(&self) -> ExtensionList {
        let running = self.refresh_running();
        let mut packages = Vec::new();
        let mut errors = Vec::new();
        let entries = match fs::read_dir(self.packages_dir()) {
            Ok(entries) => entries,
            Err(e) => {
                errors.push(format!("Cannot read extension directory: {e}"));
                return ExtensionList { dir: self.root.to_string_lossy().to_string(), packages, errors };
            }
        };
        let mut folders: Vec<PathBuf> = entries.filter_map(Result::ok).map(|e| e.path())
            .filter(|p| p.is_dir() && p.file_name().and_then(|n| n.to_str()).map(|s| !s.starts_with('.')).unwrap_or(false))
            .collect();
        folders.sort();

        for folder in folders {
            let folder_name = folder.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let manifest = match load_manifest(&folder) {
                Ok(m) => m,
                Err(e) => { errors.push(format!("{folder_name}: {e}")); continue; }
            };
            if manifest.id != folder_name {
                errors.push(format!("{folder_name}: package ID does not match its folder"));
                continue;
            }
            let mut components = Vec::new();
            for component in &manifest.components {
                if let Some(entrypoint) = &component.entrypoint {
                    if !valid_relative_path(entrypoint) || !folder.join(entrypoint).is_file() {
                        errors.push(format!("{folder_name}/{}: entrypoint is missing or unsafe", component.id));
                        continue;
                    }
                }
                let key = process_key(&manifest.id, &component.id);
                let is_running = running.contains(&key);
                components.push(InstalledComponent {
                    id: component.id.clone(),
                    name: component.name.clone(),
                    kind: component.kind.clone(),
                    description: component.description.clone(),
                    command: command_preview(component, &folder),
                    running: is_running,
                    status: if component.kind == "command" {
                        "Run on demand".into()
                    } else if is_running {
                        "Running".into()
                    } else {
                        "Stopped".into()
                    },
                });
            }
            packages.push(InstalledPackage {
                id: manifest.id,
                name: manifest.name,
                version: manifest.version,
                description: manifest.description,
                components,
            });
        }
        ExtensionList { dir: self.root.to_string_lossy().to_string(), packages, errors }
    }

    pub fn preview(&self, path: &Path) -> Result<PackagePreview, String> {
        let read = read_package(path)?;
        let manifest = read.manifest;
        let components = manifest.components.iter().map(|c| ComponentPreview {
            id: c.id.clone(),
            name: c.name.clone(),
            kind: c.kind.clone(),
            description: c.description.clone(),
            command: command_preview(c, Path::new("<install-dir>")),
        }).collect();
        Ok(PackagePreview {
            file: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            path: path.to_string_lossy().to_string(),
            id: manifest.id,
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            components,
        })
    }

    pub fn install(&self, path: &Path) -> Result<String, String> {
        let read = read_package(path)?;
        let id = read.manifest.id.clone();
        let running = self.refresh_running();
        if running.iter().any(|key| key.starts_with(&format!("{id}/"))) {
            return Err("Stop this package's running components before updating it.".into());
        }
        let packages = self.packages_dir();
        fs::create_dir_all(&packages).map_err(|e| format!("create extension folder: {e}"))?;
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
            return Err("Package ID changed while staging install.".into());
        }
        for c in &staged.components {
            if let Some(ep) = &c.entrypoint {
                if !stage.join(ep).is_file() {
                    let _ = fs::remove_dir_all(&stage);
                    return Err(format!("{}: entrypoint '{}' is not in the package", c.id, ep));
                }
            }
        }
        if target.exists() {
            fs::rename(&target, &backup).map_err(|e| format!("stage existing extension: {e}"))?;
        }
        if let Err(e) = fs::rename(&stage, &target) {
            if backup.exists() { let _ = fs::rename(&backup, &target); }
            let _ = fs::remove_dir_all(&stage);
            return Err(format!("install extension: {e}"));
        }
        if backup.exists() { let _ = fs::remove_dir_all(&backup); }
        Ok(id)
    }

    pub async fn start(&self, package_id: &str, component_id: &str) -> Result<(), String> {
        if !valid_id(component_id) {
            return Err("Invalid extension component ID.".into());
        }
        let folder = self.package_dir(package_id)?;
        let manifest = load_manifest(&folder)?;
        if manifest.id != package_id {
            return Err("Package ID does not match its installed folder.".into());
        }
        let component = manifest.components.iter().find(|c| c.id == component_id)
            .ok_or_else(|| format!("Extension component '{component_id}' was not found."))?;
        if let Some(ep) = &component.entrypoint {
            if !valid_relative_path(ep) || !folder.join(ep).is_file() {
                return Err(format!("{}: entrypoint is missing or unsafe", component.id));
            }
        }
        if component.kind == "command" {
            return Err("One-shot command components must use Run, not Start.".into());
        }

        let key = process_key(package_id, component_id);
        let mut children = self.children.lock().map_err(|_| "Extension process table is unavailable.")?;
        if let Some(current) = children.get_mut(&key) {
            match current.child.try_wait() {
                Ok(None) => return Err("This extension component is already running.".into()),
                Ok(Some(_)) | Err(_) => { children.remove(&key); }
            }
        }

        let entrypoint = component.entrypoint.as_ref()
            .map(|p| folder.join(p).to_string_lossy().to_string()).unwrap_or_default();
        let dir = folder.to_string_lossy().to_string();
        let command = substitute(&component.runtime.command, &dir, &entrypoint, component_id);
        if command.trim().is_empty() {
            return Err("The extension runtime command is empty.".into());
        }
        let args: Vec<String> = component.runtime.args.iter()
            .map(|a| substitute(a, &dir, &entrypoint, component_id)).collect();
        let cwd = component.runtime.working_directory.as_deref().unwrap_or("{extensionDir}");
        let cwd = substitute(cwd, &dir, &entrypoint, component_id);
        let mut cmd = Command::new(&command);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .env("EXTENSION_DIR", &dir)
            .env("EXTENSION_COMPONENT_ID", component_id)
            .env("EXTENSION_ENTRYPOINT", &entrypoint);
        for (name, value) in &component.runtime.environment {
            cmd.env(name, substitute(value, &dir, &entrypoint, component_id));
        }
        if !cwd.trim().is_empty() {
            cmd.current_dir(&cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.as_std_mut().creation_flags(0x0800_0000);
        }
        let child = cmd.spawn().map_err(|e| format!("could not start '{command}': {e}"))?;
        children.insert(key, RunningProcess { child });
        Ok(())
    }

    /// Runs a one-shot extension command and returns bounded stdout/stderr.
    /// Output is limited to 256 KiB per stream; excess bytes are drained but not retained.
    pub async fn run_command(
        &self,
        package_id: &str,
        component_id: &str,
    ) -> Result<ExtensionCommandResult, String> {
        if !valid_id(package_id) || !valid_id(component_id) {
            return Err("Invalid extension component identifier.".into());
        }
        let folder = self.package_dir(package_id)?;
        let manifest = load_manifest(&folder)?;
        if manifest.id != package_id {
            return Err("Package ID does not match its installed folder.".into());
        }
        let component = manifest.components.iter()
            .find(|c| c.id == component_id)
            .ok_or_else(|| format!("Extension component '{component_id}' was not found."))?;
        if component.kind != "command" {
            return Err("Only components with kind 'command' can be run as one-shot commands.".into());
        }
        if let Some(ep) = &component.entrypoint {
            if !valid_relative_path(ep) || !folder.join(ep).is_file() {
                return Err(format!("{}: entrypoint is missing or unsafe", component.id));
            }
        }

        let entrypoint = component.entrypoint.as_ref()
            .map(|path| folder.join(path).to_string_lossy().to_string()).unwrap_or_default();
        let dir = folder.to_string_lossy().to_string();
        let command = substitute(&component.runtime.command, &dir, &entrypoint, component_id);
        if command.trim().is_empty() {
            return Err("The extension command is empty.".into());
        }
        let args: Vec<String> = component.runtime.args.iter()
            .map(|arg| substitute(arg, &dir, &entrypoint, component_id)).collect();
        let cwd = component.runtime.working_directory.as_deref()
            .unwrap_or("{extensionDir}");
        let cwd = substitute(cwd, &dir, &entrypoint, component_id);

        let mut cmd = Command::new(&command);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("EXTENSION_DIR", &dir)
            .env("EXTENSION_COMPONENT_ID", component_id)
            .env("EXTENSION_ENTRYPOINT", &entrypoint);
        for (name, value) in &component.runtime.environment {
            cmd.env(name, substitute(value, &dir, &entrypoint, component_id));
        }
        if !cwd.trim().is_empty() {
            cmd.current_dir(&cwd);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.as_std_mut().creation_flags(0x0800_0000);
        }

        let started = Instant::now();
        let mut child = cmd.spawn()
            .map_err(|e| format!("could not run extension command '{command}': {e}"))?;
        let stdout = child.stdout.take().ok_or("extension stdout pipe was unavailable")?;
        let stderr = child.stderr.take().ok_or("extension stderr pipe was unavailable")?;
        let stdout_reader = tokio::spawn(read_bounded_output(stdout, MAX_COMMAND_OUTPUT_BYTES));
        let stderr_reader = tokio::spawn(read_bounded_output(stderr, MAX_COMMAND_OUTPUT_BYTES));
        let limit = component.runtime.timeout_ms.unwrap_or(DEFAULT_COMMAND_TIMEOUT_MS);
        let wait_result = timeout(Duration::from_millis(limit), child.wait()).await;
        let timed_out = wait_result.is_err();
        let exit_status = match wait_result {
            Ok(Ok(status)) => Some(status),
            Ok(Err(error)) => return Err(format!("could not wait for extension command: {error}")),
            Err(_) => {
                // Kill and reap the timed-out command before reading the final pipe state.
                let _ = child.start_kill();
                child.wait().await.ok()
            }
        };

        let stdout_result = collect_output(stdout_reader).await?;
        let stderr_result = collect_output(stderr_reader).await?;
        let (stdout_bytes, stdout_truncated) = stdout_result;
        let (stderr_bytes, stderr_truncated) = stderr_result;
        let output_truncated = stdout_truncated || stderr_truncated;
        let success = !timed_out && exit_status.as_ref().map(|status| status.success()).unwrap_or(false);
        Ok(ExtensionCommandResult {
            package_id: package_id.to_string(),
            component_id: component_id.to_string(),
            success,
            exit_code: exit_status.and_then(|status| status.code()),
            timed_out,
            output_truncated,
            duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
            stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
        })
    }

    pub async fn stop(&self, package_id: &str, component_id: &str) -> Result<(), String> {
        if !valid_id(package_id) || !valid_id(component_id) {
            return Err("Invalid extension component identifier.".into());
        }
        let process = {
            let mut children = self.children.lock().map_err(|_| "Extension process table is unavailable.")?;
            children.remove(&process_key(package_id, component_id))
        };
        if let Some(mut process) = process {
            match process.child.try_wait() {
                Ok(Some(_)) => return Ok(()),
                Ok(None) => {}
                Err(e) => return Err(format!("could not inspect extension process: {e}")),
            }
            process.child.start_kill().map_err(|e| format!("could not stop extension process: {e}"))?;
            let _ = process.child.wait().await;
        }
        Ok(())
    }

    pub async fn restart(&self, package_id: &str, component_id: &str) -> Result<(), String> {
        self.stop(package_id, component_id).await?;
        self.start(package_id, component_id).await
    }

    pub async fn uninstall(&self, package_id: &str) -> Result<(), String> {
        let folder = self.package_dir(package_id)?;
        let keys: Vec<String> = {
            let children = self.children.lock().map_err(|_| "Extension process table is unavailable.")?;
            children.keys().filter(|k| k.starts_with(&format!("{package_id}/"))).cloned().collect()
        };
        for key in keys {
            if let Some((_, component_id)) = key.split_once('/') {
                self.stop(package_id, component_id).await?;
            }
        }
        if folder.exists() {
            fs::remove_dir_all(&folder).map_err(|e| format!("remove extension package: {e}"))?;
        }
        Ok(())
    }

    pub async fn shutdown(&self) {
        let mut processes = {
            let mut children = match self.children.lock() {
                Ok(children) => children,
                Err(_) => return,
            };
            std::mem::take(&mut *children)
        };
        for (_, mut process) in processes.drain() {
            if process.child.try_wait().ok().flatten().is_none() {
                let _ = process.child.start_kill();
                let _ = process.child.wait().await;
            }
        }
    }

    fn refresh_running(&self) -> HashSet<String> {
        let mut children = match self.children.lock() {
            Ok(children) => children,
            Err(_) => return HashSet::new(),
        };
        let finished: Vec<String> = children.iter_mut().filter_map(|(key, process)| {
            match process.child.try_wait() {
                Ok(None) => None,
                Ok(Some(_)) | Err(_) => Some(key.clone()),
            }
        }).collect();
        for key in finished { children.remove(&key); }
        children.keys().cloned().collect()
    }
}

async fn read_bounded_output<R>(
    mut reader: R,
    limit: usize,
) -> std::io::Result<(Vec<u8>, bool)>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::with_capacity(limit.min(16 * 1024));
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        let retained = count.min(remaining);
        output.extend_from_slice(&buffer[..retained]);
        if retained < count {
            // Continue draining the pipe to prevent a child from blocking on a full pipe.
            truncated = true;
        }
    }
    Ok((output, truncated))
}

async fn collect_output(
    mut task: JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
) -> Result<(Vec<u8>, bool), String> {
    match timeout(Duration::from_millis(OUTPUT_READER_GRACE_MS), &mut task).await {
        Ok(Ok(Ok(result))) => Ok(result),
        Ok(Ok(Err(error))) => Err(format!("could not read extension command output: {error}")),
        Ok(Err(error)) => Err(format!("extension output reader failed: {error}")),
        Err(_) => {
            // A descendant may have inherited a pipe and kept it open after the
            // direct child exited. Never wait indefinitely for that descendant.
            task.abort();
            Ok((b"[output pipe remained open; remaining output omitted]".to_vec(), true))
        }
    }
}

fn process_key(package_id: &str, component_id: &str) -> String {
    format!("{package_id}/{component_id}")
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

fn placeholders(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let (mut out, mut i) = (Vec::new(), 0);
    while i < chars.len() {
        if chars[i] == '{' {
            if i + 1 < chars.len() && chars[i + 1] == '{' { i += 2; continue; }
            if let Some(j) = chars[i + 1..].iter().position(|&c| c == '}') {
                out.push(chars[i + 1..i + 1 + j].iter().collect());
                i += j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn validate_manifest(manifest: &Manifest, files: Option<&HashSet<String>>) -> Result<(), String> {
    if manifest.manifest_version != 1 { return Err("manifestVersion must be 1".into()); }
    if !valid_id(&manifest.id) {
        return Err("manifest id must use lowercase a-z, digits, dots, dashes or underscores (max 100 chars)".into());
    }
    if manifest.name.trim().is_empty() || manifest.name.chars().count() > 100 {
        return Err("manifest name is required (max 100 characters)".into());
    }
    if manifest.version.trim().is_empty() || manifest.version.len() > 40 {
        return Err("manifest version is required (max 40 characters)".into());
    }
    if manifest.description.chars().count() > 1000 {
        return Err("manifest description must be at most 1000 characters".into());
    }
    if manifest.components.is_empty() || manifest.components.len() > 100 {
        return Err("manifest must define between 1 and 100 components".into());
    }
    let mut ids = HashSet::new();
    for c in &manifest.components {
        if !valid_id(&c.id) || !ids.insert(c.id.clone()) {
            return Err(format!("component ID '{}' is invalid or duplicated", c.id));
        }
        if c.name.trim().is_empty() || c.name.chars().count() > 100 {
            return Err(format!("{}: component name is required (max 100 characters)", c.id));
        }
        if c.description.chars().count() > 1000 {
            return Err(format!("{}: component description is too long", c.id));
        }
        if !matches!(c.kind.as_str(), "app" | "service" | "command") {
            return Err(format!("{}: kind must be 'app', 'service', or 'command'", c.id));
        }
        if let Some(timeout_ms) = c.runtime.timeout_ms {
            if c.kind != "command" {
                return Err(format!("{}: runtime.timeoutMs is only valid for command components", c.id));
            }
            if !(MIN_COMMAND_TIMEOUT_MS..=MAX_COMMAND_TIMEOUT_MS).contains(&timeout_ms) {
                return Err(format!(
                    "{}: runtime.timeoutMs must be between {} and {}",
                    c.id, MIN_COMMAND_TIMEOUT_MS, MAX_COMMAND_TIMEOUT_MS
                ));
            }
        }
        if c.runtime.command.trim().is_empty() || c.runtime.command.len() > 2048 {
            return Err(format!("{}: runtime.command is required (max 2048 characters)", c.id));
        }
        if c.runtime.args.len() > 128 {
            return Err(format!("{}: runtime.args may contain at most 128 items", c.id));
        }
        if let Some(ep) = &c.entrypoint {
            if !valid_relative_path(ep) {
                return Err(format!("{}: entrypoint must be a safe relative path using forward slashes", c.id));
            }
            if files.map(|set| !set.contains(ep)).unwrap_or(false) {
                return Err(format!("{}: entrypoint '{}' is missing from the ZIP", c.id, ep));
            }
        }
        for t in std::iter::once(&c.runtime.command)
            .chain(c.runtime.args.iter())
            .chain(c.runtime.working_directory.iter())
            .chain(c.runtime.environment.values())
        {
            for p in placeholders(t) {
                if !matches!(p.as_str(), "extensionDir" | "entrypoint" | "componentId") {
                    return Err(format!("{}: unsupported placeholder {{{p}}}", c.id));
                }
                if p == "entrypoint" && c.entrypoint.is_none() {
                    return Err(format!("{}: {{entrypoint}} is used but entrypoint is not set", c.id));
                }
            }
        }
        for key in c.runtime.environment.keys() {
            if key.is_empty() || key.len() > 128 || !key.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                return Err(format!("{}: invalid environment variable name '{key}'", c.id));
            }
        }
    }
    Ok(())
}

fn substitute(text: &str, extension_dir: &str, entrypoint: &str, component_id: &str) -> String {
    text.replace("{extensionDir}", extension_dir)
        .replace("{entrypoint}", entrypoint)
        .replace("{componentId}", component_id)
}

fn command_preview(component: &Component, extension_dir: &Path) -> String {
    let entrypoint = component.entrypoint.as_ref()
        .map(|p| extension_dir.join(p).to_string_lossy().to_string()).unwrap_or_default();
    let dir = extension_dir.to_string_lossy().to_string();
    let command = substitute(&component.runtime.command, &dir, &entrypoint, &component.id);
    let args: Vec<String> = component.runtime.args.iter()
        .map(|a| substitute(a, &dir, &entrypoint, &component.id)).collect();
    std::iter::once(command).chain(args).map(|s| {
        if s.chars().any(char::is_whitespace) { format!("\"{}\"", s.replace('"', "\\\"")) } else { s }
    }).collect::<Vec<_>>().join(" ")
}

fn load_manifest(folder: &Path) -> Result<Manifest, String> {
    let path = folder.join("manifest.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read manifest.json: {e}"))?;
    if text.len() as u64 > MAX_MANIFEST_BYTES { return Err("manifest.json exceeds the 1 MB limit".into()); }
    let manifest: Manifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("invalid manifest.json: {e}"))?;
    validate_manifest(&manifest, None)?;
    Ok(manifest)
}

fn read_package(path: &Path) -> Result<ReadPackage, String> {
    if path.extension().and_then(|x| x.to_str()).map(|x| !x.eq_ignore_ascii_case("zip")).unwrap_or(true) {
        return Err("extension packages must be .zip archives with a root manifest.json".into());
    }
    let meta = fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > MAX_ZIP_BYTES { return Err("extension ZIP exceeds the 20 MB compressed-size limit".into()); }
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("invalid ZIP archive: {e}"))?;
    if archive.len() == 0 || archive.len() > MAX_FILES {
        return Err(format!("extension ZIP must contain between 1 and {MAX_FILES} entries"));
    }
    let mut files = HashSet::new();
    let mut windows_path_keys = HashSet::new();
    let mut total = 0u64;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| format!("cannot read ZIP entry: {e}"))?;
        let name = entry.name().to_string();
        if entry.enclosed_name().is_none() || name.contains('\\') || name.contains(':') {
            return Err(format!("unsafe ZIP path: {name}"));
        }
        if !files.insert(name.clone()) { return Err(format!("duplicate ZIP entry: {name}")); }
        // Windows filesystems are case-insensitive by default. Reject archives whose
        // entries would alias when extracted, including "folder" vs "folder/".
        register_windows_zip_path(&mut windows_path_keys, &name)?;
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 { return Err(format!("symbolic links are not allowed: {name}")); }
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES { return Err("extension ZIP expands beyond the 50 MB unpacked-size limit".into()); }
        if name.eq_ignore_ascii_case("manifest.json") && entry.size() > MAX_MANIFEST_BYTES {
            return Err("manifest.json exceeds the 1 MB limit".into());
        }
    }
    if !files.contains("manifest.json") { return Err("extension ZIP must contain manifest.json at its root".into()); }
    let mut text = String::new();
    archive.by_name("manifest.json").map_err(|e| e.to_string())?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_string(&mut text).map_err(|e| format!("manifest.json is not valid UTF-8: {e}"))?;
    if text.len() as u64 > MAX_MANIFEST_BYTES { return Err("manifest.json exceeds the 1 MB limit".into()); }
    let manifest: Manifest = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("invalid manifest.json: {e}"))?;
    validate_manifest(&manifest, Some(&files))?;
    Ok(ReadPackage { manifest })
}

fn extract_package(path: &Path, destination: &Path) -> Result<(), String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| format!("invalid ZIP archive: {e}"))?;
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let mut total = 0u64;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES { return Err("extension ZIP expands beyond the 50 MB unpacked-size limit".into()); }
        let relative = entry.enclosed_name().ok_or_else(|| format!("unsafe ZIP path: {}", entry.name()))?.to_path_buf();
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 { return Err(format!("symbolic links are not allowed: {}", entry.name())); }
        }
        let out = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() { fs::create_dir_all(parent).map_err(|e| e.to_string())?; }
        let declared_size = entry.size();
        let mut target = File::create(&out).map_err(|e| e.to_string())?;
        let copied = std::io::copy(&mut entry, &mut target).map_err(|e| format!("extract failed: {e}"))?;
        if copied != declared_size { return Err(format!("ZIP entry size mismatch: {}", relative.display())); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_identifiers_and_paths_are_restricted() {
        assert!(valid_id("com.example.desktop-helper"));
        assert!(!valid_id("../escape"));
        assert!(!valid_id("UpperCase"));
        assert!(valid_relative_path("scripts/start.py"));
        assert!(!valid_relative_path("../start.py"));
        assert!(!valid_relative_path("C:/start.py"));
        assert!(!valid_relative_path("scripts\\start.py"));
    }

    #[test]
    fn extension_zip_paths_that_alias_on_windows_are_rejected() {
        let mut seen = HashSet::new();
        assert!(register_windows_zip_path(&mut seen, "scripts/worker.py").is_ok());
        assert!(register_windows_zip_path(&mut seen, "Scripts/Worker.py")
            .unwrap_err().contains("collide on Windows"));

        let mut seen_dirs = HashSet::new();
        assert!(register_windows_zip_path(&mut seen_dirs, "assets").is_ok());
        assert!(register_windows_zip_path(&mut seen_dirs, "assets/")
            .unwrap_err().contains("collide on Windows"));

        assert!(register_windows_zip_path(&mut HashSet::new(), "../escape")
            .unwrap_err().contains("unsafe ZIP path"));
    }

    #[test]
    fn manifest_allows_arbitrary_executable_runtimes() {
        let manifest: Manifest = serde_json::from_str(r#"{
          "manifestVersion":1,"id":"com.example.runtime-test","name":"Runtime test","version":"1.0.0",
          "components":[{"id":"main","name":"Python service","kind":"service","entrypoint":"scripts/main.py",
            "runtime":{"command":"python","args":["{entrypoint}"],"workingDirectory":"{extensionDir}","environment":{"MODE":"test"}}}]
        }"#).unwrap();
        let files = HashSet::from(["manifest.json".to_string(), "scripts/main.py".to_string()]);
        assert!(validate_manifest(&manifest, Some(&files)).is_ok());
        assert_eq!(manifest.components[0].runtime.command, "python");
    }

    #[test]
    fn manifest_rejects_unknown_placeholders() {
        let manifest: Manifest = serde_json::from_str(r#"{
          "manifestVersion":1,"id":"com.example.bad","name":"Bad","version":"1.0.0",
          "components":[{"id":"main","name":"Bad runtime","kind":"app","runtime":{"command":"{unknownExecutable}"}}]
        }"#).unwrap();
        assert!(validate_manifest(&manifest, None).unwrap_err().contains("unsupported placeholder"));
    }
}
