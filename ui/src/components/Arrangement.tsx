// The arrangement view (RFC 0008 phase C, RFC 0009): the song left to
// right, one lane per track (= instrument); play always plays it. Track
// headers select the instrument for the editor and arm it (the seat's
// focus, which routes MIDI input and sets the record target), and launch a
// clip from the pool (the track loops it instead of its lane, dimmed, until
// "back"). The ruler shows bars, the loop range, the song's end, and where
// play starts from; click it to locate. "loop" sets the loop brace over the
// selected placements. Placements show their
// clip's name and a note preview, with looped repeats marked. Drag a
// placement to move it (alone, onto another track's lane too: its clip is
// copied into that pool), its right edge to resize it (longer loops the
// clip), alt-drag to copy; click selects and Delete removes; drag a clip from a
// header's pool into its lane to place it; double-click a placement to open
// its clip in the piano roll below (which selects it). The ruler's top strip
// holds the loop brace. Drags show where they go and send on release, one
// undo step each; the daemon decides, the view only draws.

import { useEffect, useMemo, useRef, useState } from "react";
import type { Clip } from "../generated/Clip";
import type { Placement } from "../generated/Placement";
import type { MethodName } from "../generated/methods";
import { act, client, mySeat, select, useApp, useSelected } from "../store";
import { PianoRoll } from "./PianoRoll";

/** Ticks per beat and bar, as in the protocol. */
const BEAT = 96;
const BAR = 384;
const LANE = 44;
/** The loop brace's strip, at the top of the ruler. */
const BRACE = 10;
const RULER = 22 + BRACE;
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

/** A selected placement: its track and the placement. */
type Pick = { id: string; p: Placement };
const pickKey = (id: string, start: number) => `${id}:${start}`;
const unpick = (key: string) => {
  const i = key.lastIndexOf(":");
  return { id: key.slice(0, i), start: Number(key.slice(i + 1)) };
};

type Drag =
  // The grabbed placement and every selected one that moves with it; `to`
  // is the track it goes to (another only when it goes alone).
  | { kind: "move"; id: string; p: Placement; x: number; dt: number; group: Pick[]; to: string }
  | { kind: "copy"; id: string; p: Placement; x: number; dt: number; group: Pick[]; to: string }
  | { kind: "resize"; id: string; p: Placement; x: number; dl: number }
  // A clip from a header's pool; `start` is where it would land (null: off
  // its lane).
  | { kind: "pool"; id: string; clip: number; start: number | null };

