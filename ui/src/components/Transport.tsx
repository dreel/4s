import type { RecordParams } from "../generated/RecordParams";
import { act, app, client, setParam, useApp } from "../store";
import { ParamKnob } from "./ParamKnob";

type AppSnapshot = typeof app.state;

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

/** Ticks as bar.beat, from 1 (96 ticks a beat, 4 beats a bar). */
export function barBeat(tick: number) {
  return `${Math.floor(tick / 384) + 1}.${Math.floor((tick % 384) / 96) + 1}`;
}

/** Launch quantize choices (`transport.launch_quantize`). */
const QUANTIZE = ["none", "1 beat", "1 bar", "2 bars", "4 bars"];

/** The song (RFC 0008, RFC 0009): play always plays the arrangement; what it
 * loops, where it plays from, where clip launches land, and back to the
 * arrangement for every track playing a launched clip. */
function SongControls() {
  const p = (k: string) => (s: AppSnapshot) => s.snapshot?.params[k] ?? 0;
  const quantize = useApp(p("transport.launch_quantize"));
  const launched = useApp((s) => (s.snapshot?.tracks ?? []).filter((t) => t.launched !== null).length);
  const loop = useApp(p("song.loop"));
  const loopStart = useApp(p("song.loop_start"));
  const loopEnd = useApp(p("song.loop_end"));
  const start = useApp((s) => s.snapshot?.transport.start ?? 0);
  const num = "w-12 mt-0.5 px-1 py-1 rounded bg-zinc-900 border border-zinc-700 text-xs text-zinc-200";
  return (
    <div className="flex items-end gap-2" data-testid="song-controls">
      <button
        data-testid="back-all"
        data-launched={launched}
        disabled={launched === 0}
        title={
          launched === 0
            ? "every track plays the arrangement"
            : `${launched} track${launched === 1 ? "" : "s"} play${launched === 1 ? "s" : ""} a launched clip; click to go back to the arrangement`
        }
        onClick={() => void act(client.call("song.back", { instrument: null, now: null }))}
        className={`h-10 px-3 rounded text-xs border ${
          launched > 0 ? "bg-emerald-500 text-zinc-950 border-emerald-400" : "bg-zinc-900 border-zinc-700 text-zinc-500"
        }`}
      >
        back to song
      </button>
      <label className="flex flex-col text-[10px] text-zinc-500" title="where clip launches and returns to the arrangement land while playing">
        launch
        <select
          className={selectCls}
          value={quantize}
          data-testid="launch-quantize"
          onChange={(e) => void setParam("transport.launch_quantize", Number(e.target.value))}
        >
          {QUANTIZE.map((q, i) => (
            <option key={q} value={i}>
              {q}
            </option>
          ))}
        </select>
      </label>
      <label className="flex flex-col text-[10px] text-zinc-500">
        loop
        <select className={selectCls} value={loop} data-testid="song-loop" onChange={(e) => void setParam("song.loop", Number(e.target.value))}>
          <option value={0}>off</option>
          <option value={1}>song</option>
          <option value={2}>bars</option>
        </select>
      </label>
      {loop === 2 && (
        <>
          <label className="flex flex-col text-[10px] text-zinc-500">
            from bar
            <input
              type="number"
              min={1}
              className={num}
              value={loopStart + 1}
              data-testid="loop-start"
              onChange={(e) => Number(e.target.value) >= 1 && void setParam("song.loop_start", Number(e.target.value) - 1)}
            />
          </label>
          <label className="flex flex-col text-[10px] text-zinc-500">
            to bar
            <input
              type="number"
              min={1}
              className={num}
              value={loopEnd}
              data-testid="loop-end"
              onChange={(e) => Number(e.target.value) >= 1 && void setParam("song.loop_end", Number(e.target.value))}
            />
          </label>
        </>
      )}
      <label className="flex flex-col text-[10px] text-zinc-500" title="where play starts from">
        start bar
        <input
          type="number"
          min={1}
          className={num}
          value={Math.floor(start / 384) + 1}
          data-testid="locate"
          onChange={(e) => Number(e.target.value) >= 1 && void act(client.call("transport.locate", { tick: (Number(e.target.value) - 1) * 384 }))}
        />
      </label>
    </div>
  );
}

export function Transport() {
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const tick = useApp((s) => s.snapshot?.transport.tick ?? null);
  const tempo = useApp((s) => s.snapshot?.params["transport.tempo"] ?? 120);
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
      <SongControls />
      <div className="w-16 text-center">
        <div className="text-[10px] text-zinc-500">bar</div>
        <div className="text-lg tabular-nums" data-testid="playhead" data-tick={tick ?? ""}>
          {!playing || tick === null ? "-" : barBeat(tick)}
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
    </div>
  );
}
