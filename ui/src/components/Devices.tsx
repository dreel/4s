// Audio effects on the console (RFC 0010): each strip (and the master) shows
// its insert chain as chips, in processing order, with a select to add an
// effect at the end. Clicking a chip opens the device panel below the
// console: one knob per parameter (from the registry, so nothing is
// duplicated here), bypass, moving it earlier or later in its chain, and
// removing it.

import { useEffect, useState } from "react";
import type { DeviceType } from "../generated/DeviceType";
import { act, app, client, setParam, useApp, useLive } from "../store";
import { Toggle } from "./controls";
import { ParamKnob } from "./ParamKnob";

/** The modulator rack (RFC 0010): LFO banks, their outputs to map onto
 * any continuous knob (map mode: press an output, then a knob), and the
 * routes with their depths. */
export function Modulators() {
  const devices = useApp((s) => s.snapshot?.graph.devices);
  const routes = useApp((s) => s.snapshot?.graph.mod_routes);
  const mapping = useApp((s) => s.mapping);
  const rack = (devices ?? []).filter((d) => d.channel === null);
  useEffect(() => {
    if (!mapping) return;
    const cancel = (e: KeyboardEvent) => e.key === "Escape" && app.set({ mapping: null });
    window.addEventListener("keydown", cancel);
    return () => window.removeEventListener("keydown", cancel);
  }, [mapping]);
  const btn = "px-2 py-0.5 rounded border border-zinc-700 text-zinc-300 hover:border-zinc-500";
  return (
    <section className="flex flex-col gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid="modulators">
      <div className="flex items-center gap-2 text-xs">
        <span className="text-zinc-300">modulators</span>
        <button
          className={btn}
          data-testid="add-lfo"
          onClick={async () => {
            const info = await act(client.call("device.add", { type: "lfo", id: null, name: null, channel: null, position: null }));
            if (info) app.set({ device: info.id });
          }}
        >
          + lfo
        </button>
        {mapping && (
          <span className="text-sky-400" data-testid="mapping">
            map {mapping}: press a knob (Escape: cancel)
          </span>
        )}
      </div>
      {rack.map((d) => (
        <div key={d.id} className="flex items-center gap-1 text-xs" data-testid={`rack-${d.id}`}>
          <button className={`${btn} w-24 truncate text-left`} data-testid={`insert-${d.id}`} onClick={() => openDevice(d.id)}>
            {d.name}
          </button>
          {[1, 2, 3, 4].map((k) => {
            const source = `${d.id}.${k}`;
            return (
              <button
                key={k}
                className={`px-2 py-0.5 rounded border ${mapping === source ? "bg-sky-500 text-zinc-950 border-sky-400" : "border-zinc-700 text-zinc-400 hover:border-zinc-500"}`}
                title={`map LFO ${k}: then press a knob`}
                data-testid={`map-${source}`}
                onClick={() => app.set({ mapping: mapping === source ? null : source })}
              >
                map {k}
              </button>
            );
          })}
        </div>
      ))}
      {(routes ?? []).map((r) => (
        <div key={`${r.source}>${r.target}`} className="flex items-center gap-2 text-xs" data-testid={`route-${r.source}-${r.target}`}>
          <span className="text-zinc-400 w-48 truncate">
            {r.source} &gt; {r.target}
          </span>
          <input
            type="range"
            min={-100}
            max={100}
            key={`${r.source}>${r.target}:${r.depth}`}
            defaultValue={Math.round(r.depth * 100)}
            title="depth"
            data-testid={`depth-${r.source}-${r.target}`}
            // Sent once, on release (one route change, one journal entry).
            onChange={(e) => {
              if (e.target.dataset.dragging !== "1") {
                void act(client.call("mod.route", { source: r.source, target: r.target, depth: Number(e.target.value) / 100 }));
              }
            }}
            onPointerDown={(e) => (e.currentTarget.dataset.dragging = "1")}
            onPointerUp={(e) => {
              e.currentTarget.dataset.dragging = "0";
              void act(client.call("mod.route", { source: r.source, target: r.target, depth: Number(e.currentTarget.value) / 100 }));
            }}
          />
          <span className="w-10 text-zinc-500 tabular-nums">{Math.round(r.depth * 100)}%</span>
          <button className="text-zinc-600 hover:text-red-400" data-testid={`unroute-${r.source}-${r.target}`} onClick={() => void act(client.call("mod.unroute", { source: r.source, target: r.target }))}>
            x
          </button>
        </div>
      ))}
    </section>
  );
}

