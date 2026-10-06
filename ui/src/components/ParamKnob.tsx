// A knob bound to a daemon parameter. Range, label default, and reset value
// come from the parameter registry, never duplicated in the UI.

import type { ParamInfo } from "../generated/ParamInfo";
import { setParam, useApp } from "../store";
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
  const info = useApp((s) => s.registry.find((p) => p.path === path));
  const [min, max] = range(info);
  return (
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
    />
  );
}

export const pct = (v: number) => `${Math.round(v * 100)}%`;
export const st = (v: number) => `${v > 0 ? "+" : ""}${v.toFixed(1)}`;
export const pan = (v: number) => (Math.abs(v) < 0.01 ? "C" : v < 0 ? `L${Math.round(-v * 100)}` : `R${Math.round(v * 100)}`);
