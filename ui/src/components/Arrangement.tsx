// The arrangement view (RFC 0008 phase C): the song left to right, one lane
// per track (= instrument). Track headers select the instrument for the
// editor and arm it (the seat's focus, which routes MIDI input and sets the
// record target). The ruler shows bars, the loop range, the song's end, and
// where song mode plays from; click it to locate. Placements show their
// clip's name and a note preview, with looped repeats marked. Drag a
// placement to move it, its right edge to resize it (longer loops the clip),
// alt-drag to copy; click selects and Delete removes; drag a clip from a
// header's pool into its lane to place it. Drags show where they go and send
// on release, one undo step each; the daemon decides, the view only draws.

import { useMemo, useRef, useState } from "react";
import type { Clip } from "../generated/Clip";
import type { Placement } from "../generated/Placement";
import { act, client, mySeat, select, useApp, useSelected } from "../store";

/** Ticks per step, beat, and bar, as in the protocol. */
const STEP = 24;
const BEAT = 96;
const BAR = 384;
const LANE = 44;
const RULER = 22;
const HEADER = 128;
/** Bars shown past the last thing on the timeline. */
const TAIL = 4;
const MIN_BARS = 16;

/** Locate snaps, in ticks. */
const SNAPS: [string, number][] = [
  ["bar", BAR],
  ["beat", BEAT],
  ["off", 1],
];
/** Pixels per bar. */
const ZOOMS = [48, 96, 192, 384];
/** The song's last tick (bar 999, `MAX_SONG_TICKS`). */
const MAX_TICKS = 999 * BAR;

type Drag =
  | { kind: "move"; id: string; p: Placement; x: number; dt: number }
  | { kind: "copy"; id: string; p: Placement; x: number; dt: number }
  | { kind: "resize"; id: string; p: Placement; x: number; dl: number }
  // A clip from a header's pool; `start` is where it would land (null: off
  // its lane).
  | { kind: "pool"; id: string; clip: number; start: number | null };

/** Pointer handlers a lane and a header hand their blocks and chips. */
type Handlers = {
  down: (e: React.PointerEvent, d: Drag) => void;
  move: (e: React.PointerEvent) => void;
  up: (e: React.PointerEvent) => void;
};

/** A placement's clip drawn as notes, repeated where the placement loops it,
 * with each repeat's start marked. */
function NotePreview({ placement, clip, clipLength, px }: { placement: Placement; clip: Clip | undefined; clipLength: number; px: number }) {
  const width = placement.length * px;
  const height = LANE - 18;
  const notes = clip?.events ?? [];
  if (!notes.length) return null;
  const lo = notes.reduce((m, e) => Math.min(m, e.note), 127);
  const hi = notes.reduce((m, e) => Math.max(m, e.note), 0);
  const y = (n: number) => (hi === lo ? height / 2 : ((hi - n) / (hi - lo)) * (height - 2));
  const marks: number[] = [];
  const bars: { x: number; w: number; y: number }[] = [];
  if (clipLength > 0) {
    // Placement time r plays clip tick (r + offset) % clipLength.
    for (let k = 0; k * clipLength - placement.offset < placement.length; k++) {
      const base = k * clipLength - placement.offset;
      if (base > 0) marks.push(base);
      for (const e of notes) {
        const r = base + e.tick;
        if (r < 0 || r >= placement.length) continue;
        bars.push({ x: r * px, w: Math.max(1, Math.min(e.len, placement.length - r) * px), y: y(e.note) });
      }
    }
  }
  return (
    <svg className="absolute left-0 bottom-0.5 pointer-events-none" width={width} height={height}>
      {marks.map((m) => (
        <line key={`m${m}`} x1={m * px} x2={m * px} y1={0} y2={height} stroke="currentColor" strokeOpacity={0.35} strokeDasharray="2 2" />
      ))}
      {bars.map((b, i) => (
        <rect key={i} x={b.x} y={b.y} width={b.w} height={2} fill="currentColor" fillOpacity={0.8} />
      ))}
    </svg>
  );
}

