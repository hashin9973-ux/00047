//! Build and install a user-selected 00047 native-mod CMake project.
//!
//! The build runs only after an explicit UI action. A CMake project can execute
//! arbitrary commands as the current user, so the frontend warns users to build
//! only source they trust. The resulting ZIP is still passed through the normal
//! native-mod package validator and installed disabled by default.

use serde::Serialize;
use serde_json::Value;
use std::{
    fs::{self, File},
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zip::{write::FileOptions, CompressionMethod, ZipWriter};

use crate::native_mods::NativeModManager;

const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SOURCE_CMAKE_BYTES: u64 = 1024 * 1024;
const MAX_SOURCE_CARGO_BYTES: u64 = 1024 * 1024;
const MAX_BUILT_DLL_BYTES: u64 = 50 * 1024 * 1024;
const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const BUILD_TIMEOUT: Duration = Duration::from_secs(5 * 60);
static WORKSPACE_COUNTER: AtomicU64 = AtomicU64::new(0);
static BUILD_IN_PROGRESS: AtomicBool = AtomicBool::new(false);

struct BuildPermit;

impl BuildPermit {
    fn acquire() -> Result<Self, String> {
        BUILD_IN_PROGRESS
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| "Another native-mod source build is already running.".to_string())
    }
}

impl Drop for BuildPermit {
    fn drop(&mut self) {
        BUILD_IN_PROGRESS.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInstallReport {
    pub id: String,
    pub message: String,
    pub build_output: String,
}

struct BuildWorkspace {
    path: PathBuf,
}

impl BuildWorkspace {
    fn create() -> Result<Self, String> {
        let temp = std::env::temp_dir();
        for _ in 0..16 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| format!("Cannot read system clock: {e}"))?
                .as_nanos();
            let sequence = WORKSPACE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = temp.join(format!(
                "00047-native-mod-build-{}-{nanos}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("Cannot create temporary build workspace: {error}")),
            }
        }
        Err("Could not allocate a unique temporary build workspace.".into())
    }
}

impl Drop for BuildWorkspace {
    fn drop(&mut self) {
        // remove_dir_all does not follow symlinks inside the workspace.
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(not(windows))]
pub async fn build_and_install(
    project_dir: PathBuf,
    sdk_dir: PathBuf,
    manager: Arc<NativeModManager>,
) -> Result<BuildInstallReport, String> {
    let _ = (project_dir, sdk_dir, manager);
    Err("Building native Windows mods is supported only on Windows.".into())
}

#[cfg(windows)]
pub async fn build_and_install(
    project_dir: PathBuf,
    sdk_dir: PathBuf,
    manager: Arc<NativeModManager>,
) -> Result<BuildInstallReport, String> {
    let _permit = BuildPermit::acquire()?;
    let (project_root, manifest_bytes, module_relative, build_system) =
        inspect_project(&project_dir)?;
    let sdk_root = if build_system == SourceBuildSystem::CMake {
        Some(checked_sdk_directory(&sdk_dir)?)
    } else {
        None
    };
    let module_name = module_relative
        .file_name()
        .ok_or("Manifest module path has no DLL filename.")?
        .to_owned();

    let workspace = BuildWorkspace::create()?;
    let (dll, build_log) = match build_system {
        SourceBuildSystem::CMake => {
            let sdk_root = sdk_root
                .as_ref()
                .ok_or("The CMake build is missing its SDK directory.")?;
            let build_dir = workspace.path.join("build");

            // Canonicalize for security checks above, but pass ordinary DOS/UNC paths
            // to CMake/MSBuild. Their generated project files can mis-handle the
            // Windows `\\?\\` verbatim prefix (e.g. turn it into `///?/C:/...`).
            let cmake_project_root = cmake_tool_path(&project_root)?;
            let cmake_sdk_root = cmake_tool_path(sdk_root)?;
            let cmake_build_dir = cmake_tool_path(&build_dir)?;
            let sdk_argument = format!("-DWMOD_SDK_DIR={}", cmake_sdk_root.display());

            let configure_args = vec![
                "-S".into(),
                cmake_project_root.as_os_str().to_os_string(),
                "-B".into(),
                cmake_build_dir.as_os_str().to_os_string(),
                "-A".into(),
                "x64".into(),
                sdk_argument.into(),
            ];
            let configure_output = run_command(
                "cmake",
                configure_args,
                &cmake_project_root,
                BUILD_TIMEOUT,
                "CMake configure",
            )
            .await?;

            let build_args = vec![
                "--build".into(),
                cmake_build_dir.as_os_str().to_os_string(),
                "--config".into(),
                "Release".into(),
                "--parallel".into(),
                "2".into(),
            ];
            let build_output = run_command(
                "cmake",
                build_args,
                &cmake_project_root,
                BUILD_TIMEOUT,
                "CMake build",
            )
            .await?;
            let dll = locate_built_dll(&cmake_build_dir, &module_name)?;
            let log = format!(
                "=== Configure ===\n{}\n=== Build ===\n{}",
                configure_output, build_output
            );
            (dll, log)
        }
        SourceBuildSystem::Cargo => {
            let target_dir = workspace.path.join("cargo-target");
            let manifest_path = project_root.join("Cargo.toml");
            let build_args = vec![
                "build".into(),
                "--manifest-path".into(),
                manifest_path.as_os_str().to_os_string(),
                "--target".into(),
                "x86_64-pc-windows-msvc".into(),
                "--release".into(),
                "--locked".into(),
                "--lib".into(),
                "--target-dir".into(),
                target_dir.as_os_str().to_os_string(),
            ];
            let build_output = run_command(
                "cargo",
                build_args,
                &project_root,
                BUILD_TIMEOUT,
                "Cargo build (x64 Windows)",
            )
            .await?;
            let dll = locate_cargo_built_dll(&target_dir, &module_name)?;
            (dll, format!("=== Cargo build (x64 Windows) ===\n{build_output}"))
        }
    };

    let dll_metadata = fs::symlink_metadata(&dll)
        .map_err(|e| format!("Cannot inspect compiled DLL: {e}"))?;
    if dll_metadata.file_type().is_symlink()
        || !dll_metadata.is_file()
        || dll_metadata.len() == 0
        || dll_metadata.len() > MAX_BUILT_DLL_BYTES
    {
        return Err(format!(
            "Compiled DLL must be a regular, non-empty file no larger than {} MB.",
            MAX_BUILT_DLL_BYTES / (1024 * 1024)
        ));
    }

    let package_path = workspace.path.join("built-native-mod.zip");
    create_package(&package_path, &manifest_bytes, &module_relative, &dll)?;
    let package_preview = manager.preview(&package_path)?;
    let installed_id = manager.install(&package_path).map_err(|error| {
        format!(
            "Build succeeded, but package installation failed: {error}\n\nBuild output:\n{build_log}"
        )
    })?;

    Ok(BuildInstallReport {
        id: installed_id,
        message: format!(
            "Built and installed {} ({}). It remains disabled until you explicitly enable it.",
            package_preview.name, package_preview.version
        ),
        build_output: build_log,
    })
}
fn checked_regular_file(path: &Path, label: &str, max_bytes: u64) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("Cannot inspect {label} '{}': {e}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > max_bytes {
        return Err(format!(
            "{label} must be a regular, non-symlink file no larger than {} MB: {}",
            max_bytes / (1024 * 1024),
            path.display()
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceBuildSystem {
    CMake,
    Cargo,
}

fn inspect_project(
    project_dir: &Path,
) -> Result<(PathBuf, Vec<u8>, PathBuf, SourceBuildSystem), String> {
    let root_metadata = fs::symlink_metadata(project_dir)
        .map_err(|e| format!("Cannot inspect source project directory: {e}"))?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err("Choose a real source project directory, not a symlink.".into());
    }
    let root = project_dir
        .canonicalize()
        .map_err(|e| format!("Cannot resolve source project directory: {e}"))?;

    let cmake_file = root.join("CMakeLists.txt");
    let cargo_file = root.join("Cargo.toml");
    let build_system = if cmake_file.exists() {
        checked_regular_file(&cmake_file, "CMakeLists.txt", MAX_SOURCE_CMAKE_BYTES)?;
        SourceBuildSystem::CMake
    } else if cargo_file.exists() {
        checked_regular_file(&cargo_file, "Cargo.toml", MAX_SOURCE_CARGO_BYTES)?;
        SourceBuildSystem::Cargo
    } else {
        return Err(
            "The source project must contain manifest.json and either CMakeLists.txt or Cargo.toml."
                .into(),
        );
    };

    let manifest_path = root.join("manifest.json");
    checked_regular_file(&manifest_path, "manifest.json", MAX_MANIFEST_BYTES)?;
    let manifest_bytes = fs::read(&manifest_path)
        .map_err(|e| format!("Cannot read source manifest.json: {e}"))?;
    let value: Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| format!("Source manifest.json is invalid: {e}"))?;
    if !value.is_object() {
        return Err("Source manifest.json must contain a JSON object.".into());
    }
    let module = value
        .get("module")
        .and_then(Value::as_str)
        .ok_or("Source manifest.json must declare a relative module DLL path.")?;
    let relative = safe_module_relative_path(module)?;
    Ok((root, manifest_bytes, relative, build_system))
}
fn safe_module_relative_path(module: &str) -> Result<PathBuf, String> {
    if module.is_empty()
        || module.contains('\\')
        || module.contains(':')
        || module.starts_with('/')
        || module.chars().any(char::is_control)
    {
        return Err("Manifest module path must be a safe relative ZIP path using forward slashes.".into());
    }
    let components: Vec<&str> = module.split('/').collect();
    if components.len() != 2
        || components[0] != "bin"
        || components[1].is_empty()
        || components[1] == "."
        || components[1] == ".."
        || !components[1].to_ascii_lowercase().ends_with(".dll")
        || Path::new(module).components().any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(
            "The in-app builder expects manifest.module to be bin/<module-name>.dll.".into()
        );
    }
    Ok(PathBuf::from("bin").join(components[1]))
}

#[cfg(windows)]
fn cmake_tool_path(path: &Path) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStrExt;

