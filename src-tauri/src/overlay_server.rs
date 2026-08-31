use crate::state::AppState;
use axum::extract::{Path as AxumPath, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::json;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

/// Shared context for handlers: app handle (to reach the frontend via
/// events) + app state (overlay channel, OBS client, config).
#[derive(Clone)]
pub struct ServerCtx {
    pub app: AppHandle,
    pub state: Arc<AppState>,
}

/// Local HTTP server backing the on-stream overlay player and the OBS
/// control dock. Bound to 127.0.0.1 only — nothing is exposed off-machine.
pub async fn spawn(
    app: AppHandle,
    state: Arc<AppState>,
    port: u16,
) -> anyhow::Result<tauri::async_runtime::JoinHandle<()>> {
    // Retry until the port frees up — if another instance held it and then
    // exited, this instance takes over instead of silently never serving.
    let listener = loop {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(l) => break l,
            Err(e) => {
                eprintln!("overlay server: port {port} busy ({e}); retrying in 3s");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    };
    let ctx = ServerCtx { app, state };
    let router = Router::new()
        .route("/overlay", get(overlay_page))
        .route("/dock", get(dock_page))
        .route("/api/state", get(api_state))
        .route("/api/cmd/:action", post(api_cmd))
        .route("/api/grab", post(api_grab))
        .route("/api/clips", get(api_clips))
        .route("/api/folder", get(api_folder))
        .route("/api/delete", post(api_delete))
        .route("/api/file", get(api_file))
        .route("/api/waveform", get(api_waveform))
        .route("/api/send_trim", post(api_send_trim))
        .route("/clip", get(serve_clip))
        .with_state(ctx);

    let handle = tauri::async_runtime::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            eprintln!("overlay server error: {e}");
        }
    });
    Ok(handle)
}

async fn api_state(State(ctx): State<ServerCtx>) -> Json<serde_json::Value> {
    let overlay = ctx.state.overlay.lock().await;
    Json(json!({
        "generation": overlay.generation,
        "hasClip": overlay.clip_path.is_some(),
        "cmdSeq": overlay.cmd_seq,
        "cmd": overlay.cmd,
        "grabSeq": overlay.grab_seq,
        "lastGrab": overlay.last_grab,
    }))
}

