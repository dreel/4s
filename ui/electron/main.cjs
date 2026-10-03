// Electron main process. Deliberately thin: it only opens a window. The
// renderer talks to 4sd directly over WebSocket; no business logic here.
const { app, BrowserWindow } = require("electron");
const path = require("node:path");

const DAEMON_URL = process.env.FOURS_URL || "ws://127.0.0.1:4440";

function createWindow() {
  const win = new BrowserWindow({
    width: 1440,
    height: 940,
    backgroundColor: "#09090b",
    title: "4S",
    webPreferences: { contextIsolation: true, nodeIntegration: false },
  });
  const devUrl = process.env.FOURS_UI_DEV_URL;
  if (devUrl) {
    win.loadURL(`${devUrl}?daemon=${encodeURIComponent(DAEMON_URL)}`);
  } else {
    win.loadFile(path.join(__dirname, "..", "dist", "index.html"), { query: { daemon: DAEMON_URL } });
  }
}

app.whenReady().then(createWindow);
app.on("window-all-closed", () => app.quit());
