#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blocks;
mod engine;
mod logstore;
mod model;
mod store;
mod sys;

use engine::{Engine, Runtime};
use model::{Automation, LogEntry, RunRecord};
use std::sync::{Arc, Mutex};
use store::Store;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, State,
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_window_state::{AppHandleExt, StateFlags};

// ---------- commands ----------
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
    e.stop_and_wait(&id).await;
    e.remove(&id)
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
        let _ = w.hide();
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

// ---------- helpers ----------
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
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
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            list_automations,
            save_automation,
            delete_automation,
            start_automation,
            stop_automation,
            restart_automation,
            get_runtime,
            get_logs,
            get_history,
            clear_logs,
            app_stats,
            get_autostart,
            set_autostart,
            hide_to_tray,
            exit_app,
            pick_file,
            export_automation,
            import_automation,
            test_block
        ])
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
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
            if let Some(e) = handle.try_state::<Arc<Engine>>() {
                let e = e.inner().clone();
                tauri::async_runtime::block_on(e.shutdown());
            }
        }
    });
}