/// Dock/hotkey command endpoint.
/// grab    -> asks the app window to run its grab flow (trim editor opens)
/// instant -> backend grab + immediate overlay playback
/// replay/pause/hide -> forwarded to the overlay page
async fn api_cmd(
    State(ctx): State<ServerCtx>,
    AxumPath(action): AxumPath<String>,
) -> Response {
    match action.as_str() {
        "grab" => match crate::commands::do_grab(&ctx.state).await {
            // do_grab bumps grab_seq, so every open dock auto-loads the
            // clip into its trim editor; the desktop app follows via event.
            Ok(path) => {
                let _ = ctx.app.emit("clip-grabbed", path);
                (StatusCode::OK, "grabbed — ready to trim").into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        },
        "instant" => match crate::commands::do_instant(&ctx.state).await {
            Ok(path) => {
                let _ = ctx.app.emit("clip-grabbed", path.clone());
                (StatusCode::OK, "playing").into_response()
            }
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        },
        "replay" | "pause" | "hide" => {
            match crate::commands::do_playback_command(&ctx.state, &action).await {
                Ok(()) => (StatusCode::OK, "ok").into_response(),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
            }
        }
        _ => (StatusCode::NOT_FOUND, "unknown action").into_response(),
    }
}

/// POST /api/grab — dock-native grab: save the buffer, return the new path.
async fn api_grab(State(ctx): State<ServerCtx>) -> Response {
    match crate::commands::do_grab(&ctx.state).await {
        Ok(path) => {
            let duration = crate::ffmpeg::probe_duration(std::path::Path::new(&path)).unwrap_or(0.0);
            let _ = ctx.app.emit("clip-grabbed", path.clone());
            Json(json!({ "path": path, "durationSecs": duration })).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// GET /api/clips — the clip library, newest first, existing files only.
async fn api_clips(State(ctx): State<ServerCtx>) -> Json<serde_json::Value> {
    let list_limit = ctx.state.config.lock().await.clip_list_limit;
    let clips = ctx.state.clips.lock().await;
    let list: Vec<_> = clips
        .iter()
        .rev()
        .filter(|c| std::path::Path::new(&c.path).exists())
        .collect();
    Json(json!({ "clips": list, "listLimit": list_limit }))
}

/// Files the app may serve/act on: library clips, the currently loaded
/// overlay clip, or any .mp4 sitting directly in the OBS recording folder.
async fn path_allowed(ctx: &ServerCtx, path: &str) -> bool {
    {
        let clips = ctx.state.clips.lock().await;
        if clips.iter().any(|c| c.path == path) {
            return true;
        }
    }
    {
        let overlay = ctx.state.overlay.lock().await;
        if overlay
            .clip_path
            .as_ref()
            .map(|p| p.to_string_lossy() == path)
            .unwrap_or(false)
        {
            return true;
        }
    }
    let p = std::path::Path::new(path);
    if p.extension().and_then(|e| e.to_str()) != Some("mp4") {
        return false;
    }
    let Some(parent) = p.parent() else {
        return false;
    };
    let record_dir = ctx.state.record_dir.lock().await;
    record_dir
        .as_ref()
        .map(|dir| same_dir(parent, dir) || same_dir(parent, &dir.join("ReplayTrim")))
        .unwrap_or(false)
}

/// Case/separator-insensitive directory comparison (Windows paths mix
/// slashes freely).
fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let norm = |p: &std::path::Path| {
        p.to_string_lossy()
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

#[derive(serde::Deserialize)]
struct FolderQuery {
    #[serde(default)]
    which: Option<String>,
}

/// GET /api/folder?which=clips|recordings — .mp4s in the ReplayTrim
/// subfolder (default) or the OBS recording folder root, newest first.
async fn api_folder(
    State(ctx): State<ServerCtx>,
    axum::extract::Query(q): axum::extract::Query<FolderQuery>,
) -> Response {
    // Resolve the recording folder: ask OBS, fall back to the newest
    // library clip's parent. Cache it for the allow-list.
    let mut dir: Option<std::path::PathBuf> = None;
    if crate::commands::ensure_obs_alive(&ctx.state).await.is_ok() {
        let guard = ctx.state.obs.lock().await;
        if let Some(client) = guard.as_ref() {
            if let Ok(d) = client.get_record_directory().await {
                if !d.is_empty() {
                    dir = Some(std::path::PathBuf::from(d));
                }
            }
        }
    }
    if dir.is_none() {
        let clips = ctx.state.clips.lock().await;
        dir = clips
            .last()
            .and_then(|c| std::path::Path::new(&c.path).parent().map(|p| p.to_path_buf()));
    }
    let Some(mut dir) = dir else {
        return (StatusCode::NOT_FOUND, "Recording folder unknown — grab a clip first").into_response();
    };
    // Cache the ROOT recording dir for the allow-list; a "clips" request
    // then lists its ReplayTrim subfolder.
    if dir.file_name().and_then(|n| n.to_str()) == Some("ReplayTrim") {
        if let Some(p) = dir.parent() {
            dir = p.to_path_buf();
        }
    }
    *ctx.state.record_dir.lock().await = Some(dir.clone());
    if q.which.as_deref() != Some("recordings") {
        dir = dir.join("ReplayTrim");
        let _ = std::fs::create_dir_all(&dir);
    }
    let list_limit = ctx.state.config.lock().await.clip_list_limit;

    let mut files: Vec<(u64, u64, String, String)> = Vec::new(); // (mtime, size, name, path)
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("mp4") {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            files.push((
                mtime,
                meta.len(),
                entry.file_name().to_string_lossy().to_string(),
                path.to_string_lossy().to_string(),
            ));
        }
    }
    files.sort_by(|a, b| b.0.cmp(&a.0));
    files.truncate(30);
    let list: Vec<_> = files
        .into_iter()
        .map(|(mtime, size, name, path)| {
            json!({ "modifiedEpoch": mtime, "sizeMb": (size as f64 / 1048576.0), "name": name, "path": path })
        })
        .collect();
    Json(json!({ "dir": dir.to_string_lossy(), "files": list, "listLimit": list_limit })).into_response()
}

/// POST /api/delete — removes a clip from the library AND from disk.
/// Only library clips / recording-folder files are deletable, and only via
/// the user's explicit per-item button in the dock.
async fn api_delete(State(ctx): State<ServerCtx>, Json(body): Json<FileQuery>) -> Response {
    if !path_allowed(&ctx, &body.path).await {
        return (StatusCode::FORBIDDEN, "not a library clip").into_response();
    }
    if let Err(e) = std::fs::remove_file(&body.path) {
        if std::path::Path::new(&body.path).exists() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Could not delete (is it playing in OBS right now?): {e}"),
            )
                .into_response();
        }
    }
    let mut clips = ctx.state.clips.lock().await;
    clips.retain(|c| c.path != body.path);
    let _ = crate::config::save_clips(&ctx.state.clips_file, &clips);
    (StatusCode::OK, "deleted").into_response()
}

#[derive(serde::Deserialize)]
struct FileQuery {
    path: String,
}

/// GET /api/file?path=… — serve a library video with Range support.
async fn api_file(
    State(ctx): State<ServerCtx>,
    axum::extract::Query(q): axum::extract::Query<FileQuery>,
    headers: HeaderMap,
) -> Response {
    if !path_allowed(&ctx, &q.path).await {
        return (StatusCode::FORBIDDEN, "not a library clip").into_response();
    }
    serve_video_file(std::path::Path::new(&q.path), &headers).await
}

/// GET /api/waveform?path=… — waveform PNG for a library clip.
async fn api_waveform(
    State(ctx): State<ServerCtx>,
    axum::extract::Query(q): axum::extract::Query<FileQuery>,
) -> Response {
    if !path_allowed(&ctx, &q.path).await {
        return (StatusCode::FORBIDDEN, "not a library clip").into_response();
    }
    let tmp = std::env::temp_dir().join(format!("replaytrim_wave_{}.png", uuid::Uuid::new_v4()));
    let result = crate::ffmpeg::generate_waveform(std::path::Path::new(&q.path), &tmp, 1200, 120);
    let response = match (result, tokio::fs::read(&tmp).await) {
        (Ok(_), Ok(bytes)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "image/png".to_string())],
            bytes,
        )
            .into_response(),
        (Err(e), _) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        (_, Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let _ = tokio::fs::remove_file(&tmp).await;
    response
}

#[derive(serde::Deserialize)]
struct SendTrimBody {
    path: String,
    start: f64,
    end: f64,
    #[serde(default)]
    fast: bool,
}

/// POST /api/send_trim — export the selection and play it through the
/// linked target (overlay or media source).
async fn api_send_trim(State(ctx): State<ServerCtx>, Json(body): Json<SendTrimBody>) -> Response {
    if !path_allowed(&ctx, &body.path).await {
        return (StatusCode::FORBIDDEN, "not a library clip").into_response();
    }
    let out =
        match crate::commands::do_export_trim(&ctx.state, &body.path, body.start, body.end, body.fast)
            .await
        {
            Ok(p) => p,
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        };
    let duration = (body.end - body.start).max(0.05);
    let config = ctx.state.config.lock().await.clone();
    let result = if config.target_kind == "overlay" {
        crate::commands::push_clip_to_overlay(&ctx.state, &out).await;
        Ok(())
    } else {
        crate::commands::push_clip_to_media_source(&ctx.state, &out, duration).await
    };
    match result {
        Ok(()) => Json(json!({ "path": out })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Serves the current clip with basic Range support — OBS's Chromium asks
/// for byte ranges when seeking; exports are +faststart so sequential
/// playback works even without them.
async fn serve_clip(State(ctx): State<ServerCtx>, headers: HeaderMap) -> Response {
    let path = {
        let overlay = ctx.state.overlay.lock().await;
        overlay.clip_path.clone()
    };
    let Some(path) = path else {
        return (StatusCode::NOT_FOUND, "no clip yet").into_response();
    };
    serve_video_file(&path, &headers).await
}

async fn serve_video_file(path: &std::path::Path, headers: &HeaderMap) -> Response {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return (StatusCode::NOT_FOUND, "clip file missing").into_response();
    };
    let total = bytes.len();

    if let Some(range) = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_byte_range)
    {
        let (start, end_incl) = range;
        let end_incl = end_incl.unwrap_or(total.saturating_sub(1)).min(total.saturating_sub(1));
        if start > end_incl || start >= total {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{total}"))],
            )
                .into_response();
        }
        let slice = bytes[start..=end_incl].to_vec();
        return (
            StatusCode::PARTIAL_CONTENT,
            [
                (header::CONTENT_TYPE, "video/mp4".to_string()),
                (header::ACCEPT_RANGES, "bytes".to_string()),
                (
                    header::CONTENT_RANGE,
                    format!("bytes {start}-{end_incl}/{total}"),
                ),
            ],
            slice,
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "video/mp4".to_string()),
            (header::ACCEPT_RANGES, "bytes".to_string()),
        ],
        bytes,
    )
        .into_response()
}

/// "bytes=start-end" (end optional). Multi-range requests are not supported.
fn parse_byte_range(value: &str) -> Option<(usize, Option<usize>)> {
    let spec = value.strip_prefix("bytes=")?;
    let (start, end) = spec.split_once('-')?;
    let start: usize = start.parse().ok()?;
    let end = if end.is_empty() {
        None
    } else {
        Some(end.parse().ok()?)
    };
    Some((start, end))
}

async fn overlay_page() -> Html<&'static str> {
    Html(OVERLAY_HTML)
}

