// Editor for a `tb303`: one column per step with a note (or rest), accent,
// and slide, plus the synth's knobs. Edits go one step at a time with
// `pattern.set_note`; the daemon's `notes_changed` event confirms them.

import type { NoteStep } from "../generated/NoteStep";
import { act, client, setNote, setParam, useApp, useLive } from "../store";
import { BlockMirror } from "./BlockMirror";
import { Toggle } from "./controls";
import { ParamKnob, pct, st } from "./ParamKnob";

const NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
export const noteName = (n: number) => `${NAMES[n % 12]}${Math.floor(n / 12) - 1}`;
/** Notes offered in the editor: C1..C4. */
const NOTES = Array.from({ length: 37 }, (_, i) => 24 + i);

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
  const pattern = useApp((s) => s.snapshot?.patterns.find((p) => p.instrument === id)?.pattern);
  const length = useApp((s) => s.snapshot?.params["sequencer.length"] ?? 16);
  const playhead = useApp((s) => s.snapshot?.transport.step ?? null);
  const playing = useApp((s) => s.snapshot?.transport.playing ?? false);
  const square = useApp((s) => (s.snapshot?.params[`${id}.waveform`] ?? 0) >= 0.5);
  const focused = useApp((s) => s.snapshot?.controller.focus === id);
  const blockSeat = useApp((s) => s.snapshot?.controller.seat ?? null);
  if (!pattern || pattern.kind !== "notes") return null;
  const steps = pattern.steps;

  // One step per call, applied locally first, so quick edits (or other
  // clients) never overwrite each other with a stale copy of the pattern.
  const edit = (i: number, change: Partial<NoteStep>) => void setNote(id, i, { ...steps[i], ...change });

  return (
    <div className="flex flex-col gap-3" data-testid={`bass-editor-${id}`}>
      <section className="flex flex-col gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800">
        <div className="flex items-center justify-between text-xs text-zinc-500">
          <span className="flex items-center gap-2">
            <NoteLight id={id} /> Notes
          </span>
          <span>A: accent - S: slide into the next step</span>
        </div>
        <div className="flex gap-1 overflow-x-auto">
          {Array.from({ length }, (_, i) => {
            const s = steps[i];
            const head = playing && playhead === i ? "ring-2 ring-zinc-100" : "";
            return (
              <div
                key={i}
                className={`flex flex-col items-center gap-1 p-1 rounded ${s.note === null ? "bg-zinc-900" : "bg-amber-900/40"} ${head} ${
                  i % 4 === 0 && i > 0 ? "ml-1" : ""
                }`}
                data-testid={`note-step-${i}`}
              >
                <div className="text-[9px] text-zinc-500 tabular-nums">{i + 1}</div>
                <select
                  className="w-12 px-0.5 py-0.5 rounded bg-zinc-950 border border-zinc-700 text-[10px] text-zinc-200"
                  value={s.note ?? ""}
                  data-testid={`note-${i}`}
                  onChange={(e) => edit(i, { note: e.target.value === "" ? null : Number(e.target.value) })}
                >
                  <option value="">-</option>
                  {NOTES.map((n) => (
                    <option key={n} value={n}>
                      {noteName(n)}
                    </option>
                  ))}
                </select>
                <Toggle label="A" on={s.accent} onClick={() => edit(i, { accent: !s.accent })} testId={`accent-${i}`} title="Accent" />
                <Toggle
                  label="S"
                  on={s.slide}
                  onClick={() => edit(i, { slide: !s.slide })}
                  activeClass="bg-sky-500 text-zinc-950 border-sky-400"
                  testId={`slide-${i}`}
                  title="Slide"
                />
              </div>
            );
          })}
        </div>
      </section>
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
        {!focused && (
          <button
            className="px-2 py-1 rounded border border-zinc-700 hover:border-zinc-500 bg-zinc-900 text-xs"
            data-testid="make-target"
            title="MIDI devices without bindings and the Block's knobs play the focused instrument"
            onClick={() => void act(client.call("seat.focus", { seat: blockSeat, instrument: id }))}
          >
            focus (MIDI + Block knobs)
          </button>
        )}
      </section>
      {focused && <BlockMirror />}
    </div>
  );
}
