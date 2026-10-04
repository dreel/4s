// Local desktop features exposed by electron/preload.cjs. Absent in a plain
// browser (e.g. `npm run dev` opened outside Electron).

import { client } from "./store";

type DesktopBridge = {
  platform: string;
  revealPath: (path: string) => Promise<{ ok: boolean; error: string | null }>;
};

declare global {
  interface Window {
    fours?: DesktopBridge;
  }
}

export const desktop: DesktopBridge | undefined = window.fours;

function daemonIsLocal(): boolean {
  try {
    const host = new URL(client.url).hostname;
    return ["127.0.0.1", "localhost", "[::1]", "::1"].includes(host);
  } catch {
    return false;
  }
}

/** Revealing files only makes sense in Electron with a daemon on this machine. */
export function canReveal(): boolean {
  return desktop !== undefined && daemonIsLocal();
}

export function revealLabel(): string {
  switch (desktop?.platform) {
    case "darwin":
      return "show in Finder";
    case "win32":
      return "show in Explorer";
    default:
      return "show folder";
  }
}