/** The registry's audio effect types, fetched once per connection and
 * shared by every strip. */
let effectTypes: Promise<DeviceType[]> | null = null;

/** The audio effect types the daemon's registry offers (`device.types`). */
function useEffectTypes(): DeviceType[] {
  const [types, setTypes] = useState<DeviceType[]>([]);
  const connected = useApp((s) => s.connection === "open");
  useEffect(() => {
    if (!connected) {
      effectTypes = null;
      return;
    }
    effectTypes ??= act(client.call("device.types", {})).then((r) =>
      (r?.types ?? []).filter((t) => t.kind === "audio_effect").map((t) => t.type as DeviceType),
    );
    void effectTypes.then(setTypes);
  }, [connected]);
  return types;
}

/** Open (or close) a device's panel. */
export function openDevice(id: string | null) {
  app.set({ device: app.state.device === id ? null : id });
}

/** A chain's inserts and a select to add one; `n` 0 is the master. */
export function Inserts({ n }: { n: number }) {
  const devices = useApp((s) => s.snapshot?.graph.devices);
  const params = useApp((s) => s.snapshot?.params);
  const open = useApp((s) => s.device);
  const types = useEffectTypes();
  const here = (devices ?? []).filter((d) => d.channel === n);
  return (
    <div className="flex flex-col items-stretch gap-0.5 w-20" data-testid={`inserts-${n}`}>
      {here.map((d) => {
        const bypassed = (params?.[`${d.id}.bypass`] ?? 0) >= 0.5;
        return (
          <button
            key={d.id}
            className={`px-1 py-0.5 rounded-sm border text-[9px] truncate text-left ${
              open === d.id ? "border-amber-400 text-amber-200" : "border-zinc-700 text-zinc-300 hover:border-zinc-500"
            } ${bypassed ? "opacity-40 line-through" : ""}`}
            title={`${d.name} (${d.type})${bypassed ? ", bypassed" : ""}: click to open`}
            data-testid={`insert-${d.id}`}
            data-bypassed={bypassed}
            onClick={() => openDevice(d.id)}
          >
            {d.name}
          </button>
        );
      })}
      <select
        className="px-0.5 py-0.5 rounded-sm bg-zinc-950 border border-zinc-800 text-[9px] text-zinc-500"
        value=""
        title="add an effect to this chain"
        data-testid={`insert-add-${n}`}
        onChange={async (e) => {
          const type = e.target.value as DeviceType;
          if (!type) return;
          const info = await act(client.call("device.add", { type, id: null, name: null, channel: n, position: null }));
          if (info) app.set({ device: info.id });
        }}
      >
        <option value="">+ fx</option>
        {types.map((t) => (
          <option key={t} value={t}>
            {t}
          </option>
        ))}
      </select>
    </div>
  );
}