async fn dock_page() -> Html<&'static str> {
    Html(DOCK_HTML)
}

/// The OBS control dock: the full grab → trim → send workflow in a dock,
/// plus playback controls and the recent-clip library.
const DOCK_HTML: &str = r##"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>ReplayTrim</title>
<style>
  :root { color-scheme: dark; }
  * { box-sizing: border-box; }
  html, body { margin: 0; height: 100%; overflow: hidden; background: #1a1c20;
    color: #e6e8eb; font-family: "Segoe UI", sans-serif; font-size: 13px; }
  #wrap { display: flex; flex-direction: column; gap: 8px; padding: 8px; height: 100vh; }
  .list-box { overflow-y: auto; min-height: 0; }
  .list-box::-webkit-scrollbar { width: 8px; }
  .list-box::-webkit-scrollbar-thumb { background: #3a3e46; border-radius: 4px; }

  #tabbar { display: none; border-bottom: 1px solid #33363d; gap: 2px; }
  #tabbar span { padding: 5px 12px; cursor: pointer; color: #9aa0aa; font-size: 12.5px;
    border-bottom: 2px solid transparent; }
  #tabbar span.active { color: #6ea8ff; border-bottom-color: #4f8cff; }
  .pill-sm { font-size: 10.5px; padding: 2px 8px; border-radius: 999px; cursor: pointer;
    background: #2a2d34; border: 1px solid #3a3e46; color: #9aa0aa; }
  .pill-sm.active { background: rgba(79,140,255,.2); border-color: #4f8cff; color: #6ea8ff; }

  #wrap.tabbed #preview { max-height: 45vh; }

  #pane-trim { min-height: 0; overflow: hidden; flex: none; }
  #pane-clips { display: flex; flex-direction: column; gap: 8px; min-height: 0;
    overflow: hidden; flex: none; }
  #divider { height: 12px; display: none; align-items: center; justify-content: center;
    cursor: ns-resize; touch-action: none; flex: none; }
  #divider .grip { width: 44px; height: 4px; background: #3a3e46; border-radius: 2px; }
  #divider:hover .grip, #divider.dragging .grip { background: #4f8cff; }
  .lock-btn { padding: 3px 8px; font-size: 11px; }
  .lock-btn.locked { border-color: #4f8cff; color: #6ea8ff; background: rgba(79,140,255,.15); }
  #wrap.tabbed #pane-trim, #wrap.tabbed #pane-clips { display: contents; }
  #wrap.tabbed #divider, #wrap.tabbed .lock-btn { display: none !important; }
  .row { display: flex; gap: 6px; flex-wrap: wrap; }
  button {
    font: 600 12.5px "Segoe UI", sans-serif; color: #fff; cursor: pointer;
    background: #2a2d34; border: 1px solid #3a3e46; border-radius: 7px;
    padding: 8px 10px;
  }
  button:hover { border-color: #4f8cff; }
  button.primary { background: #4f8cff; border-color: #4f8cff; }
  button:disabled { opacity: .5; cursor: default; }
  #status { font-size: 11.5px; color: #9aa0aa; min-height: 15px; }

  #editor { display: none; flex-direction: column; gap: 6px; }
  #editor.active { display: flex; }
  #trim-stack { display: flex; flex-direction: column; gap: 6px; width: 100%;
    margin: 0 auto; min-width: 0; }
  #preview { width: 100%; background: #000; border-radius: 6px; display: block; }
  #editor.collapsed #preview { display: none; }
  #editor.collapsed #trim-stack { width: 100%; }

  #scrubber { position: relative; height: 64px; }
  #scrubber::after { content: ""; position: absolute; left: 0; right: 0; top: 50%;
    height: 1px; background: rgba(255,255,255,.22); pointer-events: none; }
  #waveform { position: absolute; inset: 0; width: 100%; height: 100%;
    object-fit: fill; border-radius: 5px; background: #22252b; }
  #track { position: absolute; inset: 0; cursor: crosshair; }
  #selection { position: absolute; top: 0; bottom: 0;
    background: rgba(79,140,255,.25); border-left: 2px solid #4f8cff; border-right: 2px solid #4f8cff; }
  #playhead { position: absolute; top: 0; bottom: 0; left: 0; width: 2px;
    background: #fff; opacity: .85; pointer-events: none; }
  .handle { position: absolute; top: 0; bottom: 0; width: 12px; margin-left: -6px;
    background: #4f8cff; cursor: ew-resize; border-radius: 3px; touch-action: none; }
  #times { display: flex; justify-content: space-between; font-family: Consolas, monospace;
    font-size: 11px; color: #9aa0aa; }
  .inline { display: flex; align-items: center; gap: 6px; font-size: 11.5px; color: #9aa0aa; }

  #clips { display: flex; flex-direction: column; gap: 4px; }
  .clip-row { display: flex; align-items: center; gap: 8px; padding: 6px 8px;
    background: #22252b; border: 1px solid #33363d; border-radius: 6px; cursor: pointer; }
  .clip-row:hover { border-color: #4f8cff; }
  .clip-row .meta { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .clip-row .badge { font-size: 10px; padding: 1px 6px; border-radius: 999px;
    background: #33363d; color: #9aa0aa; flex: none; }
  .clip-row .badge.trim { background: rgba(62,201,138,.2); color: #3ec98a; }
  .clip-row button { padding: 4px 9px; flex: none; }
  h3 { margin: 4px 0 0; font-size: 11px; text-transform: uppercase;
    letter-spacing: .05em; color: #9aa0aa; }
</style>
</head>
<body>
<div id="wrap">
  <div class="row">
    <button class="primary" id="grab-btn">🎬 Grab &amp; Trim</button>
    <button class="primary" data-cmd="instant">⚡ Instant Replay</button>
  </div>
  <div class="row">
    <button data-cmd="replay">🔁 Replay</button>
    <button data-cmd="pause">⏯ Pause</button>
    <button data-cmd="hide">🚫 Hide</button>
    <button id="layout-btn" title="Switch dock layout" style="margin-left:auto">⇆ Tabs</button>
  </div>
  <div id="status"></div>

  <div id="tabbar">
    <span data-tab="editor" class="active">Trim</span>
    <span data-tab="clips-section">Clips</span>
    <span data-tab="folder-section">Folder</span>
  </div>

  <div id="pane-trim">
  <div id="editor" class="tab-section tab-active">
    <div class="row" style="justify-content: space-between; align-items: center; flex-wrap: nowrap;">
      <span class="inline" id="clip-name" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:55%"></span>
      <span style="display:flex;gap:4px">
        <button id="lock-trim" class="lock-btn" title="Lock this pane's height (dock resizes won't change it)">🔓</button>
        <button id="toggle-preview">▤ Hide preview</button>
      </span>
    </div>
    <div id="trim-stack">
      <video id="preview" playsinline></video>
      <div id="scrubber">
        <img id="waveform" alt="">
        <div id="track">
          <div id="selection"></div>
          <div id="playhead"></div>
          <div class="handle" id="h-start" data-handle="start"></div>
          <div class="handle" id="h-end" data-handle="end"></div>
        </div>
      </div>
      <div id="times"><span id="t-start">0.00s</span><span id="t-total"></span><span id="t-end">0.00s</span></div>
    </div>
    <div class="row" style="align-items:center">
      <label class="inline"><input type="checkbox" id="fast" checked> fast trim</label>
      <button id="preview-sel">Preview</button>
      <button class="primary" id="send-btn" style="margin-left:auto">Send &amp; Play</button>
    </div>
  </div>
  </div>

  <div id="divider" title="Drag to resize the panes"><span class="grip"></span></div>

  <div id="pane-clips">
  <div id="clips-section" class="tab-section" style="display:flex;flex-direction:column;gap:4px;min-height:0">
    <div class="row" style="justify-content: space-between; align-items: baseline; flex-wrap: nowrap;">
      <h3>Recent clips</h3>
      <span style="display:flex;gap:4px">
        <button id="lock-clips" class="lock-btn" title="Lock this pane's height (dock resizes won't change it)">🔓</button>
        <button id="folder-btn" style="padding:3px 8px;font-size:11px">📂 Browse folder</button>
      </span>
    </div>
    <div id="clips" class="list-box"><span class="inline">None yet — grab something!</span></div>
  </div>
  <div id="folder-section" class="tab-section" style="display:none;flex-direction:column;gap:4px;min-height:0">
    <div class="row" style="align-items:center;gap:6px">
      <h3 style="margin:0">Folder</h3>
      <span class="pill-sm active" data-which="clips">ReplayTrim clips</span>
      <span class="pill-sm" data-which="recordings">Recordings</span>
    </div>
    <p id="folder-title" style="font-size:10.5px;color:#9aa0aa;margin:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap"></p>
    <div id="folder-files" class="list-box"></div>
  </div>
  </div>
</div>
<script>
  const status = document.getElementById("status");
  const editor = document.getElementById("editor");
  const v = document.getElementById("preview");
  const waveform = document.getElementById("waveform");
  const track = document.getElementById("track");
  const selection = document.getElementById("selection");
  const playhead = document.getElementById("playhead");
  const hStart = document.getElementById("h-start");
  const hEnd = document.getElementById("h-end");
  const tStart = document.getElementById("t-start");
  const tEnd = document.getElementById("t-end");
  const tTotal = document.getElementById("t-total");

  let clipPath = null, duration = 0, startPct = 0, endPct = 1;

  function note(text, sticky) {
    status.textContent = text;
    if (!sticky) setTimeout(() => { if (status.textContent === text) status.textContent = ""; }, 5000);
  }

  // ---- simple command buttons ----
  document.querySelectorAll("button[data-cmd]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      note("…", true);
      try {
        const res = await fetch("/api/cmd/" + btn.dataset.cmd, { method: "POST" });
        note((res.ok ? "✓ " : "✗ ") + (await res.text()));
      } catch { note("✗ ReplayTrim app is not running"); }
    });
  });

  // ---- layout: stacked vs tabbed (remembered) ----
  const wrap = document.getElementById("wrap");
  const clipsSection = document.getElementById("clips-section");
  const folderSection = document.getElementById("folder-section");
  const clipsBox = document.getElementById("clips");
  const folderBox = document.getElementById("folder-files");
  const tabbar = document.getElementById("tabbar");
  const folderBtn = document.getElementById("folder-btn");
  const layoutBtn = document.getElementById("layout-btn");
  let layout = "stacked";
  try { layout = localStorage.getItem("rt-layout") || "stacked"; } catch {}
  let currentTab = "editor";
  let folderOpen = false;
  let listLimit = 5;

  function applyView() {
    const stacked = layout === "stacked";
    wrap.classList.toggle("tabbed", !stacked);
    tabbar.style.display = stacked ? "none" : "flex";
    layoutBtn.textContent = stacked ? "⇆ Tabs" : "⇆ Stacked";
    if (stacked) {
      const ed = document.getElementById("editor");
      ed.style.display = ed.classList.contains("active") ? "flex" : "none";
      ed.style.flex = "";
      ed.style.minHeight = "0";
      ed.style.overflow = "hidden";
      ed.style.height = "100%";
      clipsSection.style.display = "flex";
      clipsSection.style.flex = "1 1 auto";
      clipsSection.style.minHeight = "0";
      folderSection.style.display = folderOpen ? "flex" : "none";
      folderSection.style.flex = folderOpen ? "1 1 auto" : "none";
      folderSection.style.minHeight = folderOpen ? "0" : "";
      folderBtn.style.display = "";
      [clipsBox, folderBox].forEach((b) => {
        b.style.flex = "1";
        b.style.maxHeight = "none";
        b.style.minHeight = "40px";
      });
      layoutPanes();
    } else {
      paneTrim.style.height = "";
      paneClips.style.height = "";
      folderBtn.style.display = "none";
      const ed = document.getElementById("editor");
      ed.style.height = "";
      ed.style.overflow = "";
      const map = { "editor": ed, "clips-section": clipsSection, "folder-section": folderSection };
      for (const [id, el] of Object.entries(map)) {
        const show = id === currentTab;
        el.style.display = show ? "flex" : "none";
        if (show && id !== "editor") { el.style.flex = "1"; el.style.minHeight = "0"; }
      }
      [clipsBox, folderBox].forEach((b) => { b.style.maxHeight = "none"; b.style.flex = "1"; });
      if (currentTab === "folder-section") refreshFolder(true);
    }
    tabbar.querySelectorAll("span").forEach((s) =>
      s.classList.toggle("active", s.dataset.tab === currentTab));
    sizeStack();
  }

  // ---- resizable, lockable split panes (stacked layout) ----
  const paneTrim = document.getElementById("pane-trim");
  const paneClips = document.getElementById("pane-clips");
  const divider = document.getElementById("divider");
  const lockTrimBtn = document.getElementById("lock-trim");
  const lockClipsBtn = document.getElementById("lock-clips");
  const TRIM_MIN = 150, CLIPS_MIN = 70;
  let splitPx = 0;
  let lockMode = "";
  let lastAvail = 0;
  try {
    splitPx = parseFloat(localStorage.getItem("rt-split")) || 0;
    lockMode = localStorage.getItem("rt-lock") || "";
  } catch {}

  function updateLockIcons() {
    lockTrimBtn.textContent = lockMode === "trim" ? "🔒" : "🔓";
    lockTrimBtn.classList.toggle("locked", lockMode === "trim");
    lockClipsBtn.textContent = lockMode === "clips" ? "🔒" : "🔓";
    lockClipsBtn.classList.toggle("locked", lockMode === "clips");
  }
  function setLock(which) {
    lockMode = lockMode === which ? "" : which; // one lock at a time
    try { localStorage.setItem("rt-lock", lockMode); } catch {}
    updateLockIcons();
  }
  lockTrimBtn.addEventListener("click", () => setLock("trim"));
  lockClipsBtn.addEventListener("click", () => setLock("clips"));
  updateLockIcons();

  function availableHeight() {
    return wrap.clientHeight - paneTrim.offsetTop - divider.offsetHeight - 16;
  }

  function layoutPanes(fromDrag) {
    if (layout !== "stacked" || !window.innerHeight) return;
    const editorActive = editor.classList.contains("active");
    divider.style.display = editorActive ? "flex" : "none";
    paneTrim.style.display = editorActive ? "block" : "none";
    if (!editorActive) {
      paneClips.style.flex = "1 1 auto";
      paneClips.style.height = "";
      lastAvail = 0;
      return;
    }
    const avail = availableHeight();
    if (avail < TRIM_MIN + CLIPS_MIN) return;
    if (!splitPx) splitPx = Math.round(avail * 0.55);
    // Distribute dock resizes according to the lock.
    if (!fromDrag && lastAvail && Math.abs(avail - lastAvail) > 1) {
      if (lockMode === "clips") splitPx += avail - lastAvail;
      else if (lockMode !== "trim") splitPx = splitPx * (avail / lastAvail);
    }
    lastAvail = avail;
    splitPx = Math.min(Math.max(splitPx, TRIM_MIN), avail - CLIPS_MIN);
    paneTrim.style.height = Math.round(splitPx) + "px";
    paneClips.style.flex = "none";
    paneClips.style.height = Math.round(avail - splitPx) + "px";
    try { localStorage.setItem("rt-split", String(Math.round(splitPx))); } catch {}
    sizeStack();
  }
  window.addEventListener("resize", () => layoutPanes());

  let dividerDragging = false;
  divider.addEventListener("pointerdown", (e) => {
    dividerDragging = true;
    divider.classList.add("dragging");
    divider.setPointerCapture(e.pointerId);
  });
  divider.addEventListener("pointermove", (e) => {
    if (!dividerDragging) return;
    splitPx = e.clientY - paneTrim.getBoundingClientRect().top;
    layoutPanes(true);
  });
  divider.addEventListener("pointerup", () => {
    dividerDragging = false;
    divider.classList.remove("dragging");
  });
  layoutBtn.addEventListener("click", () => {
    layout = layout === "stacked" ? "tabbed" : "stacked";
    try { localStorage.setItem("rt-layout", layout); } catch {}
    applyView();
  });
  tabbar.querySelectorAll("span").forEach((s) =>
    s.addEventListener("click", () => { currentTab = s.dataset.tab; applyView(); }));

  // ---- collapse toggle (remembered) ----
  const toggleBtn = document.getElementById("toggle-preview");
  function setCollapsed(collapsed) {
    const was = editor.classList.contains("collapsed");
    editor.classList.toggle("collapsed", collapsed);
    toggleBtn.textContent = collapsed ? "▤ Show preview" : "▤ Hide preview";
    try { localStorage.setItem("rt-collapsed", collapsed ? "1" : ""); } catch {}
    // Collapsing shrinks the trim pane to just the controls; expanding
    // restores the previous split.
    if (collapsed && !was) {
      try { localStorage.setItem("rt-split-full", String(Math.round(splitPx))); } catch {}
      splitPx = TRIM_MIN;
    } else if (!collapsed && was) {
      try { splitPx = parseFloat(localStorage.getItem("rt-split-full")) || splitPx; } catch {}
    }
    layoutPanes(true);
    sizeStack();
  }
  toggleBtn.addEventListener("click", () => setCollapsed(!editor.classList.contains("collapsed")));
  try { if (localStorage.getItem("rt-collapsed") === "1") setCollapsed(true); } catch {}

  // ---- load a clip into the editor ----
  async function loadClip(path, opts) {
    clipPath = path;
    startPct = 0; endPct = 1; duration = 0;
    editor.classList.add("active");
    currentTab = "editor";
    // A fresh grab means "I want to trim this NOW" — make sure the
    // preview is visible even if the dock was in condensed mode.
    if (opts && opts.fresh) setCollapsed(false);
    applyView();
    document.getElementById("clip-name").textContent = path.split(/[\\/]/).pop();
    v.src = "/api/file?path=" + encodeURIComponent(path);
    waveform.src = "/api/waveform?path=" + encodeURIComponent(path);
    render();
  }

  // Size the video+waveform stack to the DISPLAYED video frame width so the
  // trim handles line up with the frame edges (no letterbox overhang).
  const trimStack = document.getElementById("trim-stack");
  function sizeStack() {
    const collapsed = editor.classList.contains("collapsed");
    if (collapsed || !v.videoWidth || !v.videoHeight) {
      trimStack.style.width = "100%";
      return;
    }
    if (!window.innerHeight) return; // hidden dock reports 0x0 — keep last size
    // The video's height budget is whatever its pane gives it after the
    // fixed rows (header + waveform + times + actions ≈ 175px), so the
    // trim UI itself never needs a scrollbar.
    let maxH;
    if (layout === "tabbed") {
      maxH = window.innerHeight * 0.45;
    } else {
      maxH = (paneTrim.clientHeight || 300) - 175;
    }
    maxH = Math.max(40, Math.min(420, maxH));
    const avail = editor.clientWidth || wrap.clientWidth;
    const w = Math.min(avail, maxH * v.videoWidth / v.videoHeight);
    trimStack.style.width = Math.max(120, Math.round(w)) + "px";
  }
  window.addEventListener("resize", sizeStack);

  v.addEventListener("loadedmetadata", () => {
    duration = v.duration || 0;
    tTotal.textContent = duration.toFixed(2) + "s total";
    if (v.videoWidth && v.videoHeight) v.style.aspectRatio = v.videoWidth + " / " + v.videoHeight;
    sizeStack();
    render();
  });

  function render() {
    hStart.style.left = (startPct * 100) + "%";
    hEnd.style.left = (endPct * 100) + "%";
    selection.style.left = (startPct * 100) + "%";
    selection.style.width = ((endPct - startPct) * 100) + "%";
    tStart.textContent = (startPct * duration).toFixed(2) + "s";
    tEnd.textContent = (endPct * duration).toFixed(2) + "s";
  }

  // ---- scrubber (same interaction model as the desktop app) ----
  let dragging = null;
  const pctFromEvent = (e) => {
    const r = track.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
  };
  const seek = (pct) => { if (duration) { v.pause(); v.currentTime = pct * duration; } };
  [hStart, hEnd].forEach((h) => h.addEventListener("pointerdown", (e) => {
    dragging = e.target.dataset.handle;
    e.target.setPointerCapture(e.pointerId);
    seek(dragging === "start" ? startPct : endPct);
    e.stopPropagation();
  }));
  window.addEventListener("pointermove", (e) => {
    if (!dragging || dragging === "seek") return;
    const pct = pctFromEvent(e);
    if (dragging === "start") startPct = Math.min(pct, endPct - 0.01);
    else endPct = Math.max(pct, startPct + 0.01);
    seek(dragging === "start" ? startPct : endPct);
    render();
  });
  window.addEventListener("pointerup", () => (dragging = null));
  track.addEventListener("pointerdown", (e) => {
    if (e.target === hStart || e.target === hEnd) return;
    dragging = "seek";
    track.setPointerCapture(e.pointerId);
    seek(pctFromEvent(e));
  });
  track.addEventListener("pointermove", (e) => { if (dragging === "seek") seek(pctFromEvent(e)); });
  function tickPlayhead() {
    if (duration) playhead.style.left = ((v.currentTime / duration) * 100) + "%";
    requestAnimationFrame(tickPlayhead);
  }
  tickPlayhead();

  document.getElementById("preview-sel").addEventListener("click", () => {
    v.currentTime = startPct * duration;
    v.play();
    const stop = () => {
      if (v.currentTime >= endPct * duration) { v.pause(); v.removeEventListener("timeupdate", stop); }
    };
    v.addEventListener("timeupdate", stop);
  });

  // ---- grab ----
  // Auto-load grabs triggered elsewhere (Stream Deck key, hotkey, the
  // desktop app) into this dock's trim editor, via /api/state polling.
  let lastGrabSeq = -1;
  async function pollGrabs() {
    try {
      const res = await fetch("/api/state");
      const s = await res.json();
      if (s.grabSeq !== undefined && s.grabSeq !== lastGrabSeq) {
        const first = lastGrabSeq === -1;
        lastGrabSeq = s.grabSeq;
        if (!first && s.lastGrab && s.lastGrab !== clipPath) {
          loadClip(s.lastGrab, { fresh: true });
          note("✓ grabbed — ready to trim", true);
          refreshClips();
        }
      }
    } catch { /* app offline */ }
    setTimeout(pollGrabs, 300);
  }
  pollGrabs();

  const grabBtn = document.getElementById("grab-btn");
  grabBtn.addEventListener("click", async () => {
    grabBtn.disabled = true;
    note("Grabbing…", true);
    try {
      const res = await fetch("/api/grab", { method: "POST" });
      if (!res.ok) { note("✗ " + (await res.text())); return; }
      const data = await res.json();
      await loadClip(data.path, { fresh: true });
      note("✓ grabbed — trim away");
      refreshClips();
    } catch { note("✗ ReplayTrim app is not running"); }
    finally { grabBtn.disabled = false; }
  });

  // ---- send ----
  const sendBtn = document.getElementById("send-btn");
  sendBtn.addEventListener("click", async () => {
    if (!clipPath) return;
    sendBtn.disabled = true;
    note("Trimming & sending…", true);
    try {
      const res = await fetch("/api/send_trim", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          path: clipPath,
          start: startPct * duration,
          end: endPct * duration,
          fast: document.getElementById("fast").checked,
        }),
      });
      note(res.ok ? "✓ playing on stream" : "✗ " + (await res.text()));
      if (res.ok) refreshClips();
    } catch { note("✗ send failed"); }
    finally { sendBtn.disabled = false; }
  });

  // ---- clip library ----
  function ago(epoch) {
    const s = Math.max(0, Math.floor(Date.now() / 1000 - epoch));
    if (s < 60) return s + "s ago";
    if (s < 3600) return Math.floor(s / 60) + "m ago";
    if (s < 86400) return Math.floor(s / 3600) + "h ago";
    return Math.floor(s / 86400) + "d ago";
  }
  function sendAsIs(path, durationSecs) {
    note("Sending…", true);
    fetch("/api/send_trim", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ path, start: 0, end: durationSecs || 3600, fast: true }),
    })
      .then((res) => note(res.ok ? "✓ playing on stream" : "✗ send failed"))
      .catch(() => note("✗ send failed"));
  }

  // Two-step delete: first click arms the button, second click deletes.
  function makeDeleteBtn(path, onDone) {
    const btn = document.createElement("button");
    btn.textContent = "🗑";
    btn.title = "Delete this file";
    let armed = false, timer = null;
    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      if (!armed) {
        armed = true;
        btn.textContent = "sure?";
        timer = setTimeout(() => { armed = false; btn.textContent = "🗑"; }, 3000);
        return;
      }
      clearTimeout(timer);
      const res = await fetch("/api/delete", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ path }),
      }).catch(() => null);
      note(res && res.ok ? "✓ deleted" : "✗ " + (res ? await res.text() : "delete failed"));
      onDone?.();
    });
    return btn;
  }

  function makeRow(leftHtml, path, durationSecs, onChanged) {
    const row = document.createElement("div");
    row.className = "clip-row";
    row.innerHTML = leftHtml;
    const play = document.createElement("button");
    play.textContent = "▶";
    play.title = "Play on stream as-is";
    play.addEventListener("click", (e) => { e.stopPropagation(); sendAsIs(path, durationSecs); });
    row.appendChild(play);
    row.appendChild(makeDeleteBtn(path, onChanged));
    row.addEventListener("click", () => loadClip(path));
    return row;
  }

  async function refreshClips() {
    try {
      const res = await fetch("/api/clips");
      const data = await res.json();
      if (data.listLimit && data.listLimit !== listLimit) {
        listLimit = data.listLimit;
        applyView();
      }
      if (!data.clips.length) { clipsBox.innerHTML = '<span class="inline">None yet — grab something!</span>'; return; }
      clipsBox.innerHTML = "";
      data.clips.forEach((c) => {
        const badge = '<span class="badge ' + (c.kind === "trim" ? "trim" : "") + '">' + c.kind + "</span>";
        const meta = '<span class="meta">' + ago(c.savedAtEpoch) + " · " + c.durationSecs.toFixed(1) + "s</span>";
        clipsBox.appendChild(makeRow(badge + meta, c.path, c.durationSecs, () => { refreshClips(); refreshFolder(true); }));
      });
      clipsBox.scrollTop = 0;
    } catch { /* app offline */ }
  }
  refreshClips();
  setInterval(refreshClips, 5000);

  // ---- folder browser ----
  let folderWhich = "clips";
  folderBtn.addEventListener("click", () => {
    folderOpen = !folderOpen;
    folderBtn.textContent = folderOpen ? "📂 Hide folder" : "📂 Browse folder";
    applyView();
    if (folderOpen) refreshFolder(true);
  });
  folderSection.querySelectorAll(".pill-sm").forEach((pill) =>
    pill.addEventListener("click", () => {
      folderWhich = pill.dataset.which;
      folderSection.querySelectorAll(".pill-sm").forEach((p) =>
        p.classList.toggle("active", p === pill));
      refreshFolder(true);
    }));
  async function refreshFolder(force) {
    const visible = folderOpen || (layout === "tabbed" && currentTab === "folder-section");
    if (!visible && !force) return;
    if (!visible) return;
    try {
      const res = await fetch("/api/folder?which=" + folderWhich);
      if (!res.ok) {
        folderBox.innerHTML = '<span class="inline">' + (await res.text()) + "</span>";
        return;
      }
      const data = await res.json();
      document.getElementById("folder-title").textContent = "Saved in: " + data.dir;
      folderBox.innerHTML = "";
      if (!data.files.length) {
        folderBox.innerHTML = '<span class="inline">No videos here yet.</span>';
        return;
      }
      data.files.forEach((f) => {
        const meta = '<span class="meta" title="' + f.name + '">' + f.name + "</span>" +
          '<span class="badge">' + ago(f.modifiedEpoch) + " · " + f.sizeMb.toFixed(0) + "MB</span>";
        folderBox.appendChild(makeRow(meta, f.path, 0, () => refreshFolder(true)));
      });
    } catch { /* app offline */ }
  }

  applyView();
