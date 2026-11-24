// The assistant panel's state (RFC 0013). The loop runs in Electron main
// (ui/assistant/); this mirrors its events for the panel and keeps the chat
// while the drawer is closed. Absent outside Electron.

import { useSyncExternalStore } from "react";
import "./desktop";
import { act, app, client, myFocus, undo } from "./store";

export type AssistantSettings = {
  model: string;
  models: string[];
  /** Where the key comes from: stored encrypted, this session only, the
   * environment, or none. The key itself never reaches the renderer. */
  keySource: "keychain" | "session" | "env" | "none";
  encryption: boolean;
};

export type Usage = { input: number; output: number; cacheRead: number; cacheWrite: number; cost: number };

export type AssistantEvent =
  | { type: "text"; turn: number; delta: string }
  | { type: "tool"; turn: number; id: string; name: string; input: unknown; ok: boolean; chips: string[]; error?: string }
  | { type: "done"; turn: number; stop: string; label: string; changed: string[]; usage: Usage; error?: string };

export type AssistantBridge = {
  settings: () => Promise<AssistantSettings>;
  setKey: (key: string | null) => Promise<AssistantSettings>;
  setModel: (model: string) => Promise<AssistantSettings>;
  send: (text: string, ctx: { focus: string | null; panel: string | null }) => Promise<{ ok: boolean; error?: string }>;
  stop: () => Promise<unknown>;
  reset: () => Promise<unknown>;
  onEvent: (cb: (e: AssistantEvent) => void) => () => void;
};

export type Chip = { text: string; ok: boolean };
/** One exchange: your message and the assistant's reply. */
export type Turn = {
  user: string;
  /** Text and tool chips in the order they arrived. */
  parts: ({ kind: "text"; text: string } | { kind: "chips"; chips: Chip[] })[];
  done: { stop: string; label: string; changed: string[]; error?: string } | null;
  /** What the per-message Undo did or why it could not. */
  undone?: string;
};

type State = { open: boolean; turns: Turn[]; running: boolean; settings: AssistantSettings | null; usage: Usage | null };

const bridge: AssistantBridge | undefined = window.fours?.assistant;
export const assistantAvailable = bridge !== undefined;

let state: State = { open: false, turns: [], running: false, settings: null, usage: null };
const listeners = new Set<() => void>();
function set(update: Partial<State>) {
  state = { ...state, ...update };
  for (const l of listeners) l();
}
export function useAssistant<S>(select: (s: State) => S): S {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => select(state),
  );
}

/** Update the last turn. */
function lastTurn(f: (t: Turn) => Turn) {
  const turns = state.turns.slice();
  const i = turns.length - 1;
  if (i < 0) return;
  turns[i] = f(turns[i]);
  set({ turns });
}

function onEvent(e: AssistantEvent) {
  if (e.type === "text") {
    lastTurn((t) => {
      const parts = t.parts.slice();
      const last = parts[parts.length - 1];
      if (last?.kind === "text") parts[parts.length - 1] = { kind: "text", text: last.text + e.delta };
      else parts.push({ kind: "text", text: e.delta });
      return { ...t, parts };
    });
  } else if (e.type === "tool") {
    const chips: Chip[] = e.ok ? e.chips.map((text) => ({ text, ok: true })) : [{ text: `${e.name}: ${e.error ?? "failed"}`, ok: false }];
    if (chips.length === 0) return;
    lastTurn((t) => {
      const parts = t.parts.slice();
      const last = parts[parts.length - 1];
      if (last?.kind === "chips") parts[parts.length - 1] = { kind: "chips", chips: [...last.chips, ...chips] };
      else parts.push({ kind: "chips", chips });
      return { ...t, parts };
    });
  } else {
    lastTurn((t) => ({ ...t, done: { stop: e.stop, label: e.label, changed: e.changed, error: e.error } }));
    set({ usage: e.usage });
  }
}

if (bridge) {
  bridge.onEvent(onEvent);
  void bridge.settings().then((settings) => set({ settings }));
}

export function toggleAssistant(open = !state.open) {
  set({ open });
}

/** What you are looking at, for "this" and "here". */
function context() {
  const s = app.get();
  const panel = s.device ? `device ${s.device}` : s.selected ? `${s.selected} editor` : null;
  return { focus: myFocus(s), panel };
}

export async function sendMessage(text: string) {
  if (!bridge || state.running || !text.trim()) return;
  set({ running: true, turns: [...state.turns, { user: text.trim(), parts: [], done: null }] });
  try {
    const r = await bridge.send(text.trim(), context());
    if (!r.ok) lastTurn((t) => ({ ...t, done: { stop: "error", label: "", changed: [], error: r.error } }));
  } catch (e) {
    // The IPC call itself failed: end the turn rather than leave it running.
    const error = e instanceof Error ? e.message : String(e);
    lastTurn((t) => (t.done ? t : { ...t, done: { stop: "error", label: "", changed: [], error } }));
  } finally {
    set({ running: false });
  }
}

export function stopTurn() {
  void bridge?.stop();
}

export async function newChat() {
  await bridge?.reset();
  set({ turns: [] });
}

export async function saveSettings(update: { key?: string | null; model?: string }) {
  if (!bridge) return;
  if (update.key !== undefined) set({ settings: await bridge.setKey(update.key) });
  if (update.model !== undefined) set({ settings: await bridge.setModel(update.model) });
}

/** Undo one assistant turn: only while it is still your latest undo step,
 * else say what is in the way. */
export async function undoTurn(index: number) {
  const turn = state.turns[index];
  if (!turn?.done) return;
  const h = await act(client.call("history.get", {}));
  if (!h) return;
  const mark = (undone: string) => {
    const turns = state.turns.slice();
    turns[index] = { ...turns[index], undone };
    set({ turns });
  };
  if (h.undo[0] !== turn.done.label) {
    mark(h.undo.includes(turn.done.label) ? `undo "${h.undo[0]}" first` : "already undone");
    return;
  }
  await undo();
  mark("undone");
}

/** Cmd/Ctrl-K toggles the panel. */
export function installAssistantKeys() {
  if (!bridge) return;
  window.addEventListener("keydown", (e) => {
    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k") {
      e.preventDefault();
      toggleAssistant();
    }
  });
}
