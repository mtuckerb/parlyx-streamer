//! Parlyx Streamer — Tauri entry point. The runtime wires the audio thread
//! to a Tokio task that POSTs chunks to parlyx, subscribes to the SSE event
//! feed, and forwards transcript / diarization events back to the web UI via
//! the Tauri event bus.

mod audio;
mod parlyx;
mod settings;
mod session;
mod commands;

use std::sync::Arc;
use tauri::Manager;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "parlyx_streamer=info,warn".into()),
        )
        .init();

    let app_state = Arc::new(session::AppState::new());

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::list_input_devices,
            commands::load_settings,
            commands::save_settings,
            commands::start_streaming,
            commands::pause_streaming,
            commands::resume_streaming,
            commands::stop_streaming,
            commands::update_segment,
            commands::current_state,
        ])
        .setup(|app| {
            // Hide window to tray when closed instead of exiting; keeps audio
            // capture + streaming alive in the background.
            let window = app.get_webview_window("main").unwrap();
            let app_handle = app.handle().clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if let Some(w) = app_handle.get_webview_window("main") {
                        let _ = w.hide();
                    }
                    api.prevent_close();
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running parlyx-streamer");
}
