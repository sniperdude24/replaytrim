import { register, unregister } from "@tauri-apps/plugin-global-shortcut";
import { api } from "./api.js";
import { openModal } from "./components/modal.js";
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
        <div class="empty-state-stack">
          <p class="empty-state" id="empty-msg">One button gets everything ready: OBS connection, Replay Buffer, and where replays appear.</p>
          <button id="setup-btn" class="btn btn-primary btn-big">Set Up Everything</button>
          <p class="hint">Then press the hotkey (Ctrl+Shift+R) or Grab Last Replay whenever something clip-worthy happens.</p>
        </div>
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
          await applyHotkeys();
          await refreshLinkStatus();
        }),
    });
  }

  function handleLink() {
    openLinkPanel(refreshLinkStatus);
  }

  /// The target (overlay or media source) is "linked" when it's set AND
  /// actually exists in OBS.
  async function refreshLinkStatus() {
    const config = await api.getConfig();
    let linked = false;
    if (!config.target_source) {
      status.sourceState = "not_configured";
      status.sourceText = "Not linked";
    } else if (status.obsState === "connected") {
      try {
        linked = await api.checkTargetExists();
        if (linked) {
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

  /// One-button setup: connect -> start Replay Buffer if off -> open the
  /// Link dialog if no playback target exists yet.
  async function handleSetup() {
    const emptyMsg = document.getElementById("empty-msg");
    const note = (text) => {
      if (emptyMsg) emptyMsg.textContent = text;
    };
    note("Setting up — connecting to OBS…");
    try {
      const report = await api.ensureReady();
      status.obsState = "connected";
      status.obsText = "Connected";
      await refreshLinkStatus();
      if (!report.linked) {
        note("Connected, Replay Buffer running. Last step: pick where replays appear.");
        openLinkPanel(async () => {
          await refreshLinkStatus();
          note("All set! Press Ctrl+Shift+R (or Grab Last Replay) whenever something clip-worthy happens.");
        });
      } else if (report.bufferStartedNow) {
        note("All set — the Replay Buffer was off, so I just started it. Give it a few seconds to record, then grab away.");
      } else {
        note("All set! Press Ctrl+Shift+R (or Grab Last Replay) whenever something clip-worthy happens.");
      }
    } catch (e) {
      note(`Setup hit a snag: ${e}`);
      status.obsState = "error";
      status.obsText = String(e);
      renderHeader();
    }
  }

  document.getElementById("link-btn").addEventListener("click", handleLink);
  document.getElementById("setup-btn").addEventListener("click", handleSetup);

  // Closing the window hides to the tray — hotkeys, the dock, and the
  // overlay keep working. Actually quitting happens from the tray menu.
  const appWindow = window.__TAURI__.window.getCurrentWindow();
  appWindow.onCloseRequested((event) => {
    event.preventDefault();
    appWindow.hide();
  });

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

  async function handleInstant() {
    try {
      const path = await api.instantReplay();
      editorEl.className = "";
      await renderTrimEditor(editorEl, path);
    } catch (e) {
      editorEl.className = "empty-state-wrap";
      editorEl.innerHTML = `<p class="empty-state">Instant replay failed: ${e}</p>`;
    }
  }

  let registeredHotkeys = [];

  async function applyHotkeys() {
    const config = await api.getConfig();
    for (const combo of registeredHotkeys) {
      await unregister(combo).catch(() => {});
    }
    registeredHotkeys = [];

    const bindings = [
      [config.grab_hotkey, () => handleGrab()],
      [config.instant_hotkey, () => handleInstant()],
      [config.replay_hotkey, () => api.overlayCommand("replay").catch(() => {})],
      [config.hide_hotkey, () => api.overlayCommand("hide").catch(() => {})],
    ];
    for (const [combo, handler] of bindings) {
      if (!combo) continue;
      try {
        await register(combo, handler);
        registeredHotkeys.push(combo);
      } catch (e) {
        console.error(`Failed to register hotkey ${combo}`, e);
      }
    }
  }

  renderHeader();
  await applyHotkeys();

  // Dock buttons reach the app as Tauri events from the local server.
  const { listen } = window.__TAURI__.event;
  await listen("dock-grab", () => handleGrab());
  await listen("clip-grabbed", async (event) => {
    editorEl.className = "";
    await renderTrimEditor(editorEl, event.payload);
  });

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