    let value = path.to_string_lossy();
    let ordinary = if let Some(rest) = value.strip_prefix("\\\\?\\UNC\\") {
        format!("\\\\{rest}")
    } else if let Some(rest) = value.strip_prefix("\\\\?\\") {
        rest.to_string()
    } else {
        value.into_owned()
    };
    let normalized = PathBuf::from(ordinary);
    // Visual Studio project files are not reliably generated for paths beyond
    // MAX_PATH across all supported CMake/MSBuild combinations. Fail early
    // with an actionable message rather than a misleading missing-source error.
    if normalized.as_os_str().encode_wide().count() >= 260 {
        return Err(format!(
            "CMake/Visual Studio cannot reliably build from paths of 260 or more UTF-16 code units; move the source project and SDK to shorter paths: {}",
            normalized.display()
        ));
    }
    Ok(normalized)
}

#[cfg(test)]
static SOURCE_BUILD_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
fn lock_source_build_tests() -> std::sync::MutexGuard<'static, ()> {
    SOURCE_BUILD_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn checked_sdk_directory(sdk_dir: &Path) -> Result<PathBuf, String> {
    let root = sdk_dir
        .canonicalize()
        .map_err(|e| format!("Cannot resolve the bundled native-mod SDK: {e}"))?;
    for relative in [
        "00047_mod_api.h",
        "windhawk_hook_compat.cpp",
        "third_party/minhook/src/hook.c",
        "third_party/distorm/src/decoder.c",
    ] {
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| format!("The bundled native-mod SDK is incomplete ({relative}): {e}"))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(format!("The bundled native-mod SDK is missing a regular {relative} file."));
        }
    }
    Ok(root)
}

