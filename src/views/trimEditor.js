import { api, readFileAsBlobUrl } from "../api.js";

export async function renderTrimEditor(container, replayPath, onSent) {
  container.innerHTML = `
    <section class="trim-editor">
      <div class="section-header">
        <h2>Trim</h2>
        <span class="clip-path" title="${replayPath}">${replayPath.split(/[\\/]/).pop()}</span>
      </div>
      <video id="preview" controls></video>
      <div class="scrubber" id="scrubber">
        <img id="waveform-img" class="waveform-img" alt="waveform">
        <div class="scrubber-track">
          <div class="scrubber-selection"></div>
          <div class="scrubber-handle scrubber-handle-start" data-handle="start"></div>
          <div class="scrubber-handle scrubber-handle-end" data-handle="end"></div>
        </div>
      </div>
      <div class="trim-times">
        <span id="start-label">0.00s</span>
        <span id="duration-label"></span>
        <span id="end-label">0.00s</span>
      </div>
      <div class="trim-footer">
        <div class="trim-actions">
          <label class="inline-checkbox"><input type="checkbox" id="fast-trim" checked> fast trim (instant, snaps to keyframe)</label>
          <button id="preview-btn" class="btn btn-ghost">Preview Selection</button>
          <button id="send-btn" class="btn btn-primary">Send to OBS</button>
        </div>
        <p id="status-msg" class="hint"></p>
      </div>
    </section>
  `;

  const video = container.querySelector("#preview");
  const waveformImg = container.querySelector("#waveform-img");
  const track = container.querySelector(".scrubber-track");
  const selection = container.querySelector(".scrubber-selection");
  const startHandle = container.querySelector(".scrubber-handle-start");
  const endHandle = container.querySelector(".scrubber-handle-end");
  const startLabel = container.querySelector("#start-label");
  const endLabel = container.querySelector("#end-label");
  const durationLabel = container.querySelector("#duration-label");
  const statusMsg = container.querySelector("#status-msg");

  const videoUrl = await readFileAsBlobUrl(replayPath, "video/mp4");
  video.src = videoUrl;

  let duration = 0;
  let startPct = 0;
  let endPct = 1;

  video.addEventListener("loadedmetadata", () => {
    duration = video.duration || 0;
    durationLabel.textContent = `${duration.toFixed(2)}s total`;
    renderScrubber();
  });

  api
    .generateWaveform(replayPath)
    .then((pngPath) => readFileAsBlobUrl(pngPath, "image/png"))
    .then((url) => (waveformImg.src = url))
    .catch((e) => (statusMsg.textContent = `Waveform generation failed: ${e}`));

  function renderScrubber() {
    startHandle.style.left = `${startPct * 100}%`;
    endHandle.style.left = `${endPct * 100}%`;
    selection.style.left = `${startPct * 100}%`;
    selection.style.width = `${(endPct - startPct) * 100}%`;
    startLabel.textContent = `${(startPct * duration).toFixed(2)}s`;
    endLabel.textContent = `${(endPct * duration).toFixed(2)}s`;
  }

  let dragging = null;
  function pctFromEvent(e) {
    const rect = track.getBoundingClientRect();
    return Math.min(1, Math.max(0, (e.clientX - rect.left) / rect.width));
  }
  [startHandle, endHandle].forEach((handle) => {
    handle.addEventListener("pointerdown", (e) => {
      dragging = e.target.dataset.handle;
      e.target.setPointerCapture(e.pointerId);
    });
  });
  window.addEventListener("pointermove", (e) => {
    if (!dragging) return;
    const pct = pctFromEvent(e);
    if (dragging === "start") startPct = Math.min(pct, endPct - 0.01);
    else endPct = Math.max(pct, startPct + 0.01);
    renderScrubber();
  });
  window.addEventListener("pointerup", () => (dragging = null));

  container.querySelector("#preview-btn").addEventListener("click", () => {
    video.currentTime = startPct * duration;
    video.play();
    const stopAtEnd = () => {
      if (video.currentTime >= endPct * duration) {
        video.pause();
        video.removeEventListener("timeupdate", stopAtEnd);
      }
    };
    video.addEventListener("timeupdate", stopAtEnd);
  });

  container.querySelector("#send-btn").addEventListener("click", async () => {
    const sendBtn = container.querySelector("#send-btn");
    sendBtn.disabled = true;
    statusMsg.textContent = "Trimming…";
    try {
      const fast = container.querySelector("#fast-trim").checked;
      const outputPath = await api.exportTrim(replayPath, startPct * duration, endPct * duration, fast);
      statusMsg.textContent = "Sending to OBS…";
      await api.pushToObs(outputPath);
      statusMsg.textContent = `Sent — playing from ${outputPath.split(/[\\/]/).pop()}`;
      onSent?.(outputPath);
    } catch (e) {
      statusMsg.textContent = `Error: ${e}`;
    } finally {
      sendBtn.disabled = false;
    }
  });
}
