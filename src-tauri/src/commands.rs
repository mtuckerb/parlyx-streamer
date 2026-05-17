//! Tauri command handlers. Each is invoked from the web UI via `invoke()`.

use crate::audio::{self, AudioDevice};
use crate::parlyx::ParlyxClient;
use crate::session::{set_status, AppState, SessionRunner, SessionStatus};
use crate::settings::{self, Settings};

use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
pub fn list_input_devices() -> Result<Vec<AudioDevice>, String> {
    audio::list_input_devices().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn load_settings() -> Result<Settings, String> {
    settings::load().map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn save_settings(settings: Settings) -> Result<(), String> {
    settings::save(&settings).map_err(|e| format!("{e:#}"))
}

#[derive(Debug, serde::Deserialize)]
pub struct StartArgs {
    pub title: Option<String>,
    pub min_speakers: Option<u32>,
    pub max_speakers: Option<u32>,
    pub device_name: Option<String>,
}

#[tauri::command]
pub async fn start_streaming(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    args: StartArgs,
) -> Result<SessionStatus, String> {
    {
        let guard = state.current.lock();
        if guard.is_some() {
            return Err("a session is already active".into());
        }
    }

    let settings = settings::load().map_err(|e| format!("{e:#}"))?;
    if settings.parlyx_server_base_url.is_empty() {
        return Err("parlyx_server_base_url is not set".into());
    }
    if settings.api_key.is_empty() {
        return Err("api_key is not set".into());
    }

    let state_arc: Arc<AppState> = Arc::clone(&state);
    set_status(&state_arc, &app, SessionStatus::Starting);

    let client = ParlyxClient::new(settings.parlyx_server_base_url, settings.api_key);
    let runner = SessionRunner::start(
        client,
        app.clone(),
        args.title,
        args.min_speakers,
        args.max_speakers,
        settings.webhook_url,
        args.device_name,
    )
    .await
    .map_err(|e| format!("{e:#}"))?;

    let status = SessionStatus::Recording {
        stream_id: runner.stream_id.clone(),
        task_id: runner.task_id.clone(),
    };
    {
        let mut guard = state.current.lock();
        *guard = Some(runner);
    }
    set_status(&state_arc, &app, status.clone());
    Ok(status)
}

#[tauri::command]
pub fn pause_streaming(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<SessionStatus, String> {
    let guard = state.current.lock();
    let Some(runner) = guard.as_ref() else {
        return Err("no active session".into());
    };
    runner.pause();
    let status = SessionStatus::Paused {
        stream_id: runner.stream_id.clone(),
        task_id: runner.task_id.clone(),
    };
    drop(guard);
    let state_arc: Arc<AppState> = Arc::clone(&state);
    set_status(&state_arc, &app, status.clone());
    Ok(status)
}

#[tauri::command]
pub fn resume_streaming(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<SessionStatus, String> {
    let guard = state.current.lock();
    let Some(runner) = guard.as_ref() else {
        return Err("no active session".into());
    };
    runner.resume();
    let status = SessionStatus::Recording {
        stream_id: runner.stream_id.clone(),
        task_id: runner.task_id.clone(),
    };
    drop(guard);
    let state_arc: Arc<AppState> = Arc::clone(&state);
    set_status(&state_arc, &app, status.clone());
    Ok(status)
}

#[tauri::command]
pub async fn stop_streaming(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
) -> Result<SessionStatus, String> {
    let state_arc: Arc<AppState> = Arc::clone(&state);
    set_status(&state_arc, &app, SessionStatus::Stopping);
    let runner = {
        let mut guard = state.current.lock();
        guard.take()
    };
    let Some(runner) = runner else {
        return Err("no active session".into());
    };
    let task_id = runner.stop().await.map_err(|e| format!("{e:#}"))?;
    let status = SessionStatus::Stopped {
        task_id: Some(task_id),
    };
    set_status(&state_arc, &app, status.clone());
    Ok(status)
}

#[derive(Debug, serde::Deserialize)]
pub struct UpdateSegmentArgs {
    pub stream_id: String,
    pub segment_id: String,
    pub text: Option<String>,
    pub speaker: Option<String>,
}

#[tauri::command]
pub async fn update_segment(args: UpdateSegmentArgs) -> Result<(), String> {
    let settings = settings::load().map_err(|e| format!("{e:#}"))?;
    let client = ParlyxClient::new(settings.parlyx_server_base_url, settings.api_key);
    client
        .update_segment(&args.stream_id, &args.segment_id, args.text, args.speaker)
        .await
        .map_err(|e| format!("{e:#}"))
}

#[tauri::command]
pub fn current_state(state: State<'_, Arc<AppState>>) -> SessionStatus {
    state.status.lock().clone()
}
