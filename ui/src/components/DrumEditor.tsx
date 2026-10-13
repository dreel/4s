// Editor for a `tr808`: the step view (one row per voice, RFC 0009) with
// each voice's audition, mute, and output (the instrument's main mix or a
// direct out to a mixer channel) beside its row, and each voice's sound
// (tune, decay, tone, pan, level).

import type { Voice } from "../generated/Voice";
import { act, client, setParam, useApp, useLive } from "../store";
import { BlockMirror } from "./BlockMirror";
import { Toggle } from "./controls";
import { ParamKnob, pan, pct, st } from "./ParamKnob";
import { StepView } from "./StepView";
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

/** What sits beside a voice's row in the step view: audition, mute, and
 * where the voice plays. */
function RowControls({ id, voice }: { id: string; voice: Voice }) {
  const mutePath = `${id}.${voice}.mute`;
  const mute = useApp((s) => (s.snapshot?.params[mutePath] ?? 0) >= 0.5);
  return (
    <>
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
    </>
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
  const voices = useApp((s) => s.snapshot?.graph.instruments.find((i) => i.id === id)?.layout.rows ?? []);
  const isTarget = useApp((s) => s.snapshot?.controller.focus === id);
  const blockSeat = useApp((s) => s.snapshot?.controller.seat ?? null);
  return (
    <div className="flex flex-col gap-3" data-testid={`drum-editor-${id}`}>
      <div className="flex gap-3 items-start">
        <StepView id={id} rowExtras={(r) => <RowControls id={id} voice={r.id as Voice} />} />
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
        {voices.map((r) => (
          <VoiceStrip key={r.id} id={id} voice={r.id as Voice} />
        ))}
      </section>
    </div>
  );
}
