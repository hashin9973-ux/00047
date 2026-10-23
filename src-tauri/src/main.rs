#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blocks;
mod custom;
mod spec;
mod engine;
mod extensions;
mod native_mods;
mod native_mod_config;
mod logstore;
mod model;
mod store;
mod sys;
mod vault;

use engine::{Engine, Runtime};
use model::{Automation, LogEntry, RunRecord};
use std::{sync::{Arc, Mutex}, time::Duration};
use store::Store;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_window_state::{AppHandleExt, StateFlags};
#[cfg(target_os = "windows")]
use windows::{core::Interface, Win32::Foundation::HWND};
#[cfg(target_os = "windows")]
use webview2_com::{
    Microsoft::Web::WebView2::Win32::ICoreWebView2_3,
    TrySuspendCompletedHandler,
};

#[derive(serde::Deserialize)]
struct GlassSurface {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    opacity: f32,
}

#[cfg(target_os = "windows")]
#[repr(C)]
struct NativeGlassRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
    opacity: f32,
}

#[cfg(target_os = "windows")]
extern "C" {
    fn windhawk_blur_update(owner: isize, rects: *const NativeGlassRect, count: u32) -> i32;
    fn windhawk_blur_set_visible(owner: isize, visible: bool);
    fn windhawk_blur_reposition(owner: isize);
    fn windhawk_blur_destroy(owner: isize);
}

#[cfg(target_os = "windows")]
fn set_native_blur_visible(window: &tauri::WebviewWindow, visible: bool) {
    let _ = window.with_webview(move |webview| unsafe {
        let mut hwnd = HWND::default();
        if webview.controller().ParentWindow(&mut hwnd).is_ok() {
            windhawk_blur_set_visible(hwnd.0 as isize, visible);
        }
    });
}

#[cfg(target_os = "windows")]
fn suspend_native_webview(window: &tauri::WebviewWindow) {
    let _ = window.with_webview(|webview| unsafe {
        let Ok(core) = webview.controller().CoreWebView2() else {
            return;
        };
        let Ok(core3) = core.cast::<ICoreWebView2_3>() else {
            return;
        };
        let callback = TrySuspendCompletedHandler::create(Box::new(|_result, _success| Ok(())));
        let _ = core3.TrySuspend(&callback);
    });
}

#[cfg(target_os = "windows")]
fn resume_native_webview(window: &tauri::WebviewWindow) {
    let _ = window.with_webview(|webview| unsafe {
        let Ok(core) = webview.controller().CoreWebView2() else {
            return;
        };
        let Ok(core3) = core.cast::<ICoreWebView2_3>() else {
            return;
        };
        let _ = core3.Resume();
    });
}

#[cfg(target_os = "windows")]
fn reposition_native_blur(window: &tauri::WebviewWindow) {
    let _ = window.with_webview(move |webview| unsafe {
        let mut hwnd = HWND::default();
        if webview.controller().ParentWindow(&mut hwnd).is_ok() {
            windhawk_blur_reposition(hwnd.0 as isize);
        }
    });
}

#[cfg(target_os = "windows")]
fn destroy_native_blur(window: &tauri::WebviewWindow) {
    let _ = window.with_webview(move |webview| unsafe {
        let mut hwnd = HWND::default();
        if webview.controller().ParentWindow(&mut hwnd).is_ok() {
            windhawk_blur_destroy(hwnd.0 as isize);
        }
    });
}



// ---------- commands ----------

