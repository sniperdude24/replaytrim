export function renderPill(label, state, text) {
  return `
    <div class="pill pill-${state}" title="${escapeHtml(text)}">
      <span class="pill-dot"></span>
      <span class="pill-label">${escapeHtml(label)}</span>
      <span class="pill-text">${escapeHtml(text)}</span>
    </div>
  `;
}

export function escapeHtml(str) {
  return String(str ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;",
    "<": "&lt;",
    ">": "&gt;",
    '"': "&quot;",
    "'": "&#39;",
  })[c]);
}
