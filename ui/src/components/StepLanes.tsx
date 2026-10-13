// Automation lanes in the step views (RFC 0010): each of the selected
// clip's lanes as a row of bars, one per step, under the step grid (or, for
// a monophonic instrument, a bar per lane in each step's column). A bar
// shows the lane's value at the step; press a step to set a point there
// (its height is the value), drag across steps to paint them, alt-click a
// step to remove its point. A paint is one `clip.lane.set` (one undo
// step); the daemon checks and clamps. The piano roll edits the same lanes
// at any tick (`AutomationLanes.tsx`).

import { useEffect, useRef, useState } from "react";
import type { Clip } from "../generated/Clip";
import type { Lane } from "../generated/Lane";
import { MAX_LANE_POINTS } from "../generated/limits";
import type { LanePoint } from "../generated/LanePoint";
import { act, client, useApp } from "../store";
import { rangeOf } from "./AutomationLanes";

/** A lane's value at a clip tick: linear between points, the nearest
 * point's outside them. Mirrors `LaneStore::value` in
 * `crates/engine/src/engine.rs`; change them together. */
export function laneValue(points: LanePoint[], tick: number): number | null {
  if (!points.length) return null;
  if (tick <= points[0].tick) return points[0].value;
  for (let i = 1; i < points.length; i++) {
    const [a, b] = [points[i - 1], points[i]];
    if (tick < b.tick) return a.value + ((b.value - a.value) * (tick - a.tick)) / Math.max(1, b.tick - a.tick);
  }
  return points[points.length - 1].value;
}

/** A paint in progress: the lane and the values set per step. */
type Paint = { path: string; values: Map<number, number> };

/** Bars for a clip's lanes in a step view of `res` ticks a step. */
export function useStepLanes(clip: Clip | undefined, res: number) {
  const registry = useApp((s) => s.registry);
  const [paint, setPaint] = useState<Paint | null>(null);
  const current = useRef<Paint | null>(null);
  current.current = paint;

  // A paint ends wherever the pointer is released.
  useEffect(() => {
    const up = () => {
      const p = current.current;
      if (!p) return;
      setPaint(null);
      const lane = clip?.lanes.find((l) => l.path === p.path);
      if (!clip || !lane) return;
      const ticks = new Set([...p.values.keys()].map((s) => s * res));
      const points = [
        ...lane.points.filter((q) => !ticks.has(q.tick)),
        ...[...p.values].map(([s, value]) => ({ tick: s * res, value })),
      ].sort((a, b) => a.tick - b.tick);
      void act(client.call("clip.lane.set", { instrument: clip.instrument, clip: clip.id, path: p.path, points }));
    };
    // A cancelled touch or pen drag drops the paint.
    const cancel = () => setPaint(null);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", cancel);
    return () => {
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", cancel);
    };
  }, [clip, res]);

  /** The bar for `lane` at step `s` (of `clip`, which is there by now). */
  const cell = (clip: Clip, lane: Lane, s: number, className: string) => {
    const [min, max] = rangeOf(registry, lane.path);
    const painted = paint?.path === lane.path ? paint.values.get(s) : undefined;
    const value = painted ?? laneValue(lane.points, s * res) ?? min;
    const point = painted !== undefined || lane.points.some((q) => q.tick === s * res);
    const fraction = Math.min(1, Math.max(0, (value - min) / (max - min || 1)));
    const valueAt = (e: React.PointerEvent) => {
      const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
      return min + (1 - Math.min(1, Math.max(0, (e.clientY - r.top) / r.height))) * (max - min);
    };
    const set = (e: React.PointerEvent) => {
      const values = new Map(paint?.path === lane.path ? paint.values : []);
      // Not past the points a lane holds: the daemon would refuse it all.
      const others = lane.points.filter((q) => q.tick % res !== 0 || !values.has(q.tick / res)).length;
      if (!values.has(s) && !lane.points.some((q) => q.tick === s * res) && others + values.size >= MAX_LANE_POINTS) return;
      values.set(s, valueAt(e));
      setPaint({ path: lane.path, values });
    };
    return (
      <div
        key={`${lane.path}-${s}`}
        className={`relative h-8 rounded-sm bg-zinc-900 cursor-ns-resize touch-none select-none ${className}`}
        title={`${lane.path} at step ${s + 1}: ${value.toFixed(2)}${point ? "" : " (between points)"} (alt-click: remove)`}
        data-testid={`lane-cell-${lane.path}-${s}`}
        data-point={point}
        onPointerDown={(e) => {
          e.preventDefault();
          if (e.altKey) {
            const points = lane.points.filter((q) => q.tick !== s * res);
            if (points.length === lane.points.length) return;
            void act(
              points.length
                ? client.call("clip.lane.set", { instrument: clip.instrument, clip: clip.id, path: lane.path, points })
                : client.call("clip.lane.remove", { instrument: clip.instrument, clip: clip.id, path: lane.path }),
            );
            return;
          }
          // Painting carries on into the steps the pointer enters.
          (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
          set(e);
        }}
        onPointerEnter={(e) => paint?.path === lane.path && e.buttons & 1 && set(e)}
      >
        <div
          className={`absolute bottom-0 left-0 right-0 rounded-sm ${point ? "bg-sky-400" : "bg-sky-800"}`}
          style={{ height: `${Math.max(2, fraction * 100)}%` }}
        />
      </div>
    );
  };
  return { cell };
}

/** A lane's label in a step view, with a remove button. */
export function StepLaneLabel({ clip, lane, className }: { clip: Clip; lane: Lane; className: string }) {
  return (
    <span className={`flex items-center gap-1 text-[10px] text-sky-300 truncate ${className}`} title={lane.path}>
      <span className="truncate">{lane.path}</span>
      <button
        className="text-zinc-600 hover:text-red-400"
        title="remove the lane"
        data-testid={`step-lane-remove-${lane.path}`}
        onClick={() => void act(client.call("clip.lane.remove", { instrument: clip.instrument, clip: clip.id, path: lane.path }))}
      >
        x
      </button>
    </span>
  );
}
