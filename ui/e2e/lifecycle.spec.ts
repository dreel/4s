// Daemon lifecycle as seen from the Electron app: start-if-missing, stop on
// quit only when the app owns the daemon, never touch a daemon it found.

import { _electron as electron, expect, test } from "@playwright/test";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import net from "node:net";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const BIN = path.join(ROOT, "target", "debug");
const MAIN = path.join(ROOT, "ui", "electron", "main.cjs");

function freePort(): Promise<number> {
  return new Promise((resolve) => {
    const srv = net.createServer();
    srv.listen(0, "127.0.0.1", () => {
      const port = (srv.address() as net.AddressInfo).port;
      srv.close(() => resolve(port));
    });
  });
}

function alive(pid: number) {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function runtimePid(dataDir: string): number | null {
  const f = path.join(dataDir, "4sd.json");
  return existsSync(f) ? JSON.parse(readFileSync(f, "utf8")).pid : null;
}

function cli(dataDir: string, ...args: string[]) {
  return execFileSync(path.join(BIN, "4s"), args, { env: { ...process.env, FOURS_DATA_DIR: dataDir }, encoding: "utf8" });
}

async function launch(mode: string, dataDir: string, port: number) {
  const app = await electron.launch({
    args: [MAIN],
    env: {
      ...process.env,
      FOURS_URL: `ws://127.0.0.1:${port}`,
      FOURS_DAEMON: mode,
      FOURS_DATA_DIR: dataDir,
      FOURSD_BIN: path.join(BIN, "4sd"),
      FOURSD_ARGS: "--no-audio --no-midi",
    } as Record<string, string>,
  });
  const page = await app.firstWindow();
  await page.getByTestId("connection").and(page.locator('[data-state="open"]')).waitFor();
  return { app, page };
}

test("owned: app starts the daemon and stops it on quit", async () => {
  const dataDir = mkdtempSync(path.join(tmpdir(), "4s-owned-"));
  const port = await freePort();
  const { app, page } = await launch("owned", dataDir, port);
  await expect(page.getByTestId("lifecycle")).toHaveAttribute("data-lifecycle", "started-owned");
  const pid = runtimePid(dataDir)!;
  expect(alive(pid)).toBe(true);

  await app.close();
  await expect.poll(() => alive(pid), { timeout: 8000 }).toBe(false);
  expect(runtimePid(dataDir)).toBeNull();
});

test("owned: daemon exits if the app is killed", async () => {
  const dataDir = mkdtempSync(path.join(tmpdir(), "4s-crash-"));
  const port = await freePort();
  const { app } = await launch("owned", dataDir, port);
  const pid = runtimePid(dataDir)!;
  app.process().kill("SIGKILL");
  await expect.poll(() => alive(pid), { timeout: 8000 }).toBe(false);
});

test("detached: daemon keeps running after the app quits", async () => {
  const dataDir = mkdtempSync(path.join(tmpdir(), "4s-detached-"));
  const port = await freePort();
  const { app, page } = await launch("detached", dataDir, port);
  await expect(page.getByTestId("lifecycle")).toHaveAttribute("data-lifecycle", "started-detached");
  const pid = runtimePid(dataDir)!;

  await app.close();
  await new Promise((r) => setTimeout(r, 1000));
  expect(alive(pid)).toBe(true);
  expect(cli(dataDir, "daemon", "stop")).toContain("4sd stopped");
  expect(alive(pid)).toBe(false);
});

test("existing daemon: app connects and never stops it, even in owned mode", async () => {
  const dataDir = mkdtempSync(path.join(tmpdir(), "4s-existing-"));
  const port = await freePort();
  cli(dataDir, "daemon", "start", "--no-audio", "--no-midi", "--listen", `127.0.0.1:${port}`);
  const pid = runtimePid(dataDir)!;

  const { app, page } = await launch("owned", dataDir, port);
  await expect(page.getByTestId("lifecycle")).toHaveAttribute("data-lifecycle", "connected");
  await app.close();
  await new Promise((r) => setTimeout(r, 1000));
  expect(alive(pid)).toBe(true);
  cli(dataDir, "daemon", "stop");
  expect(alive(pid)).toBe(false);
});
