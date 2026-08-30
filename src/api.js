const { invoke } = window.__TAURI__.core;

export const api = {
  getConfig: () => invoke("get_config"),
  saveConfig: (config) => invoke("save_config", { config }),
  connectObs: () => invoke("connect_obs"),
  listMediaSources: () => invoke("list_media_sources"),
  listScenes: () => invoke("list_scenes"),
  createObsSource: (sceneName, sourceName) => invoke("create_obs_source", { sceneName, sourceName }),
  createObsOverlay: (sceneName, sourceName) => invoke("create_obs_overlay", { sceneName, sourceName }),
  ensureReady: () => invoke("ensure_ready"),
  checkTargetExists: () => invoke("check_target_exists"),
  instantReplay: () => invoke("instant_replay"),
  getAutostart: () => invoke("get_autostart"),
  setAutostart: (enabled) => invoke("set_autostart", { enabled }),
  overlayCommand: (action) => invoke("overlay_command", { action }),
  grabReplay: () => invoke("grab_replay"),
  generateWaveform: (inputPath) => invoke("generate_waveform", { inputPath }),
  exportTrim: (inputPath, start, end, fast) => invoke("export_trim", { inputPath, start, end, fast }),
  pushToObs: (filePath, durationSecs) => invoke("push_to_obs", { filePath, durationSecs }),
  toggleSourceVisible: () => invoke("toggle_source_visible"),
  readFileBytes: (path) => invoke("read_file_bytes", { path }),
};

/** Reads a local file via the backend and returns a Blob URL usable in <video src>/<img src>. */
export async function readFileAsBlobUrl(path, mimeType) {
  const bytes = await api.readFileBytes(path);
  const blob = new Blob([new Uint8Array(bytes)], { type: mimeType });
  return URL.createObjectURL(blob);
}
