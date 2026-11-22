// Preload: the only bridge from the renderer to local desktop features.
// Keep it tiny; anything that changes music state goes over RPC instead.
const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("fours", {
  platform: process.platform,
  /** Open the OS file manager with `path` selected. Resolves to { ok, error }. */
  revealPath: (path) => ipcRenderer.invoke("reveal-path", path),
  /** Edit > Undo / Redo from the app menu: calls `cb("undo" | "redo")`. */
  onHistory: (cb) => ipcRenderer.on("history", (_event, what) => cb(what)),
  /** The in-app assistant (RFC 0013). Its loop and API key stay in main. */
  assistant: {
    settings: () => ipcRenderer.invoke("assistant:settings"),
    /** Store (encrypted) or forget (null) the API key; it is never read back. */
    setKey: (key) => ipcRenderer.invoke("assistant:set-key", key),
    setModel: (model) => ipcRenderer.invoke("assistant:set-model", model),
    /** Run a turn; resolves when it ends. `ctx`: { focus, panel }. */
    send: (text, ctx) => ipcRenderer.invoke("assistant:send", text, ctx),
    stop: () => ipcRenderer.invoke("assistant:stop"),
    reset: () => ipcRenderer.invoke("assistant:reset"),
    /** Streamed text, tool calls, and turn ends. Returns an unsubscribe. */
    onEvent: (cb) => {
      const h = (_event, e) => cb(e);
      ipcRenderer.on("assistant:event", h);
      return () => ipcRenderer.removeListener("assistant:event", h);
    },
  },
});
