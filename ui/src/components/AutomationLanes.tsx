// Automation lanes in the piano roll (RFC 0010): each of a clip's lanes is
// a strip under the velocity lane, on the roll's time grid, drawing its
// parameter's value over the clip (linear between points). Click empty
// space to add a point (snapped in time), drag a point to move it, alt-click
// one to remove it. Every edit is one `clip.lane.set` (one undo step); the
// daemon checks and clamps, the view only draws.

import { useState } from "react";
import type { Clip } from "../generated/Clip";
import type { Lane } from "../generated/Lane";
import type { LanePoint } from "../generated/LanePoint";
import type { ParamInfo } from "../generated/ParamInfo";
import { act, client, useApp } from "../store";

export const LANE_H = 48;

/** A parameter's range (from the registry); 0..1 if unknown. */
function rangeOf(registry: ParamInfo[], path: string): [number, number] {
  const p = registry.find((x) => x.path === path);
  return p && p.kind !== "toggle" ? [p.min, p.max] : [0, 1];
}

/** A lane's parameter range. */
function useRange(path: string): [number, number] {
  const min = useApp((s) => rangeOf(s.registry, path)[0]);
  const max = useApp((s) => rangeOf(s.registry, path)[1]);
  return [min, max];
}

/** Parameters a lane can take: continuous and whole-number, not the
 * transport's. */
export function useAutomatable(): string[] {
  const registry = useApp((s) => s.registry);
  return registry.filter((p) => p.kind !== "toggle" && !p.path.startsWith("transport.")).map((p) => p.path);
}

/** Set a lane's points (one undo step), or remove it with none left. */
function setLane(clip: Clip, path: string, points: LanePoint[]) {
  if (!points.length) return act(client.call("clip.lane.remove", { instrument: clip.instrument, clip: clip.id, path }));
  return act(client.call("clip.lane.set", { instrument: clip.instrument, clip: clip.id, path, points }));
}

/** The label beside a lane: its parameter and a remove button. */
export function LaneLabel({ clip, lane }: { clip: Clip; lane: Lane }) {
  return (
    <div className="flex items-start gap-0.5 px-1 pt-1 text-[9px] text-sky-300 border-t border-zinc-800" style={{ height: LANE_H }} title={lane.path}>
      <span className="truncate flex-1">{lane.path.split(".").pop()}</span>
      <button
        className="text-zinc-600 hover:text-red-400"
        title="remove the lane"
        data-testid={`lane-remove-${lane.path}`}
        onClick={() => void act(client.call("clip.lane.remove", { instrument: clip.instrument, clip: clip.id, path: lane.path }))}
      >
        x
      </button>
    </div>
  );
}

type Drag = { index: number; tick: number; value: number; x: number; y: number };

