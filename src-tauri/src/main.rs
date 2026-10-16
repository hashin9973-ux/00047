#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blocks;
mod engine;
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
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.hide();
    }
    eng.set_ui(false);
}

#[tauri::command]
async fn exit_app(app: AppHandle, eng: State<'_, Arc<Engine>>) -> Result<(), String> {
    let e = eng.inner().clone();
    e.shutdown().await;
    app.exit(0);
    Ok(())
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
            exit_app
        ])
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let (store, warn) = Store::load(&dir);
            let engine = Arc::new(Engine::new(app.handle().clone(), Arc::new(store)));
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