#[tauri::command]
fn update_windhawk_blur(app: AppHandle, surfaces: Vec<GlassSurface>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let window = app
            .get_webview_window("main")
            .ok_or_else(|| "Main WebView is unavailable.".to_string())?;
        if surfaces.len() > 512 {
            return Err("Too many native blur surfaces in a single update.".into());
        }
        let native: Vec<NativeGlassRect> = surfaces
            .into_iter()
            .filter(|r| r.width > 0.0 && r.height > 0.0)
            .map(|r| NativeGlassRect {
                x: r.x,
                y: r.y,
                width: r.width,
                height: r.height,
                radius: r.radius,
                opacity: r.opacity,
            })
            .collect();
        let count = u32::try_from(native.len()).map_err(|e| e.to_string())?;
        let code = Arc::new(Mutex::new(None));
        let code_for_ui = code.clone();
        window
            .with_webview(move |webview| unsafe {
                let mut hwnd = HWND::default();
                let result = match webview.controller().ParentWindow(&mut hwnd) {
                    Ok(()) => windhawk_blur_update(hwnd.0 as isize, native.as_ptr(), count),
                    Err(error) => error.code().0,
                };
                *code_for_ui.lock().unwrap() = Some(result);
            })
            .map_err(|e| e.to_string())?;
        let result = code
            .lock()
            .map_err(|e| e.to_string())?
            .unwrap_or(-2147467259);
        if result < 0 {
            return Err(format!("Native Windhawk Composition blur failed (HRESULT 0x{:08X}).", result as u32));
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (app, surfaces);
        Err("Native Windhawk Composition blur is available only on Windows.".into())
    }
}

#[tauri::command]
fn vault_list_secrets() -> Result<Vec<String>, String> {
    vault::list_secrets()
}

#[tauri::command]
async fn vault_set_secret(name: String, value: String) -> Result<(), String> {
    tokio::task::spawn_blocking(move || vault::set_secret(&name, &value))
        .await.map_err(|e| format!("Secret vault worker failed: {e}"))?
}

#[tauri::command]
fn vault_delete_secret(name: String) -> Result<(), String> {
    vault::delete_secret(&name)
}

#[tauri::command]
fn list_automations(eng: State<'_, Arc<Engine>>) -> Vec<Automation> {
    eng.store.all()
}

#[tauri::command]
fn save_automation(eng: State<'_, Arc<Engine>>, auto: Automation) -> Result<(), String> {
    eng.store.upsert(auto).map_err(|e| {
        eng.app_log("error", format!("Save failed: {e}"));
        e
    })
}

#[tauri::command]
async fn delete_automation(eng: State<'_, Arc<Engine>>, id: String) -> Result<(), String> {
    let e = eng.inner().clone();
    e.stop_and_wait(&id).await?;
    e.remove(&id)
}

#[tauri::command]
fn set_breakpoints(eng: State<'_, Arc<Engine>>, automation_id: String, node_ids: Vec<String>) -> Result<(), String> {
    eng.set_breakpoints(&automation_id, node_ids)
}

#[tauri::command]
fn debug_pause(eng: State<'_, Arc<Engine>>, id: String) -> Result<(), String> {
    eng.debug_pause(&id)
}

#[tauri::command]
fn debug_resume(eng: State<'_, Arc<Engine>>, id: String) {
    eng.debug_resume(&id);
}

#[tauri::command]
fn debug_step(eng: State<'_, Arc<Engine>>, id: String) -> Result<(), String> {
    eng.debug_step(&id)
}

#[tauri::command]
async fn start_automation(eng: State<'_, Arc<Engine>>, id: String) -> Result<(), String> {
    let e = eng.inner().clone();
    e.start(&id)
}

#[tauri::command]
async fn stop_automation(eng: State<'_, Arc<Engine>>, id: String) -> Result<(), String> {
    eng.stop(&id);
    Ok(())
}

#[tauri::command]
async fn restart_automation(eng: State<'_, Arc<Engine>>, id: String) -> Result<(), String> {
    let e = eng.inner().clone();
    e.restart(&id).await
}

#[tauri::command]
fn get_runtime(eng: State<'_, Arc<Engine>>) -> Runtime {
    eng.runtime()
}

#[tauri::command]
fn get_logs(eng: State<'_, Arc<Engine>>, since: Option<u64>, auto_id: Option<String>) -> Vec<LogEntry> {
    eng.logs(since.unwrap_or(0), auto_id)
}

#[tauri::command]
fn get_history(eng: State<'_, Arc<Engine>>, id: String) -> Vec<RunRecord> {
    eng.history(&id)
}

#[tauri::command]
fn get_workflow_variables(eng: State<'_, Arc<Engine>>, id: String) -> serde_json::Value {
    eng.workflow_variables(&id)
}

