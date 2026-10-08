// Electron main process. Thin by design: it makes sure a daemon is reachable,
// then opens a window. The renderer talks to 4sd directly over WebSocket.
//
// Daemon lifecycle (see docs/lifecycle.md), selected by FOURS_DAEMON:
//   owned    - start 4sd if none is running; stop it on quit if we started it.
//              Default for the packaged app.
//   detached - start 4sd in the background if none is running; leave it
//              running on quit. Default in development.
//   external - never start or stop a daemon (remote / multiplayer).
//              Default when FOURS_URL points at a non-loopback host.
// Other knobs: FOURS_URL, FOURS_DATA_DIR, FOURSD_BIN, FOURSD_ARGS (extra
// daemon flags, e.g. "--no-audio --no-midi" for tests).

const { app, BrowserWindow, Menu, ipcMain, shell } = require("electron");
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const net = require("node:net");
const os = require("node:os");
const path = require("node:path");

const DEFAULT_URL = "ws://127.0.0.1:4440";
const DATA_DIR = process.env.FOURS_DATA_DIR || path.join(os.homedir(), ".4s");

/** @type {import("node:child_process").ChildProcess | null} */
let ownedChild = null;
let quitting = false;

function pidAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === "EPERM";
  }
}

/** The daemon already running for our data dir, per its runtime file. */
function runtimeInfo() {
  try {
    const info = JSON.parse(fs.readFileSync(path.join(DATA_DIR, "4sd.json"), "utf8"));
    return pidAlive(info.pid) ? info : null;
  } catch {
    return null;
  }
}

function probe(host, port, timeout = 500) {
  return new Promise((resolve) => {
    const sock = net.connect({ host, port: Number(port) });
    const done = (ok) => {
      sock.destroy();
      resolve(ok);
    };
    sock.setTimeout(timeout, () => done(false));
    sock.once("connect", () => done(true));
    sock.once("error", () => done(false));
  });
}

function daemonBin() {
  if (process.env.FOURSD_BIN) return process.env.FOURSD_BIN;
  if (app.isPackaged) return path.join(process.resourcesPath, "bin", "4sd");
  const dev = path.join(__dirname, "..", "..", "target", "debug", "4sd");
  return fs.existsSync(dev) ? dev : "4sd";
}

/**
 * Make sure a daemon is reachable. Returns { url, lifecycle, error }.
 * lifecycle: "connected" (already running), "started-owned",
 * "started-detached", or "external".
 */
async function ensureDaemon() {
  let url = process.env.FOURS_URL;
  if (!url) {
    const info = runtimeInfo();
    url = info ? info.url : DEFAULT_URL;
  }
  const { hostname, port } = new URL(url);
  const loopback = ["127.0.0.1", "localhost", "[::1]", "::1"].includes(hostname);
  const mode = process.env.FOURS_DAEMON || (!loopback ? "external" : app.isPackaged ? "owned" : "detached");

  if (await probe(hostname, port)) return { url, lifecycle: "connected", error: null };
  if (mode === "external") return { url, lifecycle: "external", error: null };

  const log = path.join(DATA_DIR, "logs", "4sd.log");
  fs.mkdirSync(path.dirname(log), { recursive: true });
  const fd = fs.openSync(log, "a");
  const args = ["--listen", `${hostname}:${port}`, "--data-dir", DATA_DIR, "--log-file", log];
  if (mode === "owned") args.push("--parent-pid", String(process.pid));
  if (process.env.FOURSD_ARGS) args.push(...process.env.FOURSD_ARGS.split(/\s+/).filter(Boolean));

  const bin = daemonBin();
  let spawnError = null;
  const child = spawn(bin, args, { detached: mode === "detached", stdio: ["ignore", fd, fd] });
  fs.closeSync(fd);
  child.on("error", (e) => (spawnError = e));
  let exited = false;
  child.on("exit", () => (exited = true));
  if (mode === "detached") child.unref();
  else ownedChild = child;

  const deadline = Date.now() + 15000;
  while (Date.now() < deadline) {
    if (spawnError) return { url, lifecycle: mode, error: `could not run ${bin}: ${spawnError.message}` };
    if (exited) return { url, lifecycle: mode, error: `4sd exited during startup; see ${log}` };
    if (await probe(hostname, port, 200)) {
      return { url, lifecycle: mode === "owned" ? "started-owned" : "started-detached", error: null };
    }
    await new Promise((r) => setTimeout(r, 100));
  }
  return { url, lifecycle: mode, error: `4sd did not start within 15s; see ${log}` };
}

