import { act, client, setParam, useApp } from "../store";
import { ParamKnob } from "./ParamKnob";

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
