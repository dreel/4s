// The instrument editor below the console: a tab per instrument (in creation
// order, each with a remove button that asks once), the selected instrument's
// output channel, its clip pool (RFC 0008: the step editors edit the
// selected clip, which pattern mode plays), and its panel.

import { useEffect, useState } from "react";
import type { InstrumentInfo } from "../generated/InstrumentInfo";
import { act, client, select, useApp, useSelected } from "../store";
import { BassEditor } from "./BassEditor";
import { DrumEditor } from "./DrumEditor";
import { GM_NOTES } from "./voices";

/** Ticks per step (a 16th), as in the protocol's `TICKS_PER_STEP`. */
const TICKS_PER_STEP = 24;

/** What the step editor cannot show of a clip (RFC 0007): notes between
 * steps or outside the drum voices, and a loop length of its own. */
function ClipNote({ id, drums }: { id: string; drums: boolean }) {
  const selected = useApp((s) => s.snapshot?.tracks.find((t) => t.instrument === id)?.selected);
  const clip = useApp((s) => s.snapshot?.clips.find((c) => c.instrument === id && c.id === selected));
  if (!clip) return null;
  const hidden = clip.events.filter((e) => e.tick % TICKS_PER_STEP !== 0 || (drums && !GM_NOTES.includes(e.note))).length;
  const loop = clip.length === null ? null : clip.length / TICKS_PER_STEP;
  if (!hidden && loop === null) return null;
  return (
    <div className="text-xs text-amber-400" data-testid={`clip-note-${id}`} title={`see them with: 4s clip show ${id}`}>
      {hidden ? `${hidden} note${hidden === 1 ? "" : "s"} off the step grid (not shown here)` : ""}
      {hidden && loop !== null ? " - " : ""}
      {loop !== null ? `loops every ${loop} steps` : ""}
    </div>
  );
}

/** The instrument's clip pool: select a clip (double-click to rename), add,
 * duplicate, or delete (asks once). */
function ClipBar({ id }: { id: string }) {
  const track = useApp((s) => s.snapshot?.tracks.find((t) => t.instrument === id));
  const [armed, setArmed] = useState(false);
  const [renaming, setRenaming] = useState<{ clip: number; name: string } | null>(null);
  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), 3000);
    return () => clearTimeout(t);
  }, [armed]);
  if (!track) return null;
  const ref = { instrument: id, clip: track.selected };
  const btn = "px-2 py-0.5 rounded border border-zinc-700 text-zinc-400 hover:border-zinc-500";
  const placed = new Set(track.arrangement.map((p) => p.clip));
  return (
    <div className="flex items-center gap-1 text-xs" data-testid={`clips-${id}`}>
      <span className="text-zinc-500 mr-1">clips</span>
      {track.clips.map((c) =>
        renaming?.clip === c.id ? (
          <input
            key={c.id}
            autoFocus
            className="w-20 px-1 py-0.5 rounded bg-zinc-950 border border-zinc-600 text-zinc-100"
            value={renaming.name}
            data-testid={`clip-name-input-${id}-${c.id}`}
            onChange={(e) => setRenaming({ clip: c.id, name: e.target.value })}
            onBlur={() => setRenaming(null)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setRenaming(null);
              if (e.key === "Enter") {
                setRenaming(null);
                if (renaming.name.trim() && renaming.name !== c.name) {
                  void act(client.call("clip.rename", { instrument: id, clip: c.id, name: renaming.name }));
                }
              }
            }}
          />
        ) : (
          <button
            key={c.id}
            className={`px-2 py-0.5 rounded border ${
              c.id === track.selected ? "bg-zinc-200 text-zinc-950 border-zinc-100" : "border-zinc-700 text-zinc-300 hover:border-zinc-500"
            }`}
            title={`clip ${c.id}${placed.has(c.id) ? " (in the song)" : ""}; double-click to rename`}
            data-testid={`clip-${id}-${c.id}`}
            data-selected={c.id === track.selected}
            onClick={() => void act(client.call("clip.select", { instrument: id, clip: c.id }))}
            onDoubleClick={() => setRenaming({ clip: c.id, name: c.name })}
          >
            {c.name}
            {placed.has(c.id) ? <span className="ml-1 opacity-50">*</span> : null}
          </button>
        ),
      )}
      <button
        className={btn}
        title="new empty clip"
        data-testid={`clip-new-${id}`}
        onClick={() => void act(client.call("clip.new", { instrument: id, name: null, length: null, select: true }))}
      >
        +
      </button>
      <button className={btn} title="duplicate the selected clip" data-testid={`clip-dup-${id}`} onClick={() => void act(client.call("clip.duplicate", ref))}>
        dup
      </button>
      <button
        className={`${btn} ${armed ? "text-red-500 border-red-500" : ""}`}
        disabled={track.clips.length < 2}
        title={armed ? "click again to delete the clip and its placements" : "delete the selected clip"}
        data-testid={`clip-delete-${id}`}
        data-armed={armed}
        onClick={() => {
          if (!armed) return setArmed(true);
          setArmed(false);
          void act(client.call("clip.delete", ref));
        }}
      >
        {armed ? "delete?" : "del"}
      </button>
    </div>
  );
}