/** One lane's strip, `px` pixels per tick, snapping to `snap` ticks. */
export function LaneStrip({ clip, lane, px, snap, width }: { clip: Clip; lane: Lane; px: number; snap: number; width: number }) {
  const [min, max] = useRange(lane.path);
  const hand = useApp((s) => s.snapshot?.params[lane.path]);
  const [drag, setDrag] = useState<Drag | null>(null);
  const span = max - min || 1;
  const yOf = (v: number) => (1 - (v - min) / span) * (LANE_H - 4) + 2;
  const valueAt = (y: number) => Math.min(max, Math.max(min, min + (1 - (y - 2) / (LANE_H - 4)) * span));
  const end = Math.min(clip.length, 64 * 24) - 1;
  const tickAt = (x: number) => Math.min(end, Math.max(0, Math.round(x / px / snap) * snap));
  const rel = (e: React.PointerEvent) => {
    const r = (e.currentTarget as HTMLElement).closest("[data-lane]")!.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  // Where the points are while dragging (the daemon gets them on release).
  const points = lane.points.map((p, i) => (drag && drag.index === i ? { tick: drag.tick, value: drag.value } : p));
  const sorted = [...points].sort((a, b) => a.tick - b.tick);
  const line = sorted.length
    ? [`0,${yOf(sorted[0].value)}`, ...sorted.map((p) => `${p.tick * px},${yOf(p.value)}`), `${clip.length * px},${yOf(sorted[sorted.length - 1].value)}`].join(" ")
    : "";
  const fmt = (v: number) => (Math.abs(v) >= 10 ? v.toFixed(1) : v.toFixed(2));

  return (
    <div
      className="relative border-t border-zinc-800 cursor-crosshair touch-none select-none"
      style={{ width, height: LANE_H }}
      data-lane={lane.path}
      data-testid={`lane-${lane.path}`}
      onPointerDown={(e) => {
        // Empty space: a new point there (replacing one at the same tick).
        const p = rel(e);
        const tick = tickAt(p.x);
        const next = [...lane.points.filter((q) => q.tick !== tick), { tick, value: valueAt(p.y) }];
        void setLane(clip, lane.path, next);
      }}
      onPointerMove={(e) => {
        if (!drag) return;
        const p = rel(e);
        setDrag({ ...drag, tick: tickAt(p.x), value: valueAt(p.y) });
      }}
      onPointerUp={() => {
        if (!drag) return;
        setDrag(null);
        const was = lane.points[drag.index];
        if (was.tick === drag.tick && was.value === drag.value) return;
        // A point dropped on another's tick replaces it.
        const next = lane.points.filter((q, i) => i !== drag.index && q.tick !== drag.tick);
        void setLane(clip, lane.path, [...next, { tick: drag.tick, value: drag.value }]);
      }}
    >
      <div className="absolute top-0 bottom-0 bg-zinc-950/70 pointer-events-none" style={{ left: clip.length * px, right: 0 }} />
      <svg className="absolute inset-0 pointer-events-none" width={width} height={LANE_H}>
        {hand !== undefined && <line x1={0} x2={width} y1={yOf(hand)} y2={yOf(hand)} stroke="#52525b" strokeDasharray="3 3" />}
        <polyline points={line} fill="none" stroke="#38bdf8" strokeWidth={1.5} />
      </svg>
      {points.map((p, i) => (
        <div
          key={i}
          className="absolute w-2 h-2 -ml-1 -mt-1 rounded-full bg-sky-400 border border-sky-200 cursor-move"
          style={{ left: p.tick * px, top: yOf(p.value) }}
          title={`tick ${p.tick}: ${fmt(p.value)} (alt-click: remove)`}
          data-testid={`lane-point-${lane.path}-${lane.points[i].tick}`}
          onPointerDown={(e) => {
            e.stopPropagation();
            if (e.altKey) {
              void setLane(clip, lane.path, lane.points.filter((_, k) => k !== i));
              return;
            }
            (e.currentTarget.parentElement as HTMLElement).setPointerCapture(e.pointerId);
            const r = rel(e);
            setDrag({ index: i, tick: p.tick, value: p.value, x: r.x, y: r.y });
          }}
        />
      ))}
    </div>
  );
}

/** Add a lane: pick a parameter; it starts flat at the value set by hand. */
export function AddLane({ clip }: { clip: Clip }) {
  const paths = useAutomatable();
  const params = useApp((s) => s.snapshot?.params);
  const [text, setText] = useState("");
  const taken = new Set(clip.lanes.map((l) => l.path));
  const add = () => {
    const path = text.trim();
    if (!path || taken.has(path)) return;
    setText("");
    void act(client.call("clip.lane.set", { instrument: clip.instrument, clip: clip.id, path, points: [{ tick: 0, value: params?.[path] ?? 0 }] }));
  };
  return (
    <label className="flex items-center gap-1" title="automate a parameter over this clip">
      <input
        className="w-36 px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-zinc-200"
        list={`lane-params-${clip.instrument}`}
        placeholder="+ lane (parameter)"
        value={text}
        disabled={clip.lanes.length >= 8}
        data-testid="lane-add"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => e.key === "Enter" && add()}
      />
      <datalist id={`lane-params-${clip.instrument}`}>
        {paths.filter((p) => !taken.has(p)).map((p) => (
          <option key={p} value={p} />
        ))}
      </datalist>
    </label>
  );
}
