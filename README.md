# ReplayTrim

A small standalone companion app for OBS: grab the last N seconds from OBS's own Replay Buffer on a hotkey, trim it with a visual waveform scrubber, and push the trimmed clip into a Media Source — live on stream, no forced scene switch.

Talks to OBS purely over its built-in WebSocket server (OBS 28+, Tools → WebSocket Server Settings) — no OBS plugins required, not even a third-party replay tool.

## Prerequisites

- OBS Studio with Replay Buffer configured (Settings → Output → Replay Buffer) and the WebSocket server enabled
- `ffmpeg` on PATH
- Rust (stable-msvc) + Visual Studio Build Tools, Node.js 20+

## Run

```bash
npm install
npm run tauri dev
```

## Configure

In Settings: OBS host/port/password (from Tools → WebSocket Server Settings), the target Media Source name, and the global grab hotkey.

## Notes

- `GetLastReplayBufferReplay`'s response field is `savedReplayPath` — some published docs list a different field name; verify against a live OBS instance if requests seem to silently fail.
- The waveform is rendered via ffmpeg's `showwavespic` filter (not `showwavespng`).
- Fast trim uses `-c copy` (near-instant, snaps to the nearest keyframe); accurate trim re-encodes (frame-accurate, slower). Both are exposed as an option.
- Config and clips are stored in the OS app-data directory, not in this repo.
