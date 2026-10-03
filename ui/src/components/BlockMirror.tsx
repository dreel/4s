// Mirror of the Livid Block: LEDs as the daemon computes them, and a virtual
// grid + knobs that send the same RPCs a real Block's MIDI would trigger.

import type { KnobMode } from "../generated/KnobMode";
import { act, client, useApp } from "../store";
import { Knob } from "./controls";

const MODES: KnobMode[] = ["volume", "tune", "decay", "tone"];

function VirtualKnob({ index, path }: { index: number; path: string }) {
  const value = useApp((s) => s.snapshot?.params[path] ?? 0);
  const info = useApp((s) => s.registry.find((p) => p.path === path));
  const [min, max] = info && info.kind !== "toggle" ? [info.min, info.max] : [0, 1];
  const norm = (value - min) / (max - min || 1);
  return (
    <Knob
      label={`${index + 1}`}
      value={norm}
      min={0}
      max={1}
      defaultValue={((info?.default ?? 0) - min) / (max - min || 1)}
      format={(v) => `${Math.round(v * 127)}`}
      onChange={(v) => void act(client.call("controller.knob", { index, value: v }))}
      testId={`block-knob-${index}`}
      size={28}
    />
  );
}

export function BlockMirror() {
  const c = useApp((s) => s.snapshot?.controller);
  const length = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  if (!c) return null;
  const pages = Math.max(1, Math.ceil(length / 8));
  return (
    <section className="flex flex-col gap-3 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800 w-fit" data-testid="block">
      <div className="flex items-center justify-between text-xs">
        <span className="text-zinc-500">Livid Block</span>
        <span className={c.device ? "text-emerald-400" : "text-zinc-500"} data-testid="block-device">
          {c.device ?? "virtual"}
        </span>
      </div>
      <div className="grid grid-cols-8 gap-1 p-2 rounded bg-black">
        {c.leds.map((row, r) =>
          row.map((v, col) => (
            <button
              key={`${r}-${col}`}
              data-testid={`pad-${r}-${col}`}
              data-lit={v ? "1" : "0"}
              onClick={() => void act(client.call("controller.press", { row: r, col, pressed: null }))}
              className={`w-7 h-7 rounded-sm border ${
                v ? "bg-red-500 border-red-400 shadow-[0_0_8px_rgba(239,68,68,0.7)]" : "bg-zinc-900 border-zinc-800 hover:border-zinc-600"
              }`}
            />
          )),
        )}
      </div>
      <div className="grid grid-cols-8 gap-1">
        {c.knob_params.map((path, i) => (
          <VirtualKnob key={i} index={i} path={path} />
        ))}
      </div>
      <div className="flex flex-wrap items-center gap-1 text-[10px]">
        <span className="text-zinc-500 mr-1">knobs</span>
        {MODES.map((m) => (
          <button
            key={m}
            data-testid={`knob-mode-${m}`}
            onClick={() => void act(client.call("controller.set_mode", { knob_mode: m, page: null, follow: null }))}
            className={`px-1.5 py-0.5 rounded border ${
              c.knob_mode === m ? "bg-amber-500 text-zinc-950 border-amber-400" : "border-zinc-700 text-zinc-400"
            }`}
          >
            {m}
          </button>
        ))}
      </div>
      <div className="flex flex-wrap items-center gap-1 text-[10px]">
        <span className="text-zinc-500 mr-1">page</span>
        {Array.from({ length: pages }, (_, p) => (
          <button
            key={p}
            data-testid={`page-${p}`}
            onClick={() => void act(client.call("controller.set_mode", { knob_mode: null, page: p, follow: null }))}
            className={`w-5 py-0.5 rounded border ${
              c.page === p ? "bg-zinc-200 text-zinc-950 border-zinc-100" : "border-zinc-700 text-zinc-400"
            }`}
          >
            {p + 1}
          </button>
        ))}
        <button
          data-testid="follow"
          onClick={() => void act(client.call("controller.set_mode", { knob_mode: null, page: null, follow: !c.follow }))}
          className={`ml-2 px-1.5 py-0.5 rounded border ${
            c.follow ? "bg-sky-500 text-zinc-950 border-sky-400" : "border-zinc-700 text-zinc-400"
          }`}
        >
          follow
        </button>
      </div>
    </section>
  );
}