function Lane({ id, px, bars, drag, picked, h }: { id: string; px: number; bars: number; drag: Drag | null; picked: number | null; h: Handlers }) {
  const track = useApp((s) => s.snapshot?.tracks.find((t) => t.instrument === id));
  const clips = useApp((s) => s.snapshot?.clips);
  const stepsLength = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const song = useApp((s) => (s.snapshot?.params["song.mode"] ?? 0) >= 0.5);
  const tick = useApp((s) => s.snapshot?.transport.tick ?? null);
  if (!track) return null;
  const clipOf = (n: number) => clips?.find((c) => c.instrument === id && c.id === n);
  const lengthOf = (n: number) => clipOf(n)?.length ?? stepsLength * STEP;
  const mine = drag?.id === id ? drag : null;

  // Where the dragged placement is drawn (the daemon gets it on release).
  const shown = (p: Placement): Placement => {
    if (mine?.kind === "move" && mine.p.start === p.start) return { ...p, start: dragStart(mine) };
    if (mine?.kind === "resize" && mine.p.start === p.start) return { ...p, length: resized(mine) };
    return p;
  };
  let ghost: Placement | null = null;
  if (mine?.kind === "copy") ghost = { ...mine.p, start: dragStart(mine) };
  if (mine?.kind === "pool" && mine.start !== null) ghost = { clip: mine.clip, start: mine.start, length: lengthOf(mine.clip), offset: 0 };

  const block = (p: Placement, at: number | null) => {
    const clip = clipOf(p.clip);
    const live = at !== null && playing && song && tick !== null && p.start <= tick && tick < p.start + p.length;
    const sel = at !== null && at === picked;
    return (
      <div
        key={at ?? "ghost"}
        className={`absolute top-1 bottom-1 rounded border overflow-hidden text-violet-200 ${
          at === null
            ? "border-dashed border-violet-200 bg-violet-500/25 pointer-events-none"
            : `cursor-grab ${p.clip === track.selected ? "bg-violet-500/35 border-violet-300" : "bg-violet-500/20 border-violet-500/60"}`
        } ${sel ? "outline outline-1 outline-zinc-50" : ""} ${live ? "ring-1 ring-zinc-100" : ""}`}
        style={{ left: p.start * px, width: Math.max(2, p.length * px) }}
        title={`${clip?.name ?? `clip ${p.clip}`}: bar ${p.start / BAR + 1}, ${p.length / BAR} bars${p.offset ? `, from tick ${p.offset}` : ""}`}
        data-testid={at === null ? `placement-ghost-${id}` : `placement-${id}-${at}`}
        data-clip={p.clip}
        data-length={p.length}
        data-start={p.start}
        data-selected={sel}
        onPointerDown={at === null ? undefined : (e) => h.down(e, { kind: e.altKey ? "copy" : "move", id, p: track.arrangement.find((q) => q.start === at)!, x: e.clientX, dt: 0 })}
        onPointerMove={h.move}
        onPointerUp={h.up}
      >
        <div className="px-1 text-[10px] leading-4 truncate">{clip?.name ?? p.clip}</div>
        <NotePreview placement={p} clip={clip} clipLength={lengthOf(p.clip)} px={px} />
        {at !== null && (
          <div
            className="absolute right-0 top-0 bottom-0 w-1.5 cursor-ew-resize"
            data-testid={`placement-edge-${id}-${at}`}
            onPointerDown={(e) => h.down(e, { kind: "resize", id, p: track.arrangement.find((q) => q.start === at)!, x: e.clientX, dl: 0 })}
          />
        )}
      </div>
    );
  };

  return (
    <div className="relative border-b border-zinc-800" style={{ height: LANE, width: bars * BAR * px }} data-testid={`lane-${id}`}>
      {track.arrangement.map((p) => block(shown(p), p.start))}
      {ghost && block(ghost, null)}
    </div>
  );
}

/** A move or copy's new start. */
const dragStart = (d: { p: Placement; dt: number }) => Math.min(MAX_TICKS - d.p.length, Math.max(0, d.p.start + d.dt));
/** A resize's new length (the drag keeps it at least a snap long). */
const resized = (d: { p: Placement; dl: number }) => Math.min(MAX_TICKS - d.p.start, d.p.length + d.dl);

