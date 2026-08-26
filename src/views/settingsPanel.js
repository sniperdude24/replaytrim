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
        <legend>Grab Hotkey</legend>
        <label>Global shortcut <input name="grab_hotkey" value="${escapeHtml(config.grab_hotkey)}" placeholder="CommandOrControl+Shift+R"></label>
        <p class="hint">Works even while OBS/your game is focused. Restart the app after changing this.</p>
      </fieldset>
      <div class="modal-actions">
        <button type="button" id="cancel-btn" class="btn btn-ghost">Cancel</button>
        <button type="submit" class="btn btn-primary">Save</button>
      </div>
    </form>
  `);

  const form = document.getElementById("settings-form");
  form.querySelector("#cancel-btn").addEventListener("click", close);
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const data = new FormData(form);
    const newConfig = {
      obs_host: data.get("obs_host"),
      obs_port: Number(data.get("obs_port")),
      obs_password: data.get("obs_password"),
      target_source: data.get("target_source"),
      grab_hotkey: data.get("grab_hotkey"),
    };
    await api.saveConfig(newConfig);
    close();
    onSaved?.();
  });
}