</script>
</body>
</html>
"##;

/// The overlay itself: transparent page, full-viewport video, controls that
/// only appear while the mouse moves over the page (i.e. when the streamer
/// uses OBS's Interact window) so viewers just see the clip.
const OVERLAY_HTML: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>ReplayTrim Overlay</title>
<style>
  html, body { margin: 0; height: 100%; background: transparent; overflow: hidden; }
  #wrap { position: fixed; inset: 0; opacity: 0; transition: opacity 0.4s ease; }
  #wrap.visible { opacity: 1; }
  /* Explicit hide command = instant cut, no lingering fade */
  #wrap.instant { transition: none; }
  video { width: 100%; height: 100%; object-fit: contain; background: transparent; }
  #controls {
    position: fixed; left: 50%; bottom: 4%; transform: translateX(-50%);
    display: flex; gap: 10px; padding: 10px 14px; border-radius: 12px;
    background: rgba(15, 17, 21, 0.82); border: 1px solid rgba(255,255,255,0.18);
    opacity: 0; transition: opacity 0.25s ease; pointer-events: none;
  }
  #controls.shown { opacity: 1; pointer-events: auto; }
  #controls button {
    font: 600 15px "Segoe UI", sans-serif; color: #fff; cursor: pointer;
    background: rgba(255,255,255,0.12); border: 1px solid rgba(255,255,255,0.25);
    border-radius: 8px; padding: 8px 14px;
  }
  #controls button:hover { background: rgba(79,140,255,0.55); }
