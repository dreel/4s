// The piano roll (RFC 0008 phase B2): any clip's notes on a time grid. Rows
// are pitches (for a drum machine, its voices); columns are ticks, snapped
// to a grid. Click empty space to add a note, drag a note to move it (time
// and pitch), drag its right edge to resize, alt-drag to copy, shift-click
// to add to the selection, drag on empty space to select a box, Delete to
// remove. The velocity lane below sets velocities. Every edit is one
// `clip.update` (one undo step); the daemon decides, the view only draws.

import { useEffect, useMemo, useRef, useState } from "react";
import type { ClipEvent } from "../generated/ClipEvent";
import type { EventKey } from "../generated/EventKey";
import type { TakeNote } from "../generated/TakeNote";
import { act, client, useApp, useLive } from "../store";
import { noteName } from "./BassEditor";
import { GM_NOTES, voiceShort } from "./voices";
import type { Voice } from "../generated/Voice";

/** Ticks per step (a 16th), beat, and bar, as in the protocol. */
const STEP = 24;
const BEAT = 96;
const BAR = 384;
/** The longest clip, in ticks (`MAX_CLIP_TICKS`). */
const MAX_TICKS = 64 * STEP;
const ROW = 16;
const VEL_H = 48;
const VOICES: Voice[] = ["kick", "snare", "clap", "closed_hat", "open_hat", "low_tom", "high_tom", "cowbell"];

/** Snap grids in ticks (the protocol's `GRIDS`), and off. */
const SNAPS: [string, number][] = [
  ["1/4", 96],
  ["1/8", 48],
  ["1/8t", 32],
  ["1/16", 24],
  ["1/16t", 16],
  ["1/32", 12],
  ["off", 1],
];

const key = (e: { tick: number; note: number }) => `${e.tick}:${e.note}`;
/** One empty list, so store selectors return the same value when nothing
 * is recording (a new array each time would re-render forever). */
const NO_TAKE: TakeNote[] = [];

type Drag =
  | { kind: "move"; x: number; y: number; notes: ClipEvent[]; dt: number; dn: number }
  | { kind: "copy"; x: number; y: number; notes: ClipEvent[]; dt: number; dn: number }
  | { kind: "resize"; x: number; notes: ClipEvent[]; dl: number }
  | { kind: "box"; x: number; y: number; x2: number; y2: number; additive: boolean }
  | { kind: "velocity"; y: number; notes: ClipEvent[]; dv: number };

