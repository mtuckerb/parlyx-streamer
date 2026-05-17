//! Session lifecycle: orchestrates audio capture, chunk shipping, and the
//! SSE event subscription. One `SessionRunner` per active recording.

use crate::audio::{self, encode_wav, CaptureHandle};
use crate::parlyx::{ParlyxClient, StreamEvent};
use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use parking_lot::Mutex as PlMutex;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Tauri event name used to forward parlyx SSE events to the web UI.
pub const EVENT_STREAM: &str = "parlyx://stream-event";
/// Status updates (started, paused, stopped, error) for the web UI.
pub const EVENT_STATUS: &str = "parlyx://status";
/// Per-chunk + per-SSE-event "I'm alive" pings so the UI can show a counter.
/// Payload is the current cumulative count.
pub const EVENT_CHUNK_SENT: &str = "parlyx://chunk-sent";
pub const EVENT_EVENT_RECEIVED: &str = "parlyx://event-received";

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "status")]
pub enum SessionStatus {
    Idle,
    Starting,
    Recording { stream_id: String, task_id: String },
    Paused { stream_id: String, task_id: String },
    Stopping,
    Stopped { task_id: Option<String> },
    Error { message: String },
}

pub struct AppState {
    pub current: PlMutex<Option<SessionRunner>>,
    pub status: PlMutex<SessionStatus>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            current: PlMutex::new(None),
            status: PlMutex::new(SessionStatus::Idle),
        }
    }
}

pub struct SessionRunner {
    pub stream_id: String,
    pub task_id: String,
    capture: Option<CaptureHandle>,
    chunk_task: Option<JoinHandle<()>>,
    events_task: Option<JoinHandle<()>>,
    client: ParlyxClient,
}

impl SessionRunner {
    pub async fn start(
        client: ParlyxClient,
        app: AppHandle,
        title: Option<String>,
        min_speakers: Option<u32>,
        max_speakers: Option<u32>,
        _webhook_url: Option<String>,
        device_name: Option<String>,
    ) -> Result<Self> {
        let resp = client
            .start_streaming(true, true, title, min_speakers, max_speakers)
            .await
            .context("starting parlyx streaming session")?;

        info!(
            stream_id = %resp.stream_id,
            task_id = %resp.task_id,
            "streaming session opened"
        );

        // Audio thread → tokio task channel. `tokio::sync::mpsc`'s sender is
        // sync (`.send` does not await), so the cpal-owning thread can drop
        // chunks in without crossing the runtime.
        let (audio_tx, mut audio_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<f32>>();
        let capture = audio::start_capture(device_name, audio_tx)
            .context("starting audio capture")?;

        let client_for_chunks = client.clone();
        let stream_id_for_chunks = resp.stream_id.clone();
        let app_for_chunks = app.clone();
        let chunk_task = tokio::spawn(async move {
            let mut idx: u64 = 0;
            while let Some(chunk) = audio_rx.recv().await {
                let wav = match encode_wav(&chunk) {
                    Ok(b) => Bytes::from(b),
                    Err(e) => {
                        warn!(error = ?e, "encode_wav failed");
                        continue;
                    }
                };
                let bytes_len = wav.len();
                match client_for_chunks
                    .send_chunk(&stream_id_for_chunks, idx, wav)
                    .await
                {
                    Ok(()) => {
                        info!(idx, bytes = bytes_len, "chunk uploaded");
                        let _ = app_for_chunks.emit(EVENT_CHUNK_SENT, idx + 1);
                    }
                    Err(e) => {
                        warn!(error = ?e, idx, "send_chunk failed");
                        let _ = app_for_chunks.emit(
                            EVENT_STATUS,
                            SessionStatus::Error {
                                message: format!("chunk upload failed: {}", e),
                            },
                        );
                    }
                }
                idx += 1;
            }
            info!("chunk shipping task exiting");
        });

        let client_for_events = client.clone();
        let stream_id_for_events = resp.stream_id.clone();
        let app_for_events = app.clone();
        let events_task = tokio::spawn(async move {
            match client_for_events.open_events(&stream_id_for_events).await {
                Ok(mut rx) => {
                    info!("SSE event stream open");
                    let mut received: u64 = 0;
                    while let Some(event) = rx.recv().await {
                        received += 1;
                        info!(received, ?event, "SSE event");
                        let _ = app_for_events.emit(EVENT_EVENT_RECEIVED, received);
                        let _ = app_for_events.emit(EVENT_STREAM, &event);
                        if matches!(event, StreamEvent::Complete) {
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!(error = ?e, "could not open SSE event stream");
                    let _ = app_for_events.emit(
                        EVENT_STATUS,
                        SessionStatus::Error {
                            message: format!("SSE connection failed: {}", e),
                        },
                    );
                }
            }
            info!("events task exiting");
        });

        Ok(Self {
            stream_id: resp.stream_id,
            task_id: resp.task_id,
            capture: Some(capture),
            chunk_task: Some(chunk_task),
            events_task: Some(events_task),
            client,
        })
    }

    pub fn pause(&self) {
        if let Some(c) = self.capture.as_ref() {
            c.pause();
        }
    }
    pub fn resume(&self) {
        if let Some(c) = self.capture.as_ref() {
            c.resume();
        }
    }

    pub async fn stop(mut self) -> Result<String> {
        if let Some(c) = self.capture.take() {
            c.stop();
        }
        let resp = self
            .client
            .finish(&self.stream_id)
            .await
            .context("finishing parlyx session")?;
        if let Some(t) = self.chunk_task.take() {
            t.abort();
        }
        if let Some(t) = self.events_task.take() {
            t.abort();
        }
        Ok(resp.task_id)
    }

    pub async fn cancel(mut self) -> Result<()> {
        if let Some(c) = self.capture.take() {
            c.stop();
        }
        let _ = self.client.cancel(&self.stream_id).await;
        if let Some(t) = self.chunk_task.take() {
            t.abort();
        }
        if let Some(t) = self.events_task.take() {
            t.abort();
        }
        Ok(())
    }
}

/// Set the global session status and emit it to the UI.
pub fn set_status(state: &Arc<AppState>, app: &AppHandle, status: SessionStatus) {
    *state.status.lock() = status.clone();
    let _ = app.emit(EVENT_STATUS, &status);
}

#[allow(dead_code)]
pub fn already_active(state: &Arc<AppState>) -> bool {
    state.current.lock().is_some()
}

#[allow(dead_code)]
pub fn fail<T>(msg: impl Into<String>) -> Result<T> {
    Err(anyhow!(msg.into()))
}
