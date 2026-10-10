// Client-side mirror of daemon state. The daemon is the source of truth: the
// store loads a snapshot, then applies sequence-numbered events. High-rate
// data (meters, trigger flashes) lives in a separate store so it does not
// re-render the whole app.

import { useSyncExternalStore } from "react";
import type { EventEnvelope } from "./generated/EventEnvelope";
import type { HistoryStepResult } from "./generated/HistoryStepResult";
import type { ParamInfo } from "./generated/ParamInfo";
import type { InstrumentPattern } from "./generated/InstrumentPattern";
import type { NoteStep } from "./generated/NoteStep";
import type { Seat } from "./generated/Seat";
import type { Snapshot } from "./generated/Snapshot";
import { RpcClient, type ConnectionState } from "./rpc";

export const PROTOCOL_VERSION = 7;

export type AppState = {
  connection: ConnectionState;
  snapshot: Snapshot | null;
  registry: ParamInfo[];
  error: string | null;
  /** Instrument shown in the editor (view state only). */
  selected: string | null;
  /** This connection's client id (`ui#n`), to find our seat. */
  clientId: string | null;
  /** The seat chooser is open (opened by the user, or because we have no seat). */
  choosingSeat: boolean;
  /** Our undo/redo stacks: top labels and depths. */
  history: History;
};

export type History = {
  user: string | null;
  undo: string | null;
  redo: string | null;
  undoCount: number;
  redoCount: number;
};

const NO_HISTORY: History = { user: null, undo: null, redo: null, undoCount: 0, redoCount: 0 };

