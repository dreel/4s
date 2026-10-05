import { Console } from "./components/Console";
import { Editor } from "./components/Editor";
import { MidiPanel, ProjectPanel, RenderPanel } from "./components/Panels";
import { app, client, launch, useApp } from "./store";

const LIFECYCLE_LABELS: Record<string, string> = {
  connected: "using running daemon",
  "started-owned": "daemon started by app (stops on quit)",
  "started-detached": "daemon started in background",
  external: "external daemon",
};

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
      <div className="text-xs text-zinc-500" data-testid="lifecycle" data-lifecycle={launch.lifecycle}>
        {LIFECYCLE_LABELS[launch.lifecycle] ?? launch.lifecycle}
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
        <div className="m-auto flex flex-col items-center gap-2 text-sm text-zinc-500" data-testid="waiting">
          <div>waiting for 4sd at {client.url} ...</div>
          {launch.error && <div className="text-red-400" data-testid="daemon-error">{launch.error}</div>}
          {launch.lifecycle === "external" && <div>start it with: 4s daemon start</div>}
        </div>
      ) : (
        <main className="flex flex-col gap-3 p-4 overflow-auto *:shrink-0">
          <Console />
          <Editor />
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
