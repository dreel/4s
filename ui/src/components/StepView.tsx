// The step view (RFC 0009 phase 2): any instrument's selected clip as a grid
// of steps, laid out by the instrument's declared note layout instead of
// per instrument type. A drum machine's named notes are rows (the panel can
// put its own controls beside each row); a monophonic instrument gets one
// column per step with a note and a toggle per declared expression (the
// 303's accent and slide); other pitched instruments (the synth) get a row
// per pitch, covering every note in the clip.
// Steps are a resolution (1/16 by default) over the clip's length. Every
// edit is one `step.set` (one undo step); the daemon decides, the view only
// draws what the clip's events say through the layout.

import { useState, type ReactNode } from "react";
import type { ClipEvent } from "../generated/ClipEvent";
import type { NoteExpression } from "../generated/NoteExpression";
import type { NoteLayout } from "../generated/NoteLayout";
import type { NoteRow } from "../generated/NoteRow";
import { act, client, useApp, useSelectedClipTick } from "../store";
import { AddLane } from "./AutomationLanes";
import { Toggle } from "./controls";
import { StepLaneLabel, useStepLanes } from "./StepLanes";

const NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
export const noteName = (n: number) => `${NAMES[n % 12]}${Math.floor(n / 12) - 1}`;

/** Resolutions in ticks a step (96 a quarter). */
const RESOLUTIONS: [string, number][] = [
  ["1/8", 48],
  ["1/16", 24],
  ["1/16t", 16],
  ["1/32", 12],
];

/** Pitches offered by default: C1..C4, within the instrument's range. */
const DEFAULT_LOW = 24;
const DEFAULT_HIGH = 60;
/** Rows a polyphonic instrument shows by default: C3 to C5. */
const CHORD_LOW = 48;
const CHORD_HIGH = 72;

// The reading below mirrors `crates/protocol/src/layout.rs` (`NoteLayout::has`,
// `shows`, `step_cells`); change them together.

/** Whether an expression is on in an event, as the layout declares it. */
function has(x: NoteExpression, e: ClipEvent, res: number) {
  return x.kind === "velocity" ? e.velocity >= x.min : e.len > res;
}

/** Whether the layout shows a note (a named row, or within its range). */
function shows(layout: NoteLayout, note: number) {
  return layout.rows.length ? layout.rows.some((r) => r.note === note) : note >= layout.low && note <= layout.high;
}

type Edit = { step: number; note: number | null; on: boolean; expressions?: Record<string, boolean> };

