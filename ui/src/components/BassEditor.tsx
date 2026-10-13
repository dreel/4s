// Editor for a `tb303`: the step view (RFC 0009: one column per step with a
// note or rest and the 303's declared expressions, accent and slide), plus
// the synth's knobs.

import { act, client, mySeat, setParam, useApp, useLive } from "../store";
import { BlockMirror } from "./BlockMirror";
import { StepView, noteName } from "./StepView";
import { Toggle } from "./controls";
import { ParamKnob, pct, st } from "./ParamKnob";


function NoteLight({ id }: { id: string }) {
  const t = useLive((s) => s.triggers[id] ?? 0);
  const note = useLive((s) => s.lastNote[id]);
  const recent = performance.now() - t < 120;
  return (
    <span className="flex items-center gap-1.5">
      <span className={`w-1.5 h-1.5 rounded-full ${recent ? "bg-amber-300" : "bg-zinc-700"}`} />
      <span className="w-8 text-zinc-400 tabular-nums" data-testid="bass-last-note" data-note={note ?? ""}>
        {note === undefined ? "" : noteName(note)}
      </span>
    </span>
  );
}

export function BassEditor({ id }: { id: string }) {
  const square = useApp((s) => (s.snapshot?.params[`${id}.waveform`] ?? 0) >= 0.5);
  // The Block (and this engine's devices) follow the host seat; the button
  // focuses this UI's own seat, so it shows by that seat's focus.
  const focused = useApp((s) => s.snapshot?.controller.focus === id);
  const mine = useApp((s) => mySeat(s)?.name ?? null);
  // As the daemon resolves it: the seat's focus if it exists, else the first instrument.
  const myFocus = useApp((s) => {
    const ids = s.snapshot?.graph.instruments.map((i) => i.id) ?? [];
    const f = mySeat(s)?.config.focus;
    return (f && ids.includes(f) ? f : ids[0]) === id;
  });

  return (
    <div className="flex flex-col gap-3" data-testid={`bass-editor-${id}`}>
      <div className="flex items-center gap-2 text-xs text-zinc-500">
        <NoteLight id={id} />
      </div>
      <StepView id={id} />
      <section className="flex items-center gap-4 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800">
        <Toggle
          label={square ? "square" : "saw"}
          on={square}
          onClick={() => void setParam(`${id}.waveform`, square ? 0 : 1)}
          testId="bass-waveform"
          title="Waveform"
        />
        <ParamKnob path={`${id}.tune`} label="tune" format={st} />
        <ParamKnob path={`${id}.cutoff`} label="cutoff" format={pct} />
        <ParamKnob path={`${id}.resonance`} label="reso" format={pct} />
        <ParamKnob path={`${id}.env_mod`} label="env mod" format={pct} />
        <ParamKnob path={`${id}.decay`} label="decay" format={pct} />
        <ParamKnob path={`${id}.accent`} label="accent" format={pct} />
        <button
          className="px-2 py-1 rounded border border-zinc-700 hover:border-zinc-500 bg-zinc-900 text-xs"
          data-testid="bass-audition"
          onClick={() => void act(client.call("voice.trigger", { instrument: id, voice: null, note: 36, velocity: null }))}
        >
          audition C2
        </button>
        {!myFocus && (
          <button
            className="px-2 py-1 rounded border border-zinc-700 hover:border-zinc-500 bg-zinc-900 text-xs"
            data-testid="make-target"
            title="Focus it for your seat: your MIDI devices without bindings play it (and the Block's knobs, if this engine's devices are in your seat)"
            onClick={() => void act(client.call("seat.focus", { seat: mine, instrument: id }))}
          >
            focus for my seat
          </button>
        )}
      </section>
      {focused && <BlockMirror />}
    </div>
  );
}
