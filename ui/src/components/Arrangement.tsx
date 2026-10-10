// The arrangement view (RFC 0008 phase C): the song left to right, one lane
// per track (= instrument). Track headers select the instrument for the
// editor and arm it (the seat's focus, which routes MIDI input and sets the
// record target). The ruler shows bars, the loop range, the song's end, and
// where song mode plays from; click it to locate. Placements show their
// clip's name and a note preview, with looped repeats marked. This first
// step only draws and locates; editing placements comes next.

import { useMemo, useState } from "react";
import type { Clip } from "../generated/Clip";
import type { Placement } from "../generated/Placement";
import { act, app, client, mySeat, select, useApp, useSelected } from "../store";

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

/** A placement's clip drawn as notes, repeated where the placement loops it,
 * with each repeat's start marked. */
function NotePreview({ placement, clip, clipLength, px }: { placement: Placement; clip: Clip | undefined; clipLength: number; px: number }) {
  const width = placement.length * px;
  const height = LANE - 18;
  const notes = clip?.events ?? [];
  const lo = Math.min(...notes.map((e) => e.note));
  const hi = Math.max(...notes.map((e) => e.note));
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

function Lane({ id, px, bars }: { id: string; px: number; bars: number }) {
  const track = useApp((s) => s.snapshot?.tracks.find((t) => t.instrument === id));
  const clips = useApp((s) => s.snapshot?.clips);
  const stepsLength = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const tick = useApp((s) => s.snapshot?.transport.tick ?? null);
  if (!track) return null;
  return (
    <div className="relative border-b border-zinc-800" style={{ height: LANE, width: bars * BAR * px }} data-testid={`lane-${id}`}>
      {track.arrangement.map((p) => {
        const clip = clips?.find((c) => c.instrument === id && c.id === p.clip);
        const clipLength = clip?.length ?? stepsLength * STEP;
        const live = playing && tick !== null && p.start <= tick && tick < p.start + p.length;
        return (
          <div
            key={p.start}
            className={`absolute top-1 bottom-1 rounded border overflow-hidden text-violet-200 ${
              p.clip === track.selected ? "bg-violet-500/35 border-violet-300" : "bg-violet-500/20 border-violet-500/60"
            } ${live ? "ring-1 ring-zinc-100" : ""}`}
            style={{ left: p.start * px, width: Math.max(2, p.length * px) }}
            title={`${clip?.name ?? `clip ${p.clip}`}: bar ${p.start / BAR + 1}, ${p.length / BAR} bars${p.offset ? `, from tick ${p.offset}` : ""}`}
            data-testid={`placement-${id}-${p.start}`}
            data-clip={p.clip}
            data-length={p.length}
          >
            <div className="px-1 text-[10px] leading-4 truncate">{clip?.name ?? p.clip}</div>
            <NotePreview placement={p} clip={clip} clipLength={clipLength} px={px} />
          </div>
        );
      })}
    </div>
  );
}

function TrackHeader({ id, name, type, armed, selected, seated }: { id: string; name: string; type: string; armed: boolean; selected: boolean; seated: boolean }) {
  return (
    <div
      className={`flex items-center gap-1 px-2 border-b border-zinc-800 text-xs ${selected ? "bg-zinc-800" : ""}`}
      style={{ height: LANE }}
      data-testid={`track-header-${id}`}
    >
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
  );
}

export function Arrangement() {
  const instruments = useApp((s) => s.snapshot?.graph.instruments ?? []);
  const tracks = useApp((s) => s.snapshot?.tracks);
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
  const line = (every: number, color: string) => `repeating-linear-gradient(to right, ${color} 0 1px, transparent 1px ${every * px}px)`;
  const grid = [line(BAR, "#3f3f46"), ...(zoom >= 192 ? [line(BEAT, "#27272a")] : [])].join(", ");
  const sel = "px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-zinc-200";

  return (
    <section className="flex flex-col gap-2 p-3 min-w-0 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid="arrangement" data-end={end}>
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
        <span className="ml-auto text-zinc-500">click the ruler: play from there</span>
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
          <div className="relative" style={{ backgroundImage: grid }}>
            {instruments.map((i) => (
              <Lane key={i.id} id={i.id} px={px} bars={bars} />
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

/** Whether `app` has a snapshot with any instrument (for tests and callers). */
export const hasTracks = () => (app.state.snapshot?.graph.instruments.length ?? 0) > 0;
