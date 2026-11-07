// @ts-check
// Wires the in-app assistant (RFC 0013) into the Electron main process:
// its own daemon connection, its settings, and IPC to the renderer. The
// renderer gets streamed text and tool-call events, never the API key.

const path = require("node:path");
const { DaemonClient } = require("./daemon.cjs");
const { loadSchema } = require("./schema.cjs");
const { Assistant } = require("./loop.cjs");
const { Settings } = require("./settings.cjs");

/**
 * @param {{
 *   ipcMain: import("electron").IpcMain,
 *   safeStorage: import("electron").SafeStorage,
 *   send: (channel: string, payload: unknown) => void,
 *   url: string,
 *   user: string,
 *   token?: string | null,
 *   dataDir: string,
 * }} o
 */
function setupAssistant(o) {
  const schema = loadSchema();
  const settings = new Settings(path.join(o.dataDir, "assistant"), o.safeStorage);
  const daemon = new DaemonClient({ url: o.url, user: o.user, token: o.token, protocolVersion: schema.protocol_version });
  const assistant = new Assistant({
    daemon,
    schema,
    user: o.user,
    apiKey: () => settings.apiKey(),
    model: () => settings.model(),
    emit: (e) => o.send("assistant:event", e),
    // The SDK also reads ANTHROPIC_BASE_URL; tests point it at a fake server.
    baseURL: process.env.ANTHROPIC_BASE_URL || undefined,
  });

  o.ipcMain.handle("assistant:settings", () => settings.view());
  o.ipcMain.handle("assistant:set-key", (_e, key) => {
    settings.setKey(typeof key === "string" ? key : null);
    return settings.view();
  });
  o.ipcMain.handle("assistant:set-model", (_e, model) => {
    settings.setModel(String(model));
    return settings.view();
  });
  // Resolves when the turn has ended; events arrive on "assistant:event".
  o.ipcMain.handle("assistant:send", async (_e, text, ctx) => {
    if (typeof text !== "string" || !text.trim()) return { ok: false, error: "empty message" };
    if (assistant.running) return { ok: false, error: "a turn is already running" };
    await assistant.send(text, ctx && typeof ctx === "object" ? ctx : {});
    return { ok: true, turn: assistant.turn };
  });
  o.ipcMain.handle("assistant:stop", () => {
    assistant.stop();
    return { ok: true };
  });
  o.ipcMain.handle("assistant:reset", () => {
    assistant.reset();
    return { ok: true };
  });
  return { assistant, daemon, settings };
}

module.exports = { setupAssistant };
