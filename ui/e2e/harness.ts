// Test harness: starts a real headless 4sd, launches the Electron app against
// it, and gives tests a typed RPC client to verify state from the daemon side.

import { _electron as electron, type ElectronApplication, type Page } from "@playwright/test";
import { spawn, type ChildProcess } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type { MethodName, Methods } from "../src/generated/methods";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

export type Harness = {
  url: string;
  daemon: ChildProcess;
  app: ElectronApplication;
  page: Page;
  rpc: <M extends MethodName>(method: M, params: Methods[M]["params"]) => Promise<Methods[M]["result"]>;
  close: () => Promise<void>;
};

async function startDaemon(): Promise<{ url: string; proc: ChildProcess }> {
  const bin = process.env.FOURSD_BIN ?? path.join(ROOT, "target", "debug", "4sd");
  const dataDir = mkdtempSync(path.join(tmpdir(), "4s-e2e-"));
  const proc = spawn(bin, ["--no-audio", "--no-midi", "--listen", "127.0.0.1:0", "--data-dir", dataDir], {
    stdio: ["ignore", "pipe", "inherit"],
  });
  const url = await new Promise<string>((resolve, reject) => {
    let buf = "";
    proc.stdout!.on("data", (d) => {
      buf += String(d);
      const m = buf.match(/ws:\/\/[\d.:]+/);
      if (m) resolve(m[0]);
    });
    proc.on("exit", (code) => reject(new Error(`4sd exited early (${code}); build it with cargo build`)));
    setTimeout(() => reject(new Error("4sd did not start")), 10_000);
  });
  return { url, proc };
}

/** One-shot RPC over Node's built-in WebSocket. */
function makeRpc(url: string) {
  return async <M extends MethodName>(method: M, params: Methods[M]["params"]): Promise<Methods[M]["result"]> => {
    const ws = new WebSocket(url);
    await new Promise((res, rej) => {
      ws.onopen = res;
      ws.onerror = rej;
    });
    try {
      const reply = new Promise<{ result?: unknown; error?: { message: string } }>((res) => {
        ws.onmessage = (m) => {
          const msg = JSON.parse(String(m.data));
          if (msg.id === 1) res(msg);
        };
      });
      ws.send(JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }));
      const msg = await reply;
      if (msg.error) throw new Error(`${method}: ${msg.error.message}`);
      return msg.result as Methods[M]["result"];
    } finally {
      ws.close();
    }
  };
}

export async function startHarness(): Promise<Harness> {
  const { url, proc } = await startDaemon();
  const app = await electron.launch({
    args: [path.join(ROOT, "ui", "electron", "main.cjs")],
    env: { ...process.env, FOURS_URL: url } as Record<string, string>,
  });
  const page = await app.firstWindow();
  await page.getByTestId("connection").and(page.locator('[data-state="open"]')).waitFor();
  return {
    url,
    daemon: proc,
    app,
    page,
    rpc: makeRpc(url),
    close: async () => {
      await app.close();
      proc.kill();
    },
  };
}
