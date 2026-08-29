// ReplayTrim Stream Deck plugin.
// Speaks the raw Stream Deck registration protocol (no SDK dependency —
// Node 24's built-in WebSocket and fetch are enough) and forwards key
// presses to ReplayTrim's local HTTP API.

const fs = require("node:fs");
const path = require("node:path");

const REPLAYTRIM = "http://127.0.0.1:8930";

const ACTION_ENDPOINTS = {
  "com.davidwallace.replaytrim.grab": "/api/cmd/grab",
  "com.davidwallace.replaytrim.instant": "/api/cmd/instant",
  "com.davidwallace.replaytrim.replay": "/api/cmd/replay",
  "com.davidwallace.replaytrim.pause": "/api/cmd/pause",
  "com.davidwallace.replaytrim.hide": "/api/cmd/hide",
};

const logFile = path.join(__dirname, "..", "logs", "plugin.log");
try {
  fs.mkdirSync(path.dirname(logFile), { recursive: true });
} catch {}
function log(message) {
  try {
    fs.appendFileSync(logFile, `${new Date().toISOString()} ${message}\n`);
  } catch {}
}

// Stream Deck launches us with: -port N -pluginUUID X -registerEvent Y -info {...}
const args = {};
for (let i = 2; i < process.argv.length; i += 2) {
  args[process.argv[i].replace(/^-/, "")] = process.argv[i + 1];
}
if (!args.port || !args.pluginUUID || !args.registerEvent) {
  log(`missing launch args: ${process.argv.slice(2).join(" ")}`);
  process.exit(1);
}

const ws = new WebSocket(`ws://127.0.0.1:${args.port}`);

ws.addEventListener("open", () => {
  ws.send(JSON.stringify({ event: args.registerEvent, uuid: args.pluginUUID }));
  log(`registered as ${args.pluginUUID} on port ${args.port}`);
});

ws.addEventListener("message", async (event) => {
  let msg;
  try {
    msg = JSON.parse(event.data);
  } catch {
    return;
  }
  if (msg.event !== "keyDown") return;

  const endpoint = ACTION_ENDPOINTS[msg.action];
  if (!endpoint) return;

  try {
    const res = await fetch(REPLAYTRIM + endpoint, {
      method: "POST",
      // Instant replay includes a real buffer save — allow it time.
      signal: AbortSignal.timeout(15000),
    });
    if (res.ok) {
      ws.send(JSON.stringify({ event: "showOk", context: msg.context }));
      log(`${msg.action} -> ok`);
    } else {
      ws.send(JSON.stringify({ event: "showAlert", context: msg.context }));
      log(`${msg.action} -> HTTP ${res.status}: ${await res.text().catch(() => "")}`);
    }
  } catch (e) {
    // ReplayTrim app not running (or timed out)
    ws.send(JSON.stringify({ event: "showAlert", context: msg.context }));
    log(`${msg.action} -> unreachable: ${e.message}`);
  }
});

ws.addEventListener("close", () => {
  log("stream deck closed the socket; exiting");
  process.exit(0);
});
ws.addEventListener("error", (e) => log(`websocket error: ${e.message || e.type}`));
