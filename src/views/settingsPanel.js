import { api } from "../api.js";
import { openModal } from "../components/modal.js";
import { escapeHtml } from "../components/pill.js";

export async function openSettingsPanel(onSaved) {
  const config = await api.getConfig();
  let sources = [];
  try {
    sources = await api.listMediaSources();
  } catch {
    // not connected yet — fine, the field falls back to free text
  }

  const { close } = openModal(`
    <h2>Settings</h2>
    <form id="settings-form">
      <fieldset>
        <legend>OBS WebSocket</legend>
        <label>Host <input name="obs_host" value="${escapeHtml(config.obs_host)}" required></label>
        <label>Port <input name="obs_port" type="number" value="${config.obs_port}" required></label>
        <label>Password <input name="obs_password" type="password" value="${escapeHtml(config.obs_password)}" placeholder="Tools → WebSocket Server Settings"></label>
      </fieldset>
      <fieldset>
        <legend>Target Media Source</legend>
        ${
          sources.length
            ? `<select name="target_source">
                <option value="">— select —</option>
                ${sources.map((s) => `<option value="${escapeHtml(s)}" ${config.target_source === s ? "selected" : ""}>${escapeHtml(s)}</option>`).join("")}
               </select>`
            : `<input name="target_source" value="${escapeHtml(config.target_source)}" placeholder="Media Source name in OBS (connect first to pick from a list)">`
        }
        <p class="hint">This Media Source's file will be updated and restarted whenever you send a trimmed clip. Place it anywhere in your scene layout.</p>
      </fieldset>
      <fieldset>
        <legend>Keybinds</legend>
        <label>Grab &amp; trim <input name="grab_hotkey" class="keybind-input" value="${escapeHtml(config.grab_hotkey)}" placeholder="click, then press keys" readonly></label>
        <label>Instant replay (no trim) <input name="instant_hotkey" class="keybind-input" value="${escapeHtml(config.instant_hotkey ?? "")}" placeholder="click, then press keys" readonly></label>
        <label>Replay again <input name="replay_hotkey" class="keybind-input" value="${escapeHtml(config.replay_hotkey ?? "")}" placeholder="click, then press keys" readonly></label>
        <label>Hide replay <input name="hide_hotkey" class="keybind-input" value="${escapeHtml(config.hide_hotkey ?? "")}" placeholder="click, then press keys" readonly></label>
        <p class="hint">Click a field, then press the key combo you want. Backspace clears it. Applies on Save — no restart needed. All keybinds work globally, even in-game.</p>
      </fieldset>
      <fieldset>
        <legend>Startup</legend>
        <label class="inline-checkbox" style="flex-direction:row"><input type="checkbox" id="autostart-cb"> Launch with Windows (starts minimized to the tray)</label>
        <p class="hint">Closing the window hides ReplayTrim to the tray; quit from the tray icon.</p>
      </fieldset>
      <fieldset>
        <legend>Clip Lists</legend>
        <label>Visible clips before scrolling <input name="clip_list_limit" type="number" min="3" max="30" value="${config.clip_list_limit ?? 5}"></label>
        <p class="hint">The dock's lists show this many rows; older clips stay reachable by scrolling inside the list.</p>
      </fieldset>
      <fieldset>
        <legend>OBS Control Dock</legend>
        <p class="hint">Get these buttons inside OBS: View → Docks → Custom Browser Docks → add<br>
        <code style="user-select:all">http://127.0.0.1:${config.overlay_port ?? 8930}/dock</code></p>
      </fieldset>
      <div class="modal-actions">
        <button type="button" id="cancel-btn" class="btn btn-ghost">Cancel</button>
        <button type="submit" class="btn btn-primary">Save</button>
      </div>
    </form>
  `);

  const form = document.getElementById("settings-form");
  form.querySelector("#cancel-btn").addEventListener("click", close);

  // Autostart applies immediately (it's system state, not config-file state).
  const autostartCb = form.querySelector("#autostart-cb");
  api.getAutostart().then((on) => (autostartCb.checked = on)).catch(() => {});
  autostartCb.addEventListener("change", () => {
    api.setAutostart(autostartCb.checked).catch(() => {});
  });

  // Press-to-record keybind fields: click, press a combo, done.
  form.querySelectorAll(".keybind-input").forEach((input) => {
    input.addEventListener("keydown", (e) => {
      e.preventDefault();
      if (e.key === "Backspace" || e.key === "Delete") {
        input.value = "";
        return;
      }
      if (e.key === "Escape") {
        input.blur();
        return;
      }
      // Ignore bare modifier presses — wait for a real key.
      if (["Control", "Shift", "Alt", "Meta"].includes(e.key)) return;
      const parts = [];
      if (e.ctrlKey || e.metaKey) parts.push("CommandOrControl");
      if (e.altKey) parts.push("Alt");
      if (e.shiftKey) parts.push("Shift");
      let key = e.key.length === 1 ? e.key.toUpperCase() : e.key;
      if (key === " ") key = "Space";
      parts.push(key);
      input.value = parts.join("+");
    });
  });
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const data = new FormData(form);
    // Spread the existing config so fields this form doesn't show
    // (target_kind, overlay_port, ...) survive the save.
    const newConfig = {
      ...config,
      obs_host: data.get("obs_host"),
      obs_port: Number(data.get("obs_port")),
      obs_password: data.get("obs_password"),
      target_source: data.get("target_source"),
      grab_hotkey: data.get("grab_hotkey"),
      instant_hotkey: data.get("instant_hotkey"),
      replay_hotkey: data.get("replay_hotkey"),
      hide_hotkey: data.get("hide_hotkey"),
      clip_list_limit: Number(data.get("clip_list_limit")) || 5,
    };
    await api.saveConfig(newConfig);
    close();
    onSaved?.();
  });
}