/** Pointer handlers a lane and a header hand their blocks and chips. */
type Handlers = {
  down: (e: React.PointerEvent, d: Drag) => void;
  move: (e: React.PointerEvent) => void;
  up: (e: React.PointerEvent) => void;
  /** The pointer was taken away (a system gesture, the window lost focus). */
  cancel: () => void;
  /** Open a placement's clip in the piano roll below. */
  open: (id: string, clip: number) => void;
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

function Lane({ id, px, bars, drag, picked, h }: { id: string; px: number; bars: number; drag: Drag | null; picked: Set<string>; h: Handlers }) {
  const track = useApp((s) => s.snapshot?.tracks.find((t) => t.instrument === id));
  const clips = useApp((s) => s.snapshot?.clips);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const tick = useApp((s) => s.snapshot?.transport.tick ?? null);
  if (!track) return null;
  // A launched clip plays instead of the lane, which is dimmed.
  const launched = track.launched !== null;
  const clipOf = (n: number, from = id) => clips?.find((c) => c.instrument === from && c.id === n);
  const lengthOf = (n: number, from = id) => clipOf(n, from)?.length ?? BAR;
  const mine = drag?.id === id ? drag : null;
  // A move or copy carries this lane's selected placements too.
  const group = drag?.kind === "move" || drag?.kind === "copy" ? drag.group.filter((g) => g.id === id) : [];
  const dt = drag?.kind === "move" || drag?.kind === "copy" ? groupDt(drag) : 0;
  // Going to another track: drawn there (from its own pool's clip), and
  // left in place here until the daemon has it.
  const cross = (drag?.kind === "move" || drag?.kind === "copy") && drag.to !== drag.id ? drag : null;

  // Where the dragged placements are drawn (the daemon gets them on release).
  const shown = (p: Placement): Placement => {
    if (drag?.kind === "move" && !cross && group.some((g) => g.p.start === p.start)) return { ...p, start: p.start + dt };
    if (mine?.kind === "resize" && mine.p.start === p.start) return { ...p, length: resized(mine) };
    return p;
  };
  const ghosts: Placement[] = drag?.kind === "copy" && !cross ? group.map((g) => ({ ...g.p, start: g.p.start + dt })) : [];
  const away = cross?.to === id ? { ...cross.p, start: cross.p.start + dt } : null;
  if (mine?.kind === "pool" && mine.start !== null) ghosts.push({ clip: mine.clip, start: mine.start, length: lengthOf(mine.clip), offset: 0 });

  const block = (p: Placement, at: number | null, from = id) => {
    const clip = clipOf(p.clip, from);
    const live = at !== null && playing && !launched && tick !== null && p.start <= tick && tick < p.start + p.length;
    const sel = at !== null && picked.has(pickKey(id, at));
    return (
      <div
        key={at ?? `ghost-${p.start}`}
        className={`absolute top-1 bottom-1 rounded border overflow-hidden text-violet-200 ${
          at === null
            ? "border-dashed border-violet-200 bg-violet-500/25 pointer-events-none"
            : `cursor-grab ${p.clip === track.selected ? "bg-violet-500/35 border-violet-300" : "bg-violet-500/20 border-violet-500/60"}`
        } ${sel ? "outline outline-1 outline-zinc-50" : ""} ${live ? "ring-1 ring-zinc-100" : ""} ${launched && at !== null ? "opacity-40" : ""}`}
        style={{ left: p.start * px, width: Math.max(2, p.length * px) }}
        title={`${clip?.name ?? `clip ${p.clip}`}: bar ${p.start / BAR + 1}, ${p.length / BAR} bars${p.offset ? `, from tick ${p.offset}` : ""}`}
        data-testid={at === null ? `placement-ghost-${id}` : `placement-${id}-${at}`}
        data-clip={p.clip}
        data-length={p.length}
        data-start={p.start}
        data-selected={sel}
        onPointerDown={at === null ? undefined : (e) => h.down(e, { kind: e.altKey ? "copy" : "move", id, p: track.arrangement.find((q) => q.start === at)!, x: e.clientX, dt: 0, group: [], to: id })}
        onPointerMove={h.move}
        onPointerUp={h.up}
        onPointerCancel={h.cancel}
        onDoubleClick={at === null ? undefined : () => h.open(id, p.clip)}
      >
        <div className="px-1 text-[10px] leading-4 truncate">{clip?.name ?? p.clip}</div>
        <NotePreview placement={p} clip={clip} clipLength={lengthOf(p.clip, from)} px={px} />
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
    <div
      className={`relative border-b border-zinc-800 ${launched ? "bg-emerald-950/30" : ""}`}
      style={{ height: LANE, width: bars * BAR * px }}
      data-testid={`lane-${id}`}
      data-lane={id}
      data-launched={launched}
    >
      {track.arrangement.map((p) => block(shown(p), p.start))}
      {ghosts.map((g) => block(g, null))}
      {away && cross && block(away, null, cross.id)}
    </div>
  );
}

/** How far a move or copy goes: its distance, kept inside the song for
 * every placement in the group. */
const groupDt = (d: { dt: number; group: Pick[] }) => {
  const lo = -Math.min(...d.group.map((g) => g.p.start));
  const hi = Math.min(...d.group.map((g) => MAX_TICKS - g.p.start - g.p.length));
  return Math.min(hi, Math.max(lo, d.dt));
};
/** A resize's new length (the drag keeps it at least a snap long). */
const resized = (d: { p: Placement; dl: number }) => Math.min(MAX_TICKS - d.p.start, d.p.length + d.dl);

function TrackHeader({ id, name, type, armed, selected, seated, h }: { id: string; name: string; type: string; armed: boolean; selected: boolean; seated: boolean; h: Handlers }) {
  const clips = useApp((s) => s.snapshot?.clips);
  const track = useApp((s) => s.snapshot?.tracks.find((t) => t.instrument === id));
  const pool = useMemo(() => (clips ?? []).filter((c) => c.instrument === id), [clips, id]);
  const launched = track?.launched ?? null;
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
        <button
          className={`px-1 h-5 shrink-0 rounded border text-[9px] ${
            launched !== null ? "border-emerald-500 text-emerald-300 hover:border-emerald-300" : "border-zinc-700 text-zinc-600"
          } ${launched !== null && track?.pending ? "animate-pulse" : ""}`}
          disabled={launched === null}
          title={launched === null ? "plays the arrangement" : "back to the arrangement (from the next bar while playing)"}
          data-testid={`track-back-${id}`}
          data-pending={track?.pending ?? false}
          onClick={() => void act(client.call("song.back", { instrument: id, now: null }))}
        >
          back
        </button>
      </div>
      {/* The clip pool: drag a clip into the lane to place it; its
          triangle launches it (green while launched, pulsing while it waits
          for its bar). */}
      <div className="flex gap-0.5 overflow-hidden" data-testid={`pool-${id}`}>
        {pool.map((c) => {
          const on = launched === c.id;
          return (
            <div
              key={c.id}
              className={`flex items-center gap-0.5 max-w-14 pr-1 rounded-sm border text-[9px] leading-3 truncate cursor-grab touch-none select-none ${
                on ? "bg-emerald-500/30 border-emerald-400 text-emerald-100" : "bg-violet-500/25 border-violet-500/50 text-violet-200"
              } ${on && track?.pending ? "animate-pulse" : ""}`}
              title={`${c.name}: drag into the lane to place it`}
              data-testid={`pool-${id}-${c.id}`}
              data-launched={on}
              onPointerDown={(e) => h.down(e, { kind: "pool", id, clip: c.id, start: null })}
              onPointerMove={h.move}
              onPointerUp={h.up}
              onPointerCancel={h.cancel}
            >
              <button
                className="shrink-0 h-3 px-0.5 flex items-center"
                title={`launch ${c.name}: the track loops it instead of its lane (from the next bar while playing)`}
                data-testid={`launch-${id}-${c.id}`}
                onPointerDown={(e) => e.stopPropagation()}
                onClick={() => void act(client.call("clip.launch", { instrument: id, clip: c.id, now: null }))}
              >
                <span className={`w-0 h-0 border-y-[3px] border-y-transparent border-l-[5px] ${on ? "border-l-emerald-200" : "border-l-violet-300"}`} />
              </button>
              <span className="truncate">{c.name}</span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

/** How a brace drag changes the loop: an end, the whole brace, or a new
 * one drawn from where it started. */
type BraceDrag = {
  mode: "start" | "end" | "move" | "new";
  x: number;
  bar: number;
  /** The loop when the drag started (another client may change it). */
  s0: number;
  e0: number;
  s: number;
  e: number;
  moved: boolean;
};

/** The loop brace (`song.loop` = 2 over bars `song.loop_start` to
 * `song.loop_end`, end exclusive): drag its ends to resize it, its body to
 * move it, or empty strip to draw a new one; click its body to turn the
 * loop on or off, or empty strip to locate. Bars only: the loop's
 * parameters are whole bars. Each change is one `batch` (one undo step). */
function LoopBrace({ px, bars, loop, loopStart, loopEnd, locate }: { px: number; bars: number; loop: number; loopStart: number; loopEnd: number; locate: (x: number) => void }) {
  const [drag, setDrag] = useState<BraceDrag | null>(null);
  const strip = useRef<HTMLDivElement>(null);
  const barPx = BAR * px;
  const on = loop === 2;
  const xOf = (e: React.PointerEvent) => e.clientX - strip.current!.getBoundingClientRect().left;
  const clamp = (n: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, n));

  const follow = (d: BraceDrag, e: React.PointerEvent): BraceDrag => {
    const x = xOf(e);
    const moved = d.moved || Math.abs(x - d.x) >= 3;
    if (d.mode === "new") {
      const b = clamp(Math.floor(x / barPx), 0, 998);
      return { ...d, moved, s: Math.min(d.bar, b), e: Math.max(d.bar, b) + 1 };
    }
    const dx = Math.round((x - d.x) / barPx);
    if (d.mode === "start") return { ...d, moved, s: clamp(d.s0 + dx, 0, d.e0 - 1) };
    if (d.mode === "end") return { ...d, moved, e: clamp(d.e0 + dx, d.s0 + 1, 999) };
    const shift = clamp(dx, -d.s0, 999 - d.e0);
    return { ...d, moved, s: d.s0 + shift, e: d.e0 + shift };
  };

  const down = (e: React.PointerEvent, mode: BraceDrag["mode"]) => {
    e.stopPropagation();
    e.preventDefault();
    strip.current!.setPointerCapture(e.pointerId);
    const x = xOf(e);
    const bar = clamp(Math.floor(x / barPx), 0, 998);
    const [s, end] = mode === "new" ? [bar, bar + 1] : [loopStart, loopEnd];
    setDrag({ mode, x, bar, s0: loopStart, e0: loopEnd, s, e: end, moved: false });
  };

  const up = (e: React.PointerEvent) => {
    if (!drag) return;
    setDrag(null);
    const d = follow(drag, e);
    if (!d.moved) {
      // A click: the body turns the loop on or off; empty strip locates.
      if (d.mode === "new") locate(d.x);
      else if (d.mode === "move") void act(client.call("param.set", { path: "song.loop", value: on ? 0 : 2 }));
      return;
    }
    // A drag that ends where the brace was does nothing.
    if (d.mode !== "new" && d.s === d.s0 && d.e === d.e0) return;
    const set = (path: string, value: number) => ({ method: "param.set", params: { path, value } });
    const range = [...(d.s !== d.s0 ? [set("song.loop_start", d.s)] : []), ...(d.e !== d.e0 ? [set("song.loop_end", d.e)] : [])];
    // The two are independent parameters (any order succeeds); the end
    // goes first when the brace moved right past its old end, so the range
    // is never inverted between the two. The loop turns on last.
    if (d.s >= d.e0) range.reverse();
    const requests = [...range, ...(!on ? [set("song.loop", 2)] : [])];
    if (requests.length) void act(client.call("batch", { requests }));
  };

  const s = drag?.moved ? drag.s : loopStart;
  const e = drag?.moved ? drag.e : loopEnd;
  return (
    <div
      ref={strip}
      className="absolute top-0 left-0 cursor-crosshair touch-none"
      style={{ height: BRACE, width: bars * barPx }}
      title="drag: draw a loop - click: play from there"
      data-testid="arr-loop-strip"
      onClick={(ev) => ev.stopPropagation()}
      onPointerDown={(ev) => down(ev, "new")}
      onPointerMove={(ev) => drag && setDrag(follow(drag, ev))}
      onPointerUp={up}
      onPointerCancel={() => setDrag(null)}
    >
      {e > s && (
        <div
          className={`absolute top-0.5 rounded-sm cursor-grab ${on || drag?.moved ? "bg-amber-500/70" : "bg-zinc-600/50"}`}
          style={{ left: s * barPx, width: (e - s) * barPx, height: BRACE - 2 }}
          title={`loop ${on ? "on" : "off"}: bars ${s + 1} to ${e} - drag to move, its ends to resize, click to turn ${on ? "off" : "on"}`}
          data-testid="arr-loop"
          data-on={on}
          data-start={s}
          data-end={e}
          onPointerDown={(ev) => down(ev, "move")}
        >
          <div className="absolute left-0 top-0 bottom-0 w-1.5 cursor-ew-resize" data-testid="arr-loop-start" onPointerDown={(ev) => down(ev, "start")} />
          <div className="absolute right-0 top-0 bottom-0 w-1.5 cursor-ew-resize" data-testid="arr-loop-end" onPointerDown={(ev) => down(ev, "end")} />
        </div>
      )}
    </div>
  );
}

export function Arrangement() {
  const instruments = useApp((s) => s.snapshot?.graph.instruments ?? []);
  const tracks = useApp((s) => s.snapshot?.tracks);
  const clips = useApp((s) => s.snapshot?.clips);
  const seat = useApp(mySeat);
  const selected = useSelected();
  const loop = useApp((s) => s.snapshot?.params["song.loop"] ?? 0);
  const loopStart = useApp((s) => s.snapshot?.params["song.loop_start"] ?? 0);
  const loopEnd = useApp((s) => s.snapshot?.params["song.loop_end"] ?? 4);
  const start = useApp((s) => s.snapshot?.transport.start ?? 0);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const tick = useApp((s) => s.snapshot?.transport.tick ?? null);
  const [zoom, setZoom] = useState(96);
  const [snap, setSnap] = useState(BAR);
  const [drag, setDrag] = useState<Drag | null>(null);
  // The selected placements (`pickKey`s).
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const none = () => setPicked(new Set());
  // The instrument whose selected clip the piano roll panel shows.
  const [roll, setRoll] = useState<string | null>(null);
  const rolled = instruments.find((i) => i.id === roll);
  // A removed instrument closes it (one added later with its id stays shut).
  useEffect(() => {
    if (roll && !rolled) setRoll(null);
  }, [roll, rolled]);
  const root = useRef<HTMLElement>(null);
  const lanes = useRef<HTMLDivElement>(null);
  const px = zoom / BAR;

  // The end of the last placement (`song.get`'s `length`).
  const end = useMemo(() => Math.max(0, ...(tracks ?? []).flatMap((t) => t.arrangement.map((p) => p.start + p.length))), [tracks]);
  const bars = Math.max(MIN_BARS, Math.ceil(Math.max(end, start, loopEnd * BAR) / BAR) + TAIL);
  const width = bars * BAR * px;
  const focus = seat ? (seat.config.focus ?? instruments[0]?.id ?? null) : null;
  const head = playing && tick !== null ? tick : null;

  /** Play from `x` pixels into the timeline, snapped. */
  const locateAt = (x: number) => void act(client.call("transport.locate", { tick: Math.max(0, Math.round(x / px / snap) * snap) }));
  const locate = (e: React.MouseEvent<HTMLDivElement>) => locateAt(e.clientX - e.currentTarget.getBoundingClientRect().left);
  const snapTo = (t: number) => Math.round(t / snap) * snap;
  const placementAt = (id: string, start: number) => tracks?.find((t) => t.instrument === id)?.arrangement.find((p) => p.start === start);
  const place = (id: string, p: { clip: number | null; start: number; length: number | null; offset: number | null }) => ({
    method: "song.place",
    params: { instrument: id, ...p },
  });

  /** The track whose lane is at page height `y`, if any. */
  const laneAt = (y: number) => {
    for (const el of lanes.current?.querySelectorAll<HTMLElement>("[data-lane]") ?? []) {
      const r = el.getBoundingClientRect();
      if (y >= r.top && y < r.bottom) return el.dataset.lane!;
    }
    return null;
  };

  /** A drag after the pointer moved to `e`. */
  const follow = (drag: Drag, e: React.PointerEvent): Drag => {
    if (drag.kind === "move" || drag.kind === "copy") {
      // One placement alone can go to the lane under the pointer.
      const lane = drag.group.length === 1 ? laneAt(e.clientY) : null;
      return { ...drag, dt: snapTo((e.clientX - drag.x) / px), to: lane ?? drag.id };
    }
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
    const length = clips?.find((c) => c.instrument === drag.id && c.id === drag.clip)?.length ?? BAR;
    return { ...drag, start: on ? Math.min(Math.floor((MAX_TICKS - length) / snap) * snap, Math.floor(x / px / snap) * snap) : null };
  };

  /** One request alone, or several as one `batch` (one undo step). */
  const send = (requests: { method: string; params: unknown }[]) =>
    requests.length === 1
      ? act(client.call(requests[0].method as MethodName, requests[0].params as never))
      : act(client.call("batch", { requests }));

  /** Move or copy a placement to another track: its clip is copied into
   * that track's pool (a new clip, selected while the batch places it, then
   * the old selection back), placed there, and (for a move) removed here.
   * One `batch`, one undo step. */
  const toTrack = (d: { kind: "move" | "copy"; id: string; p: Placement; to: string }, dt: number) => {
    const src = clips?.find((c) => c.instrument === d.id && c.id === d.p.clip);
    const prev = tracks?.find((t) => t.instrument === d.to)?.selected;
    if (!src || prev === undefined) return;
    const start = d.p.start + dt;
    setPicked(new Set([pickKey(d.to, start)]));
    void send([
      { method: "clip.new", params: { instrument: d.to, name: src.name, length: src.length, select: true } },
      ...(src.events.length ? [{ method: "clip.add", params: { instrument: d.to, clip: null, events: src.events } }] : []),
      place(d.to, { clip: null, start, length: d.p.length, offset: d.p.offset }),
      { method: "clip.select", params: { instrument: d.to, clip: prev } },
      ...(d.kind === "move" ? [{ method: "song.remove", params: { instrument: d.id, start: d.p.start } }] : []),
    ]);
  };

  const h: Handlers = {
    down: (e, d) => {
      e.stopPropagation();
      e.preventDefault();
      (e.currentTarget as Element).setPointerCapture(e.pointerId);
      root.current?.focus();
      if (d.kind === "pool") return setDrag(d);
      const k = pickKey(d.id, d.p.start);
      if (d.kind === "resize") {
        setPicked(new Set([k]));
        return setDrag(d);
      }
      // Shift-click adds to the selection or takes one out (no drag).
      if (e.shiftKey) {
        const next = new Set(picked);
        if (next.has(k)) next.delete(k);
        else next.add(k);
        return setPicked(next);
      }
      // Grabbing a selected placement drags the whole selection; another
      // one is selected alone.
      const keys = picked.has(k) ? picked : new Set([k]);
      setPicked(keys);
      const group = [...keys].flatMap((key) => {
        const { id, start } = unpick(key);
        const p = placementAt(id, start);
        return p ? [{ id, p }] : [];
      });
      setDrag({ ...d, group: group.length ? group : [{ id: d.id, p: d.p }] });
    },
    move: (e) => {
      const d = drag && follow(drag, e);
      if (d) setDrag(d);
    },
    cancel: () => setDrag(null),
    open: (id, clip) => {
      select(id);
      setRoll(id);
      if (tracks?.find((t) => t.instrument === id)?.selected !== clip) void act(client.call("clip.select", { instrument: id, clip }));
    },
    up: (e) => {
      if (!drag) return;
      setDrag(null);
      // Where the pointer let go, even if its last move has not drawn yet.
      const d = follow(drag, e);
      const { id } = d;
      if (d.kind === "move" || d.kind === "copy") {
        const dt = groupDt(d);
        if (d.to !== d.id) return toTrack(d, dt);
        // A click selects only the clicked one (grabbing it kept the rest
        // in case of a drag).
        if (dt === 0) return setPicked(new Set([pickKey(d.id, d.p.start)]));
        // Moves go furthest along first, so none lands on (and cuts) a
        // selected one that has not moved yet. Copies lay over whatever is
        // there, the selection's originals included, as any place does.
        const group = [...d.group].sort((a, b) => (dt > 0 ? b.p.start - a.p.start : a.p.start - b.p.start));
        const requests = group.map(({ id, p }) =>
          d.kind === "move"
            ? { method: "song.move", params: { instrument: id, start: p.start, to: p.start + dt } }
            : place(id, { clip: p.clip, start: p.start + dt, length: p.length, offset: p.offset }),
        );
        setPicked(new Set(group.map(({ id, p }) => pickKey(id, p.start + dt))));
        void send(requests);
      } else if (d.kind === "resize") {
        const length = resized(d);
        if (length === d.p.length) return;
        const p = place(id, { clip: d.p.clip, start: d.p.start, length, offset: d.p.offset });
        // Placing over it at the same start replaces it, but a shorter one
        // would leave its tail playing: lift it off first (one batch, one
        // undo step). `batch` does not roll back, but this place cannot
        // fail where the remove succeeded: same clip and start, shorter.
        if (length > d.p.length) void act(client.call("song.place", p.params));
        else void act(client.call("batch", { requests: [{ method: "song.remove", params: { instrument: id, start: d.p.start } }, p] }));
      } else if (d.start !== null) {
        setPicked(new Set([pickKey(id, d.start)]));
        void act(client.call("song.place", place(id, { clip: d.clip, start: d.start, length: null, offset: null }).params));
      }
    },
  };

  /** Loop the selected placements: the brace over the whole bars they
   * cover, turned on (one `batch` of the `param.set`s, one undo step). */
  const loopSelection = () => {
    const ps = [...picked].flatMap((k) => {
      const { id, start } = unpick(k);
      const p = placementAt(id, start);
      return p ? [p] : [];
    });
    if (!ps.length) return;
    const from = Math.floor(Math.min(...ps.map((p) => p.start)) / BAR);
    const to = Math.ceil(Math.max(...ps.map((p) => p.start + p.length)) / BAR);
    const set = (path: string, value: number) => ({ method: "param.set", params: { path, value } });
    void act(client.call("batch", { requests: [set("song.loop_start", from), set("song.loop_end", to), set("song.loop", 2)] }));
  };

  // Delete removes the selected placements (one undo step); Escape clears
  // the selection; L loops them.
  const key = (e: React.KeyboardEvent) => {
    const t = e.target as HTMLElement;
    // Only for the section itself (focused by a placement), not its buttons
    // and fields.
    if (!picked.size || t !== root.current) return;
    if (e.key === "Delete" || e.key === "Backspace") {
      // Not also the piano roll's selected notes.
      e.preventDefault();
      e.stopPropagation();
      const requests = [...picked].flatMap((k) => {
        const { id, start } = unpick(k);
        return placementAt(id, start) ? [{ method: "song.remove", params: { instrument: id, start } }] : [];
      });
      if (requests.length) void send(requests);
      none();
    } else if (e.key === "Escape") none();
    else if (e.key === "l" || e.key === "L") loopSelection();
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
      // The selection goes with the focus, so Delete elsewhere never
      // removes a placement picked long ago.
      onBlur={(e) => !root.current?.contains(e.relatedTarget as Node | null) && none()}
    >
      <div className="flex items-center gap-3 text-xs text-zinc-400">
        <span className="text-zinc-300">arrangement</span>
        <button
          className="px-2 py-0.5 rounded border border-zinc-700 text-zinc-300 hover:border-zinc-500 disabled:text-zinc-600 disabled:border-zinc-800"
          disabled={picked.size === 0}
          title="loop the selected placements: the loop brace over the bars they cover (L)"
          data-testid="arr-loop-selection"
          onClick={loopSelection}
        >
          loop
        </button>
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
        <span className="ml-auto text-zinc-500">ruler: play from there - top strip: drag a loop, click it on/off - drag: move (to another lane too) - edge: length - alt-drag: copy - Delete: remove - L: loop it - double-click: piano roll - drag a clip from the pool into its lane - pool triangle: launch</span>
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
            <LoopBrace px={px} bars={bars} loop={loop} loopStart={loopStart} loopEnd={loopEnd} locate={locateAt} />
            {Array.from({ length: bars }, (_, b) => (
              <span key={b} className="absolute pl-1 border-l border-zinc-700 bottom-0" style={{ left: b * BAR * px, top: BRACE + 2 }}>
                {b + 1}
              </span>
            ))}
            <div
              className="absolute bottom-0 w-0 h-0 border-x-[5px] border-x-transparent border-b-[7px] border-b-sky-400 -translate-x-1/2"
              style={{ left: start * px }}
              title={`play starts from bar ${start / BAR + 1}`}
              data-testid="arr-start"
              data-tick={start}
            />
          </div>
          <div className="relative touch-none select-none" ref={lanes} style={{ backgroundImage: grid }} onPointerDown={none}>
            {instruments.map((i) => (
              <Lane key={i.id} id={i.id} px={px} bars={bars} drag={drag} picked={picked} h={h} />
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
      {rolled && (
        // A click in the roll leaves the placement selection (its Delete
        // is for notes there).
        <div className="flex flex-col gap-1" data-testid="arr-roll" data-instrument={rolled.id} onPointerDownCapture={none}>
          <div className="flex items-center gap-2 text-xs text-zinc-400">
            <span>
              piano roll: <span className="text-zinc-200">{rolled.name}</span>
            </span>
            <button
              className="ml-auto px-2 py-0.5 rounded border border-zinc-700 text-zinc-300 hover:border-zinc-500"
              title="close the piano roll"
              data-testid="arr-roll-close"
              onClick={() => setRoll(null)}
            >
              close
            </button>
          </div>
          <PianoRoll id={rolled.id} drums={rolled.type === "tr808"} />
        </div>
      )}
    </section>
  );
}