/** The open device's controls. */
export function DevicePanel() {
  const id = useApp((s) => s.device);
  const device = useApp((s) => s.snapshot?.graph.devices.find((d) => d.id === s.device));
  const chain = useApp((s) => s.snapshot?.graph.devices.filter((d) => d.channel === device?.channel).length ?? 0);
  const modulator = device?.channel === null;
  const registry = useApp((s) => s.registry);
  const bypassed = useApp((s) => (s.snapshot?.params[`${s.device}.bypass`] ?? 0) >= 0.5);
  if (!id || !device) return null;
  const params = registry.filter((p) => p.path.startsWith(`${id}.`));
  const move = (position: number) => void act(client.call("device.move", { id, channel: null, position }));
  const btn = "px-2 py-0.5 rounded border border-zinc-700 text-zinc-300 hover:border-zinc-500 disabled:opacity-30";
  return (
    <section className="flex flex-col gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid={`device-panel-${id}`}>
      <div className="flex items-center gap-2 text-xs">
        <span className="text-zinc-200 font-semibold">{device.name}</span>
        <span className="text-zinc-500">
          {modulator
            ? `${device.type} in the modulator rack`
            : `${device.type} on ${device.channel === 0 ? "master" : `ch ${device.channel}`}, ${device.position + 1} of ${chain}`}
        </span>
        {!modulator && (
          <>
            <Toggle
              label="bypass"
              on={bypassed}
              onClick={() => void setParam(`${id}.bypass`, bypassed ? 0 : 1)}
              testId={`device-bypass-${id}`}
              title="Bypass"
            />
            <button className={btn} disabled={device.position === 0} data-testid={`device-earlier-${id}`} onClick={() => move(device.position - 1)}>
              earlier
            </button>
            <button className={btn} disabled={device.position + 1 >= chain} data-testid={`device-later-${id}`} onClick={() => move(device.position + 1)}>
              later
            </button>
          </>
        )}
        <button
          className={`${btn} hover:text-red-400`}
          data-testid={`device-remove-${id}`}
          onClick={() => {
            app.set({ device: null });
            void act(client.call("device.remove", { id }));
          }}
        >
          remove
        </button>
        <button className={`${btn} ml-auto`} data-testid="device-close" onClick={() => openDevice(null)}>
          close
        </button>
      </div>
      <div className="flex flex-wrap gap-3">
        {device.type === "compressor" && <Reduction id={id} />}
        {params
          .filter((p) => p.kind !== "toggle" || p.path !== `${id}.bypass`)
          .map((p) =>
            p.path === `${id}.sidechain` ? (
              <Sidechain key={p.path} path={p.path} />
            ) : p.kind === "toggle" ? (
              <ToggleParam key={p.path} path={p.path} label={p.label} />
            ) : (
              <ParamKnob key={p.path} path={p.path} label={p.label.toLowerCase()} format={p.kind === "integer" ? (v) => `${Math.round(v)}` : undefined} />
            ),
          )}
      </div>
    </section>
  );
}

/** A compressor's gain reduction meter: 0 at the top, down to -24 dB. */
function Reduction({ id }: { id: string }) {
  const db = useLive((s) => s.reduction[id] ?? 0);
  return (
    <div className="flex flex-col items-center gap-1 text-[10px] text-zinc-500" title="gain reduction" data-testid={`reduction-${id}`} data-db={db.toFixed(1)}>
      <div className="relative w-2 h-12 rounded-sm bg-zinc-800 overflow-hidden">
        <div className="absolute top-0 inset-x-0 bg-amber-400" style={{ height: `${Math.min(100, (db / 24) * 100)}%` }} />
      </div>
      <span className="tabular-nums w-10 text-center">-{db.toFixed(1)}</span>
      <span>gr dB</span>
    </div>
  );
}

/** Which channel keys a compressor: its own input, or a track channel's
 * signal before that channel's inserts and fader. */
function Sidechain({ path }: { path: string }) {
  const value = useApp((s) => Math.round(s.snapshot?.params[path] ?? 0));
  const channels = useApp((s) => s.snapshot?.graph.channels ?? []);
  return (
    <label className="flex flex-col items-center gap-1 text-[10px] text-zinc-500">
      <select
        className="px-1 py-0.5 rounded-sm bg-zinc-950 border border-zinc-700 text-xs text-zinc-300"
        value={value}
        title="sidechain: what the compressor listens to"
        data-testid={`sidechain-${path}`}
        onChange={(e) => void setParam(path, Number(e.target.value))}
      >
        <option value={0}>own input</option>
        {channels
          .filter((c) => c.kind !== "return")
          .map((c) => (
            <option key={c.n} value={c.n}>
              ch {c.n} {c.name}
            </option>
          ))}
      </select>
      <span>sidechain</span>
    </label>
  );
}

function ToggleParam({ path, label }: { path: string; label: string }) {
  const on = useApp((s) => (s.snapshot?.params[path] ?? 0) >= 0.5);
  return <Toggle label={label.toLowerCase()} on={on} onClick={() => void setParam(path, on ? 0 : 1)} testId={`toggle-${path}`} title={label} />;
}