fn locate_built_dll(build_dir: &Path, module_name: &std::ffi::OsStr) -> Result<PathBuf, String> {
    let candidates = [
        build_dir.join("bin").join("Release").join(module_name),
        build_dir.join("bin").join(module_name),
        build_dir.join("Release").join(module_name),
        build_dir.join(module_name),
    ];
    for candidate in candidates {
        if let Ok(metadata) = fs::symlink_metadata(&candidate) {
            if !metadata.file_type().is_symlink() && metadata.is_file() {
                return candidate
                    .canonicalize()
                    .map_err(|e| format!("Cannot resolve compiled DLL path: {e}"));
            }
        }
    }
    Err(format!(
        "CMake completed, but the DLL named '{}' was not found in the expected Release output folders. Check manifest.module and RUNTIME_OUTPUT_DIRECTORY in CMakeLists.txt.",
        module_name.to_string_lossy()
    ))
}

fn locate_cargo_built_dll(
    target_dir: &Path,
    module_name: &std::ffi::OsStr,
) -> Result<PathBuf, String> {
    let candidate = target_dir
        .join("x86_64-pc-windows-msvc")
        .join("release")
        .join(module_name);
    let metadata = fs::symlink_metadata(&candidate).map_err(|e| {
        format!(
            "Cargo completed, but the expected DLL was not found at '{}': {e}. Check that Cargo.toml declares a `cdylib` whose `[lib] name` matches manifest.module.",
            candidate.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "Cargo output '{}' is not a regular DLL file. Check the `cdylib` name and manifest.module.",
            candidate.display()
        ));
    }
    candidate
        .canonicalize()
        .map_err(|e| format!("Cannot resolve Cargo output DLL path: {e}"))
}

