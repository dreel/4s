// The instrument editor below the console: a tab per instrument (in creation
// order) and the selected instrument's panel.

import { act, client, select, useApp, useSelected } from "../store";
import { BassEditor } from "./BassEditor";
import { DrumEditor } from "./DrumEditor";

export function Editor() {
  const instruments = useApp((s) => s.snapshot?.graph.instruments ?? []);
  const selected = useSelected();
  const current = instruments.find((i) => i.id === selected);
  return (
    <section className="flex flex-col gap-3" data-testid="editor">
      <div className="flex items-center gap-1 text-xs">
        {instruments.map((i) => (
          <button
            key={i.id}
            data-testid={`select-${i.id}`}
            data-selected={i.id === selected}
            onClick={() => select(i.id)}
            className={`px-2 py-1 rounded border ${
              i.id === selected ? "bg-zinc-200 text-zinc-950 border-zinc-100" : "border-zinc-700 text-zinc-400 hover:border-zinc-500"
            }`}
          >
            {i.name} <span className="opacity-60">{i.type}</span>
          </button>
        ))}
        {current && (
          <button
            className="ml-auto px-2 py-1 rounded border border-zinc-800 text-zinc-500 hover:text-red-400 hover:border-red-900"
            data-testid={`remove-${current.id}`}
            onClick={() => void act(client.call("instrument.remove", { id: current.id, keep_channels: false }))}
          >
            remove {current.id}
          </button>
        )}
      </div>
      {!current && <div className="text-xs text-zinc-500">No instruments. Add one from the console.</div>}
      {current?.type === "tr808" && <DrumEditor id={current.id} />}
      {current?.type === "tb303" && <BassEditor id={current.id} />}
    </section>
  );
}
