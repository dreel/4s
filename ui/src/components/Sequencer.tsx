// Step sequencer grid: one row per voice, one cell per step.
// Click toggles a step; shift-click or right-click toggles accent.

import type { TrackPattern } from "../generated/TrackPattern";
import { act, client, setParam, useApp, useLive } from "../store";
import { Toggle } from "./controls";
import { voiceLabel } from "./voices";

function TriggerLight({ voice }: { voice: TrackPattern["voice"] }) {
  const t = useLive((s) => s.triggers[voice] ?? 0);
  const recent = performance.now() - t < 120;
  return <div className={`w-1.5 h-1.5 rounded-full ${recent ? "bg-amber-300" : "bg-zinc-700"}`} />;
}

function Row({ track, index, length, playhead }: { track: TrackPattern; index: number; length: number; playhead: number | null }) {
  const ch = index + 1;
  const mute = useApp((s) => (s.snapshot?.params[`mixer.${ch}.mute`] ?? 0) >= 0.5);
  const solo = useApp((s) => (s.snapshot?.params[`mixer.${ch}.solo`] ?? 0) >= 0.5);
  const voice = track.voice;

  const click = (step: number, accent: boolean) => {
    const level = track.steps[step];
    if (accent) {
      void act(client.call("pattern.set_step", { voice, step, level: level === 2 ? 1 : 2 }));
    } else {
      void act(client.call("pattern.toggle_step", { voice, step }));
    }
  };

  return (
    <div className="flex items-center gap-2" data-testid={`row-${voice}`}>
      <button
        className="w-24 h-8 flex items-center gap-2 px-2 rounded bg-zinc-900 border border-zinc-800 hover:border-zinc-600 text-left text-xs"
        onClick={() => void act(client.call("voice.trigger", { voice, velocity: null }))}
        data-testid={`voice-${voice}`}
        title="Audition"
      >
        <TriggerLight voice={voice} />
        <span className="truncate">{voiceLabel(voice)}</span>
      </button>
      <Toggle label="M" on={mute} onClick={() => void setParam(`mixer.${ch}.mute`, mute ? 0 : 1)} testId={`mute-${ch}`} title="Mute" />
      <Toggle
        label="S"
        on={solo}
        onClick={() => void setParam(`mixer.${ch}.solo`, solo ? 0 : 1)}
        activeClass="bg-sky-500 text-zinc-950 border-sky-400"
        testId={`solo-${ch}`}
        title="Solo"
      />
      <div className="flex gap-1 flex-1 min-w-0">
        {Array.from({ length }, (_, s) => {
          const level = track.steps[s] ?? 0;
          const group = Math.floor(s / 4) % 2 === 0;
          const base =
            level === 2
              ? "bg-amber-300 shadow-[inset_0_0_0_2px_rgba(255,255,255,0.6)]"
              : level === 1
                ? "bg-amber-600"
                : group
                  ? "bg-zinc-800"
                  : "bg-zinc-800/60";
          const head = playhead === s ? "ring-2 ring-zinc-100" : "";
          return (
            <button
              key={s}
              data-testid={`step-${voice}-${s}`}
              data-level={level}
              className={`h-8 flex-1 min-w-3 rounded-sm ${base} ${head} hover:brightness-125 ${s % 4 === 0 && s > 0 ? "ml-1" : ""}`}
              onClick={(e) => click(s, e.shiftKey)}
              onContextMenu={(e) => {
                e.preventDefault();
                click(s, true);
              }}
            />
          );
        })}
      </div>
    </div>
  );
}

export function Sequencer() {
  const pattern = useApp((s) => s.snapshot?.pattern);
  const length = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  const playhead = useApp((s) => s.snapshot?.transport.step ?? null);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  if (!pattern) return null;
  return (
    <section className="flex flex-col gap-1.5 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800">
      <div className="flex items-center justify-between text-xs text-zinc-500 mb-1">
        <span>Sequencer</span>
        <span>click: on/off - shift/right-click: accent</span>
      </div>
      {pattern.map((t, i) => (
        <Row key={t.voice} track={t} index={i} length={length} playhead={playing ? playhead : null} />
      ))}
    </section>
  );
}
