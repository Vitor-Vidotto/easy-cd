#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod vision;

use device_query::{DeviceQuery, DeviceState, Keycode};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Size, State, WebviewUrl,
    WebviewWindowBuilder,
};
use vision::{
    capture_single_preview_snapshot, get_virtual_screen_bounds, init_dpi_awareness,
    spawn_vision_loop, RegionBBox, SharedVisionState, VisionStateHandle, VisionStateSnapshot,
};

#[tauri::command]
async fn get_vision_state(
    state: State<'_, VisionStateHandle>,
) -> Result<VisionStateSnapshot, String> {
    let handle = state.inner().clone();
    Ok(capture_single_preview_snapshot(&handle))
}

#[tauri::command]
async fn open_region_selector(app: AppHandle) -> Result<(), String> {
    if let Some(existing) = app.get_webview_window("selector") {
        let _ = existing.close();
    }

    if let Some(main_win) = app.get_webview_window("easycd") {
        let _ = main_win.hide();
    }

    std::thread::sleep(Duration::from_millis(180));

    let (vx, vy, vw, vh) = get_virtual_screen_bounds();
    let url_str = format!("selector.html?vx={}&vy={}&vw={}&vh={}", vx, vy, vw, vh);

    let selector_win = WebviewWindowBuilder::new(&app, "selector", WebviewUrl::App(url_str.into()))
        .title("Selecionar Barra de Skills")
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .focused(true)
        .build()
        .map_err(|e| format!("Falha ao abrir seletor: {}", e))?;

    let _ = selector_win.set_position(Position::Physical(PhysicalPosition { x: vx, y: vy }));
    let _ = selector_win.set_size(Size::Physical(PhysicalSize {
        width: vw as u32,
        height: vh as u32,
    }));
    let _ = selector_win.set_focus();

    Ok(())
}

#[tauri::command]
async fn finish_region_selection(
    app: AppHandle,
    state: State<'_, VisionStateHandle>,
    bbox: Option<RegionBBox>,
) -> Result<VisionStateSnapshot, String> {
    if let Some(sel) = app.get_webview_window("selector") {
        let _ = sel.close();
    }

    std::thread::sleep(Duration::from_millis(100));

    if let Some(valid_box) = bbox {
        if let Ok(mut guard) = state.lock() {
            guard.save_bbox(Some(valid_box));
        }
    }

    let snapshot = capture_single_preview_snapshot(state.inner());

    if let Some(main_win) = app.get_webview_window("easycd") {
        let _ = main_win.show();
        let _ = main_win.set_focus();
    }

    let _ = app.emit("cv-region-updated", snapshot.clone());
    Ok(snapshot)
}

#[tauri::command]
fn set_tracking_active(state: State<'_, VisionStateHandle>, active: bool) -> Result<(), String> {
    let mut guard = state.lock().map_err(|_| "Falha ao acessar estado")?;
    guard.is_tracking = active;
    if active {
        guard.reset_streaks();
    }
    Ok(())
}

// Alterna as bordas da janela (decorations) e permite clicar através com F2
fn toggle_decorations(app: &AppHandle, vision_state: &VisionStateHandle) {
    let Some(window) = app.get_webview_window("easycd") else {
        return;
    };
    let is_decorated = window.is_decorated().unwrap_or(true);
    let _ = window.set_decorations(!is_decorated);

    if !is_decorated {
        // Destravando: volta a aceitar cliques e exibe controles
        let _ = window.set_ignore_cursor_events(false);
        if let Ok(mut guard) = vision_state.lock() {
            guard.is_overlay_locked = false;
        }
        let _ = app.emit("exibir", None::<()>);
    } else {
        // Travando: click-through por cima do Albion e esconde controles
        let _ = window.set_ignore_cursor_events(true);
        if let Ok(mut guard) = vision_state.lock() {
            guard.is_overlay_locked = true;
        }
        let _ = app.emit("esconder", None::<()>);
    }
}

fn monitor_keyboard(app: AppHandle, vision_state: VisionStateHandle) {
    let device_state = DeviceState::new();
    loop {
        let keys = device_state.get_keys();
        if keys.contains(&Keycode::F2) {
            toggle_decorations(&app, &vision_state);
            std::thread::sleep(Duration::from_millis(500));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn main() {
    init_dpi_awareness();

    tauri::Builder::default()
        .setup(|app| {
            let config_dir = app
                .path()
                .app_config_dir()
                .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default());

            let vision_state: VisionStateHandle =
                Arc::new(Mutex::new(SharedVisionState::new(config_dir)));
            app.manage(vision_state.clone());

            // Inicia o loop de visão computacional + OCR do Windows em background
            spawn_vision_loop(app.handle().clone(), vision_state.clone());

            // Inicia o listener de F2 (travar / destravar overlay)
            let app_handle = app.handle().clone();
            let vs_kb = vision_state.clone();
            std::thread::spawn(move || {
                monitor_keyboard(app_handle, vs_kb);
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_vision_state,
            open_region_selector,
            finish_region_selection,
            set_tracking_active
        ])
        .run(tauri::generate_context!())
        .expect("Erro ao rodar o aplicativo Easy CD");
}
