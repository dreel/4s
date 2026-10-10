import type { RecordParams } from "../generated/RecordParams";
import { act, client, setParam, useApp } from "../store";
import { ParamKnob } from "./ParamKnob";

/** Record quantize grids in ticks (96 per quarter), as the protocol's `GRIDS`. */
const GRIDS: [string, number | null][] = [
  ["off", null],
  ["1/4", 96],
  ["1/8", 48],
  ["1/8t", 32],
  ["1/16", 24],
  ["1/16t", 16],
  ["1/32", 12],
];

function record(p: Partial<RecordParams>) {
  const none: RecordParams = { arm: null, instrument: null, mode: null, quantize: null, strength: null, count_in: null, offset_ms: null };
  return act(client.call("transport.record", { ...none, ...p }));
}

const selectCls = "mt-0.5 px-1 py-1 rounded bg-zinc-900 border border-zinc-700 text-xs text-zinc-200";

/** Record (RFC 0008): arm/end a take into your focus, the metronome, and the
 * settings the next take uses. */
function RecordControls() {
  const r = useApp((s) => s.snapshot?.record);
  const metronome = useApp((s) => (s.snapshot?.params["metronome.on"] ?? 0) >= 0.5);
  if (!r) return null;
  return (
    <div className="flex items-end gap-3" data-testid="record-controls">
      <button
        data-testid="record-toggle"
        data-recording={r.recording}
        title={r.recording ? `recording into ${r.instrument}; click to end the take` : "record into your focus (plays after the count-in)"}
        onClick={() => void record({ arm: !r.recording })}
        className={`h-10 px-3 rounded font-semibold text-sm border flex items-center gap-2 ${
          r.recording ? "bg-red-600 text-white border-red-500" : "bg-zinc-900 border-zinc-700 hover:border-zinc-500"
        }`}
      >
        <span className={`w-2.5 h-2.5 rounded-full ${r.recording ? "bg-white animate-pulse" : "bg-red-500"}`} />
        {r.recording ? `Rec ${r.instrument}` : "Rec"}
      </button>
      <button
        data-testid="metronome-toggle"
        data-on={metronome}
        title="metronome (it always clicks during a count-in)"
        onClick={() => void setParam("metronome.on", metronome ? 0 : 1)}
        className={`h-10 px-3 rounded text-xs border ${
          metronome ? "bg-sky-500 text-zinc-950 border-sky-400" : "bg-zinc-900 border-zinc-700 hover:border-zinc-500"
        }`}
      >
        click
      </button>
      <ParamKnob path="metronome.level" label="click" format={(v) => `${Math.round(v * 100)}%`} />
      <label className="flex flex-col text-[10px] text-zinc-500">
        mode
        <select
          className={selectCls}
          value={r.mode}
          data-testid="record-mode"
          onChange={(e) => void record({ mode: e.target.value as RecordParams["mode"] })}
        >
          <option value="overdub">overdub</option>
          <option value="replace">replace</option>
        </select>
      </label>
      <label className="flex flex-col text-[10px] text-zinc-500" title="record quantize: move played notes toward this grid">
        quantize
        <select
          className={selectCls}
          value={r.quantize ?? 0}
          data-testid="record-quantize"
          onChange={(e) => void record({ quantize: Number(e.target.value) })}
        >
          {GRIDS.map(([name, ticks]) => (
            <option key={name} value={ticks ?? 0}>
              {name}
            </option>
          ))}
        </select>
      </label>
      <label className="flex flex-col text-[10px] text-zinc-500" title="how far notes move toward the grid">
        strength
        <select
          className={selectCls}
          value={Math.round(r.strength * 100)}
          disabled={r.quantize === null}
          data-testid="record-strength"
          onChange={(e) => void record({ strength: Number(e.target.value) / 100 })}
        >
          {[...new Set([25, 50, 75, 100, Math.round(r.strength * 100)])].sort((a, b) => a - b).map((p) => (
            <option key={p} value={p}>
              {p}%
            </option>
          ))}
        </select>
      </label>
      <label className="flex flex-col text-[10px] text-zinc-500">
        count-in
        <select
          className={selectCls}
          value={r.count_in}
          data-testid="record-count-in"
          onChange={(e) => void record({ count_in: Number(e.target.value) })}
        >
          {[0, 1, 2, 4].map((n) => (
            <option key={n} value={n}>
              {n === 0 ? "none" : `${n} bar${n === 1 ? "" : "s"}`}
            </option>
          ))}
        </select>
      </label>
    </div>
  );
}

export function Transport() {
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const step = useApp((s) => s.snapshot?.transport.step ?? null);
  const tempo = useApp((s) => s.snapshot?.params["transport.tempo"] ?? 120);
  const length = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  return (
    <div className="flex items-center gap-5" data-testid="transport">
      <button
        data-testid="transport-toggle"
        data-playing={playing}
        onClick={() => void act(client.call(playing ? "transport.stop" : "transport.play", {}))}
        className={`w-24 h-10 rounded font-semibold text-sm border ${
          playing ? "bg-emerald-500 text-zinc-950 border-emerald-400" : "bg-zinc-900 border-zinc-700 hover:border-zinc-500"
        }`}
      >
        {playing ? "Stop" : "Play"}
      </button>
      <RecordControls />
      <div className="w-16 text-center">
        <div className="text-[10px] text-zinc-500">step</div>
        <div className="text-lg tabular-nums" data-testid="playhead">
          {playing && step !== null ? step + 1 : "-"}
        </div>
      </div>
      <label className="flex flex-col text-[10px] text-zinc-500">
        tempo (bpm)
        <input
          data-testid="tempo-input"
          type="number"
          min={20}
          max={300}
          step={1}
          value={Math.round(tempo * 10) / 10}
          onChange={(e) => {
            const v = Number(e.target.value);
            if (Number.isFinite(v) && v > 0) void setParam("transport.tempo", v);
          }}
          className="w-20 mt-0.5 px-2 py-1 rounded bg-zinc-900 border border-zinc-700 text-sm text-zinc-200"
        />
      </label>
      <ParamKnob path="transport.tempo" label="tempo" format={(v) => v.toFixed(0)} />
      <ParamKnob path="transport.swing" label="swing" format={(v) => `${Math.round(50 + v * 25)}%`} />
      <label className="flex flex-col text-[10px] text-zinc-500">
        length (steps)
        <div className="flex gap-1 mt-0.5">
          {[8, 16, 32, 64].map((n) => (
            <button
              key={n}
              data-testid={`length-${n}`}
              onClick={() => void setParam("sequencer.length", n)}
              className={`px-2 py-1 rounded text-xs border ${
                length === n ? "bg-amber-500 text-zinc-950 border-amber-400" : "bg-zinc-900 border-zinc-700 hover:border-zinc-500"
              }`}
            >
              {n}
            </button>
          ))}
        </div>
      </label>
    </div>
  );
}
