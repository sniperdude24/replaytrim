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
    config.target_kind = "media_source".into();
    crate::config::save(&app, &config).map_err(|e| e.to_string())?;
    Ok(())
}

/// Creates the on-stream overlay Browser Source in the given scene and makes
/// it the playback target.
#[tauri::command]
pub async fn create_obs_overlay(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    scene_name: String,
    source_name: String,
) -> Result<(), String> {
    let port = state.config.lock().await.overlay_port;
    let url = format!("http://127.0.0.1:{port}/overlay");
    {
        let guard = state.obs.lock().await;
        let client = guard.as_ref().ok_or("Not connected to OBS")?;
        let (w, h) = client.get_canvas_size().await.map_err(|e| e.to_string())?;
        client
            .create_browser_source(&scene_name, &source_name, &url, w, h)
            .await
            .map_err(|e| e.to_string())?;
    }
    let mut config = state.config.lock().await;
    config.target_source = source_name;
    config.target_kind = "overlay".into();
    crate::config::save(&app, &config).map_err(|e| e.to_string())?;
    Ok(())
}

/// True when the configured playback target (overlay or media source)
/// actually exists in OBS right now.
#[tauri::command]
pub async fn check_target_exists(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    let config = state.config.lock().await.clone();
    if config.target_source.is_empty() {
        return Ok(false);
    }
    let guard = state.obs.lock().await;
    let client = guard.as_ref().ok_or("Not connected to OBS")?;
    client
        .input_exists(&config.target_source)
        .await
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyReport {
    pub connected: bool,
    pub buffer_active: bool,
    pub buffer_started_now: bool,
    pub linked: bool,
    pub target_kind: String,
    pub target_source: String,
}

/// The one-button setup chain: connect -> ensure the Replay Buffer is
/// running -> report whether a playback target is linked (the frontend opens
/// the Link dialog when it isn't).
#[tauri::command]
pub async fn ensure_ready(state: State<'_, Arc<AppState>>) -> Result<ReadyReport, String> {
    let config = state.config.lock().await.clone();

    // 1. Connect if we aren't already.
    {
        let mut guard = state.obs.lock().await;
        if guard.is_none() {
            let client =
                ObsClient::connect(&config.obs_host, config.obs_port, &config.obs_password)
                    .await
                    .map_err(|e| format!("Could not connect to OBS: {e}"))?;
            *guard = Some(client);
        }
    }

    let guard = state.obs.lock().await;
    let client = guard.as_ref().ok_or("Not connected to OBS")?;

    // 2. Replay Buffer running?
    let was_active = client
        .get_replay_buffer_active()
        .await
        .map_err(|e| e.to_string())?;
    if !was_active {
        client
            .start_replay_buffer()
            .await
            .map_err(|e| format!("Could not start the Replay Buffer (is it configured in OBS Settings → Output?): {e}"))?;
    }

    // 3. Playback target linked?
    let linked = if config.target_source.is_empty() {
        false
    } else {
        client
            .input_exists(&config.target_source)
            .await
            .unwrap_or(false)
    };

    Ok(ReadyReport {
        connected: true,
        buffer_active: true,
        buffer_started_now: !was_active,
        linked,
        target_kind: config.target_kind.clone(),
        target_source: config.target_source.clone(),
    })
}

/// Appends to the clip library (capped) and persists it.
pub(crate) async fn record_clip(state: &AppState, path: &str, kind: &str) {
    let duration = crate::ffmpeg::probe_duration(std::path::Path::new(path)).unwrap_or(0.0);
    let entry = crate::state::ClipEntry {
        path: path.to_string(),
        kind: kind.to_string(),
        saved_at_epoch: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        duration_secs: duration,
    };
    let mut clips = state.clips.lock().await;
    clips.retain(|c| c.path != entry.path);
    clips.push(entry);
    let len = clips.len();
    if len > 20 {
        clips.drain(0..len - 20);
    }
    let _ = crate::config::save_clips(&state.clips_file, &clips);
}

/// Grab core, shared by the tauri command, the dock, and instant replay:
/// checks the buffer, triggers a save, and waits for the new file.
pub(crate) async fn do_grab(state: &AppState) -> Result<String, String> {
    {
        let guard = state.obs.lock().await;
        let client = guard.as_ref().ok_or("Not connected to OBS")?;
        if !client.get_replay_buffer_active().await.unwrap_or(false) {
            let _ = client.start_replay_buffer().await;
            return Err(
                "The Replay Buffer was off — I just started it. Give it a few seconds to record, then grab again.".into(),
            );
        }
    }

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
        let path = {
            let guard = state.obs.lock().await;
            let client = guard.as_ref().ok_or("Not connected to OBS")?;
            client.get_last_replay_buffer_replay().await.ok()
        };
        if let Some(path) = path {
            if !path.is_empty() && path != previous {
                record_clip(state, &path, "grab").await;
                return Ok(path);
            }
        }
    }
    Err("Timed out waiting for OBS to save the replay buffer".into())
}

/// Retroactively grabs the last N seconds (whatever OBS's Replay Buffer is
/// configured for) by triggering a save and waiting for a new file to appear.
#[tauri::command]
pub async fn grab_replay(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    do_grab(&state).await
}

/// Sends a clip straight to the overlay page (fade in, play, fade out).
pub(crate) async fn push_clip_to_overlay(state: &AppState, file_path: &str) {
    let mut overlay = state.overlay.lock().await;
    overlay.clip_path = Some(std::path::PathBuf::from(file_path));
    overlay.generation += 1;
}

