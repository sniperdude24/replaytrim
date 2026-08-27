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
            let mut overlay = ctx.state.overlay.lock().await;
            overlay.cmd_seq += 1;
            overlay.cmd = Some(action);
            (StatusCode::OK, "ok").into_response()
        }
        _ => (StatusCode::NOT_FOUND, "unknown action").into_response(),
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
    let Ok(bytes) = tokio::fs::read(&path).await else {
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

/// The OBS control dock: added once via View → Docks → Custom Browser Docks.
/// Buttons drive the app and the overlay through /api/cmd/*.
const DOCK_HTML: &str = r#"<!doctype html>
<html>
<head>
<meta charset="utf-8">
<title>ReplayTrim</title>
<style>
  :root { color-scheme: dark; }
  html, body { margin: 0; height: 100%; background: #1a1c20; color: #e6e8eb;
    font-family: "Segoe UI", sans-serif; }
  #wrap { display: flex; flex-direction: column; gap: 8px; padding: 10px; }
  button {
    font: 600 14px "Segoe UI", sans-serif; color: #fff; cursor: pointer;
    background: #2a2d34; border: 1px solid #3a3e46; border-radius: 8px;
    padding: 12px 10px; text-align: left;
  }
  button:hover { border-color: #4f8cff; }
  button.primary { background: #4f8cff; border-color: #4f8cff; }
  #status { font-size: 12px; color: #9aa0aa; padding: 2px 2px 0; min-height: 16px; }
</style>
</head>
<body>
<div id="wrap">
  <button class="primary" data-cmd="grab">🎬 Grab &amp; Trim <span style="font-weight:400;opacity:.75">— opens in ReplayTrim</span></button>
  <button class="primary" data-cmd="instant">⚡ Instant Replay <span style="font-weight:400;opacity:.75">— play whole buffer now</span></button>
  <button data-cmd="replay">🔁 Replay Again</button>
  <button data-cmd="pause">⏯ Pause / Resume</button>
  <button data-cmd="hide">🚫 Hide Replay</button>
  <div id="status"></div>
</div>
<script>
  const status = document.getElementById("status");
  document.querySelectorAll("button[data-cmd]").forEach((btn) => {
    btn.addEventListener("click", async () => {
      const cmd = btn.dataset.cmd;
      status.textContent = "…";
      try {
        const res = await fetch("/api/cmd/" + cmd, { method: "POST" });
        const text = await res.text();
        status.textContent = res.ok ? "✓ " + text : "✗ " + text;
      } catch (e) {
        status.textContent = "✗ ReplayTrim app is not running";
      }
      setTimeout(() => { if (status.textContent) status.textContent = ""; }, 4000);
    });
  });
</script>
</body>
</html>
"#;

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
  document.getElementById("hide").addEventListener("click", () => {
    v.pause();
    wrap.classList.remove("visible");
  });
  v.addEventListener("ended", () => wrap.classList.remove("visible"));

  function applyCommand(cmd) {
    if (cmd === "replay") {
      if (!v.src) v.src = "/clip?g=" + generation;
      v.currentTime = 0;
      wrap.classList.add("visible");
      v.play().catch(() => {});
    } else if (cmd === "pause") {
      if (v.paused) { v.play().catch(() => {}); } else { v.pause(); }
    } else if (cmd === "hide") {
      v.pause();
      wrap.classList.remove("visible");
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
    setTimeout(poll, 500);
  }
  poll();
</script>
</body>
</html>
"#;
