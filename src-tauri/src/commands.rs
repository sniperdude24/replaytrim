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

#[derive(serde::Serialize)]
pub struct SceneList {
    pub scenes: Vec<String>,
    pub current: String,
}

#[tauri::command]
pub async fn list_scenes(state: State<'_, Arc<AppState>>) -> Result<SceneList, String> {
    let guard = state.obs.lock().await;
    let client = guard.as_ref().ok_or("Not connected to OBS")?;
    let (scenes, current) = client.get_scene_list().await.map_err(|e| e.to_string())?;
    Ok(SceneList { scenes, current })
}

/// Creates a new Media Source in the given OBS scene and makes it the
/// target for trimmed clips, so the user never has to touch OBS's own
/// source dialogs to get set up.
#[tauri::command]
pub async fn create_obs_source(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    scene_name: String,
    source_name: String,
) -> Result<(), String> {
    {
        let guard = state.obs.lock().await;
        let client = guard.as_ref().ok_or("Not connected to OBS")?;
        client
            .create_media_source(&scene_name, &source_name)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut config = state.config.lock().await;
    config.target_source = source_name;
    crate::config::save(&app, &config).map_err(|e| e.to_string())?;
    Ok(())
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaveformResult {
    pub png_path: String,
    pub max_volume_db: f64,
}

#[tauri::command]
pub async fn generate_waveform(
    app: AppHandle,
    input_path: String,
) -> Result<WaveformResult, String> {
    let work_dir = crate::config::work_dir(&app).map_err(|e| e.to_string())?;
    let output = work_dir.join("waveform.png");
    let max_volume_db =
        ffmpeg::generate_waveform(std::path::Path::new(&input_path), &output, 1200, 120)
            .map_err(|e| e.to_string())?;
    Ok(WaveformResult {
        png_path: output.to_string_lossy().to_string(),
        max_volume_db,
    })
}

/// Trims to a fixed output filename NEXT TO the source replay file, in the
/// user's own OBS recording folder. Never write to the app-data dir: when the
/// app runs inside an MSIX/AppContainer context, AppData writes are
/// virtualized into a private store that OBS (outside the container) cannot
/// see, and playback silently fails.
#[tauri::command]
pub async fn export_trim(
    input_path: String,
    start: f64,
    end: f64,
    fast: bool,
) -> Result<String, String> {
    let input = std::path::PathBuf::from(&input_path);
    let out_dir = input
        .parent()
        .ok_or("Replay file has no parent directory")?;
    // Unique name per export: OBS keeps the previously-played file open, so
    // reusing one fixed name makes the second send fight a live file handle.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let output = out_dir.join(format!("replaytrim_{stamp}.mp4"));
    ffmpeg::trim(&input, &output, start, end, fast).map_err(|e| e.to_string())?;

    // Best-effort cleanup of older exports; a file OBS still holds open just
    // stays until next time.
    if let Ok(entries) = std::fs::read_dir(out_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_ours = (name.starts_with("replaytrim_") && name.ends_with(".mp4"))
                || name == "current_replay.mp4"; // legacy fixed-name export
            if is_ours && entry.path() != output {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    Ok(output.to_string_lossy().to_string())
}

/// The full replay sequence: hide the source everywhere it appears, load the
/// trimmed file, restart playback, reveal it, and auto-hide once the clip is
/// over (unless a newer push/toggle superseded this one).
#[tauri::command]
pub async fn push_to_obs(
    state: State<'_, Arc<AppState>>,
    file_path: String,
    duration_secs: f64,
) -> Result<(), String> {
    let config = state.config.lock().await.clone();
    if config.target_source.is_empty() {
        return Err("Not linked to OBS yet — click the \"Plays through\" pill (or the Link to OBS banner) to set up a source".into());
    }
    let gen = state
        .push_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;

    let items = {
        let guard = state.obs.lock().await;
        let client = guard.as_ref().ok_or("Not connected to OBS")?;
        let items = client
            .find_scene_items(&config.target_source)
            .await
            .map_err(|e| e.to_string())?;
        if items.is_empty() {
            return Err(format!(
                "Source \"{}\" isn't in any OBS scene — re-link via the \"Plays through\" pill",
                config.target_source
            ));
        }
        for (scene, id) in &items {
            client
                .set_scene_item_enabled(scene, *id, false)
                .await
                .map_err(|e| e.to_string())?;
        }
        client
            .set_input_file(&config.target_source, &file_path)
            .await
            .map_err(|e| e.to_string())?;
        client
            .restart_media(&config.target_source)
            .await
            .map_err(|e| e.to_string())?;
        // Give the media a beat to open so the reveal shows the first frame,
        // not an empty source.
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        for (scene, id) in &items {
            client
                .set_scene_item_enabled(scene, *id, true)
                .await
                .map_err(|e| e.to_string())?;
        }
        items
    };

    let state_bg = Arc::clone(state.inner());
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs_f64(duration_secs + 0.7)).await;
        if state_bg.push_gen.load(std::sync::atomic::Ordering::SeqCst) != gen {
            return; // a newer push or manual toggle took over
        }
        let guard = state_bg.obs.lock().await;
        if let Some(client) = guard.as_ref() {
            for (scene, id) in &items {
                let _ = client.set_scene_item_enabled(scene, *id, false).await;
            }
        }
    });
    Ok(())
}

/// Manual show/hide for the replay source. Returns the new visibility.
#[tauri::command]
pub async fn toggle_source_visible(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let config = state.config.lock().await.clone();
    if config.target_source.is_empty() {
        return Err("Not linked to OBS yet".into());
    }
    // Cancel any pending auto-hide so it can't fight the manual choice.
    state
        .push_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let guard = state.obs.lock().await;
    let client = guard.as_ref().ok_or("Not connected to OBS")?;
    let items = client
        .find_scene_items(&config.target_source)
        .await
        .map_err(|e| e.to_string())?;
    if items.is_empty() {
        return Err(format!(
            "Source \"{}\" isn't in any OBS scene",
            config.target_source
        ));
    }
    let currently = client
        .get_scene_item_enabled(&items[0].0, items[0].1)
        .await
        .map_err(|e| e.to_string())?;
    for (scene, id) in &items {
        client
            .set_scene_item_enabled(scene, *id, !currently)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(!currently)
}