/** Ask an owned daemon to shut down, falling back to signals. */
async function stopOwnedDaemon(url) {
  const child = ownedChild;
  if (!child || child.exitCode !== null) return;
  const exited = new Promise((r) => child.once("exit", r));
  const within = (ms) => Promise.race([exited.then(() => true), new Promise((r) => setTimeout(() => r(false), ms))]);
  try {
    const ws = new WebSocket(url);
    await new Promise((res, rej) => {
      ws.onopen = res;
      ws.onerror = rej;
    });
    const hello = { client_name: "electron", protocol_version: 1, token: process.env.FOURS_TOKEN ?? null };
    ws.send(JSON.stringify({ jsonrpc: "2.0", id: 1, method: "session.hello", params: hello }));
    ws.send(JSON.stringify({ jsonrpc: "2.0", id: 2, method: "daemon.shutdown", params: {} }));
  } catch {
    // fall through to signals
  }
  if (await within(3000)) return;
  child.kill("SIGTERM");
  if (await within(2000)) return;
  child.kill("SIGKILL");
}

// The app menu, with Edit > Undo / Redo sent to the page: it undoes in the
// focused text field if there is one, else the project's history (over RPC).
function setMenu() {
  const isMac = process.platform === "darwin";
  const send = (what) => (_item, win) =>
    (win ?? BrowserWindow.getFocusedWindow() ?? BrowserWindow.getAllWindows()[0])?.webContents.send("history", what);
  Menu.setApplicationMenu(
    Menu.buildFromTemplate([
      ...(isMac ? [{ role: "appMenu" }] : []),
      { role: "fileMenu" },
      {
        label: "Edit",
        submenu: [
          { id: "undo", label: "Undo", accelerator: "CmdOrCtrl+Z", click: send("undo") },
          { id: "redo", label: "Redo", accelerator: isMac ? "Shift+Cmd+Z" : "Ctrl+Y", click: send("redo") },
          { type: "separator" },
          { role: "cut" },
          { role: "copy" },
          { role: "paste" },
          { role: "selectAll" },
        ],
      },
      { role: "viewMenu" },
      { role: "windowMenu" },
    ]),
  );
}

async function main() {
  await app.whenReady();
  setMenu();
  const { url, lifecycle, error } = await ensureDaemon();
  if (error) console.error(`[4s] ${error}`);

  const win = new BrowserWindow({
    width: 1440,
    height: 940,
    backgroundColor: "#09090b",
    title: "4S",
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      preload: path.join(__dirname, "preload.cjs"),
    },
  });
  // Seats are matched by the user's name, as the daemon's host seat is.
  const user = process.env.FOURS_USER || os.userInfo().username;
  const query = { daemon: url, lifecycle, user, ...(error ? { daemonError: error } : {}) };
  const devUrl = process.env.FOURS_UI_DEV_URL;
  if (devUrl) {
    win.loadURL(`${devUrl}?${new URLSearchParams(query)}`);
  } else {
    win.loadFile(path.join(__dirname, "..", "dist", "index.html"), { query });
  }

  app.on("before-quit", (e) => {
    if (ownedChild && !quitting) {
      e.preventDefault();
      quitting = true;
      stopOwnedDaemon(url).finally(() => app.quit());
    }
  });
}

// Reveal a local file or folder in Finder / Explorer / the file manager.
// Only for paths on this machine (the renderer hides the button when the
// daemon is remote, but check anyway).
ipcMain.handle("reveal-path", (_event, p) => {
  if (typeof p !== "string" || !path.isAbsolute(p)) return { ok: false, error: "not an absolute path" };
  if (!fs.existsSync(p)) return { ok: false, error: `not found on this machine: ${p}` };
  shell.showItemInFolder(p);
  return { ok: true, error: null };
});

app.on("window-all-closed", () => app.quit());
main();
