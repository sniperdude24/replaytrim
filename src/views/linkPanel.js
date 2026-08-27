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

  const sceneOptions = sceneList.scenes
    .map(
      (s) =>
        `<option value="${escapeHtml(s)}" ${s === sceneList.current ? "selected" : ""}>${escapeHtml(s)}${s === sceneList.current ? " (current)" : ""}</option>`
    )
    .join("");

  const { close } = openModal(`
    <h2>Link to OBS</h2>
    <p class="hint">Pick how replays show up in your stream. The overlay player is invisible
    until a clip plays, fades out when it ends, and has on-video controls (play/pause/restart/hide)
    you can click through OBS's Interact window.</p>
    <form id="link-form">
      <fieldset>
        <legend>Overlay player (recommended)</legend>
        <label>Put it in this scene
          <select id="overlay-scene-select">${sceneOptions}</select>
        </label>
        <label>Overlay name <input id="overlay-name" value="ReplayTrim Overlay"></label>
        <button type="button" id="create-overlay-btn" class="btn btn-primary">Create Overlay &amp; Link</button>
        <p class="hint">Fills the whole scene, video letterboxed inside. Resize/move it in OBS if you want it smaller.</p>
      </fieldset>
      <fieldset>
        <legend>Or a plain Media Source</legend>
        <label>Put it in this scene
          <select id="scene-select">${sceneOptions}</select>
        </label>
        <label>Source name <input id="new-source-name" value="ReplayTrim Replay"></label>
        <button type="button" id="create-source-btn" class="btn">Create in OBS &amp; Link</button>
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

  document.getElementById("create-overlay-btn").addEventListener("click", async () => {
    const sceneName = document.getElementById("overlay-scene-select").value;
    const sourceName = document.getElementById("overlay-name").value.trim();
    if (!sceneName) {
      msg.textContent = "No scene selected — are you connected to OBS?";
      return;
    }
    if (!sourceName) {
      msg.textContent = "Give the overlay a name.";
      return;
    }
    msg.textContent = "Creating overlay in OBS…";
    try {
      await api.createObsOverlay(sceneName, sourceName);
      close();
      onLinked?.();
    } catch (e) {
      msg.textContent = `Failed: ${e}`;
    }
  });

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
      config.target_kind = "media_source";
      await api.saveConfig(config);
      close();
      onLinked?.();
    } catch (e) {
      msg.textContent = `Failed: ${e}`;
    }
  });
}
