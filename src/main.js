import { register, unregister } from "@tauri-apps/plugin-global-shortcut";
import { api } from "./api.js";
import { renderStatusHeader } from "./views/statusHeader.js";
import { openSettingsPanel } from "./views/settingsPanel.js";
import { openLinkPanel } from "./views/linkPanel.js";
import { renderTrimEditor } from "./views/trimEditor.js";

async function main() {
  const app = document.getElementById("app");
  app.innerHTML = `
    <div id="header"></div>
    <div id="link-banner" class="banner" style="display:none">
      <span><strong>Almost there —</strong> ReplayTrim needs a Media Source in OBS to play your clips through. One click sets it up.</span>
      <button id="link-btn" class="btn btn-primary">Link to OBS</button>
    </div>
    <main class="app-main">
      <div id="editor" class="empty-state-wrap">
        <p class="empty-state">Connect to OBS, then Grab Last Replay (or press the hotkey) to start trimming.</p>
      </div>
    </main>
  `;

  const headerEl = document.getElementById("header");
  const bannerEl = document.getElementById("link-banner");
  const editorEl = document.getElementById("editor");

  let status = {
    obsState: "not_configured",
    obsText: "Not connected",
    sourceState: "not_configured",
    sourceText: "Not linked",
  };
  let registeredHotkey = null;

  function renderHeader() {
    renderStatusHeader(headerEl, status, {
      onConnect: handleConnect,
      onGrab: handleGrab,
      onLink: handleLink,
      onSettings: () =>
        openSettingsPanel(async () => {
          await applyHotkey();
          await refreshLinkStatus();
        }),
    });
  }

  function handleLink() {
    openLinkPanel(refreshLinkStatus);
  }

  /// The target source is "linked" when it's set AND actually exists in OBS.
  async function refreshLinkStatus() {
    const config = await api.getConfig();
    let linked = false;
    if (!config.target_source) {
      status.sourceState = "not_configured";
      status.sourceText = "Not linked";
    } else if (status.obsState === "connected") {
      try {
        const sources = await api.listMediaSources();
        if (sources.includes(config.target_source)) {
          linked = true;
          status.sourceState = "connected";
          status.sourceText = config.target_source;
        } else {
          status.sourceState = "error";
          status.sourceText = `"${config.target_source}" not found in OBS`;
        }
      } catch {
        status.sourceState = "not_configured";
        status.sourceText = config.target_source;
      }
    } else {
      status.sourceState = "not_configured";
      status.sourceText = config.target_source;
    }
    bannerEl.style.display = status.obsState === "connected" && !linked ? "flex" : "none";
    renderHeader();
  }

  document.getElementById("link-btn").addEventListener("click", handleLink);

  async function handleConnect() {
    status.obsState = "connecting";
    status.obsText = "Connecting…";
    renderHeader();
    try {
      await api.connectObs();
      status.obsState = "connected";
      status.obsText = "Connected";
    } catch (e) {
      status.obsState = "error";
      status.obsText = String(e);
    }
    await refreshLinkStatus();
  }

  async function handleGrab() {
    if (status.obsState !== "connected") {
      alert("Connect to OBS first.");
      return;
    }
    editorEl.className = "empty-state-wrap";
    editorEl.innerHTML = `<p class="empty-state">Grabbing last replay…</p>`;
    try {
      const path = await api.grabReplay();
      // Drop the flex-centering wrapper class: a centered child taller than
      // the wrapper overflows off the top unreachably.
      editorEl.className = "";
      await renderTrimEditor(editorEl, path);
    } catch (e) {
      editorEl.className = "empty-state-wrap";
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

  // Auto-connect on launch when OBS credentials are already saved, so the
  // global hotkey works immediately without opening the app window.
  const config = await api.getConfig();
  if (config.obs_password) {
    await handleConnect();
  } else {
    await refreshLinkStatus();
  }
}

main();