export function StepView({ id, rowExtras }: { id: string; rowExtras?: (row: NoteRow) => ReactNode }) {
  const layout = useApp((s) => s.snapshot?.graph.instruments.find((i) => i.id === id)?.layout);
  const selected = useApp((s) => s.snapshot?.tracks.find((t) => t.instrument === id)?.selected);
  const clip = useApp((s) => s.snapshot?.clips.find((c) => c.instrument === id && c.id === selected));
  const tick = useSelectedClipTick(id);
  const [res, setRes] = useState(24);
  const lanes = useStepLanes(clip, res);
  if (!layout || !clip) return null;
  const steps = Math.min(Math.ceil(clip.length / res), 256);
  const head = tick === null ? null : Math.floor(tick / res);
  // Each step's notes on its first tick.
  const at = new Map<number, ClipEvent[]>();
  for (const e of clip.events) {
    if (e.tick % res !== 0 || !shows(layout, e.note)) continue;
    const s = e.tick / res;
    at.set(s, [...(at.get(s) ?? []), e]);
  }
  const edit = (p: Edit) =>
    void act(
      client.call("step.set", {
        instrument: id,
        clip: clip.id,
        resolution: res,
        step: p.step,
        note: p.note,
        on: p.on,
        expressions: p.expressions ?? {},
      }),
    );
  const first = layout.expressions[0];
  const hint = layout.mono
    ? layout.expressions.map((x) => `${x.label[0]}: ${x.label.toLowerCase()}`).join(" - ")
    : `click: on/off${first ? ` - shift/right-click: ${first.label.toLowerCase()}` : ""}`;

  /** A grid cell: click toggles the note; shift or right-click toggles the
   * first expression (turning the note on with it if it was off). */
  const cell = (s: number, note: number, testId: string) => {
    const e = at.get(s)?.find((x) => x.note === note);
    const level = !e ? 0 : first && has(first, e, res) ? 2 : 1;
    const group = Math.floor(s / 4) % 2 === 0;
    const base =
      level === 2
        ? "bg-amber-300 shadow-[inset_0_0_0_2px_rgba(255,255,255,0.6)]"
        : level === 1
          ? "bg-amber-600"
          : group
            ? "bg-zinc-800"
            : "bg-zinc-800/60";
    const toggleFirst = () => first && edit({ step: s, note, on: true, expressions: { [first.id]: level !== 2 } });
    return (
      <button
        key={s}
        data-testid={testId}
        data-level={level}
        className={`h-8 flex-1 min-w-3 rounded-sm ${base} ${head === s ? "ring-2 ring-zinc-100" : ""} hover:brightness-125 ${
          s % 4 === 0 && s > 0 ? "ml-1" : ""
        }`}
        onClick={(ev) => (ev.shiftKey ? toggleFirst() : edit({ step: s, note, on: !e }))}
        onContextMenu={(ev) => {
          ev.preventDefault();
          toggleFirst();
        }}
      />
    );
  };

  const header = (
    <div className="flex items-center justify-between gap-2 text-xs text-zinc-500 mb-1">
      <span>Steps</span>
      <span className="ml-auto">{hint}</span>
      <AddLane clip={clip} testId={`step-lane-add-${id}`} />
      <select
        className="px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-zinc-200"
        value={res}
        title="step resolution"
        data-testid={`step-res-${id}`}
        onChange={(e) => setRes(Number(e.target.value))}
      >
        {RESOLUTIONS.map(([n, t]) => (
          <option key={n} value={t}>
            {n}
          </option>
        ))}
      </select>
    </div>
  );

  // Lane rows under a grid: a label, then a bar per step, spaced as the
  // steps are.
  const laneRows = clip.lanes.map((l) => (
    <div key={l.path} className="contents" data-testid={`step-lane-${id}-${l.path}`}>
      <StepLaneLabel clip={clip} lane={l} className="w-24" />
      <div className="flex gap-1 min-w-0">
        {Array.from({ length: steps }, (_, s) => lanes.cell(clip, l, s, `flex-1 min-w-3 ${s % 4 === 0 && s > 0 ? "ml-1" : ""}`))}
      </div>
    </div>
  ));

  // Named rows (a drum machine's voices), each with the panel's controls.
  if (layout.rows.length) {
    return (
      <section className="flex-1 min-w-0 flex flex-col gap-1.5 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid={`step-view-${id}`}>
        {header}
        {/* Two columns, so lane rows line up whatever sits beside a row. */}
        <div className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-1.5 items-center">
          {layout.rows.map((r) => (
            <div key={r.note} className="contents" data-testid={`row-${r.id}`}>
              <div className="flex items-center gap-2">{rowExtras ? rowExtras(r) : <span className="w-24 text-xs text-zinc-300 truncate">{r.name}</span>}</div>
              <div className="flex gap-1 min-w-0">{Array.from({ length: steps }, (_, s) => cell(s, r.note, `step-${r.id}-${s}`))}</div>
            </div>
          ))}
          {laneRows}
        </div>
      </section>
    );
  }

  // A monophonic instrument: a column per step, its note and expressions.
  const low = Math.max(layout.low, DEFAULT_LOW);
  const high = Math.min(layout.high, DEFAULT_HIGH);
  if (layout.mono) {
    return (
      <section className="flex flex-col gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid={`step-view-${id}`}>
        {header}
        {clip.lanes.length > 0 && (
          <div className="flex flex-wrap gap-3" title="each step's bars, top to bottom">
            {clip.lanes.map((l) => (
              <StepLaneLabel key={l.path} clip={clip} lane={l} className="" />
            ))}
          </div>
        )}
        <div className="flex gap-1 overflow-x-auto">
          {Array.from({ length: steps }, (_, i) => {
            const e = (at.get(i) ?? []).reduce<ClipEvent | null>((lo, x) => (!lo || x.note < lo.note ? x : lo), null);
            const notes = Array.from({ length: high - low + 1 }, (_, k) => low + k);
            if (e && !notes.includes(e.note)) notes.push(e.note);
            return (
              <div
                key={i}
                className={`flex flex-col items-center gap-1 p-1 rounded ${e ? "bg-amber-900/40" : "bg-zinc-900"} ${
                  head === i ? "ring-2 ring-zinc-100" : ""
                } ${i % 4 === 0 && i > 0 ? "ml-1" : ""}`}
                data-testid={`note-step-${i}`}
              >
                <div className="text-[9px] text-zinc-500 tabular-nums">{i + 1}</div>
                <select
                  className="w-12 px-0.5 py-0.5 rounded bg-zinc-950 border border-zinc-700 text-[10px] text-zinc-200"
                  value={e?.note ?? ""}
                  data-testid={`note-${i}`}
                  onChange={(ev) =>
                    ev.target.value === "" ? edit({ step: i, note: null, on: false }) : edit({ step: i, note: Number(ev.target.value), on: true })
                  }
                >
                  <option value="">-</option>
                  {notes.map((n) => (
                    <option key={n} value={n}>
                      {noteName(n)}
                    </option>
                  ))}
                </select>
                {clip.lanes.map((l) => lanes.cell(clip, l, i, "w-12"))}
                {layout.expressions.map((x, k) => {
                  const on = !!e && has(x, e, res);
                  return (
                    <Toggle
                      key={x.id}
                      label={x.label[0]}
                      on={on}
                      // A rest has no note to carry it.
                      onClick={() => e && edit({ step: i, note: e.note, on: true, expressions: { [x.id]: !on } })}
                      activeClass={k === 0 ? undefined : "bg-sky-500 text-zinc-950 border-sky-400"}
                      testId={`${x.id}-${i}`}
                      title={x.label}
                    />
                  );
                })}
              </div>
            );
          })}
        </div>
      </section>
    );
  }

  // Other pitched instruments: a row per pitch, highest first, C3 to C5
  // (where chords sit) widened to every note the clip has.
  const notes = clip.events.map((e) => e.note).filter((n) => n >= layout.low && n <= layout.high);
  const rowLow = Math.max(layout.low, Math.min(CHORD_LOW, ...notes));
  const rowHigh = Math.min(layout.high, Math.max(CHORD_HIGH, ...notes));
  return (
    <section className="flex flex-col gap-1 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid={`step-view-${id}`}>
      {header}
      <div className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-1 items-center">
        {Array.from({ length: rowHigh - rowLow + 1 }, (_, k) => rowHigh - k).map((n) => (
          <div key={n} className="contents">
            <span className="w-10 text-[10px] text-zinc-400">{noteName(n)}</span>
            <div className="flex gap-1 min-w-0">{Array.from({ length: steps }, (_, s) => cell(s, n, `step-${n}-${s}`))}</div>
          </div>
        ))}
        {laneRows}
      </div>
    </section>
  );
}
