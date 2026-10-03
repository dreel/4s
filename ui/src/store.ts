// Client-side mirror of daemon state. The daemon is the source of truth: the
// store loads a snapshot, then applies sequence-numbered events. High-rate
// data (meters, trigger flashes) lives in a separate store so it does not
// re-render the whole app.

import { useSyncExternalStore } from "react";
import type { EventEnvelope } from "./generated/EventEnvelope";
import type { ParamInfo } from "./generated/ParamInfo";
import type { Snapshot } from "./generated/Snapshot";
import type { Voice } from "./generated/Voice";
import { RpcClient, type ConnectionState } from "./rpc";

export const PROTOCOL_VERSION = 1;

export type AppState = {
  connection: ConnectionState;
  snapshot: Snapshot | null;
  registry: ParamInfo[];
  error: string | null;
};

export type LiveState = {
  tracks: number[];
  master: number[];
  /** performance.now() of the last trigger per voice. */
  triggers: Partial<Record<Voice, number>>;
};

class Store<T> {
  private listeners = new Set<() => void>();
  constructor(public state: T) {}
  get = () => this.state;
  set(update: Partial<T>) {
    this.state = { ...this.state, ...update };
    for (const l of this.listeners) l();
  }
  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => this.listeners.delete(l);
  };
}

function daemonUrl(): string {
  const q = new URLSearchParams(window.location.search).get("daemon");
  return q || "ws://127.0.0.1:4440";
}

export const client = new RpcClient(daemonUrl());
export const app = new Store<AppState>({ connection: "closed", snapshot: null, registry: [], error: null });
export const live = new Store<LiveState>({ tracks: new Array(8).fill(0), master: [0, 0], triggers: {} });

export function useApp<S>(select: (s: AppState) => S): S {
  return useSyncExternalStore(app.subscribe, () => select(app.get()));
}

export function useLive<S>(select: (s: LiveState) => S): S {
  return useSyncExternalStore(live.subscribe, () => select(live.get()));
}

// ---- sync protocol --------------------------------------------------------

let buffered: EventEnvelope[] | null = null;

async function resync() {
  // Subscribe first and buffer, then fetch the snapshot, then replay any
  // buffered events newer than it. Nothing is missed or applied twice.
  if (!buffered) buffered = [];
  const [registry, snapshot] = await Promise.all([
    client.call("param.list", { prefix: null }),
    client.call("state.get", {}),
  ]);
  const pending = buffered;
  buffered = null;
  app.set({ registry: registry.params, snapshot });
  for (const e of pending) if (e.seq > snapshot.seq) apply(e);
}

client.onConnect = async () => {
  await client.call("session.hello", { client_name: "ui", protocol_version: PROTOCOL_VERSION, token: null });
  buffered = [];
  await client.call("events.subscribe", { types: null });
  await resync();
};
client.onState((connection) => app.set({ connection }));
client.onEvent((e) => {
  if (buffered) buffered.push(e);
  else apply(e);
});

function apply(env: EventEnvelope) {
  const ev = env.event;
  const s = app.state.snapshot;
  switch (ev.type) {
    case "meters":
      live.set({ tracks: ev.tracks, master: ev.master });
      return;
    case "trigger":
      live.set({ triggers: { ...live.state.triggers, [ev.voice]: performance.now() } });
      return;
    case "midi_in":
      return;
    case "reset":
    case "lagged":
      void resync();
      return;
  }
  if (!s) return;
  switch (ev.type) {
    case "param_changed":
      app.set({ snapshot: { ...s, params: { ...s.params, [ev.path]: ev.value } } });
      break;
    case "step_changed":
      app.set({
        snapshot: {
          ...s,
          pattern: s.pattern.map((t) =>
            t.voice === ev.voice ? { ...t, steps: t.steps.map((v, i) => (i === ev.step ? ev.level : v)) } : t,
          ),
        },
      });
      break;
    case "pattern_changed":
      app.set({
        snapshot: { ...s, pattern: s.pattern.map((t) => (t.voice === ev.voice ? { ...t, steps: ev.steps } : t)) },
      });
      break;
    case "transport":
      app.set({ snapshot: { ...s, transport: { playing: ev.playing, step: ev.playing ? s.transport.step : null } } });
      break;
    case "playhead":
      app.set({ snapshot: { ...s, transport: { ...s.transport, step: ev.step } } });
      break;
    case "controller":
      app.set({ snapshot: { ...s, controller: ev.state } });
      break;
    case "midi":
      app.set({ snapshot: { ...s, midi: ev.connections } });
      break;
    case "project":
      app.set({ snapshot: { ...s, project: ev.info } });
      break;
  }
}

/** Run an RPC and surface failures in the UI instead of throwing. */
export async function act<T>(p: Promise<T>): Promise<T | undefined> {
  try {
    const r = await p;
    if (app.state.error) app.set({ error: null });
    return r;
  } catch (e) {
    app.set({ error: e instanceof Error ? e.message : String(e) });
    return undefined;
  }
}

export function param(path: string): number {
  return app.state.snapshot?.params[path] ?? 0;
}

/** Optimistically set a param locally, then send it. The daemon's event confirms. */
export function setParam(path: string, value: number) {
  const s = app.state.snapshot;
  if (s) app.set({ snapshot: { ...s, params: { ...s.params, [path]: value } } });
  return act(client.call("param.set", { path, value }));
}
