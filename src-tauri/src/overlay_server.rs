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
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let ctx = ServerCtx { app, state };
    let router = Router::new()
        .route("/overlay", get(overlay_page))
        .route("/dock", get(dock_page))
        .route("/api/state", get(api_state))
        .route("/api/cmd/:action", post(api_cmd))
        .route("/api/grab", post(api_grab))
        .route("/api/clips", get(api_clips))
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
        "grab" => {
            let _ = ctx.app.emit("dock-grab", ());
            (StatusCode::OK, "grabbing").into_response()
        }
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
    let clips = ctx.state.clips.lock().await;
    let list: Vec<_> = clips
        .iter()
        .rev()
        .filter(|c| std::path::Path::new(&c.path).exists())
        .collect();
    Json(json!({ "clips": list }))
}

/// Only files the app itself produced/grabbed (i.e. in the clip library or
/// currently loaded in the overlay) may be served.
async fn path_allowed(ctx: &ServerCtx, path: &str) -> bool {
    let clips = ctx.state.clips.lock().await;
    if clips.iter().any(|c| c.path == path) {
        return true;
    }
    drop(clips);
    let overlay = ctx.state.overlay.lock().await;
    overlay
        .clip_path
        .as_ref()
        .map(|p| p.to_string_lossy() == path)
        .unwrap_or(false)
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
  html, body { margin: 0; background: #1a1c20; color: #e6e8eb;
    font-family: "Segoe UI", sans-serif; font-size: 13px; }
  #wrap { display: flex; flex-direction: column; gap: 8px; padding: 8px; }
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
  #preview { width: 100%; max-height: 220px; background: #000; border-radius: 6px; display: block; }
  #editor.collapsed #preview { display: none; }

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
  </div>
  <div id="status"></div>

  <div id="editor">
    <div class="row" style="justify-content: space-between; align-items: center;">
      <span class="inline" id="clip-name" style="overflow:hidden;text-overflow:ellipsis;white-space:nowrap;max-width:60%"></span>
      <button id="toggle-preview">▤ Hide preview</button>
    </div>
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
    <div class="row" style="align-items:center">
      <label class="inline"><input type="checkbox" id="fast" checked> fast trim</label>
      <button id="preview-sel">Preview</button>
      <button class="primary" id="send-btn" style="margin-left:auto">Send &amp; Play</button>
    </div>
  </div>

  <h3>Recent clips</h3>
  <div id="clips"><span class="inline">None yet — grab something!</span></div>
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

  // ---- collapse toggle (remembered) ----
  const toggleBtn = document.getElementById("toggle-preview");
  function setCollapsed(collapsed) {
    editor.classList.toggle("collapsed", collapsed);
    toggleBtn.textContent = collapsed ? "▤ Show preview" : "▤ Hide preview";
    try { localStorage.setItem("rt-collapsed", collapsed ? "1" : ""); } catch {}
  }
  toggleBtn.addEventListener("click", () => setCollapsed(!editor.classList.contains("collapsed")));
  try { if (localStorage.getItem("rt-collapsed") === "1") setCollapsed(true); } catch {}

  // ---- load a clip into the editor ----
  async function loadClip(path) {
    clipPath = path;
    startPct = 0; endPct = 1; duration = 0;
    editor.classList.add("active");
    document.getElementById("clip-name").textContent = path.split(/[\\/]/).pop();
    v.src = "/api/file?path=" + encodeURIComponent(path);
    waveform.src = "/api/waveform?path=" + encodeURIComponent(path);
    render();
  }

  v.addEventListener("loadedmetadata", () => {
    duration = v.duration || 0;
    tTotal.textContent = duration.toFixed(2) + "s total";
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
  const grabBtn = document.getElementById("grab-btn");
  grabBtn.addEventListener("click", async () => {
    grabBtn.disabled = true;
    note("Grabbing…", true);
    try {
      const res = await fetch("/api/grab", { method: "POST" });
      if (!res.ok) { note("✗ " + (await res.text())); return; }
      const data = await res.json();
      await loadClip(data.path);
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
  async function refreshClips() {
    try {
      const res = await fetch("/api/clips");
      const data = await res.json();
      const box = document.getElementById("clips");
      if (!data.clips.length) { box.innerHTML = '<span class="inline">None yet — grab something!</span>'; return; }
      box.innerHTML = "";
      data.clips.slice(0, 8).forEach((c) => {
        const row = document.createElement("div");
        row.className = "clip-row";
        const badge = '<span class="badge ' + (c.kind === "trim" ? "trim" : "") + '">' + c.kind + "</span>";
        row.innerHTML = badge +
          '<span class="meta">' + ago(c.savedAtEpoch) + " · " + c.durationSecs.toFixed(1) + "s</span>" +
          '<button title="Play on stream as-is">▶</button>';
        row.addEventListener("click", () => loadClip(c.path));
        row.querySelector("button").addEventListener("click", async (e) => {
          e.stopPropagation();
          note("Sending…", true);
          const res = await fetch("/api/send_trim", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ path: c.path, start: 0, end: c.durationSecs, fast: true }),
          }).catch(() => null);
          note(res && res.ok ? "✓ playing on stream" : "✗ send failed");
        });
        box.appendChild(row);
      });
    } catch { /* app offline */ }
  }
  refreshClips();
  setInterval(refreshClips, 5000);
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
