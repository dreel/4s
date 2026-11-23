// Editor for a `sampler` (RFC 0010): the step view with each pad's sample
// beside its row (audition, load a WAV from this computer, clear), and the
// selected pad's knobs. A file chosen here (any format the engine reads:
// WAV, AIFF, FLAC, MP3, AAC/M4A, ALAC, Ogg Vorbis) is sent to the engine
// (`sample.upload`, `upload.ts`), so it works with a remote engine too.
// A file longer than a pad plays (a whole song, to cut a hit out of) loads
// its first MAX_REGION seconds and opens the sample editor on all of it,
// to choose the part the pad plays.

import { useState } from "react";
import type { NoteRow } from "../generated/NoteRow";
import { act, client, useApp } from "../store";
import { uploadFile } from "../upload";
import { ParamKnob, pan, pct, st } from "./ParamKnob";
import { MAX_REGION, SampleEditor } from "./SampleEditor";
import { StepView } from "./StepView";

/** File types the picker offers (the engine checks what it gets). */
export const AUDIO_ACCEPT = "audio/*,.wav,.aif,.aiff,.flac,.mp3,.m4a,.aac,.ogg,.caf";

function PadControls({
  id,
  row,
  pad,
  selected,
  onSelect,
  onCut,
}: {
  id: string;
  row: NoteRow;
  pad: number;
  selected: boolean;
  onSelect: () => void;
  /** The file was longer than a pad plays: the pad has its start. */
  onCut: () => void;
}) {
  const sample = useApp((s) => s.snapshot?.samplers.find((x) => x.instrument === id)?.pads.find((p) => p.pad === pad));
  const load = async (file: File | undefined) => {
    if (!file) return;
    const upload = await uploadFile(file);
    if (!upload) return;
    // The first MAX_REGION seconds: all of a file up to that long (the
    // engine stops the region at the file's end), the start of a longer one.
    const info = await act(client.call("sampler.load", { instrument: id, pad, path: null, upload, start: 0, end: MAX_REGION }));
    const loaded = info?.pads.find((p) => p.pad === pad);
    if (loaded && loaded.file_seconds > loaded.end - loaded.start + 0.001) onCut();
  };
  return (
    <div className="flex items-center gap-1 w-56 shrink-0">
      <button
        className={`w-28 h-8 px-2 rounded border text-left text-xs truncate ${
          selected ? "bg-zinc-800 border-amber-500/70" : "bg-zinc-900 border-zinc-800 hover:border-zinc-600"
        } ${sample ? "text-zinc-200" : "text-zinc-600"}`}
        title={sample ? `${sample.name} (${sample.seconds.toFixed(2)} s): click to audition and select` : `${row.name}: no sample`}
        data-testid={`pad-${id}-${pad}`}
        onClick={() => {
          onSelect();
          if (sample) void act(client.call("voice.trigger", { instrument: id, voice: null, note: row.note, velocity: null }));
        }}
      >
        <span className="text-zinc-500 mr-1">{pad}</span>
        {sample?.name ?? "empty"}
      </button>
      <label className="px-1.5 h-6 flex items-center rounded border border-zinc-700 text-[10px] text-zinc-400 hover:border-zinc-500 cursor-pointer" title="load an audio file">
        load
        <input type="file" accept={AUDIO_ACCEPT} className="hidden" data-testid={`pad-load-${id}-${pad}`} onChange={(e) => void load(e.target.files?.[0])} />
      </label>
      {sample && (
        <button
          className="px-1 text-[10px] text-zinc-600 hover:text-red-400"
          title="clear the pad"
          data-testid={`pad-clear-${id}-${pad}`}
          onClick={() => void act(client.call("sampler.clear", { instrument: id, pad }))}
        >
          x
        </button>
      )}
    </div>
  );
}

export function SamplerEditor({ id }: { id: string }) {
  const [pad, setPad] = useState(1);
  /** The pad just loaded with the start of a longer file, whose editor
   * opens on the whole file. */
  const [cut, setCut] = useState<number | null>(null);
  const loaded = useApp((s) => !!s.snapshot?.samplers.find((x) => x.instrument === id)?.pads.some((x) => x.pad === pad));
  const p = (name: string) => `${id}.${pad}.${name}`;
  return (
    <div className="flex flex-col gap-3" data-testid={`sampler-editor-${id}`}>
      <StepView
        id={id}
        rowExtras={(r) => {
          const k = Number(r.id.replace("pad", ""));
          return (
            <PadControls
              id={id}
              row={r}
              pad={k}
              selected={k === pad}
              onSelect={() => setPad(k)}
              onCut={() => {
                setPad(k);
                setCut(k);
              }}
            />
          );
        }}
      />
      <section className="flex items-center gap-4 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid={`sampler-pad-${id}`}>
        <span className="text-xs text-zinc-400">pad {pad}</span>
        <ParamKnob path={p("gain")} label="gain" format={pct} />
        <ParamKnob path={p("pan")} label="pan" format={pan} />
        <ParamKnob path={p("tune")} label="tune" format={st} />
        <ParamKnob path={p("start")} label="start" format={pct} />
        <ParamKnob path={p("end")} label="end" format={pct} />
        <ParamKnob path={p("attack")} label="attack" format={pct} />
        <ParamKnob path={p("release")} label="release" format={pct} />
        <ParamKnob path={p("choke")} label="choke" format={(v) => (Math.round(v) === 0 ? "off" : `${Math.round(v)}`)} />
      </section>
      {loaded && <SampleEditor key={`${pad}-${cut === pad}`} id={id} pad={pad} whole={cut === pad} />}
    </div>
  );
}