</style>
</head>
<body>
<div id="wrap">
  <video id="v" playsinline></video>
</div>
<div id="controls">
  <button id="playpause">Pause</button>
  <button id="restart">Restart</button>
  <button id="hide">Hide</button>
</div>
<script>
  const wrap = document.getElementById("wrap");
  const v = document.getElementById("v");
  const controls = document.getElementById("controls");
  const playpause = document.getElementById("playpause");
  let generation = -1;
  let lastCmdSeq = -1;
  let hideTimer = null;

  // Controls appear only while the mouse is moving over the page — that
  // only happens in OBS's Interact window, so the stream stays clean.
  document.addEventListener("mousemove", () => {
    controls.classList.add("shown");
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => controls.classList.remove("shown"), 2500);
  });

  playpause.addEventListener("click", () => {
    if (v.paused) { v.play(); } else { v.pause(); }
  });
  v.addEventListener("play", () => (playpause.textContent = "Pause"));
  v.addEventListener("pause", () => (playpause.textContent = "Play"));
  document.getElementById("restart").addEventListener("click", () => {
    v.currentTime = 0;
    wrap.classList.add("visible");
    v.play();
  });
  document.getElementById("hide").addEventListener("click", hideNow);
  v.addEventListener("ended", () => wrap.classList.remove("visible"));

  function hideNow() {
    v.pause();
    wrap.classList.add("instant");
    wrap.classList.remove("visible");
    // restore the fade for future natural clip-endings
    requestAnimationFrame(() => requestAnimationFrame(() => wrap.classList.remove("instant")));
  }

  function applyCommand(cmd) {
    if (cmd === "replay") {
      if (!v.src) v.src = "/clip?g=" + generation;
      v.currentTime = 0;
      wrap.classList.add("visible");
      v.play().catch(() => {});
    } else if (cmd === "pause") {
      if (v.paused) { v.play().catch(() => {}); } else { v.pause(); }
    } else if (cmd === "hide") {
      hideNow();
    }
  }

  async function poll() {
    try {
      const res = await fetch("/api/state");
      const s = await res.json();
      const firstLoad = generation === -1;
      if (s.hasClip && s.generation !== generation) {
        generation = s.generation;
        // On the very first poll after (re)load, remember the generation but
        // don't replay an old clip — only NEW sends should trigger playback.
        if (!firstLoad) {
          v.src = "/clip?g=" + generation;
          wrap.classList.add("visible");
          v.play().catch(() => {});
        }
      }
      if (s.cmdSeq !== undefined && s.cmdSeq !== lastCmdSeq) {
        const skip = lastCmdSeq === -1; // don't replay stale commands on page load
        lastCmdSeq = s.cmdSeq;
        if (!skip && s.cmd) applyCommand(s.cmd);
      }
    } catch (e) { /* app not running; keep polling quietly */ }
    setTimeout(poll, 150);
  }
  poll();
</script>
</body>
</html>
"#;