export type LiveState = {
  /** Peak [left, right] per channel number. */
  channels: Record<number, [number, number]>;
  master: number[];
  /** performance.now() of the last trigger, keyed `instrument` and `instrument.voice`. */
  triggers: Record<string, number>;
  /** Last note each note instrument played (MIDI number). */
  lastNote: Record<string, number>;
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

const query = new URLSearchParams(window.location.search);

function daemonUrl(): string {
  return query.get("daemon") || "ws://127.0.0.1:4440";
}

/** How the Electron main process found the daemon (see electron/main.cjs). */
export const launch = {
  lifecycle: query.get("lifecycle") ?? "external",
  error: query.get("daemonError"),
  /** The person using the app; seats are matched by this name (RFC 0007). */
  user: query.get("user"),
};

export const client = new RpcClient(daemonUrl());
export const app = new Store<AppState>({
  connection: "closed",
  snapshot: null,
  registry: [],
  error: null,
  selected: null,
  clientId: null,
  choosingSeat: false,
  history: NO_HISTORY,
});
export const live = new Store<LiveState>({ channels: {}, master: [0, 0], triggers: {}, lastNote: {} });

/** Select an instrument for the editor. */
export function select(id: string) {
  app.set({ selected: id });
}

/** The selected instrument, falling back to the first one. */
export function useSelected(): string | null {
  return useApp((s) => {
    const ids = s.snapshot?.graph.instruments.map((i) => i.id) ?? [];
    return s.selected && ids.includes(s.selected) ? s.selected : (ids[0] ?? null);
  });
}

/** The seat this client sits in, if any. */
export function mySeat(s: AppState): Seat | null {
  const id = s.clientId;
  return (id && s.snapshot?.seats.seats.find((seat) => seat.occupants.includes(id))) || null;
}

export function useApp<S>(select: (s: AppState) => S): S {
  return useSyncExternalStore(app.subscribe, () => select(app.get()));
}

export function useLive<S>(select: (s: LiveState) => S): S {
  return useSyncExternalStore(live.subscribe, () => select(live.get()));
}

// ---- sync protocol --------------------------------------------------------

let buffered: EventEnvelope[] | null = null;

const UI_EVENTS: EventEnvelope["event"]["type"][] = [
  "param_changed",
  "step_changed",
  "pattern_changed",
  "notes_changed",
  "clip_changed",
  "clip_deleted",
  "track",
  "located",
  "graph",
  "transport",
  "record",
  "playhead",
  "trigger",
  "meters",
  "controller",
  "midi",
  "seats",
  "project",
  "history",
  "reset",
];

async function resync() {
  // Subscribe first and buffer, then fetch the snapshot, then replay any
  // buffered events newer than it. Nothing is missed or applied twice.
  if (!buffered) buffered = [];
  const [registry, snapshot, h] = await Promise.all([
    client.call("param.list", { prefix: null }),
    client.call("state.get", {}),
    client.call("history.get", {}),
  ]);
  const pending = buffered;
  buffered = null;
  const history = {
    user: h.user,
    undo: h.undo[0] ?? null,
    redo: h.redo[0] ?? null,
    undoCount: h.undo.length,
    redoCount: h.redo.length,
  };
  app.set({ registry: registry.params, snapshot, history });
  // No seat after a (re)connect or a project load: ask.
  if (!mySeat(app.state)) app.set({ choosingSeat: true });
  live.set({ channels: {} });
  for (const e of pending) if (e.seq > snapshot.seq) apply(e);
}

client.onConnect = async () => {
  // `user` names our seat and owns our undo history; locally it is the
  // daemon host's user too, so this app and a local CLI share one history.
  const hello = await client.call("session.hello", {
    client_name: "ui",
    protocol_version: PROTOCOL_VERSION,
    token: null,
    user: launch.user,
    seat: null,
    auto_seat: true,
  });
  app.set({ clientId: hello.client_id, choosingSeat: hello.choose_seat });
  buffered = [];
  // Everything the UI applies; not `journal` or `midi_in`, which can be
  // dense under MIDI input.
  await client.call("events.subscribe", { types: UI_EVENTS });
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
    case "meters": {
      const channels: Record<number, [number, number]> = {};
      for (const c of ev.channels) channels[c.channel] = [c.left, c.right];
      live.set({ channels, master: ev.master });
      return;
    }
    case "trigger": {
      const now = performance.now();
      const key = ev.voice ? `${ev.instrument}.${ev.voice}` : ev.instrument;
      live.set({
        triggers: { ...live.state.triggers, [key]: now, [ev.instrument]: now },
        lastNote: ev.note === null ? live.state.lastNote : { ...live.state.lastNote, [ev.instrument]: ev.note },
      });
      return;
    }
    case "midi_in":
    case "journal":
      return;
    case "history":
      if (ev.user === app.state.history.user) {
        app.set({
          history: {
            user: ev.user,
            undo: ev.undo_label,
            redo: ev.redo_label,
            undoCount: ev.undo_count,
            redoCount: ev.redo_count,
          },
        });
      }
      return;
    case "reset":
    case "lagged":
    case "graph":
      // A graph change can add or remove parameters: refetch everything.
      void resync();
      return;
  }
  if (!s) return;
  const patch = (id: string, f: (p: InstrumentPattern["pattern"]) => InstrumentPattern["pattern"]) =>
    app.set({
      snapshot: { ...s, patterns: s.patterns.map((p) => (p.instrument === id ? { ...p, pattern: f(p.pattern) } : p)) },
    });
  switch (ev.type) {
    case "param_changed":
      app.set({ snapshot: { ...s, params: { ...s.params, [ev.path]: ev.value } } });
      break;
    case "step_changed":
      patch(ev.instrument, (p) =>
        p.kind !== "drums"
          ? p
          : {
              ...p,
              tracks: p.tracks.map((t) =>
                t.voice === ev.voice ? { ...t, steps: t.steps.map((v, i) => (i === ev.step ? ev.level : v)) } : t,
              ),
            },
      );
      break;
    case "pattern_changed":
      patch(ev.instrument, (p) =>
        p.kind !== "drums" ? p : { ...p, tracks: p.tracks.map((t) => (t.voice === ev.voice ? { ...t, steps: ev.steps } : t)) },
      );
      break;
    case "clip_changed": {
      const same = (c: { instrument: string; id: number }) => c.instrument === ev.clip.instrument && c.id === ev.clip.id;
      const clips = s.clips.some(same) ? s.clips.map((c) => (same(c) ? ev.clip : c)) : [...s.clips, ev.clip];
      app.set({ snapshot: { ...s, clips } });
      break;
    }
    case "clip_deleted":
      app.set({ snapshot: { ...s, clips: s.clips.filter((c) => !(c.instrument === ev.instrument && c.id === ev.id)) } });
      break;
    case "track":
      app.set({
        snapshot: { ...s, tracks: s.tracks.map((t) => (t.instrument === ev.track.instrument ? ev.track : t)) },
      });
      break;
    case "located":
      app.set({ snapshot: { ...s, transport: { ...s.transport, start: ev.tick } } });
      break;
    case "notes_changed":
      patch(ev.instrument, (p) => (p.kind !== "notes" ? p : { ...p, steps: ev.steps }));
      break;
    case "transport":
      app.set({
        snapshot: {
          ...s,
          transport: {
            ...s.transport,
            playing: ev.playing,
            step: ev.playing ? s.transport.step : null,
            tick: ev.playing ? s.transport.tick : null,
          },
        },
      });
      break;
    case "playhead":
      app.set({ snapshot: { ...s, transport: { ...s.transport, step: ev.step, tick: ev.tick } } });
      break;
    case "record":
      app.set({ snapshot: { ...s, record: ev.state } });
      break;
    case "controller":
      app.set({ snapshot: { ...s, controller: ev.state } });
      break;
    case "midi":
      app.set({ snapshot: { ...s, midi: ev.connections } });
      break;
    case "seats": {
      app.set({ snapshot: { ...s, seats: ev.state } });
      // Our seat went away (deleted, or a project without it): ask again.
      if (!mySeat(app.state)) app.set({ choosingSeat: true });
      break;
    }
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

/** Undo (or redo) our last change; report keys someone else changed since. */
export async function undo(redo = false) {
  const r: HistoryStepResult | undefined = await act(client.call(redo ? "history.redo" : "history.undo", {}));
  if (r && r.skipped.length > 0) {
    const what = r.changed.length === 0 ? `could not ${redo ? "redo" : "undo"}` : `${redo ? "redo" : "undo"}: kept`;
    app.set({ error: `${what} ${r.skipped.join(", ")} (changed by someone else)` });
  }
}

export function param(path: string): number {
  return app.state.snapshot?.params[path] ?? 0;
}

/** Optimistically set one note step locally, then send it. The daemon's
 * `notes_changed` event confirms. */
export function setNote(instrument: string, step: number, note: NoteStep) {
  const s = app.state.snapshot;
  if (s) {
    const patterns = s.patterns.map((p) =>
      p.instrument === instrument && p.pattern.kind === "notes"
        ? { ...p, pattern: { ...p.pattern, steps: p.pattern.steps.map((x, i) => (i === step ? note : x)) } }
        : p,
    );
    app.set({ snapshot: { ...s, patterns } });
  }
  return act(client.call("pattern.set_note", { instrument, step, note }));
}

/** Optimistically set a param locally, then send it. The daemon's event confirms. */
export function setParam(path: string, value: number) {
  const s = app.state.snapshot;
  if (s) app.set({ snapshot: { ...s, params: { ...s.params, [path]: value } } });
  return act(client.call("param.set", { path, value }));
}