async fn run_command(
    program: &str,
    arguments: Vec<std::ffi::OsString>,
    current_dir: &Path,
    timeout: Duration,
    stage: &str,
) -> Result<String, String> {
    use tokio::{
        io::{AsyncRead, AsyncReadExt},
        process::Command,
    };
    use std::process::Stdio;

    async fn capture<R>(mut reader: R) -> std::io::Result<(Vec<u8>, bool)>
    where
        R: AsyncRead + Unpin,
    {
        let mut stored = Vec::new();
        let mut truncated = false;
        let mut chunk = [0u8; 8192];
        loop {
            let read = reader.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            let remaining = MAX_CAPTURE_BYTES.saturating_sub(stored.len());
            let keep = remaining.min(read);
            stored.extend_from_slice(&chunk[..keep]);
            if keep < read {
                truncated = true;
            }
        }
        Ok((stored, truncated))
    }

    let mut child = Command::new(program)
        .args(&arguments)
        .current_dir(current_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!(
            "Could not start {program} for {stage}; install the required build toolchain and ensure it is on PATH. Details: {e}"
        ))?;
    let stdout = child.stdout.take().ok_or_else(|| format!("{stage}: stdout pipe is unavailable"))?;
    let stderr = child.stderr.take().ok_or_else(|| format!("{stage}: stderr pipe is unavailable"))?;
    let stdout_task = tokio::spawn(capture(stdout));
    let stderr_task = tokio::spawn(capture(stderr));

    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            stdout_task.abort();
            stderr_task.abort();
            return Err(format!("{stage} could not be completed: {error}"));
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(format!(
                "{stage} exceeded the {}-second timeout. The build was terminated.",
                timeout.as_secs()
            ));
        }
    };

    let read_result = |result: Result<
        Result<(Vec<u8>, bool), std::io::Error>,
        tokio::task::JoinError
    >, label: &str| -> Result<(String, bool), String> {
        let (bytes, truncated) = result
            .map_err(|e| format!("{stage}: could not collect {label}: {e}"))?
            .map_err(|e| format!("{stage}: error reading {label}: {e}"))?;
        let mut text = String::from_utf8_lossy(&bytes).into_owned();
        if truncated {
            text.push_str("\n[output truncated at 256 KiB; remaining output was drained]\n");
        }
        Ok((text, truncated))
    };
    let (stdout_text, _) = read_result(stdout_task.await, "stdout")?;
    let (stderr_text, _) = read_result(stderr_task.await, "stderr")?;
    let combined = format!("{}\n{}", stdout_text.trim(), stderr_text.trim());

    if !status.success() {
        return Err(format!(
            "{stage} failed with exit code {}.\n{combined}",
            status.code().map(|c| c.to_string()).unwrap_or_else(|| "unknown".into())
        ));
    }
    Ok(combined)
}