/// One keypress: grab the whole buffer and play it immediately, no trim
/// step — through the overlay when linked to one, otherwise through the
/// media source. Returns the grabbed path so the UI can also load it for
/// optional re-trimming.
pub(crate) async fn do_instant(state: &Arc<AppState>) -> Result<String, String> {
    let config = state.config.lock().await.clone();
    if config.target_source.is_empty() {
        return Err("Not linked to OBS yet — open Link to OBS in the app first".into());
    }
    let path = do_grab(state).await?;
    if config.target_kind == "overlay" {
        push_clip_to_overlay(state, &path).await;
    } else {
        let duration =
            crate::ffmpeg::probe_duration(std::path::Path::new(&path)).unwrap_or(180.0);
        push_clip_to_media_source(state, &path, duration).await?;
    }
    Ok(path)
}

#[tauri::command]
pub async fn instant_replay(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    do_instant(state.inner()).await
}

/// Playback control ("replay" | "pause" | "hide") routed to whichever
/// target is linked: the overlay page (via its command channel) or the
/// media source (direct OBS calls — hide must be instant).
pub(crate) async fn do_playback_command(
    state: &Arc<AppState>,
    action: &str,
) -> Result<(), String> {
    let config = state.config.lock().await.clone();

    if config.target_kind == "overlay" {
        let mut overlay = state.overlay.lock().await;
        overlay.cmd_seq += 1;
        overlay.cmd = Some(action.to_string());
        return Ok(());
    }

    if config.target_source.is_empty() {
        return Err("Not linked to OBS yet".into());
    }
    // Manual control supersedes any pending auto-hide.
    let gen = state
        .push_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;

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

    match action {
        "hide" => {
            // Visibility first — that's the on-screen change — then stop.
            for (scene, id) in &items {
                client
                    .set_scene_item_enabled(scene, *id, false)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let _ = client
                .trigger_media_action(&config.target_source, "STOP")
                .await;
        }
        "pause" => {
            let (media_state, _) = client
                .get_media_state(&config.target_source)
                .await
                .map_err(|e| e.to_string())?;
            let next = if media_state == "OBS_MEDIA_STATE_PLAYING" {
                "PAUSE"
            } else {
                "PLAY"
            };
            client
                .trigger_media_action(&config.target_source, next)
                .await
                .map_err(|e| e.to_string())?;
        }
        "replay" => {
            client
                .trigger_media_action(&config.target_source, "RESTART")
                .await
                .map_err(|e| e.to_string())?;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            for (scene, id) in &items {
                client
                    .set_scene_item_enabled(scene, *id, true)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            // Re-arm the auto-hide using the media's own reported duration.
            if let Ok((_, Some(duration_ms))) =
                client.get_media_state(&config.target_source).await
            {
                let state_bg = Arc::clone(state);
                let items_bg = items.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs_f64(
                        duration_ms / 1000.0 + 0.7,
                    ))
                    .await;
                    if state_bg.push_gen.load(std::sync::atomic::Ordering::SeqCst) != gen {
                        return;
                    }
                    let guard = state_bg.obs.lock().await;
                    if let Some(client) = guard.as_ref() {
                        for (scene, id) in &items_bg {
                            let _ = client.set_scene_item_enabled(scene, *id, false).await;
                        }
                    }
                });
            }
        }
        other => return Err(format!("unknown action: {other}")),
    }
    Ok(())
}

#[tauri::command]
pub async fn overlay_command(
    state: State<'_, Arc<AppState>>,
    action: String,
) -> Result<(), String> {
    do_playback_command(state.inner(), &action).await
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
pub(crate) async fn do_export_trim(
    state: &AppState,
    input_path: &str,
    start: f64,
    end: f64,
    fast: bool,
) -> Result<String, String> {
    let input = std::path::PathBuf::from(input_path);
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

    // Keep the 5 newest exports (they're part of the clip library now);
    // best-effort delete the rest — a file OBS holds open just stays.
    if let Ok(entries) = std::fs::read_dir(out_dir) {
        let mut ours: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_ours = (name.starts_with("replaytrim_") && name.ends_with(".mp4"))
                    || name == "current_replay.mp4"; // legacy fixed-name export
                if !is_ours || entry.path() == output {
                    return None;
                }
                let modified = entry.metadata().and_then(|m| m.modified()).ok()?;
                Some((modified, entry.path()))
            })
            .collect();
        ours.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
        for (_, path) in ours.into_iter().skip(4) {
            let _ = std::fs::remove_file(path);
        }
    }

    let output_str = output.to_string_lossy().to_string();
    record_clip(state, &output_str, "trim").await;
    Ok(output_str)
}

#[tauri::command]
pub async fn export_trim(
    state: State<'_, Arc<AppState>>,
    input_path: String,
    start: f64,
    end: f64,
    fast: bool,
) -> Result<String, String> {
    do_export_trim(&state, &input_path, start, end, fast).await
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

    // Overlay path: just tell the overlay page about the new clip — it
    // fades in, plays, and fades out on end all by itself.
    if config.target_kind == "overlay" {
        push_clip_to_overlay(&state, &file_path).await;
        return Ok(());
    }

    push_clip_to_media_source(state.inner(), &file_path, duration_secs).await
}

/// Media-source playback sequence, shared by trimmed sends and instant
/// replay: hide -> load -> restart -> reveal -> auto-hide after the clip.
pub(crate) async fn push_clip_to_media_source(
    state: &Arc<AppState>,
    file_path: &str,
    duration_secs: f64,
) -> Result<(), String> {
    let config = state.config.lock().await.clone();
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

    let state_bg = Arc::clone(state);
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
