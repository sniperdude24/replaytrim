use crate::config::Config;
use crate::ffmpeg;
use crate::obs_client::ObsClient;
use crate::state::AppState;
use std::sync::Arc;
use tauri::{AppHandle, State};

#[tauri::command]
pub async fn get_config(state: State<'_, Arc<AppState>>) -> Result<Config, String> {
    Ok(state.config.lock().await.clone())
}

#[tauri::command]
pub async fn save_config(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    config: Config,
) -> Result<(), String> {
    crate::config::save(&app, &config).map_err(|e| e.to_string())?;
    *state.config.lock().await = config;
    Ok(())
}

#[tauri::command]
pub async fn connect_obs(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let config = state.config.lock().await.clone();
    let client = ObsClient::connect(&config.obs_host, config.obs_port, &config.obs_password)
        .await
        .map_err(|e| e.to_string())?;
    *state.obs.lock().await = Some(client);
    Ok(())
}

#[tauri::command]
pub async fn list_media_sources(state: State<'_, Arc<AppState>>) -> Result<Vec<String>, String> {
    let guard = state.obs.lock().await;
    let client = guard.as_ref().ok_or("Not connected to OBS")?;
    client
        .get_media_input_list()
        .await
        .map_err(|e| e.to_string())
}

/// Retroactively grabs the last N seconds (whatever OBS's Replay Buffer is
/// configured for) by triggering a save and waiting for a new file to appear.
#[tauri::command]
pub async fn grab_replay(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    let previous = {
        let guard = state.obs.lock().await;
        let client = guard.as_ref().ok_or("Not connected to OBS")?;
        client
            .get_last_replay_buffer_replay()
            .await
            .unwrap_or_default()
    };

    {
        let guard = state.obs.lock().await;
        let client = guard.as_ref().ok_or("Not connected to OBS")?;
        client
            .save_replay_buffer()
            .await
            .map_err(|e| e.to_string())?;
    }

    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        let guard = state.obs.lock().await;
        let client = guard.as_ref().ok_or("Not connected to OBS")?;
        if let Ok(path) = client.get_last_replay_buffer_replay().await {
            if !path.is_empty() && path != previous {
                return Ok(path);
            }
        }
    }
    Err("Timed out waiting for OBS to save the replay buffer".into())
}

/// Reads a local file's bytes so the frontend can build a Blob URL for
/// preview playback, regardless of where OBS's output folder is configured
/// (avoids needing to pre-declare an asset-protocol scope for an arbitrary,
/// user-chosen directory).
#[tauri::command]
pub async fn read_file_bytes(path: String) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_waveform(app: AppHandle, input_path: String) -> Result<String, String> {
    let work_dir = crate::config::work_dir(&app).map_err(|e| e.to_string())?;
    let output = work_dir.join("waveform.png");
    ffmpeg::generate_waveform(std::path::Path::new(&input_path), &output, 1200, 120)
        .map_err(|e| e.to_string())?;
    Ok(output.to_string_lossy().to_string())
}

/// Trims to a fixed output filename so the target Media Source's file
/// setting only ever needs to be pointed at it once.
#[tauri::command]
pub async fn export_trim(
    app: AppHandle,
    input_path: String,
    start: f64,
    end: f64,
    fast: bool,
) -> Result<String, String> {
    let work_dir = crate::config::work_dir(&app).map_err(|e| e.to_string())?;
    let output = work_dir.join("current_replay.mp4");
    ffmpeg::trim(std::path::Path::new(&input_path), &output, start, end, fast)
        .map_err(|e| e.to_string())?;
    Ok(output.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn push_to_obs(state: State<'_, Arc<AppState>>, file_path: String) -> Result<(), String> {
    let config = state.config.lock().await.clone();
    if config.target_source.is_empty() {
        return Err("No target Media Source configured in Settings".into());
    }
    let guard = state.obs.lock().await;
    let client = guard.as_ref().ok_or("Not connected to OBS")?;
    client
        .set_input_file(&config.target_source, &file_path)
        .await
        .map_err(|e| e.to_string())?;
    client
        .restart_media(&config.target_source)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
