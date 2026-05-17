//! Minimal HTTP client for the parlyx streaming API.
//!
//! Endpoints exercised:
//!   POST /api/streaming/start              → { stream_id, task_id }
//!   POST /api/streaming/:id/chunk           (multipart audio)
//!   POST /api/streaming/:id/finish         → { task_id }
//!   POST /api/streaming/:id/cancel
//!   PUT  /api/streaming/:id/segments/:sid   { text?, speaker? }
//!   GET  /api/streaming/:id/events          (SSE)

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use futures_util::StreamExt;
use reqwest::multipart::{Form, Part};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::{debug, warn};

#[derive(Debug, Clone)]
pub struct ParlyxClient {
    pub base_url: String,
    pub api_key: String,
    http: Client,
}

impl ParlyxClient {
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            http: Client::builder()
                .user_agent("parlyx-streamer/0.1.0")
                .build()
                .expect("reqwest client"),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api{}", self.base_url, path)
    }

    pub async fn start_streaming(
        &self,
        title: Option<String>,
        min_speakers: Option<u32>,
        max_speakers: Option<u32>,
        webhook_url: Option<String>,
    ) -> Result<StartStreamingResponse> {
        let body = StartStreamingRequest {
            title,
            min_speakers,
            max_speakers,
            webhook_url,
        };
        let resp = self
            .http
            .post(self.url("/streaming/start"))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("POST /streaming/start")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "start_streaming HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(resp.json::<StartStreamingResponse>().await?)
    }

    pub async fn send_chunk(&self, stream_id: &str, chunk_index: u64, wav: Bytes) -> Result<()> {
        let filename = format!("chunk_{}.wav", chunk_index);
        let part = Part::bytes(wav.to_vec())
            .file_name(filename)
            .mime_str("audio/wav")?;
        let form = Form::new()
            .text("chunk_index", chunk_index.to_string())
            .part("file", part);

        let resp = self
            .http
            .post(self.url(&format!("/streaming/{}/chunk", stream_id)))
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .context("POST /streaming/:id/chunk")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "send_chunk HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    pub async fn finish(&self, stream_id: &str) -> Result<FinishResponse> {
        let resp = self
            .http
            .post(self.url(&format!("/streaming/{}/finish", stream_id)))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("POST /streaming/:id/finish")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "finish HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(resp.json::<FinishResponse>().await?)
    }

    pub async fn cancel(&self, stream_id: &str) -> Result<()> {
        let _ = self
            .http
            .post(self.url(&format!("/streaming/{}/cancel", stream_id)))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("POST /streaming/:id/cancel")?;
        Ok(())
    }

    pub async fn update_segment(
        &self,
        stream_id: &str,
        segment_id: &str,
        text: Option<String>,
        speaker: Option<String>,
    ) -> Result<()> {
        let body = SegmentEditRequest { text, speaker };
        let resp = self
            .http
            .put(self.url(&format!(
                "/streaming/{}/segments/{}",
                stream_id, segment_id
            )))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("PUT segment edit")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "update_segment HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }
        Ok(())
    }

    /// Open the SSE stream for a session and emit parsed events on the
    /// returned channel. Returns immediately; the reading task lives on the
    /// tokio runtime until the stream ends or the receiver is dropped.
    pub async fn open_events(&self, stream_id: &str) -> Result<mpsc::UnboundedReceiver<StreamEvent>> {
        let url = self.url(&format!("/streaming/{}/events", stream_id));
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.api_key)
            .header("accept", "text/event-stream")
            .send()
            .await
            .context("GET /streaming/:id/events")?;
        if !resp.status().is_success() {
            return Err(anyhow!(
                "events HTTP {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            ));
        }

        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut buffer = String::new();
            let mut stream = resp.bytes_stream();
            while let Some(item) = stream.next().await {
                match item {
                    Ok(chunk) => {
                        let chunk_str = String::from_utf8_lossy(&chunk);
                        buffer.push_str(&chunk_str);
                        while let Some(idx) = buffer.find("\n\n") {
                            let raw = buffer[..idx].to_string();
                            buffer.drain(..idx + 2);
                            if let Some(evt) = parse_sse(&raw) {
                                if tx.send(evt).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(error = ?e, "SSE stream error");
                        break;
                    }
                }
            }
            debug!("SSE stream closed");
        });
        Ok(rx)
    }
}

fn parse_sse(raw: &str) -> Option<StreamEvent> {
    let mut data_lines: Vec<&str> = Vec::new();
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }
    if data_lines.is_empty() {
        return None;
    }
    let joined = data_lines.join("\n");
    serde_json::from_str::<StreamEvent>(&joined).ok()
}

#[derive(Debug, Serialize)]
pub struct StartStreamingRequest {
    pub title: Option<String>,
    pub min_speakers: Option<u32>,
    pub max_speakers: Option<u32>,
    pub webhook_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StartStreamingResponse {
    pub stream_id: String,
    pub task_id: String,
}

#[derive(Debug, Deserialize)]
pub struct FinishResponse {
    pub task_id: String,
}

#[derive(Debug, Serialize)]
pub struct SegmentEditRequest {
    pub text: Option<String>,
    pub speaker: Option<String>,
}

/// Server-sent events emitted by parlyx's streaming session.
/// Mirrors `StreamEvent` in `src/handlers/streaming.rs`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Transcript {
        text: String,
        timestamp: String,
        speaker: Option<String>,
        segment_id: String,
    },
    Partial {
        text: String,
    },
    Diarization {
        segments: Vec<DiarizationSegment>,
    },
    SpeakerRename {
        from: String,
        to: String,
    },
    Error {
        message: String,
    },
    Complete,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiarizationSegment {
    pub start: f64,
    pub end: f64,
    pub speaker: String,
}
