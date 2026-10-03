import { BlockMirror } from "./components/BlockMirror";
import { Mixer } from "./components/Mixer";
import { MidiPanel, ProjectPanel, RenderPanel } from "./components/Panels";
import { Sequencer } from "./components/Sequencer";
import { Transport } from "./components/Transport";
import { app, client, useApp } from "./store";

function Header() {
  const connection = useApp((s) => s.connection);
  const audio = useApp((s) => s.snapshot?.audio);
  const error = useApp((s) => s.error);
  const color = connection === "open" ? "bg-emerald-500" : connection === "connecting" ? "bg-amber-500" : "bg-red-500";
  return (
    <header className="flex items-center gap-4 px-4 py-2 border-b border-zinc-800">
      <div className="text-lg font-black tracking-tight">
        4S <span className="text-xs font-normal text-zinc-500">sequencer + synthesizer set</span>
      </div>
      <div className="flex items-center gap-2 text-xs text-zinc-400" data-testid="connection" data-state={connection}>
        <div className={`w-2 h-2 rounded-full ${color}`} />
        {connection} <span className="text-zinc-600">{client.url}</span>
      </div>
      {audio && (
        <div className="text-xs text-zinc-500" data-testid="audio-status" title={audio.error ?? ""}>
          audio: {audio.backend}
          {audio.device ? ` - ${audio.device}` : ""} @ {audio.sample_rate} Hz
        </div>
      )}
      {error && (
        <button className="ml-auto text-xs text-red-400 truncate max-w-[40%]" title="dismiss" onClick={() => app.set({ error: null })} data-testid="error">
          {error}
        </button>
      )}
    </header>
  );
}

export function App() {
  const ready = useApp((s) => s.snapshot !== null);
  return (
    <div className="flex flex-col h-full">
      <Header />
      {!ready ? (
        <div className="m-auto text-zinc-500 text-sm">waiting for 4sd at {client.url} ...</div>
      ) : (
        <main className="flex flex-col gap-3 p-4 overflow-auto *:shrink-0">
          <Transport />
          <div className="flex gap-3 items-start">
            <div className="flex-1 min-w-0">
              <Sequencer />
            </div>
            <BlockMirror />
          </div>
          <Mixer />
          <div className="grid grid-cols-3 gap-3">
            <MidiPanel />
            <ProjectPanel />
            <RenderPanel />
          </div>
        </main>
      )}
    </div>
  );
}