export function PianoRoll({ id, drums }: { id: string; drums: boolean }) {
  const track = useApp((s) => s.snapshot?.tracks.find((t) => t.instrument === id));
  const clip = useApp((s) => s.snapshot?.clips.find((c) => c.instrument === id && c.id === track?.selected));
  const stepsLength = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  const songMode = useApp((s) => (s.snapshot?.params["song.mode"] ?? 0) >= 0.5);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const step = useApp((s) => s.snapshot?.transport.step ?? null);
  const tick = useApp((s) => s.snapshot?.transport.tick ?? null);
  const taking = useLive((s) => s.takeNotes[id] ?? NO_TAKE);
  const [snap, setSnap] = useState(STEP);
  const [zoom, setZoom] = useState(2);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [drag, setDrag] = useState<Drag | null>(null);
  const grid = useRef<HTMLDivElement>(null);

  const events = clip?.events ?? [];
  const length = clip?.length ?? stepsLength * STEP;
  const px = zoom; // pixels per tick
  // Rows, top first: a drum machine's voices (and any other notes in the
  // clip), else pitches C1..C5, widened to fit the clip's notes.
  const rows = useMemo(() => {
    if (drums) {
      const extra = [...new Set(events.map((e) => e.note).filter((n) => !GM_NOTES.includes(n)))].sort((a, b) => a - b);
      return [...GM_NOTES, ...extra];
    }
    const notes = events.map((e) => e.note);
    const lo = Math.max(0, Math.min(24, ...notes.map((n) => n - 2)));
    const hi = Math.min(127, Math.max(72, ...notes.map((n) => n + 2)));
    return Array.from({ length: hi - lo + 1 }, (_, i) => hi - i);
  }, [drums, events]);
  const rowOf = (note: number) => rows.indexOf(note);
  const label = (note: number) => {
    const v = GM_NOTES.indexOf(note);
    return drums && v >= 0 ? voiceShort(VOICES[v]) : noteName(note);
  };

  // Drop selected keys that no longer exist.
  useEffect(() => {
    setSelected((s) => {
      const kept = [...s].filter((k) => events.some((e) => key(e) === k));
      return kept.length === s.size ? s : new Set(kept);
    });
  }, [events]);

  const update = (remove: EventKey[], add: ClipEvent[]) =>
    act(client.call("clip.update", { instrument: id, clip: clip?.id ?? null, remove, add, recorded: false }));

  // Keyboard: Delete removes the selection, Escape clears it.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (t && (["INPUT", "SELECT", "TEXTAREA"].includes(t.tagName) || t.isContentEditable)) return;
      if ((e.key === "Delete" || e.key === "Backspace") && selected.size) {
        e.preventDefault();
        void update(events.filter((x) => selected.has(key(x))).map((x) => ({ tick: x.tick, note: x.note })), []);
      }
      if (e.key === "Escape") setSelected(new Set());
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  if (!track || !clip) return null;

  const width = Math.max(length, BAR) * px;
  const height = rows.length * ROW;
  const snapTo = (t: number) => Math.round(t / snap) * snap;
  const at = (e: React.PointerEvent) => {
    const r = grid.current!.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));

  // Where notes are while dragging (the daemon gets them on release).
  const moved = (e: ClipEvent): ClipEvent => {
    if (!drag || !("notes" in drag) || !drag.notes.some((n) => key(n) === key(e))) return e;
    if (drag.kind === "move" || drag.kind === "copy") {
      const r = clamp(rowOf(e.note) + drag.dn, 0, rows.length - 1);
      return { ...e, tick: clamp(e.tick + drag.dt, 0, Math.min(length, MAX_TICKS) - 1), note: rows[r] };
    }
    if (drag.kind === "resize") return { ...e, len: Math.max(1, e.len + drag.dl) };
    return { ...e, velocity: clamp(e.velocity + drag.dv, 1, 127) };
  };

  const down = (e: React.PointerEvent, note?: ClipEvent, edge = false) => {
    e.stopPropagation();
    grid.current!.setPointerCapture(e.pointerId);
    const p = at(e);
    if (!note) {
      setDrag({ kind: "box", x: p.x, y: p.y, x2: p.x, y2: p.y, additive: e.shiftKey });
      return;
    }
    let sel = selected;
    if (e.shiftKey) {
      sel = new Set(selected);
      if (sel.has(key(note))) sel.delete(key(note));
      else sel.add(key(note));
      setSelected(sel);
      return;
    }
    if (!sel.has(key(note))) {
      sel = new Set([key(note)]);
      setSelected(sel);
    }
    const notes = events.filter((x) => sel.has(key(x)));
    if (edge) setDrag({ kind: "resize", x: p.x, notes, dl: 0 });
    else setDrag({ kind: "move", x: p.x, y: p.y, notes, dt: 0, dn: 0, ...(e.altKey ? { kind: "copy" as const } : {}) });
  };

  const move = (e: React.PointerEvent) => {
    if (!drag) return;
    const p = at(e);
    if (drag.kind === "box") setDrag({ ...drag, x2: p.x, y2: p.y });
    else if (drag.kind === "move" || drag.kind === "copy") {
      setDrag({ ...drag, dt: snapTo((p.x - drag.x) / px), dn: Math.round((p.y - drag.y) / ROW) });
    } else if (drag.kind === "resize") setDrag({ ...drag, dl: snapTo((p.x - drag.x) / px) });
  };

  const up = () => {
    if (!drag) return;
    setDrag(null);
    if (drag.kind === "box") {
      const [x1, x2] = [Math.min(drag.x, drag.x2), Math.max(drag.x, drag.x2)];
      const [y1, y2] = [Math.min(drag.y, drag.y2), Math.max(drag.y, drag.y2)];
      if (x2 - x1 < 3 && y2 - y1 < 3) {
        // A click on empty space: a new note there.
        const r = Math.floor(drag.y / ROW);
        const t = Math.floor(drag.x / px / snap) * snap;
        // Not past the clip's loop (it would never play).
        if (r < 0 || r >= rows.length || t >= Math.min(length, MAX_TICKS)) return;
        const n = { tick: t, len: drums ? STEP : Math.max(snap, STEP / 2), note: rows[r], velocity: 100 };
        setSelected(new Set([key(n)]));
        void update([], [n]);
        return;
      }
      const inside = events.filter((n) => {
        const nx = n.tick * px;
        const ny = rowOf(n.note) * ROW;
        return nx + n.len * px > x1 && nx < x2 && ny + ROW > y1 && ny < y2;
      });
      setSelected(new Set([...(drag.additive ? selected : []), ...inside.map(key)]));
      return;
    }
    const out = drag.notes.map(moved);
    const changed = out.some((n, i) => JSON.stringify(n) !== JSON.stringify(drag.notes[i]));
    if (!changed) return;
    const remove = drag.kind === "copy" ? [] : drag.notes.map((n) => ({ tick: n.tick, note: n.note }));
    setSelected(new Set(out.map(key)));
    void update(remove, out);
  };

  // The playhead, where this clip is playing.
  let head: number | null = null;
  if (playing && tick !== null) {
    if (songMode) {
      const p = track.arrangement.find((q) => q.clip === clip.id && q.start <= tick && tick < q.start + q.length);
      if (p) head = (tick - p.start + p.offset) % length;
    } else if (clip.length !== null) head = tick % length;
    else if (step !== null) head = step * STEP;
  }

  const ghosts = taking.filter((n) => n.clip === clip.id && rowOf(n.note) >= 0);
  // A copy leaves the originals where they are.
  const shown = drag?.kind === "copy" ? events : events.map(moved);
  const copies = drag?.kind === "copy" ? drag.notes.map(moved) : [];
  const line = (every: number, color: string) =>
    `repeating-linear-gradient(to right, ${color} 0 1px, transparent 1px ${every * px}px)`;
  const rowLines = `repeating-linear-gradient(to bottom, transparent 0 ${ROW - 1}px, #1f1f23 ${ROW - 1}px ${ROW}px)`;
  const background = [line(BAR, "#71717a"), line(BEAT, "#3f3f46"), line(STEP, "#27272a"), rowLines].join(", ");
  const btn = "px-2 py-0.5 rounded border border-zinc-700 text-zinc-300 hover:border-zinc-500 disabled:opacity-30";

  return (
    <section className="flex flex-col gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid={`pianoroll-${id}`}>
      <div className="flex items-center gap-3 text-xs text-zinc-400">
        <span className="text-zinc-300">
          {clip.name} <span className="text-zinc-500">({events.length} notes)</span>
        </span>
        <label className="flex items-center gap-1">
          snap
          <select
            className="px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-zinc-200"
            value={snap}
            data-testid="roll-snap"
            onChange={(e) => setSnap(Number(e.target.value))}
          >
            {SNAPS.map(([n, t]) => (
              <option key={n} value={t}>
                {n}
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1">
          zoom
          <select
            className="px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-zinc-200"
            value={zoom}
            data-testid="roll-zoom"
            onChange={(e) => setZoom(Number(e.target.value))}
          >
            {[0.5, 1, 2, 4].map((z) => (
              <option key={z} value={z}>
                {z * 100}%
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1" title="the clip's own length; empty follows sequencer.length">
          length (steps)
          <input
            className="w-14 px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-zinc-200"
            type="number"
            min={1}
            max={64}
            placeholder={`${stepsLength}`}
            defaultValue={clip.length === null ? "" : Math.round(clip.length / STEP)}
            key={`${clip.id}-${clip.length}`}
            data-testid="roll-length"
            onKeyDown={(e) => e.key === "Enter" && (e.target as HTMLInputElement).blur()}
            onBlur={(e) => {
              // Sent once, when done typing (one undo step).
              const v = e.target.value === "" ? null : Math.min(64, Math.max(1, Math.round(Number(e.target.value)))) * STEP;
              if (v !== clip.length && (v === null || Number.isFinite(v))) {
                void act(client.call("clip.length", { instrument: id, clip: clip.id, length: v }));
              }
            }}
          />
        </label>
        <button
          className={btn}
          disabled={!events.length}
          title={selected.size ? "quantize the selected notes to the snap grid" : "quantize every note to the snap grid"}
          data-testid="roll-quantize"
          onClick={() =>
            void act(
              client.call("clip.quantize", {
                instrument: id,
                clip: clip.id,
                grid: snap,
                strength: null,
                events: selected.size ? events.filter((x) => selected.has(key(x))).map((x) => ({ tick: x.tick, note: x.note })) : null,
              }),
            )
          }
        >
          quantize{selected.size ? ` ${selected.size}` : ""}
        </button>
        <button className={btn} disabled={!selected.size} data-testid="roll-delete" onClick={() => void update(events.filter((x) => selected.has(key(x))).map((x) => ({ tick: x.tick, note: x.note })), [])}>
          delete
        </button>
        <span className="ml-auto text-zinc-500">click: add - drag: move - edge: length - alt-drag: copy - shift: select</span>
      </div>
      <div className="flex max-h-96 overflow-auto rounded border border-zinc-800 bg-zinc-950" data-testid="roll-scroll">
        {/* Row labels, beside the grid (they scroll with it vertically). */}
        <div className="sticky left-0 z-10 shrink-0 w-12 bg-zinc-900 border-r border-zinc-800">
          <div className="h-5" />
          {rows.map((n) => (
            <div
              key={n}
              className={`h-4 px-1 text-[9px] leading-4 text-zinc-400 truncate ${!drums && [1, 3, 6, 8, 10].includes(n % 12) ? "bg-zinc-950" : ""}`}
              style={{ height: ROW }}
            >
              {label(n)}
            </div>
          ))}
          <div className="text-[9px] text-zinc-500 px-1 pt-1" style={{ height: VEL_H }}>
            vel
          </div>
        </div>
        <div className="relative shrink-0" style={{ width }}>
          {/* Bar/beat ruler. */}
          <div className="h-5 relative border-b border-zinc-800 text-[9px] text-zinc-500">
            {Array.from({ length: Math.ceil(width / px / BEAT) }, (_, i) => (
              <span key={i} className="absolute top-0.5" style={{ left: i * BEAT * px + 2 }}>
                {i % 4 === 0 ? `${i / 4 + 1}` : `.${(i % 4) + 1}`}
              </span>
            ))}
          </div>
          <div
            ref={grid}
            className="relative cursor-crosshair touch-none select-none"
            style={{ width, height, backgroundImage: background }}
            data-testid="roll-grid"
            onPointerDown={(e) => down(e)}
            onPointerMove={move}
            onPointerUp={up}
          >
            {/* Past the clip's loop: dimmed. */}
            <div className="absolute top-0 bottom-0 bg-zinc-950/70 pointer-events-none" style={{ left: length * px, right: 0 }} />
            {ghosts.map((n, i) => (
              <div
                key={`take-${i}`}
                className={`absolute rounded-sm border border-red-400/70 bg-red-500/30 pointer-events-none ${n.held ? "animate-pulse" : ""}`}
                style={{ left: n.tick * px, top: rowOf(n.note) * ROW + 1, width: Math.max(2, n.len * px), height: ROW - 2 }}
                data-testid={`take-note-${n.tick}-${n.note}`}
              />
            ))}
            {[...shown, ...copies].map((n, i) => {
              const sel = selected.has(key(i < shown.length ? events[i] : n));
              const alpha = 0.35 + 0.65 * (n.velocity / 127);
              return (
                <div
                  key={`${i}-${key(n)}`}
                  className={`absolute rounded-sm border ${sel ? "border-zinc-50" : "border-amber-700"} cursor-move`}
                  style={{
                    left: n.tick * px,
                    top: rowOf(n.note) * ROW + 1,
                    width: Math.max(4, n.len * px),
                    height: ROW - 2,
                    background: `rgba(245, 158, 11, ${alpha})`,
                  }}
                  title={`${label(n.note)} at tick ${n.tick}, ${n.len} long, velocity ${n.velocity}`}
                  data-testid={`note-${n.tick}-${n.note}`}
                  data-selected={sel}
                  onPointerDown={(e) => down(e, events[i] ?? n)}
                >
                  <div
                    className="absolute right-0 top-0 bottom-0 w-1.5 cursor-ew-resize"
                    data-testid={`note-edge-${n.tick}-${n.note}`}
                    onPointerDown={(e) => down(e, events[i] ?? n, true)}
                  />
                </div>
              );
            })}
            {drag?.kind === "box" && (
              <div
                className="absolute border border-zinc-300 bg-zinc-300/10 pointer-events-none"
                style={{
                  left: Math.min(drag.x, drag.x2),
                  top: Math.min(drag.y, drag.y2),
                  width: Math.abs(drag.x2 - drag.x),
                  height: Math.abs(drag.y2 - drag.y),
                }}
              />
            )}
            {head !== null && (
              <div className="absolute top-0 bottom-0 w-px bg-zinc-100 pointer-events-none" style={{ left: head * px }} data-testid="roll-playhead" />
            )}
          </div>
          {/* Velocity lane. */}
          <div className="relative border-t border-zinc-800" style={{ height: VEL_H }} data-testid="roll-velocity">
            {shown.map((n, i) => (
              <div
                key={`v-${i}`}
                className={`absolute bottom-0 w-1.5 cursor-ns-resize ${selected.has(key(events[i])) ? "bg-zinc-100" : "bg-amber-500"}`}
                style={{ left: n.tick * px, height: (n.velocity / 127) * VEL_H }}
                title={`velocity ${n.velocity}`}
                data-testid={`vel-${events[i].tick}-${events[i].note}`}
                onPointerDown={(e) => {
                  e.stopPropagation();
                  (e.target as Element).setPointerCapture(e.pointerId);
                  const sel = selected.has(key(events[i])) ? selected : new Set([key(events[i])]);
                  setSelected(sel);
                  setDrag({ kind: "velocity", y: e.clientY, notes: events.filter((x) => sel.has(key(x))), dv: 0 });
                }}
                onPointerMove={(e) => drag?.kind === "velocity" && setDrag({ ...drag, dv: Math.round(((drag.y - e.clientY) / VEL_H) * 127) })}
                onPointerUp={up}
              />
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