fn create_package(
    output_path: &Path,
    manifest_bytes: &[u8],
    module_relative: &Path,
    dll_path: &Path,
) -> Result<(), String> {
    let dll_bytes = fs::read(dll_path).map_err(|e| format!("Cannot read compiled DLL: {e}"))?;
    if dll_bytes.is_empty() || dll_bytes.len() as u64 > MAX_BUILT_DLL_BYTES {
        return Err("Compiled DLL size is outside the accepted limit.".into());
    }
    let output = File::create(output_path)
        .map_err(|e| format!("Cannot create temporary native-mod ZIP: {e}"))?;
    let mut archive = ZipWriter::new(output);
    archive
        .start_file("manifest.json", FileOptions::default().compression_method(CompressionMethod::Deflated))
        .map_err(|e| format!("Cannot write package manifest entry: {e}"))?;
    archive.write_all(manifest_bytes)
        .map_err(|e| format!("Cannot write package manifest: {e}"))?;
    if module_relative.parent() != Some(Path::new("bin")) {
        return Err("Manifest module path must be a safe bin/<module-name>.dll path.".into());
    }
    let module_file_name = module_relative
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or("Manifest module path is not valid UTF-8.")?;
    // ZIP paths always use forward slashes, independent of the build host OS.
    let module_name = format!("bin/{module_file_name}");
    archive
        .start_file(module_name.as_str(), FileOptions::default().compression_method(CompressionMethod::Deflated))
        .map_err(|e| format!("Cannot write package DLL entry: {e}"))?;
    archive.write_all(&dll_bytes)
        .map_err(|e| format!("Cannot write package DLL bytes: {e}"))?;
    archive.finish()
        .map_err(|e| format!("Cannot finalize native-mod ZIP: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_source_build_can_run_at_a_time() {
        let _serial = lock_source_build_tests();
        let first = BuildPermit::acquire().unwrap();
        assert!(BuildPermit::acquire().is_err());
        drop(first);
        assert!(BuildPermit::acquire().is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn cmake_tool_path_normalizes_verbatim_dos_and_unc_paths() {
        assert_eq!(
            cmake_tool_path(Path::new(r"\\?\C:\build\source")).unwrap(),
            PathBuf::from(r"C:\build\source")
        );
        assert_eq!(
            cmake_tool_path(Path::new(r"\\?\UNC\server\share\source")).unwrap(),
            PathBuf::from(r"\\server\share\source")
        );
        let long = PathBuf::from(format!(r"C:\{}", "a".repeat(270)));
        assert!(cmake_tool_path(&long).unwrap_err().contains("260 or more UTF-16 code units"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn in_app_builder_compiles_validates_and_installs_disabled() {
        let _serial = lock_source_build_tests();
        const MOD_ID: &str = "com.example.inapp-builder-smoke";
        const MODULE_NAME: &str = "com_example_inapp_builder_smoke";
        let root = std::env::temp_dir().join(format!(
            "00047-native-mod-builder-smoke-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let project = root.join("source");
        fs::create_dir_all(&project).unwrap();

        let manifest = serde_json::json!({
            "manifestVersion": 1,
            "id": MOD_ID,
            "name": "In-app Builder Smoke Test",
            "version": "1.0.0",
            "description": "Windows CI exercise for the in-app source builder",
            "architecture": "x64",
            "module": format!("bin/{MODULE_NAME}.dll"),
            "targets": ["explorer.exe"]
        });
        fs::write(
            project.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        ).unwrap();
        fs::write(project.join("CMakeLists.txt"), r#"cmake_minimum_required(VERSION 3.21)
project(native_mod_builder_smoke LANGUAGES CXX)

set(WMOD_SDK_DIR "" CACHE PATH "Path to the 00047 native-mod SDK")
add_library(com_example_inapp_builder_smoke SHARED mod.cpp)
target_compile_features(com_example_inapp_builder_smoke PRIVATE cxx_std_17)
target_compile_definitions(com_example_inapp_builder_smoke PRIVATE WMOD_BUILD_DLL WIN32_LEAN_AND_MEAN NOMINMAX)
target_include_directories(com_example_inapp_builder_smoke PRIVATE "${WMOD_SDK_DIR}")
set_target_properties(com_example_inapp_builder_smoke PROPERTIES
  PREFIX ""
  OUTPUT_NAME "com_example_inapp_builder_smoke"
  RUNTIME_OUTPUT_DIRECTORY "${CMAKE_BINARY_DIR}/bin"
)
"#).unwrap();
        fs::write(project.join("mod.cpp"), r#"#include "00047_mod_api.h"

WMOD_00047_EXPORT std::uint32_t WINAPI WmodGetApiVersion(void) {
    return WMOD_00047_ABI_VERSION;
}
WMOD_00047_EXPORT BOOL WINAPI WmodInitializeV1(void) {
    return TRUE;
}
WMOD_00047_EXPORT void WINAPI WmodShutdownV1(void) {}
WMOD_00047_EXPORT BOOL WINAPI WmodIsInitializedV1(void) {
    return FALSE;
}
WMOD_00047_EXPORT BOOL WINAPI WmodCanUnloadV1(void) {
    return TRUE;
}
"#).unwrap();

        let sdk = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../sdk/native-mod");
        let manager = Arc::new(NativeModManager::new(root.join("managed")));
        let report = build_and_install(project, sdk, manager.clone()).await.unwrap();
        assert_eq!(report.id, MOD_ID);
        assert!(report.message.contains("disabled"));
        assert!(!manager.enabled_snapshot().contains(MOD_ID));
        let package = manager.package_dir(MOD_ID).unwrap();
        assert!(package.join("manifest.json").is_file());
        assert!(package.join("bin").join(format!("{MODULE_NAME}.dll")).is_file());

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn in_app_builder_builds_rust_cdylib_and_installs_disabled() {
        let _serial = lock_source_build_tests();
        const MOD_ID: &str = "com.example.inapp-rust-builder-smoke";
        const MODULE_NAME: &str = "com_example_inapp_rust_builder_smoke";
        let root = std::env::temp_dir().join(format!(
            "00047-native-mod-rust-builder-smoke-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let project = root.join("source");
        fs::create_dir_all(project.join("src")).unwrap();

        let manifest = serde_json::json!({
            "manifestVersion": 1,
            "id": MOD_ID,
            "name": "In-app Rust Builder Smoke Test",
            "version": "1.0.0",
            "description": "Windows CI exercise for the in-app Rust source builder",
            "architecture": "x64",
            "module": format!("bin/{MODULE_NAME}.dll"),
            "targets": ["explorer.exe"]
        });
        fs::write(
            project.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        ).unwrap();
        fs::write(project.join("Cargo.toml"), r#"[package]
name = "inapp-rust-builder-smoke"
version = "0.1.0"
edition = "2021"

[lib]
name = "com_example_inapp_rust_builder_smoke"
crate-type = ["cdylib"]
"#).unwrap();
        fs::write(project.join("Cargo.lock"), r#"# This file is automatically @generated by Cargo.
# It is not intended for manual editing.
version = 4

[[package]]
name = "inapp-rust-builder-smoke"
version = "0.1.0"
"#).unwrap();
        fs::write(project.join("src").join("lib.rs"), r#"#![allow(non_snake_case)]
use std::sync::atomic::{AtomicBool, Ordering};

static INITIALIZED: AtomicBool = AtomicBool::new(false);

#[no_mangle]
pub extern "system" fn WmodGetApiVersion() -> u32 { 2 }

#[no_mangle]
pub extern "system" fn WmodInitializeV1() -> i32 {
    INITIALIZED.store(true, Ordering::Release);
    1
}

#[no_mangle]
pub extern "system" fn WmodShutdownV1() {
    INITIALIZED.store(false, Ordering::Release);
}

#[no_mangle]
pub extern "system" fn WmodIsInitializedV1() -> i32 {
    if INITIALIZED.load(Ordering::Acquire) { 1 } else { 0 }
}

#[no_mangle]
pub extern "system" fn WmodCanUnloadV1() -> i32 {
    if !INITIALIZED.load(Ordering::Acquire) { 1 } else { 0 }
}
"#).unwrap();

        let manager = Arc::new(NativeModManager::new(root.join("managed")));
        // Rust-only lifecycle modules do not need the C++ compatibility SDK.
        let report = build_and_install(project, root.join("unused-sdk"), manager.clone())
            .await
            .unwrap();
        assert_eq!(report.id, MOD_ID);
        assert!(report.build_output.contains("Cargo build"));
        assert!(report.message.contains("disabled"));
        assert!(!manager.enabled_snapshot().contains(MOD_ID));
        let package = manager.package_dir(MOD_ID).unwrap();
        assert!(package.join("manifest.json").is_file());
        assert!(package.join("bin").join(format!("{MODULE_NAME}.dll")).is_file());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn create_package_writes_forward_slash_zip_paths() {
        let root = std::env::temp_dir().join(format!(
            "00047-zip-entry-path-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let manifest = root.join("manifest.json");
        let dll = root.join("test.dll");
        let package = root.join("out.zip");
        fs::write(&manifest, b"{}").unwrap();
        fs::write(&dll, b"MZ").unwrap();

        let module = safe_module_relative_path("bin/test.dll").unwrap();
        create_package(&package, b"{}", &module, &dll).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&package).unwrap()).unwrap();
        let mut names: Vec<String> = archive.file_names().map(str::to_owned).collect();
        names.sort();
        assert_eq!(names, vec!["bin/test.dll", "manifest.json"]);
        assert!(archive.by_name("bin/test.dll").is_ok());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn module_path_is_limited_to_the_expected_bin_dll_shape() {
        assert_eq!(
            safe_module_relative_path("bin/com_example_mod.dll").unwrap(),
            PathBuf::from("bin/com_example_mod.dll")
        );
        for rejected in [
            "",
            "../mod.dll",
            "bin/../mod.dll",
            "bin\\mod.dll",
            "C:/mod.dll",
            "/bin/mod.dll",
            "lib/mod.dll",
            "bin/not-a-dll.txt",
            "bin/a/b.dll",
        ] {
            assert!(safe_module_relative_path(rejected).is_err(), "{rejected}");
        }
    }
}
