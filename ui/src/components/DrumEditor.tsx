// Editor for a `tr808`: the step sequencer (one row per voice), each voice's
// internal mix (level, pan, mute) and sound (tune, decay, tone), and its
// output: the instrument's main mix or a direct out to a mixer channel.
// Click a step to toggle it; shift-click or right-click toggles accent.

import type { TrackPattern } from "../generated/TrackPattern";
import type { Voice } from "../generated/Voice";
import { act, client, setParam, useApp, useLive } from "../store";
import { BlockMirror } from "./BlockMirror";
import { Toggle } from "./controls";
import { ParamKnob, pan, pct, st } from "./ParamKnob";
import { voiceLabel } from "./voices";

function TriggerLight({ id, voice }: { id: string; voice: Voice }) {
  const t = useLive((s) => s.triggers[`${id}.${voice}`] ?? 0);
  const recent = performance.now() - t < 120;
  return <div className={`w-1.5 h-1.5 rounded-full ${recent ? "bg-amber-300" : "bg-zinc-700"}`} />;
}

/** Where a voice plays: its instrument's main mix, or a channel (direct out). */
function OutputSelect({ id, voice }: { id: string; voice: Voice }) {
  const source = `${id}.${voice}`;
  const routed = useApp((s) => s.snapshot?.graph.routes[source] ?? null);
  const channels = useApp((s) => s.snapshot?.graph.channels ?? []);
  return (
    <select
      className="w-20 px-1 py-0.5 rounded bg-zinc-900 border border-zinc-700 text-[10px] text-zinc-300"
      value={routed ?? ""}
      title="output: main mix, or a direct out to a channel"
      data-testid={`out-${voice}`}
      onChange={(e) =>
        void act(client.call("route.set", { source, channel: e.target.value === "" ? null : Number(e.target.value), swap: false }))
      }
    >
      <option value="">main</option>
      {channels.map((c) => (
        <option key={c.n} value={c.n}>
          ch {c.n} {c.name}
        </option>
      ))}
    </select>
  );
}

function Row({ id, track, length, playhead }: { id: string; track: TrackPattern; length: number; playhead: number | null }) {
  const voice = track.voice;
  const mutePath = `${id}.${voice}.mute`;
  const mute = useApp((s) => (s.snapshot?.params[mutePath] ?? 0) >= 0.5);

  const click = (step: number, accent: boolean) => {
    const level = track.steps[step];
    if (accent) {
      void act(client.call("pattern.set_step", { instrument: id, voice, step, level: level === 2 ? 1 : 2 }));
    } else {
      void act(client.call("pattern.toggle_step", { instrument: id, voice, step }));
    }
  };

  return (
    <div className="flex items-center gap-2" data-testid={`row-${voice}`}>
      <button
        className="w-24 h-8 flex items-center gap-2 px-2 rounded bg-zinc-900 border border-zinc-800 hover:border-zinc-600 text-left text-xs"
        onClick={() => void act(client.call("voice.trigger", { instrument: id, voice, note: null, velocity: null }))}
        data-testid={`voice-${voice}`}
        title="Audition"
      >
        <TriggerLight id={id} voice={voice} />
        <span className="truncate">{voiceLabel(voice)}</span>
      </button>
      <Toggle label="M" on={mute} onClick={() => void setParam(mutePath, mute ? 0 : 1)} testId={`voice-mute-${voice}`} title="Mute voice" />
      <OutputSelect id={id} voice={voice} />
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

function VoiceStrip({ id, voice }: { id: string; voice: Voice }) {
  const p = (name: string) => `${id}.${voice}.${name}`;
  return (
    <div className="flex flex-col items-center gap-1.5 p-2 rounded bg-zinc-900 border border-zinc-800" data-testid={`voice-strip-${voice}`}>
      <div className="text-[10px] font-semibold text-zinc-300">{voiceLabel(voice)}</div>
      <ParamKnob path={p("tune")} label="tune" format={st} size={28} />
      <ParamKnob path={p("decay")} label="decay" format={pct} size={28} />
      <ParamKnob path={p("tone")} label="tone" format={pct} size={28} />
      <ParamKnob path={p("pan")} label="pan" format={pan} size={28} />
      <ParamKnob path={p("level")} label="level" format={pct} size={28} />
    </div>
  );
}

export function DrumEditor({ id }: { id: string }) {
  const pattern = useApp((s) => s.snapshot?.patterns.find((p) => p.instrument === id)?.pattern);
  const length = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  const playhead = useApp((s) => s.snapshot?.transport.step ?? null);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const isTarget = useApp((s) => s.snapshot?.controller.focus === id);
  const blockSeat = useApp((s) => s.snapshot?.controller.seat ?? null);
  if (!pattern || pattern.kind !== "drums") return null;
  return (
    <div className="flex flex-col gap-3" data-testid={`drum-editor-${id}`}>
      <div className="flex gap-3 items-start">
        <section className="flex-1 min-w-0 flex flex-col gap-1.5 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800">
          <div className="flex items-center justify-between text-xs text-zinc-500 mb-1">
            <span>Sequencer</span>
            <span>click: on/off - shift/right-click: accent</span>
          </div>
          {pattern.tracks.map((t) => (
            <Row key={t.voice} id={id} track={t} length={length} playhead={playing ? playhead : null} />
          ))}
        </section>
        {isTarget ? (
          <BlockMirror />
        ) : (
          <button
            className="px-2 py-1 rounded border border-zinc-700 hover:border-zinc-500 bg-zinc-900 text-xs"
            data-testid="make-target"
            onClick={() => void act(client.call("seat.focus", { seat: blockSeat, instrument: id }))}
          >
            control with Block
          </button>
        )}
      </div>
      <section className="flex gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800 overflow-x-auto">
        {pattern.tracks.map((t) => (
          <VoiceStrip key={t.voice} id={id} voice={t.voice} />
        ))}
      </section>
    </div>
  );
}
