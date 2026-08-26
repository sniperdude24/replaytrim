export function openModal(innerHtml) {
  const overlay = document.createElement("div");
  overlay.className = "modal-overlay";
  overlay.innerHTML = `<div class="modal-box">${innerHtml}</div>`;
  document.body.appendChild(overlay);

  const close = () => overlay.remove();
  overlay.addEventListener("click", (e) => {
    if (e.target === overlay) close();
  });

  return { overlay, close };
}
