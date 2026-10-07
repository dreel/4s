// The instrument editor below the console: a tab per instrument (in creation
// order, each with a remove button that asks once), the selected instrument's
// output channel, and its panel.

import { useEffect, useState } from "react";
import type { InstrumentInfo } from "../generated/InstrumentInfo";
import { act, client, select, useApp, useSelected } from "../store";
import { BassEditor } from "./BassEditor";
import { DrumEditor } from "./DrumEditor";

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
      {current?.type === "tr808" && <DrumEditor id={current.id} />}
      {current?.type === "tb303" && <BassEditor id={current.id} />}
    </section>
  );
}
