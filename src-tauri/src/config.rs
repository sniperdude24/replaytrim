use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_host")]
    pub obs_host: String,
    #[serde(default = "default_port")]
    pub obs_port: u16,
    #[serde(default)]
    pub obs_password: String,
    #[serde(default)]
    pub target_source: String,
    /// "overlay" (browser-source overlay player) or "media_source".
    #[serde(default = "default_target_kind")]
    pub target_kind: String,
    #[serde(default = "default_hotkey")]
    pub grab_hotkey: String,
    /// Optional extra keybinds; empty string = unbound.
    #[serde(default)]
    pub instant_hotkey: String,
    #[serde(default)]
    pub replay_hotkey: String,
    #[serde(default)]
    pub hide_hotkey: String,
    #[serde(default = "default_overlay_port")]
    pub overlay_port: u16,
    /// How many clip rows the dock lists show before internal scrolling.
    #[serde(default = "default_clip_list_limit")]
    pub clip_list_limit: u32,
}

fn default_host() -> String {
    "localhost".into()
}
fn default_port() -> u16 {
    4455
}
fn default_hotkey() -> String {
    "CommandOrControl+Shift+R".into()
}
fn default_target_kind() -> String {
    "media_source".into()
}
fn default_overlay_port() -> u16 {
    8930
}
fn default_clip_list_limit() -> u32 {
    5
}

impl Default for Config {
    fn default() -> Self {
        Self {
            obs_host: default_host(),
            obs_port: default_port(),
            obs_password: String::new(),
            target_source: String::new(),
            target_kind: default_target_kind(),
            grab_hotkey: default_hotkey(),
            instant_hotkey: String::new(),
            replay_hotkey: String::new(),
            hide_hotkey: String::new(),
            overlay_port: default_overlay_port(),
            clip_list_limit: default_clip_list_limit(),
        }
    }
}

fn config_path(app: &AppHandle) -> anyhow::Result<PathBuf> {
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("config.json"))
}

pub fn load(app: &AppHandle) -> anyhow::Result<Config> {
    let path = config_path(app)?;
    if !path.exists() {
        return Ok(Config::default());
    }
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

pub fn save(app: &AppHandle, config: &Config) -> anyhow::Result<()> {
    let path = config_path(app)?;
    std::fs::write(path, serde_json::to_string_pretty(config)?)?;
    Ok(())
}

pub fn work_dir(app: &AppHandle) -> anyhow::Result<PathBuf> {
    let dir = app.path().app_data_dir()?.join("clips");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub fn clips_path(app: &AppHandle) -> anyhow::Result<PathBuf> {
    let dir = app.path().app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("clips.json"))
}

/// Loads the clip library, dropping entries whose file no longer exists.
pub fn load_clips(path: &std::path::Path) -> Vec<crate::state::ClipEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let entries: Vec<crate::state::ClipEntry> = serde_json::from_str(&text).unwrap_or_default();
    entries
        .into_iter()
        .filter(|e| std::path::Path::new(&e.path).exists())
        .collect()
}

pub fn save_clips(path: &std::path::Path, clips: &[crate::state::ClipEntry]) -> anyhow::Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(clips)?)?;
    Ok(())
}
