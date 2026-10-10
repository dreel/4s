// MIDI devices, project files, and offline render panels.

import { useEffect, useState } from "react";
import type { DeviceProfile } from "../generated/DeviceProfile";
import type { MidiPortsResult } from "../generated/MidiPortsResult";
import type { RenderResult } from "../generated/RenderResult";
import { canReveal, desktop, revealLabel } from "../desktop";
import { act, app, client, useApp } from "../store";
import { SeatPanel } from "./Seats";

const panel = "flex flex-col gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800 text-xs min-w-0";
const btn = "px-2 py-1 rounded border border-zinc-700 hover:border-zinc-500 bg-zinc-900 disabled:opacity-40";
const input = "px-2 py-1 rounded bg-zinc-900 border border-zinc-700 text-zinc-200 min-w-0";

export function MidiPanel() {
  const connections = useApp((s) => s.snapshot?.midi ?? []);
  const connected = useApp((s) => s.connection === "open");
  const hostSeat = useApp((s) => s.snapshot?.seats.host);
  const [ports, setPorts] = useState<MidiPortsResult | null>(null);
  const [profile, setProfile] = useState<DeviceProfile | "">("");
  const refresh = async () => setPorts((await act(client.call("midi.ports", {}))) ?? null);
  useEffect(() => {
    if (connected) void refresh();
  }, [connected, connections.length]);

  const isConnected = (name: string) => connections.some((c) => c.input === name);
  return (
    <section className={panel} data-testid="midi-panel">
      <div className="flex items-center justify-between">
        <span className="text-zinc-500">MIDI</span>
        <button className={btn} onClick={() => void refresh()} data-testid="midi-refresh">
          refresh
        </button>
      </div>
      <label className="flex items-center gap-2 text-zinc-400">
        connect as
        <select className={input} value={profile} onChange={(e) => setProfile(e.target.value as DeviceProfile | "")} data-testid="midi-profile">
          <option value="">auto</option>
          <option value="generic">notes + CCs</option>
          <option value="livid_block">Livid Block</option>
        </select>
      </label>
      <div className="flex flex-col gap-1">
        {(ports?.inputs ?? []).length === 0 && <div className="text-zinc-500">no MIDI inputs found</div>}
        {(ports?.inputs ?? []).map((name) => (
          <div key={name} className="flex items-center justify-between gap-2">
            <span className="truncate" title={name}>
              {name}
            </span>
            {isConnected(name) ? (
              <button className={btn} data-testid={`midi-disconnect-${name}`} onClick={() => void act(client.call("midi.disconnect", { input: name }))}>
                disconnect
              </button>
            ) : (
              <button
                className={btn}
                data-testid={`midi-connect-${name}`}
                onClick={() =>
                  void act(client.call("midi.connect", { input: name, output: null, name: null, profile: profile || null }))
                }
              >
                connect
              </button>
            )}
          </div>
        ))}
      </div>
      {connections.length > 0 && (
        <div className="text-zinc-400">
          {connections.map((c) => (
            <div key={c.input} data-testid={`midi-device-${c.device}`}>
              {c.device} <span className="text-zinc-600">{c.input}</span> ({c.profile === "livid_block" ? "Block" : "notes + CCs"}
              {c.model ? `, ${c.model}` : ""})
              {c.output ? ` -> ${c.output}` : ""}
            </div>
          ))}
          {hostSeat && <div className="text-zinc-500">devices here play in seat {hostSeat}</div>}
        </div>
      )}
      <SeatPanel />
    </section>
  );
}

export function ProjectPanel() {
  const project = useApp((s) => s.snapshot?.project);
  const [path, setPath] = useState("");
  const [list, setList] = useState<string[]>([]);
  const refresh = async () => setList((await act(client.call("project.list", {})))?.projects ?? []);
  useEffect(() => {
    void refresh();
  }, [project?.path]);
  const name = (p: string) => p.split(/[\\/]/).pop() ?? p;
  const reveal = async () => {
    if (!project?.path || !desktop) return;
    const r = await desktop.revealPath(project.path);
    if (!r.ok) app.set({ error: r.error ?? "could not reveal project" });
  };
  return (
    <section className={panel} data-testid="project-panel">
      <div className="flex items-center justify-between gap-2">
        <span className="text-zinc-500">Project</span>
        <div className="flex items-center gap-2 min-w-0">
          <span data-testid="project-name" className="text-zinc-300 truncate" title={project?.path ?? "not saved yet"}>
            {project?.path ? name(project.path) : "untitled"}
            {project?.dirty ? " *" : ""}
          </span>
          {canReveal() && (
            <button
              className={`${btn} shrink-0`}
              data-testid="project-reveal"
              disabled={!project?.path}
              title={project?.path ? project.path : "save the project first"}
              onClick={() => void reveal()}
            >
              {revealLabel()}
            </button>
          )}
        </div>
      </div>
      <div className="flex gap-1">
        <input
          className={`${input} flex-1`}
          placeholder="name (e.g. beat1)"
          value={path}
          onChange={(e) => setPath(e.target.value)}
          data-testid="project-path"
        />
        <button
          className={btn}
          data-testid="project-save"
          onClick={() => void act(client.call("project.save", { path: path.trim() || null }))}
        >
          save
        </button>
        <button className={btn} data-testid="project-new" onClick={() => void act(client.call("project.new", {}))}>
          new
        </button>
      </div>
      <div className="flex flex-col gap-0.5 max-h-24 overflow-y-auto">
        {list.map((p) => (
          <button key={p} className="text-left text-zinc-400 hover:text-zinc-100 truncate" onClick={() => void act(client.call("project.load", { path: p }))}>
            {name(p)}
          </button>
        ))}
      </div>
    </section>
  );
}

export function RenderPanel() {
  const [bars, setBars] = useState(1);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<RenderResult | null>(null);
  const render = async () => {
    setBusy(true);
    setResult((await act(client.call("render.offline", { bars, tail: 0.5, path: null, sample_rate: null }))) ?? null);
    setBusy(false);
  };
  return (
    <section className={panel} data-testid="render-panel">
      <span className="text-zinc-500">Render</span>
      <div className="flex gap-1 items-center">
        <input type="number" min={1} max={64} value={bars} onChange={(e) => setBars(Number(e.target.value) || 1)} className={`${input} w-16`} />
        <span className="text-zinc-500">bars</span>
        <button className={btn} disabled={busy} onClick={() => void render()} data-testid="render-button">
          {busy ? "rendering..." : "render wav"}
        </button>
      </div>
      {result && (
        <div className="text-zinc-400 flex flex-col gap-0.5" data-testid="render-result">
          <span className="truncate" title={result.path}>
            {result.path.split("/").pop()}
          </span>
          <span>
            {result.duration.toFixed(2)}s - peak {result.peak.toFixed(2)} - {result.triggers.length} hits, {result.onsets.length} onsets
          </span>
        </div>
      )}
    </section>
  );
}
