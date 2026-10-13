// A knob bound to a daemon parameter. Range, label default, and reset value
// come from the parameter registry, never duplicated in the UI.

import type { ParamInfo } from "../generated/ParamInfo";
import { act, app, client, setParam, useApp, useLive } from "../store";
import { Knob } from "./controls";

function range(info: ParamInfo | undefined): [number, number] {
  if (!info) return [0, 1];
  if (info.kind === "continuous" || info.kind === "integer") return [info.min, info.max];
  return [0, 1];
}

export function ParamKnob({
  path,
  label,
  format,
  size,
}: {
  path: string;
  label: string;
  format?: (v: number) => string;
  size?: number;
}) {
  const value = useApp((s) => s.snapshot?.params[path] ?? 0);
  const moving = useLive((s) => s.moving[path]);
  const info = useApp((s) => s.registry.find((p) => p.path === path));
  const mapping = useApp((s) => s.mapping);
  const routed = useApp((s) => s.snapshot?.graph.mod_routes.some((r) => r.target === path) ?? false);
  const [min, max] = range(info);
  // Map mode (RFC 0010): a press on a continuous knob routes the waiting
  // modulator output to it instead of turning it.
  const mappable = mapping !== null && info?.kind === "continuous" && !path.startsWith("transport.");
  const knob = (
    <Knob
      label={label}
      value={value}
      min={min}
      max={max}
      defaultValue={info?.default ?? min}
      format={format}
      onChange={(v) => void setParam(path, info?.kind === "integer" ? Math.round(v) : v)}
      testId={`knob-${path}`}
      size={size}
      moving={moving}
    />
  );
  return (
    <div
      className={`rounded ${mappable ? "outline outline-1 outline-sky-400 cursor-crosshair" : ""} ${routed ? "shadow-[0_0_0_1px_rgba(56,189,248,0.5)]" : ""}`}
      data-routed={routed}
      onPointerDownCapture={
        mappable
          ? (e) => {
              e.stopPropagation();
              e.preventDefault();
              const source = mapping!;
              app.set({ mapping: null });
              void act(client.call("mod.route", { source, target: path, depth: 0.5 }));
            }
          : undefined
      }
    >
      {knob}
    </div>
  );
}

export const pct = (v: number) => `${Math.round(v * 100)}%`;
export const st = (v: number) => `${v > 0 ? "+" : ""}${v.toFixed(1)}`;
export const pan = (v: number) => (Math.abs(v) < 0.01 ? "C" : v < 0 ? `L${Math.round(-v * 100)}` : `R${Math.round(v * 100)}`);
