// Preload: the only bridge from the renderer to local desktop features.
// Keep it tiny; anything that changes music state goes over RPC instead.
const { contextBridge, ipcRenderer } = require("electron");

contextBridge.exposeInMainWorld("fours", {
  platform: process.platform,
  /** Open the OS file manager with `path` selected. Resolves to { ok, error }. */
  revealPath: (path) => ipcRenderer.invoke("reveal-path", path),
});