function Tab({ instrument, selected }: { instrument: InstrumentInfo; selected: boolean }) {
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), 3000);
    return () => clearTimeout(t);
  }, [armed]);
  const remove = () => {
    if (!armed) return setArmed(true);
    setArmed(false);
    void act(client.call("instrument.remove", { id: instrument.id, keep_channels: false }));
  };
  return (
    <div
      className={`flex items-center rounded border ${
        selected ? "bg-zinc-200 text-zinc-950 border-zinc-100" : "border-zinc-700 text-zinc-400 hover:border-zinc-500"
      }`}
    >
      <button className="pl-2 pr-1 py-1" data-testid={`select-${instrument.id}`} data-selected={selected} onClick={() => select(instrument.id)}>
        {instrument.name} <span className="opacity-60">{instrument.type}</span>
      </button>
      <button
        className={`pr-2 pl-1 py-1 ${armed ? "text-red-500 font-semibold" : "opacity-40 hover:opacity-100 hover:text-red-500"}`}
        title={armed ? "click again to remove" : `remove ${instrument.id}`}
        data-testid={`remove-${instrument.id}`}
        data-armed={armed}
        onClick={remove}
      >
        {armed ? "remove?" : "x"}
      </button>
    </div>
  );
}

/** The channel the instrument's main output feeds; picking one swaps with whatever was there. */
function OutputSelect({ instrument }: { instrument: InstrumentInfo }) {
  const source = instrument.outputs[0]?.source;
  const channels = useApp((s) => s.snapshot?.graph.channels ?? []);
  const routed = useApp((s) => (source ? (s.snapshot?.graph.routes[source] ?? null) : null));
  if (!source) return null;
  return (
    <label className="ml-auto flex items-center gap-1 text-zinc-500">
      out
      <select
        className="px-1 py-1 rounded bg-zinc-900 border border-zinc-700 text-zinc-200"
        value={routed ?? ""}
        title="mixer channel for this instrument (swaps with the instrument already there)"
        data-testid={`instrument-out-${instrument.id}`}
        onChange={(e) =>
          void act(
            client.call("route.set", { source, channel: e.target.value === "" ? null : Number(e.target.value), swap: true }),
          )
        }
      >
        <option value="">(none)</option>
        {channels.map((c) => (
          <option key={c.n} value={c.n}>
            {c.n}: {c.name}
          </option>
        ))}
      </select>
    </label>
  );
}

export function Editor() {
  const instruments = useApp((s) => s.snapshot?.graph.instruments ?? []);
  const selected = useSelected();
  const current = instruments.find((i) => i.id === selected);
  return (
    <section className="flex flex-col gap-3" data-testid="editor">
      <div className="flex items-center gap-1 text-xs">
        {instruments.map((i) => (
          <Tab key={i.id} instrument={i} selected={i.id === selected} />
        ))}
        {current && <OutputSelect instrument={current} />}
      </div>
      {!current && <div className="text-xs text-zinc-500">No instruments. Add one from the console.</div>}
      {current && <ClipBar id={current.id} />}
      {current && <ClipNote id={current.id} drums={current.type === "tr808"} />}
      {current?.type === "tr808" && <DrumEditor id={current.id} />}
      {current?.type === "tb303" && <BassEditor id={current.id} />}
    </section>
  );
}
