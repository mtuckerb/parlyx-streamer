//! Persistent app settings. Stored as JSON at the platform-specific config
//! dir so they survive app restarts. The web UI loads + saves these via the
//! `load_settings` / `save_settings` Tauri commands.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const APP_QUALIFIER: &str = "com";
const APP_ORG: &str = "tuckerbradford";
const APP_NAME: &str = "parlyx-streamer";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    /// Base URL of the parlyx server (e.g. http://10.1.0.75:5555).
    /// Should NOT include the /api suffix; the streamer appends it.
    pub parlyx_server_base_url: String,
    /// Long-lived API key (Authorization: Bearer …).
    pub api_key: String,
    /// Optional webhook callback URL the server should hit on task transitions.
    pub webhook_url: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            parlyx_server_base_url: "http://localhost:5555".to_string(),
            api_key: String::new(),
            webhook_url: None,
        }
    }
}

fn settings_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from(APP_QUALIFIER, APP_ORG, APP_NAME)
        .context("could not resolve project directories")?;
    let cfg_dir = dirs.config_dir();
    std::fs::create_dir_all(cfg_dir)?;
    Ok(cfg_dir.join("settings.json"))
}

pub fn load() -> Result<Settings> {
    let path = settings_path()?;
    if !path.exists() {
        return Ok(Settings::default());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("reading {}", path.display()))?;
    let parsed: Settings = serde_json::from_str(&text).unwrap_or_default();
    Ok(parsed)
}

pub fn save(s: &Settings) -> Result<()> {
    let path = settings_path()?;
    let text = serde_json::to_string_pretty(s)?;
    std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
