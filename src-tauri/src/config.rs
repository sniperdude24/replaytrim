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
    #[serde(default = "default_hotkey")]
    pub grab_hotkey: String,
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

impl Default for Config {
    fn default() -> Self {
        Self {
            obs_host: default_host(),
            obs_port: default_port(),
            obs_password: String::new(),
            target_source: String::new(),
            grab_hotkey: default_hotkey(),
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