#[tauri::command]
fn clear_logs(eng: State<'_, Arc<Engine>>) {
    eng.clear_logs();
}

#[tauri::command]
fn app_stats(st: State<'_, Mutex<sys::CpuState>>) -> sys::AppStats {
    sys::app_stats(&mut st.lock().unwrap())
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> Result<bool, String> {
    app.autolaunch().is_enabled().map_err(|e| e.to_string())
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let m = app.autolaunch();
    (if enabled { m.enable() } else { m.disable() }).map_err(|e| e.to_string())
}

#[tauri::command]
fn hide_to_tray(app: AppHandle, eng: State<'_, Arc<Engine>>) {
    save_window(&app);
    if let Some(w) = app.get_webview_window("main") {
        #[cfg(target_os = "windows")]
        set_native_blur_visible(&w, false);
        let _ = w.hide();
        #[cfg(target_os = "windows")]
        let _ = w.with_webview(|webview| unsafe {
            let _ = webview.controller().SetIsVisible(false);
        });
        // Suspend the renderer after hiding it. This pauses page timers/animations
        // and lets WebView2 reclaim memory while Rust automations keep running.
        #[cfg(target_os = "windows")]
        suspend_native_webview(&w);
    }
    eng.set_ui(false);
}

#[tauri::command]
async fn exit_app(app: AppHandle, eng: State<'_, Arc<Engine>>) -> Result<(), String> {
    save_window(&app);
    let e = eng.inner().clone();
    e.shutdown().await;
    app.exit(0);
    Ok(())
}

#[tauri::command]
async fn test_block(eng: State<'_, Arc<Engine>>, auto: Automation, node_id: String) -> Result<String, String> {
    let engine = eng.inner().clone();
    engine.test_block(auto, node_id).await
}

fn save_window(app: &AppHandle) {
    let _ = app.save_window_state(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED);
}

#[tauri::command]
async fn pick_file(app: AppHandle) -> Option<String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().pick_file(move |p| {
        let _ = tx.send(p);
    });
    let p = rx.await.ok()??;
    p.into_path().ok().map(|p| p.to_string_lossy().to_string())
}

#[tauri::command]
async fn export_automation(app: AppHandle, eng: State<'_, Arc<Engine>>, id: String) -> Result<bool, String> {
    let a = eng.store.get(&id).ok_or_else(|| "automation not found".to_string())?;
    let safe: String = a.name.chars().map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c }).collect();
    let safe = safe.trim().trim_end_matches('.').to_string();
    let safe = if safe.is_empty() { "automation".to_string() } else { safe };
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .add_filter("00047 automation", &["json"])
        .set_file_name(format!("{safe}.json"))
        .save_file(move |p| {
            let _ = tx.send(p);
        });
    let Some(p) = rx.await.map_err(|e| e.to_string())? else { return Ok(false) };
    let path = p.into_path().map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(&a).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| format!("write file: {e}"))?;
    Ok(true)
}

#[tauri::command]
async fn import_automation(app: AppHandle, eng: State<'_, Arc<Engine>>) -> Result<Option<Automation>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().add_filter("00047 automation", &["json"]).pick_file(move |p| {
        let _ = tx.send(p);
    });
    let Some(p) = rx.await.map_err(|e| e.to_string())? else { return Ok(None) };
    let path = p.into_path().map_err(|e| e.to_string())?;
    let bytes = std::fs::read(&path).map_err(|e| format!("read file: {e}"))?;
    let mut a: Automation = serde_json::from_slice(&bytes).map_err(|e| format!("not a valid 00047 automation: {e}"))?;
    if a.nodes.len() > 2000 {
        return Err("automation is too large (over 2000 blocks)".into());
    }
    let base = engine::now_ms();
    let mut suffix = 0u32;
    loop {
        let candidate = if suffix == 0 { format!("imp-{base}") } else { format!("imp-{base}-{suffix}") };
        if eng.store.get(&candidate).is_none() {
            a.id = candidate;
            break;
        }
        suffix = suffix.saturating_add(1);
    }
    a.auto_start = false;
    eng.store.upsert(a.clone())?;
    Ok(Some(a))
}

