// Editor for a `poly` synth: the step view (a row per pitch, chords as
// several notes on a step), then its knobs in three rows, oscillators,
// filter, and amp, as its knob pages group them. Ranges and defaults come
// from the registry.

import { act, client, setParam, useApp } from "../store";
import { StepView } from "./StepView";
import { ParamKnob, pct, st } from "./ParamKnob";

const WAVES = ["saw", "square", "triangle", "sine"];

function Wave({ path, label }: { path: string; label: string }) {
  const value = useApp((s) => Math.round(s.snapshot?.params[path] ?? 0));
  return (
    <label className="flex flex-col items-center gap-1 text-[10px] text-zinc-500">
      <select
        className="px-1 py-0.5 rounded-sm bg-zinc-950 border border-zinc-700 text-xs text-zinc-300"
        value={value}
        data-testid={`wave-${path}`}
        onChange={(e) => void setParam(path, Number(e.target.value))}
      >
        {WAVES.map((w, i) => (
          <option key={w} value={i}>
            {w}
          </option>
        ))}
      </select>
      <span>{label}</span>
    </label>
  );
}

const ct = (v: number) => `${v > 0 ? "+" : ""}${Math.round(v)}`;

export function SynthEditor({ id }: { id: string }) {
  const k = (name: string, label: string, format = pct) => <ParamKnob path={`${id}.${name}`} label={label} format={format} />;
  const row = "flex flex-wrap items-center gap-4 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800";
  return (
    <div className="flex flex-col gap-3" data-testid={`synth-editor-${id}`}>
      <StepView id={id} />
      <section className={row} data-testid={`synth-osc-${id}`}>
        <span className="w-12 text-xs text-zinc-400">osc</span>
        <Wave path={`${id}.osc1_wave`} label="osc 1" />
        <Wave path={`${id}.osc2_wave`} label="osc 2" />
        {k("osc2_tune", "tune 2", st)}
        {k("osc2_fine", "fine 2", ct)}
        {k("osc2_level", "level 2")}
        {k("noise", "noise")}
        {k("detune", "detune")}
        {k("width", "width")}
      </section>
      <section className={row} data-testid={`synth-filter-${id}`}>
        <span className="w-12 text-xs text-zinc-400">filter</span>
        {k("cutoff", "cutoff")}
        {k("resonance", "reso")}
        {k("env_amount", "env")}
        {k("f_attack", "attack")}
        {k("f_decay", "decay")}
        {k("f_sustain", "sustain")}
        {k("f_release", "release")}
        {k("lfo_cutoff", "lfo")}
      </section>
      <section className={row} data-testid={`synth-amp-${id}`}>
        <span className="w-12 text-xs text-zinc-400">amp</span>
        {k("attack", "attack")}
        {k("decay", "decay")}
        {k("sustain", "sustain")}
        {k("release", "release")}
        {k("velocity", "velocity")}
        {k("level", "level")}
        {k("lfo_rate", "lfo rate")}
        {k("vibrato", "vibrato")}
        <button
          className="px-2 py-1 rounded border border-zinc-700 hover:border-zinc-500 bg-zinc-900 text-xs"
          data-testid="synth-audition"
          title="Play a C major chord"
          onClick={() => {
            for (const note of [60, 64, 67]) void act(client.call("voice.trigger", { instrument: id, voice: null, note, velocity: null }));
          }}
        >
          audition chord
        </button>
      </section>
    </div>
  );
}
