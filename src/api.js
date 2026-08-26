const { invoke } = window.__TAURI__.core;

export const api = {
  getConfig: () => invoke("get_config"),
  saveConfig: (config) => invoke("save_config", { config }),
  connectObs: () => invoke("connect_obs"),
  listMediaSources: () => invoke("list_media_sources"),
  grabReplay: () => invoke("grab_replay"),
  generateWaveform: (inputPath) => invoke("generate_waveform", { inputPath }),
  exportTrim: (inputPath, start, end, fast) => invoke("export_trim", { inputPath, start, end, fast }),
  pushToObs: (filePath) => invoke("push_to_obs", { filePath }),
  readFileBytes: (path) => invoke("read_file_bytes", { path }),
};

/** Reads a local file via the backend and returns a Blob URL usable in <video src>/<img src>. */
export async function readFileAsBlobUrl(path, mimeType) {
  const bytes = await api.readFileBytes(path);
  const blob = new Blob([new Uint8Array(bytes)], { type: mimeType });
  return URL.createObjectURL(blob);
}
