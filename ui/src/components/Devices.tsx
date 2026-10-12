// Audio effects on the console (RFC 0010): each strip (and the master) shows
// its insert chain as chips, in processing order, with a select to add an
// effect at the end. Clicking a chip opens the device panel below the
// console: one knob per parameter (from the registry, so nothing is
// duplicated here), bypass, moving it earlier or later in its chain, and
// removing it.

import { useEffect, useState } from "react";
import type { DeviceType } from "../generated/DeviceType";
import { act, app, client, setParam, useApp } from "../store";
import { Toggle } from "./controls";
import { ParamKnob } from "./ParamKnob";

/** The audio effect types the daemon's registry offers (`device.types`). */
function useEffectTypes(): DeviceType[] {
  const [types, setTypes] = useState<DeviceType[]>([]);
  const connected = useApp((s) => s.connection === "open");
  useEffect(() => {
    if (!connected) return;
    void act(client.call("device.types", {})).then(
      (r) => r && setTypes(r.types.filter((t) => t.kind === "audio_effect").map((t) => t.type as DeviceType)),
    );
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
  const registry = useApp((s) => s.registry);
  const bypassed = useApp((s) => (s.snapshot?.params[`${s.device}.bypass`] ?? 0) >= 0.5);
  if (!id || !device) return null;
  const params = registry.filter((p) => p.path.startsWith(`${id}.`));
  const move = (position: number) => void act(client.call("device.move", { id, channel: device.channel, position }));
  const btn = "px-2 py-0.5 rounded border border-zinc-700 text-zinc-300 hover:border-zinc-500 disabled:opacity-30";
  return (
    <section className="flex flex-col gap-2 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid={`device-panel-${id}`}>
      <div className="flex items-center gap-2 text-xs">
        <span className="text-zinc-200 font-semibold">{device.name}</span>
        <span className="text-zinc-500">
          {device.type} on {device.channel === 0 ? "master" : `ch ${device.channel}`}, {device.position + 1} of {chain}
        </span>
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
        {params
          .filter((p) => p.kind !== "toggle" || p.path !== `${id}.bypass`)
          .map((p) =>
            p.kind === "toggle" ? (
              <ToggleParam key={p.path} path={p.path} label={p.label} />
            ) : (
              <ParamKnob key={p.path} path={p.path} label={p.label.toLowerCase()} format={p.kind === "integer" ? (v) => `${Math.round(v)}` : undefined} />
            ),
          )}
      </div>
    </section>
  );
}

function ToggleParam({ path, label }: { path: string; label: string }) {
  const on = useApp((s) => (s.snapshot?.params[path] ?? 0) >= 0.5);
  return <Toggle label={label.toLowerCase()} on={on} onClick={() => void setParam(path, on ? 0 : 1)} testId={`toggle-${path}`} title={label} />;
}