function TrackHeader({ id, name, type, armed, selected, seated, h }: { id: string; name: string; type: string; armed: boolean; selected: boolean; seated: boolean; h: Handlers }) {
  const clips = useApp((s) => s.snapshot?.clips);
  const pool = useMemo(() => (clips ?? []).filter((c) => c.instrument === id), [clips, id]);
  return (
    <div
      className={`flex flex-col justify-center gap-0.5 px-2 border-b border-zinc-800 text-xs ${selected ? "bg-zinc-800" : ""}`}
      style={{ height: LANE }}
      data-testid={`track-header-${id}`}
    >
      <div className="flex items-center gap-1">
        <button
          className="flex-1 min-w-0 text-left truncate text-zinc-200"
          title="open in the editor"
          data-testid={`track-select-${id}`}
          data-selected={selected}
          onClick={() => select(id)}
        >
          {name} <span className="text-zinc-500">{type}</span>
        </button>
        <button
          className={`w-5 h-5 shrink-0 rounded-full border flex items-center justify-center ${
            armed ? "bg-red-600 border-red-400" : "border-zinc-600 hover:border-zinc-400"
          }`}
          disabled={!seated}
          title={!seated ? "join a seat to arm" : armed ? "armed: MIDI input plays it and recording goes here" : "arm: play MIDI input here and record into it"}
          data-testid={`arm-${id}`}
          data-armed={armed}
          onClick={() => !armed && void act(client.call("seat.focus", { seat: null, instrument: id }))}
        >
          <span className={`w-2 h-2 rounded-full ${armed ? "bg-white" : "bg-red-500/70"}`} />
        </button>
      </div>
      {/* The clip pool: drag a clip into the lane to place it. */}
      <div className="flex gap-0.5 overflow-hidden" data-testid={`pool-${id}`}>
        {pool.map((c) => (
          <div
            key={c.id}
            className="max-w-12 px-1 rounded-sm bg-violet-500/25 border border-violet-500/50 text-[9px] leading-3 text-violet-200 truncate cursor-grab touch-none select-none"
            title={`${c.name}: drag into the lane to place it`}
            data-testid={`pool-${id}-${c.id}`}
            onPointerDown={(e) => h.down(e, { kind: "pool", id, clip: c.id, start: null })}
            onPointerMove={h.move}
            onPointerUp={h.up}
          >
            {c.name}
          </div>
        ))}
      </div>
    </div>
  );
}