#[tauri::command]
async fn export_logs(app: AppHandle, csv: String) -> Result<bool, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().add_filter("CSV", &["csv"]).set_file_name("00047-logs.csv")
        .save_file(move |p| { let _ = tx.send(p); });
    let Some(p) = rx.await.map_err(|e| e.to_string())? else { return Ok(false) };
    let path = p.into_path().map_err(|e| e.to_string())?;
    std::fs::write(path, format!("\u{feff}{csv}")).map_err(|e| format!("write file: {e}"))?;
    Ok(true)
}

#[tauri::command]
fn list_block_specs() -> serde_json::Value {
    custom::all_specs()
}

#[tauri::command]
async fn block_pack_preview(app: AppHandle) -> Result<Option<custom::PackPreview>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().add_filter("00047 script plugin pack", &["zip"]).pick_file(move |p| {
        let _ = tx.send(p);
    });
    let Some(p) = rx.await.map_err(|e| e.to_string())? else { return Ok(None) };
    let path = p.into_path().map_err(|e| e.to_string())?;
    custom::preview(&path).map(Some)
}

#[tauri::command]
fn block_pack_install(path: String) -> Result<String, String> {
    custom::install(std::path::Path::new(&path))
}

#[tauri::command]
fn open_blocks_folder() -> Result<(), String> {
    let d = custom::blocks_dir().ok_or_else(|| "blocks folder is not initialised".to_string())?;
    let _ = std::fs::create_dir_all(d);
    sys::start_file(&d.to_string_lossy(), "")
}

#[tauri::command]
fn list_extensions(ext: State<'_, extensions::ExtensionManager>) -> extensions::ExtensionList {
    ext.list()
}

#[tauri::command]
async fn extension_pack_preview(
    app: AppHandle,
    ext: State<'_, extensions::ExtensionManager>,
) -> Result<Option<extensions::PackagePreview>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().add_filter("00047 extension package", &["zip"]).pick_file(move |p| {
        let _ = tx.send(p);
    });
    let Some(p) = rx.await.map_err(|e| e.to_string())? else { return Ok(None) };
    let path = p.into_path().map_err(|e| e.to_string())?;
    ext.preview(&path).map(Some)
}

#[tauri::command]
fn extension_pack_install(ext: State<'_, extensions::ExtensionManager>, path: String) -> Result<String, String> {
    ext.install(std::path::Path::new(&path))
}

#[tauri::command]
async fn extension_start(
    ext: State<'_, extensions::ExtensionManager>,
    package_id: String,
    component_id: String,
) -> Result<(), String> {
    ext.start(&package_id, &component_id).await
}

#[tauri::command]
async fn extension_run_command(
    ext: State<'_, extensions::ExtensionManager>,
    package_id: String,
    component_id: String,
) -> Result<extensions::ExtensionCommandResult, String> {
    ext.run_command(&package_id, &component_id).await
}

#[tauri::command]
async fn extension_stop(
    ext: State<'_, extensions::ExtensionManager>,
    package_id: String,
    component_id: String,
) -> Result<(), String> {
    ext.stop(&package_id, &component_id).await
}

#[tauri::command]
async fn extension_restart(
    ext: State<'_, extensions::ExtensionManager>,
    package_id: String,
    component_id: String,
) -> Result<(), String> {
    ext.restart(&package_id, &component_id).await
}

#[tauri::command]
async fn extension_uninstall(
    ext: State<'_, extensions::ExtensionManager>,
    package_id: String,
) -> Result<(), String> {
    ext.uninstall(&package_id).await
}

#[tauri::command]
fn open_extensions_folder(ext: State<'_, extensions::ExtensionManager>) -> Result<(), String> {
    let dir = ext.root_path();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    sys::start_file(&dir.to_string_lossy(), "")
}

#[tauri::command]
fn list_native_mods(mods: State<'_, Arc<native_mods::NativeModManager>>) -> native_mods::NativeModList {
    mods.list()
}

