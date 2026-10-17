#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blocks;
mod custom;
mod spec;
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


#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct GlassCaptureFrame {
    data_url: String,
    screen_x: i32,
    screen_y: i32,
    screen_width: u32,
    screen_height: u32,
    window_x: i32,
    window_y: i32,
}


#[tauri::command]
fn capture_glass_frame(window: tauri::WebviewWindow) -> Result<GlassCaptureFrame, String> {
    #[cfg(target_os = "windows")]
    {
        capture_glass_frame_windows(&window)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = window;
        Err("Live glass reflection capture is available on Windows only.".to_string())
    }
}

#[cfg(target_os = "windows")]
fn capture_glass_frame_windows(window: &tauri::WebviewWindow) -> Result<GlassCaptureFrame, String> {
    use base64::Engine as _;
    use image::codecs::jpeg::JpegEncoder;
    use image::{ExtendedColorType, RgbImage};
    use std::{ffi::c_void, mem::size_of, ptr::{null, null_mut}, slice, sync::atomic::{AtomicBool, Ordering}};

    // Display affinity is process/window setup, not per-frame work.
    static DISPLAY_AFFINITY_ATTEMPTED: AtomicBool = AtomicBool::new(false);

    // GDI is used only to acquire the real desktop pixels. The captured pixels
    // are downsampled and JPEG encoded before they cross the Tauri IPC bridge.
    #[repr(C)]
    struct BitmapInfoHeader {
        size: u32,
        width: i32,
        height: i32,
        planes: u16,
        bit_count: u16,
        compression: u32,
        size_image: u32,
        x_pels_per_meter: i32,
        y_pels_per_meter: i32,
        colors_used: u32,
        colors_important: u32,
    }
    #[repr(C)]
    struct RgbQuad { blue: u8, green: u8, red: u8, reserved: u8 }
    #[repr(C)]
    struct BitmapInfo { header: BitmapInfoHeader, colors: [RgbQuad; 1] }

    #[link(name = "user32")]
    extern "system" {
        fn GetDC(hwnd: *mut c_void) -> *mut c_void;
        fn ReleaseDC(hwnd: *mut c_void, hdc: *mut c_void) -> i32;
        fn SetWindowDisplayAffinity(hwnd: *mut c_void, affinity: u32) -> i32;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn CreateCompatibleDC(hdc: *mut c_void) -> *mut c_void;
        fn CreateDIBSection(
            hdc: *mut c_void, info: *const BitmapInfo, usage: u32,
            pixels: *mut *mut c_void, section: *mut c_void, offset: u32
        ) -> *mut c_void;
        fn SelectObject(hdc: *mut c_void, object: *mut c_void) -> *mut c_void;
        fn StretchBlt(
            dest: *mut c_void, x: i32, y: i32, width: i32, height: i32,
            source: *mut c_void, source_x: i32, source_y: i32,
            source_width: i32, source_height: i32, rop: u32
        ) -> i32;
        fn DeleteObject(object: *mut c_void) -> i32;
        fn DeleteDC(hdc: *mut c_void) -> i32;
    }

    let window_pos = window.inner_position().map_err(|e| format!("Window position unavailable: {e}"))?;
    let window_size = window.inner_size().map_err(|e| format!("Window size unavailable: {e}"))?;
    let margin = 96i32;
    let screen_x = window_pos.x.saturating_sub(margin);
    let screen_y = window_pos.y.saturating_sub(margin);
    let screen_width = window_size.width.saturating_add((margin * 2) as u32).max(1);
    let screen_height = window_size.height.saturating_add((margin * 2) as u32).max(1);

    // Keep IPC and capture costs bounded even when the window is maximized on
    // a high-DPI 4K display. CSS scales this sample back to desktop coordinates.
    // A slightly smaller sample has materially less GDI, JPEG and IPC work;
    // the 12px glass blur hides most source-detail loss while improving cadence.
    const MAX_CAPTURE_WIDTH: f64 = 1024.0;
    const MAX_CAPTURE_HEIGHT: f64 = 640.0;
    let scale = (MAX_CAPTURE_WIDTH / screen_width as f64)
        .min(MAX_CAPTURE_HEIGHT / screen_height as f64)
        .min(1.0);
    let bitmap_width = ((screen_width as f64 * scale).round() as i32).max(1);
    let bitmap_height = ((screen_height as f64 * scale).round() as i32).max(1);

    let hwnd = window.hwnd().map_err(|e| format!("Native window handle unavailable: {e}"))?.0;
    unsafe {
        // Best-effort once: some transparent/layered windows reject display affinity.
        // Never spend a native API call on every animation frame or abort capture
        // if exclusion is unavailable.
        if !DISPLAY_AFFINITY_ATTEMPTED.swap(true, Ordering::Relaxed) {
            let _capture_exclusion_enabled = SetWindowDisplayAffinity(hwnd, 0x00000011) != 0;
        }

        let screen_dc = GetDC(null_mut());
        if screen_dc.is_null() {
            return Err("Could not open the desktop capture surface.".to_string());
        }
        let memory_dc = CreateCompatibleDC(screen_dc);
        if memory_dc.is_null() {
            ReleaseDC(null_mut(), screen_dc);
            return Err("Could not create a desktop capture buffer.".to_string());
        }

        let mut bitmap_info = BitmapInfo {
            header: BitmapInfoHeader {
                size: size_of::<BitmapInfoHeader>() as u32,
                width: bitmap_width,
                height: -bitmap_height, // top-down DIB, matching screen coordinates
                planes: 1,
                bit_count: 32,
                compression: 0, // BI_RGB
                size_image: (bitmap_width as u32).saturating_mul(bitmap_height as u32).saturating_mul(4),
                x_pels_per_meter: 0,
                y_pels_per_meter: 0,
                colors_used: 0,
                colors_important: 0,
            },
            colors: [RgbQuad { blue: 0, green: 0, red: 0, reserved: 0 }],
        };
        let mut pixels: *mut c_void = null_mut();
        let bitmap = CreateDIBSection(
            screen_dc, &bitmap_info, 0, &mut pixels, null_mut(), 0
        );
        if bitmap.is_null() || pixels.is_null() {
            DeleteDC(memory_dc);
            ReleaseDC(null_mut(), screen_dc);
            return Err("Could not allocate the desktop reflection texture.".to_string());
        }

        let old_object = SelectObject(memory_dc, bitmap);
        if old_object.is_null() || old_object as isize == -1 {
            DeleteObject(bitmap);
            DeleteDC(memory_dc);
            ReleaseDC(null_mut(), screen_dc);
            return Err("Could not select the desktop reflection texture.".to_string());
        }

        // SRCCOPY | CAPTUREBLT includes layered desktop windows as well.
        let copied = StretchBlt(
            memory_dc, 0, 0, bitmap_width, bitmap_height,
            screen_dc, screen_x, screen_y, screen_width as i32, screen_height as i32,
            0x00CC0020 | 0x40000000
        );
        if copied == 0 {
            SelectObject(memory_dc, old_object);
            DeleteObject(bitmap);
            DeleteDC(memory_dc);
            ReleaseDC(null_mut(), screen_dc);
            return Err("Windows failed to copy desktop pixels for the reflection.".to_string());
        }

        let bgra = slice::from_raw_parts(
            pixels.cast::<u8>(),
            bitmap_width as usize * bitmap_height as usize * 4
        );
        let mut rgb = Vec::with_capacity(bitmap_width as usize * bitmap_height as usize * 3);
        for px in bgra.chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }

        SelectObject(memory_dc, old_object);
        DeleteObject(bitmap);
        DeleteDC(memory_dc);
        ReleaseDC(null_mut(), screen_dc);

        let image = RgbImage::from_raw(bitmap_width as u32, bitmap_height as u32, rgb)
            .ok_or_else(|| "Captured desktop frame had an invalid pixel buffer.".to_string())?;
        let mut small = Vec::new();
        JpegEncoder::new_with_quality(&mut small, 50)
            .encode(image.as_raw(), image.width(), image.height(), ExtendedColorType::Rgb8)
            .map_err(|e| format!("Could not encode desktop reflection frame: {e}"))?;

        Ok(GlassCaptureFrame {
            data_url: format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(small)),
            screen_x,
            screen_y,
            screen_width,
            screen_height,
            window_x: window_pos.x,
            window_y: window_pos.y,
        })
    }
}


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
    e.stop_and_wait(&id).await?;
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
        // Hide the native window and its WebView2 controller together to avoid a stale compositor surface.
        let _ = w.hide();
        #[cfg(target_os = "windows")]
        let _ = w.with_webview(|webview| unsafe {
            let _ = webview.controller().SetIsVisible(false);
        });
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
    app.dialog().file().add_filter("00047 block pack", &["json"]).pick_file(move |p| {
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
async fn restart_app(app: AppHandle, eng: State<'_, Arc<Engine>>) -> Result<(), String> {
    save_window(&app);
    let e = eng.inner().clone();
    e.shutdown().await;
    app.restart()
}

// ---------- helpers ----------
fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        #[cfg(target_os = "windows")]
        {
            let _ = w.with_webview(|webview| unsafe {
                let _ = webview.controller().SetIsVisible(true);
            });
        }
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
            capture_glass_frame,
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
            restart_app
        ])
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            custom::init(dir.join("blocks"));
            let _ = std::fs::create_dir_all(dir.join("blocks"));
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
