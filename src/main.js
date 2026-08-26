import { register, unregister } from "@tauri-apps/plugin-global-shortcut";
import { api } from "./api.js";
import { renderStatusHeader } from "./views/statusHeader.js";
import { openSettingsPanel } from "./views/settingsPanel.js";
import { renderTrimEditor } from "./views/trimEditor.js";

async function main() {
  const app = document.getElementById("app");
  app.innerHTML = `
    <div id="header"></div>
    <main class="app-main">
      <div id="editor" class="empty-state-wrap">
        <p class="empty-state">Connect to OBS, then Grab Last Replay (or press the hotkey) to start trimming.</p>
      </div>
    </main>
  `;

  const headerEl = document.getElementById("header");
  const editorEl = document.getElementById("editor");

  let status = { obsState: "not_configured", obsText: "Not connected" };
  let registeredHotkey = null;

  function renderHeader() {
    renderStatusHeader(headerEl, status, {
      onConnect: handleConnect,
      onGrab: handleGrab,
      onSettings: () =>
        openSettingsPanel(async () => {
          await applyHotkey();
        }),
    });
  }

  async function handleConnect() {
    status = { obsState: "connecting", obsText: "Connecting…" };
    renderHeader();
    try {
      await api.connectObs();
      status = { obsState: "connected", obsText: "Connected" };
    } catch (e) {
      status = { obsState: "error", obsText: String(e) };
    }
    renderHeader();
  }

  async function handleGrab() {
    if (status.obsState !== "connected") {
      alert("Connect to OBS first.");
      return;
    }
    editorEl.innerHTML = `<p class="empty-state">Grabbing last replay…</p>`;
    try {
      const path = await api.grabReplay();
      await renderTrimEditor(editorEl, path);
    } catch (e) {
      editorEl.innerHTML = `<p class="empty-state">Grab failed: ${e}</p>`;
    }
  }

  async function applyHotkey() {
    const config = await api.getConfig();
    if (registeredHotkey) {
      await unregister(registeredHotkey).catch(() => {});
      registeredHotkey = null;
    }
    if (config.grab_hotkey) {
      try {
        await register(config.grab_hotkey, () => handleGrab());
        registeredHotkey = config.grab_hotkey;
      } catch (e) {
        console.error("Failed to register hotkey", e);
      }
    }
  }

  renderHeader();
  await applyHotkey();
}

main();