#[tauri::command]
async fn native_mod_pack_preview(
    app: AppHandle,
    mods: State<'_, Arc<native_mods::NativeModManager>>,
) -> Result<Option<native_mods::NativeModPreview>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog().file().add_filter("00047 native mod package", &["zip"]).pick_file(move |p| {
        let _ = tx.send(p);
    });
    let Some(p) = rx.await.map_err(|e| e.to_string())? else { return Ok(None) };
    let path = p.into_path().map_err(|e| e.to_string())?;
    mods.preview(&path).map(Some)
}

#[tauri::command]
fn native_mod_pack_install(
    mods: State<'_, Arc<native_mods::NativeModManager>>,
    path: String,
) -> Result<String, String> {
    mods.install(std::path::Path::new(&path))
}

#[tauri::command]
fn native_mod_enable(mods: State<'_, Arc<native_mods::NativeModManager>>, id: String) -> Result<String, String> {
    mods.enable(&id)
}

#[tauri::command]
fn native_mod_disable(mods: State<'_, Arc<native_mods::NativeModManager>>, id: String) -> Result<String, String> {
    mods.disable(&id)
}

#[tauri::command]
fn native_mod_uninstall(mods: State<'_, Arc<native_mods::NativeModManager>>, id: String) -> Result<(), String> {
    mods.uninstall(&id)
}

#[tauri::command]
fn native_mod_refresh(mods: State<'_, Arc<native_mods::NativeModManager>>) -> native_mods::NativeModList {
    // Reconciliation runs in the background monitor. Keep this command read-only
    // so the UI can poll and display intermediate per-target lifecycle states.
    mods.list()
}

#[tauri::command]
fn open_native_mods_folder(mods: State<'_, Arc<native_mods::NativeModManager>>) -> Result<(), String> {
    let dir = mods.root_path();
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    sys::start_file(&dir.to_string_lossy(), "")
}

#[tauri::command]
fn native_mod_settings_read(
    mods: State<'_, Arc<native_mods::NativeModManager>>,
    id: String,
) -> Result<native_mod_config::NativeModSettings, String> {
    native_mod_config::read_settings(mods.root_path(), &id)
}

#[tauri::command]
fn native_mod_settings_write(
    mods: State<'_, Arc<native_mods::NativeModManager>>,
    id: String,
    values: serde_json::Value,
) -> Result<native_mod_config::NativeModSettings, String> {
    native_mod_config::write_settings(mods.root_path(), &id, values)
}

#[tauri::command]
fn native_mod_logs_read(
    mods: State<'_, Arc<native_mods::NativeModManager>>,
    id: String,
) -> Result<Vec<String>, String> {
    native_mod_config::read_logs(mods.root_path(), &id)
}

#[tauri::command]
async fn restart_app(
    app: AppHandle,
    eng: State<'_, Arc<Engine>>,
    ext: State<'_, extensions::ExtensionManager>,
) -> Result<(), String> {
    save_window(&app);
    let e = eng.inner().clone();
    e.shutdown().await;
    ext.shutdown().await;
    app.restart()
}

// ---------- helpers ----------
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        #[cfg(target_os = "windows")]
        resume_native_webview(&w);
        let _ = w.show();
        #[cfg(target_os = "windows")]
        let _ = w.with_webview(|webview| unsafe {
            let _ = webview.controller().SetIsVisible(true);
        });
        #[cfg(target_os = "windows")]
        set_native_blur_visible(&w, true);
        let _ = w.set_focus();
    }
    if let Some(e) = app.try_state::<Arc<Engine>>() {
        e.set_ui(true);
    }
}

