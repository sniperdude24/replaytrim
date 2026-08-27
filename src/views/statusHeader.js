import { renderPill } from "../components/pill.js";

export function renderStatusHeader(
  container,
  { obsState, obsText, sourceState, sourceText },
  handlers
) {
  container.innerHTML = `
    <header class="app-header">
      <h1>ReplayTrim</h1>
      ${renderPill("OBS", obsState, obsText)}
      <button id="source-pill-btn" class="pill-btn" title="Change which OBS source clips play through">
        ${renderPill("Plays through", sourceState, sourceText)}
      </button>
      <div class="header-actions">
        <button id="connect-btn" class="btn btn-ghost">Connect</button>
        <button id="grab-btn" class="btn btn-primary">Grab Last Replay</button>
        <button id="settings-btn" class="btn btn-ghost">Settings</button>
      </div>
    </header>
  `;
  container.querySelector("#connect-btn").addEventListener("click", handlers.onConnect);
  container.querySelector("#grab-btn").addEventListener("click", handlers.onGrab);
  container.querySelector("#settings-btn").addEventListener("click", handlers.onSettings);
  container.querySelector("#source-pill-btn").addEventListener("click", handlers.onLink);
}
