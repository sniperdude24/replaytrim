import { api } from "../api.js";
import { openModal } from "../components/modal.js";
import { escapeHtml } from "../components/pill.js";

/**
 * One-stop dialog for wiring ReplayTrim into OBS: creates a Media Source
 * in a chosen scene (the normal path), or links an existing one.
 */
export async function openLinkPanel(onLinked) {
  let sceneList = { scenes: [], current: "" };
  let sources = [];
  try {
    sceneList = await api.listScenes();
  } catch {}
  try {
    sources = await api.listMediaSources();
  } catch {}

  const { close } = openModal(`
    <h2>Link to OBS</h2>
    <p class="hint">Trimmed clips play through a Media Source inside OBS. Create one below —
    it shows up in the scene you pick, and you can move and resize it in OBS like any other source.
    It stays invisible until a clip is playing.</p>
    <form id="link-form">
      <fieldset>
        <legend>Create a new source (recommended)</legend>
        <label>Put it in this scene
          <select id="scene-select">
            ${sceneList.scenes
              .map(
                (s) =>
                  `<option value="${escapeHtml(s)}" ${s === sceneList.current ? "selected" : ""}>${escapeHtml(s)}${s === sceneList.current ? " (current)" : ""}</option>`
              )
              .join("")}
          </select>
        </label>
        <label>Source name <input id="new-source-name" value="ReplayTrim Replay"></label>
        <button type="button" id="create-source-btn" class="btn btn-primary">Create in OBS &amp; Link</button>
      </fieldset>
      ${
        sources.length
          ? `<fieldset>
              <legend>Or use a Media Source you already have</legend>
              <select id="existing-select">
                ${sources.map((s) => `<option value="${escapeHtml(s)}">${escapeHtml(s)}</option>`).join("")}
              </select>
              <div class="modal-actions" style="justify-content:flex-start; margin-top:0.5rem">
                <button type="button" id="use-existing-btn" class="btn">Link This Source</button>
              </div>
            </fieldset>`
          : ""
      }
      <p id="link-msg" class="hint"></p>
    </form>
  `);

  const msg = document.getElementById("link-msg");

  document.getElementById("create-source-btn").addEventListener("click", async () => {
    const sceneName = document.getElementById("scene-select").value;
    const sourceName = document.getElementById("new-source-name").value.trim();
    if (!sceneName) {
      msg.textContent = "No scene selected — are you connected to OBS?";
      return;
    }
    if (!sourceName) {
      msg.textContent = "Give the source a name.";
      return;
    }
    msg.textContent = "Creating source in OBS…";
    try {
      await api.createObsSource(sceneName, sourceName);
      close();
      onLinked?.();
    } catch (e) {
      msg.textContent = `Failed: ${e}`;
    }
  });

  document.getElementById("use-existing-btn")?.addEventListener("click", async () => {
    const sourceName = document.getElementById("existing-select").value;
    try {
      const config = await api.getConfig();
      config.target_source = sourceName;
      await api.saveConfig(config);
      close();
      onLinked?.();
    } catch (e) {
      msg.textContent = `Failed: ${e}`;
    }
  });
}