fn quit_app(app: &AppHandle) {
    save_window(app);
    let app = app.clone();
    let eng = app.state::<Arc<Engine>>().inner().clone();
    tauri::async_runtime::spawn(async move {
        eng.shutdown().await;
        app.exit(0);
    });
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_window_state::Builder::default()
            .with_state_flags(StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED)
            .build())
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.emit("close-requested", ());
            }
            tauri::WindowEvent::Resized(_) => {
                if let Some(e) = window.app_handle().try_state::<Arc<Engine>>() {
                    let min = window.is_minimized().unwrap_or(false);
                    let vis = window.is_visible().unwrap_or(false);
                    e.set_ui(vis && !min);
                }
                #[cfg(target_os = "windows")]
                if let Some(w) = window.app_handle().get_webview_window(window.label()) {
                    reposition_native_blur(&w);
                }
            }
            tauri::WindowEvent::Moved(_) | tauri::WindowEvent::Focused(true) => {
                #[cfg(target_os = "windows")]
                if let Some(w) = window.app_handle().get_webview_window(window.label()) {
                    reposition_native_blur(&w);
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            update_windhawk_blur,
            vault_list_secrets,
            vault_set_secret,
            vault_delete_secret,
            list_automations,
            save_automation,
            delete_automation,
            start_automation,
            set_breakpoints,
            debug_pause,
            debug_resume,
            debug_step,
            stop_automation,
            restart_automation,
            get_runtime,
            get_logs,
            get_history,
            get_workflow_variables,
            clear_logs,
            app_stats,
            get_autostart,
            set_autostart,
            hide_to_tray,
            exit_app,
            pick_file,
            export_automation,
            import_automation,
            export_logs,
            test_block,
            list_block_specs,
            block_pack_preview,
            block_pack_install,
            open_blocks_folder,
            list_extensions,
            extension_pack_preview,
            extension_pack_install,
            extension_start,
            extension_run_command,
            extension_stop,
            extension_restart,
            extension_uninstall,
            open_extensions_folder,
            list_native_mods,
            native_mod_pack_preview,
            native_mod_pack_install,
            native_mod_enable,
            native_mod_disable,
            native_mod_uninstall,
            native_mod_refresh,
            open_native_mods_folder,
            native_mod_settings_read,
            native_mod_settings_write,
            native_mod_logs_read,
            restart_app
        ])
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            vault::init(dir.join("secrets")).map_err(std::io::Error::other)?;
            custom::init(dir.join("blocks"));
            let _ = std::fs::create_dir_all(dir.join("blocks"));
            app.manage(extensions::ExtensionManager::new(dir.join("extensions")));
            let native_mod_manager = Arc::new(native_mods::NativeModManager::new(dir.join("native-mods")));
            app.manage(native_mod_manager.clone());
            // Reapply explicitly enabled mods only to the fixed shell-process allow-list.
            let native_mod_monitor = native_mod_manager.clone();
            tauri::async_runtime::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(5));
                loop {
                    ticker.tick().await;
                    let manager = native_mod_monitor.clone();
                    let _ = tokio::task::spawn_blocking(move || manager.apply_enabled_mods()).await;
                }
            });
            let (store, warn) = Store::load(&dir);
            let engine = Arc::new(Engine::new(app.handle().clone(), Arc::new(store), &dir));
            app.manage(engine.clone());
            app.manage(Mutex::new(sys::CpuState::default()));
            engine.app_log("info", format!("00047 started (data folder: {})", dir.display()));
            if let Some(w) = warn {
                engine.app_log("error", w);
            }

            let show = MenuItem::with_id(app, "show", "Show 00047", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Exit (stops all automations)", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().ok_or("missing window icon: run `cargo tauri icon app-icon.png`")?.clone())
                .tooltip("00047")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, ev| match ev.id.as_ref() {
                    "show" => show_main(app),
                    "quit" => quit_app(app),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;

            if !std::env::args().any(|a| a == "--minimized") {
                show_main(app.handle());
            }
            engine.start_autostart();
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build 00047");

    app.run(|handle, event| {
        if let tauri::RunEvent::Exit = event {
            #[cfg(target_os = "windows")]
            if let Some(w) = handle.get_webview_window("main") {
                destroy_native_blur(&w);
            }
            if let Some(ext) = handle.try_state::<extensions::ExtensionManager>() {
                tauri::async_runtime::block_on(ext.shutdown());
            }
            if let Some(e) = handle.try_state::<Arc<Engine>>() {
                let e = e.inner().clone();
                tauri::async_runtime::block_on(e.shutdown());
            }
        }
    });
}