export function Arrangement() {
  const instruments = useApp((s) => s.snapshot?.graph.instruments ?? []);
  const tracks = useApp((s) => s.snapshot?.tracks);
  const clips = useApp((s) => s.snapshot?.clips);
  const stepsLength = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  const seat = useApp(mySeat);
  const selected = useSelected();
  const song = useApp((s) => (s.snapshot?.params["song.mode"] ?? 0) >= 0.5);
  const loop = useApp((s) => s.snapshot?.params["song.loop"] ?? 0);
  const loopStart = useApp((s) => s.snapshot?.params["song.loop_start"] ?? 0);
  const loopEnd = useApp((s) => s.snapshot?.params["song.loop_end"] ?? 0);
  const start = useApp((s) => s.snapshot?.transport.start ?? 0);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const tick = useApp((s) => s.snapshot?.transport.tick ?? null);
  const [zoom, setZoom] = useState(96);
  const [snap, setSnap] = useState(BAR);
  const [drag, setDrag] = useState<Drag | null>(null);
  // The selected placement: its track and start.
  const [picked, setPicked] = useState<{ id: string; start: number } | null>(null);
  const root = useRef<HTMLElement>(null);
  const lanes = useRef<HTMLDivElement>(null);
  const px = zoom / BAR;

  // The end of the last placement (`song.get`'s `length`).
  const end = useMemo(() => Math.max(0, ...(tracks ?? []).flatMap((t) => t.arrangement.map((p) => p.start + p.length))), [tracks]);
  const bars = Math.max(MIN_BARS, Math.ceil(Math.max(end, start, loop === 2 ? loopEnd * BAR : 0) / BAR) + TAIL);
  const width = bars * BAR * px;
  const focus = seat ? (seat.config.focus ?? instruments[0]?.id ?? null) : null;
  const head = playing && song && tick !== null ? tick : null;

  const locate = (e: React.MouseEvent<HTMLDivElement>) => {
    const x = e.clientX - e.currentTarget.getBoundingClientRect().left;
    const t = Math.max(0, Math.round(x / px / snap) * snap);
    void act(client.call("transport.locate", { tick: t }));
  };
  const snapTo = (t: number) => Math.round(t / snap) * snap;
  const placementAt = (id: string, start: number) => tracks?.find((t) => t.instrument === id)?.arrangement.find((p) => p.start === start);
  const place = (id: string, p: { clip: number; start: number; length: number | null; offset: number | null }) => ({
    method: "song.place",
    params: { instrument: id, ...p },
  });

  /** A drag after the pointer moved to `e`. */
  const follow = (drag: Drag, e: React.PointerEvent): Drag => {
    if (drag.kind === "move" || drag.kind === "copy") return { ...drag, dt: snapTo((e.clientX - drag.x) / px) };
    if (drag.kind === "resize") {
      // At least a snap long (or as long as it was, if shorter).
      const least = Math.min(snap, drag.p.length) - drag.p.length;
      return { ...drag, dl: Math.max(least, snapTo((e.clientX - drag.x) / px)) };
    }
    // Over its own lane: the snap cell under the pointer, ending by the
    // song's last tick.
    const lane = lanes.current?.querySelector(`[data-testid="lane-${drag.id}"]`);
    const r = lane?.getBoundingClientRect();
    const x = r ? e.clientX - r.left : -1;
    const on = !!r && x >= 0 && x < r.width && e.clientY >= r.top && e.clientY < r.bottom;
    const length = clips?.find((c) => c.instrument === drag.id && c.id === drag.clip)?.length ?? stepsLength * STEP;
    return { ...drag, start: on ? Math.min(Math.floor((MAX_TICKS - length) / snap) * snap, Math.floor(x / px / snap) * snap) : null };
  };

  const h: Handlers = {
    down: (e, d) => {
      e.stopPropagation();
      e.preventDefault();
      (e.currentTarget as Element).setPointerCapture(e.pointerId);
      root.current?.focus();
      if (d.kind !== "pool") setPicked({ id: d.id, start: d.p.start });
      setDrag(d);
    },
    move: (e) => {
      const d = drag && follow(drag, e);
      if (d) setDrag(d);
    },
    up: (e) => {
      if (!drag) return;
      setDrag(null);
      // Where the pointer let go, even if its last move has not drawn yet.
      const d = follow(drag, e);
      const { id } = d;
      if (d.kind === "move" || d.kind === "copy") {
        const to = dragStart(d);
        if (to === d.p.start) return; // a click: selected
        const { clip, length, offset } = d.p;
        setPicked({ id, start: to });
        if (d.kind === "move") void act(client.call("song.move", { instrument: id, start: d.p.start, to }));
        else void act(client.call("song.place", { instrument: id, clip, start: to, length, offset }));
      } else if (d.kind === "resize") {
        const length = resized(d);
        if (length === d.p.length) return;
        const p = place(id, { clip: d.p.clip, start: d.p.start, length, offset: d.p.offset });
        // Placing over it at the same start replaces it, but a shorter one
        // would leave its tail playing: lift it off first (one batch, one
        // undo step).
        if (length > d.p.length) void act(client.call("song.place", p.params));
        else void act(client.call("batch", { requests: [{ method: "song.remove", params: { instrument: id, start: d.p.start } }, p] }));
      } else if (d.start !== null) {
        setPicked({ id, start: d.start });
        void act(client.call("song.place", place(id, { clip: d.clip, start: d.start, length: null, offset: null }).params));
      }
    },
  };

  // Delete removes the selected placement; Escape clears the selection.
  const key = (e: React.KeyboardEvent) => {
    const t = e.target as HTMLElement;
    if (!picked || ["INPUT", "SELECT", "TEXTAREA"].includes(t.tagName) || t.isContentEditable) return;
    if (e.key === "Delete" || e.key === "Backspace") {
      // Not also the piano roll's selected notes.
      e.preventDefault();
      e.stopPropagation();
      if (placementAt(picked.id, picked.start)) void act(client.call("song.remove", { instrument: picked.id, start: picked.start }));
      setPicked(null);
    } else if (e.key === "Escape") setPicked(null);
  };

  const line = (every: number, color: string) => `repeating-linear-gradient(to right, ${color} 0 1px, transparent 1px ${every * px}px)`;
  const grid = [line(BAR, "#3f3f46"), ...(zoom >= 192 ? [line(BEAT, "#27272a")] : [])].join(", ");
  const sel = "px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-zinc-200";

  return (
    <section
      ref={root}
      tabIndex={-1}
      className="flex flex-col gap-2 p-3 min-w-0 rounded-lg bg-zinc-900/50 border border-zinc-800 outline-none"
      data-testid="arrangement"
      data-end={end}
      onKeyDown={key}
    >
      <div className="flex items-center gap-3 text-xs text-zinc-400">
        <span className="text-zinc-300">arrangement</span>
        {!song && <span className="text-zinc-500">pattern mode: the song plays in song mode</span>}
        <label className="flex items-center gap-1">
          snap
          <select className={sel} value={snap} data-testid="arr-snap" onChange={(e) => setSnap(Number(e.target.value))}>
            {SNAPS.map(([n, t]) => (
              <option key={n} value={t}>
                {n}
              </option>
            ))}
          </select>
        </label>
        <label className="flex items-center gap-1">
          zoom
          <select className={sel} value={zoom} data-testid="arr-zoom" onChange={(e) => setZoom(Number(e.target.value))}>
            {ZOOMS.map((z) => (
              <option key={z} value={z}>
                {Math.round((z / 96) * 100)}%
              </option>
            ))}
          </select>
        </label>
        <span className="ml-auto text-zinc-500">ruler: play from there - drag: move - edge: length - alt-drag: copy - Delete: remove - drag a clip from the pool into its lane</span>
      </div>
      <div className="flex overflow-x-auto rounded border border-zinc-800 bg-zinc-950" data-testid="arr-scroll">
        {/* Track headers, sticky on the left. */}
        <div className="sticky left-0 z-20 shrink-0 bg-zinc-900 border-r border-zinc-800" style={{ width: HEADER }}>
          <div className="border-b border-zinc-800" style={{ height: RULER }} />
          {instruments.map((i) => (
            <TrackHeader
              key={i.id}
              id={i.id}
              name={i.name}
              type={i.type}
              armed={i.id === focus}
              selected={i.id === selected}
              seated={!!seat}
              h={h}
            />
          ))}
          {!instruments.length && <div className="p-2 text-xs text-zinc-500">No instruments.</div>}
        </div>
        <div className="relative shrink-0" style={{ width }}>
          {/* Ruler: bars, the loop range, the song's end, the start point. */}
          <div
            className="relative border-b border-zinc-800 text-[10px] text-zinc-500 cursor-pointer select-none"
            style={{ height: RULER }}
            data-testid="arr-ruler"
            onClick={locate}
          >
            {loop === 2 && loopEnd > loopStart && (
              <div
                className="absolute top-0 h-1.5 bg-amber-500/70 rounded-sm"
                style={{ left: loopStart * BAR * px, width: (loopEnd - loopStart) * BAR * px }}
                title={`loop: bars ${loopStart + 1} to ${loopEnd}`}
                data-testid="arr-loop"
              />
            )}
            {Array.from({ length: bars }, (_, b) => (
              <span key={b} className="absolute top-1.5 pl-1 border-l border-zinc-700 h-full" style={{ left: b * BAR * px }}>
                {b + 1}
              </span>
            ))}
            <div
              className="absolute bottom-0 w-0 h-0 border-x-[5px] border-x-transparent border-b-[7px] border-b-sky-400 -translate-x-1/2"
              style={{ left: start * px }}
              title={`song mode plays from bar ${start / BAR + 1}`}
              data-testid="arr-start"
              data-tick={start}
            />
          </div>
          <div className="relative touch-none select-none" ref={lanes} style={{ backgroundImage: grid }} onPointerDown={() => setPicked(null)}>
            {instruments.map((i) => (
              <Lane key={i.id} id={i.id} px={px} bars={bars} drag={drag} picked={picked?.id === i.id ? picked.start : null} h={h} />
            ))}
            {loop === 2 && loopEnd > loopStart && (
              <div
                className="absolute top-0 bottom-0 bg-amber-500/5 pointer-events-none"
                style={{ left: loopStart * BAR * px, width: (loopEnd - loopStart) * BAR * px }}
              />
            )}
          </div>
          {end > 0 && (
            <div className="absolute top-0 bottom-0 w-px bg-zinc-400/60 pointer-events-none" style={{ left: end * px }} title="the song's end" data-testid="arr-end" />
          )}
          {head !== null && (
            <div className="absolute top-0 bottom-0 w-px bg-zinc-100 pointer-events-none" style={{ left: head * px }} data-testid="arr-playhead" data-tick={head} />
          )}
        </div>
      </div>
    </section>
  );
}
