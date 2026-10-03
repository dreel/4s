// Mixer strips: per-voice sound macros (tune/decay/tone), pan, volume, meter.
// All knobs are generated from the daemon's parameter registry.

import type { ParamInfo } from "../generated/ParamInfo";
import { setParam, useApp, useLive } from "../store";
import { Fader, Knob, Meter } from "./controls";
import { voiceShort } from "./voices";

function range(info: ParamInfo | undefined): [number, number] {
  if (!info) return [0, 1];
  if (info.kind === "continuous" || info.kind === "integer") return [info.min, info.max];
  return [0, 1];
}

export function ParamKnob({ path, label, format }: { path: string; label: string; format?: (v: number) => string }) {
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
    />
  );
}

const pct = (v: number) => `${Math.round(v * 100)}%`;
const st = (v: number) => `${v > 0 ? "+" : ""}${v.toFixed(1)}`;
const pan = (v: number) => (Math.abs(v) < 0.01 ? "C" : v < 0 ? `L${Math.round(-v * 100)}` : `R${Math.round(v * 100)}`);

function TrackMeter({ index }: { index: number }) {
  const level = useLive((s) => s.tracks[index] ?? 0);
  return <Meter level={level} />;
}

function MasterMeters() {
  const l = useLive((s) => s.master[0] ?? 0);
  const r = useLive((s) => s.master[1] ?? 0);
  return (
    <div className="flex gap-0.5">
      <Meter level={l} />
      <Meter level={r} />
    </div>
  );
}

function Strip({ index }: { index: number }) {
  const voice = useApp((s) => s.snapshot!.pattern[index].voice);
  const ch = index + 1;
  const volume = useApp((s) => s.snapshot?.params[`mixer.${ch}.volume`] ?? 0);
  return (
    <div className="flex flex-col items-center gap-2 p-2 rounded bg-zinc-900 border border-zinc-800" data-testid={`strip-${ch}`}>
      <div className="text-xs font-semibold text-zinc-300">
        {ch} <span className="text-amber-400">{voiceShort(voice)}</span>
      </div>
      <ParamKnob path={`drums.${voice}.tune`} label="tune" format={st} />
      <ParamKnob path={`drums.${voice}.decay`} label="decay" format={pct} />
      <ParamKnob path={`drums.${voice}.tone`} label="tone" format={pct} />
      <ParamKnob path={`mixer.${ch}.pan`} label="pan" format={pan} />
      <div className="flex items-end gap-1">
        <Fader value={volume} onChange={(v) => void setParam(`mixer.${ch}.volume`, v)} testId={`fader-${ch}`} />
        <TrackMeter index={index} />
      </div>
      <div className="text-[10px] text-zinc-400 tabular-nums" data-testid={`volume-${ch}`}>
        {pct(volume)}
      </div>
    </div>
  );
}

export function Mixer() {
  const tracks = useApp((s) => s.snapshot?.pattern.length ?? 0);
  const master = useApp((s) => s.snapshot?.params["mixer.master.volume"] ?? 0);
  return (
    <section className="flex gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800 overflow-x-auto">
      {Array.from({ length: tracks }, (_, i) => (
        <Strip key={i} index={i} />
      ))}
      <div className="flex flex-col items-center justify-end gap-2 p-2 rounded bg-zinc-900 border border-zinc-700 min-w-20">
        <div className="text-xs font-semibold text-zinc-300">Master</div>
        <div className="flex items-end gap-1">
          <Fader value={master} onChange={(v) => void setParam("mixer.master.volume", v)} testId="fader-master" height={180} />
          <MasterMeters />
        </div>
        <div className="text-[10px] text-zinc-400 tabular-nums">{pct(master)}</div>
      </div>
    </section>
  );
}
