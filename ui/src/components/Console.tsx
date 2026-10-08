// The console: the app's central control point. Transport on top, then one
// strip per mixer channel (in display order), the master, and controls to
// add instruments and channels. Each strip picks its input instrument and can
// be moved left or right. Clicking a strip's source selects the instrument
// that feeds it for the editor below.

import { useEffect, useState } from "react";
import type { ChannelInfo } from "../generated/ChannelInfo";
import type { InstrumentType } from "../generated/InstrumentType";
import type { InstrumentTypeInfo } from "../generated/InstrumentTypeInfo";
import { act, client, select, setParam, useApp, useLive, useSelected } from "../store";
import { Fader, Meter, Toggle } from "./controls";
import { ParamKnob, pan, pct } from "./ParamKnob";
import { Transport } from "./Transport";

function StereoMeter({ n, height }: { n: number; height?: number }) {
  const l = useLive((s) => s.channels[n]?.[0] ?? 0);
  const r = useLive((s) => s.channels[n]?.[1] ?? 0);
  return (
    <div className="flex gap-0.5">
      <Meter level={l} height={height} />
      <Meter level={r} height={height} />
    </div>
  );
}

function MasterMeters() {
  const l = useLive((s) => s.master[0] ?? 0);
  const r = useLive((s) => s.master[1] ?? 0);
  return (
    <div className="flex gap-0.5">
      <Meter level={l} height={150} />
      <Meter level={r} height={150} />
    </div>
  );
}

function ChannelName({ channel }: { channel: ChannelInfo }) {
  const [editing, setEditing] = useState(false);
  const [name, setName] = useState(channel.name);
  useEffect(() => setName(channel.name), [channel.name]);
  const commit = () => {
    setEditing(false);
    if (name.trim() && name !== channel.name) void act(client.call("channel.rename", { n: channel.n, name }));
  };
  if (editing) {
    return (
      <input
        autoFocus
        className="w-20 px-1 rounded bg-zinc-950 border border-zinc-600 text-xs text-zinc-100"
        value={name}
        data-testid={`channel-name-input-${channel.n}`}
        onChange={(e) => setName(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") commit();
          if (e.key === "Escape") setEditing(false);
        }}
      />
    );
  }
  return (
    <div
      className="max-w-20 truncate text-xs font-semibold text-zinc-200"
      title="double-click to rename"
      data-testid={`channel-name-${channel.n}`}
      onDoubleClick={() => setEditing(true)}
    >
      {channel.name}
    </div>
  );
}

function StripInput({ n }: { n: number }) {
  const routes = useApp((s) => s.snapshot?.graph.routes ?? {});
  const instruments = useApp((s) => s.snapshot?.graph.instruments ?? []);
  const here = Object.entries(routes)
    .filter(([, c]) => c === n)
    .map(([src]) => src);
  const mains = instruments.map((i) => ({ id: i.id, name: i.name, source: i.outputs[0]?.source })).filter((m) => m.source);
  const main = mains.find((m) => here.includes(m.source!));
  const others = here.filter((src) => src !== main?.source);
  const value = main ? main.source! : others.length ? "@other" : "";
  const onChange = (v: string) => {
    if (v === "") {
      for (const source of here) void act(client.call("route.set", { source, channel: null, swap: false }));
    } else {
      void act(client.call("route.set", { source: v, channel: n, swap: true }));
    }
  };
  return (
    <select
      className="w-20 px-1 py-0.5 rounded bg-zinc-950 border border-zinc-700 text-[10px] text-zinc-300"
      value={value}
      title="input: the instrument this channel plays (swaps with the instrument's old channel)"
      data-testid={`strip-input-${n}`}
      onChange={(e) => onChange(e.target.value)}
    >
      <option value="">(none)</option>
      {value === "@other" && (
        <option value="@other" disabled>
          {others.join(", ")}
        </option>
      )}
      {mains.map((m) => (
        <option key={m.id} value={m.source}>
          {m.name}
        </option>
      ))}
    </select>
  );
}

function Strip({ channel, index, count }: { channel: ChannelInfo; index: number; count: number }) {
  const n = channel.n;
  const sources = useApp((s) =>
    Object.entries(s.snapshot?.graph.routes ?? {})
      .filter(([, c]) => c === n)
      .map(([src]) => src)
      .join(", "),
  );
  const instrument = useApp((s) => {
    const first = Object.entries(s.snapshot?.graph.routes ?? {}).find(([, c]) => c === n)?.[0];
    return s.snapshot?.graph.instruments.find((i) => i.outputs.some((o) => o.source === first))?.id ?? null;
  });
  const selected = useSelected();
  const volume = useApp((s) => s.snapshot?.params[`mixer.${n}.volume`] ?? 0);
  const mute = useApp((s) => (s.snapshot?.params[`mixer.${n}.mute`] ?? 0) >= 0.5);
  const solo = useApp((s) => (s.snapshot?.params[`mixer.${n}.solo`] ?? 0) >= 0.5);
  const active = instrument !== null && instrument === selected;
  const move = (position: number) => void act(client.call("channel.move", { n, position }));
  const small = "text-[10px] text-zinc-600 hover:text-zinc-300 disabled:opacity-30 disabled:hover:text-zinc-600";
  return (
    <div
      className={`flex flex-col items-center gap-2 p-2 rounded bg-zinc-900 border ${
        active ? "border-amber-500/70" : "border-zinc-800"
      }`}
      data-testid={`strip-${n}`}
      data-selected={active}
    >
      <div className="flex items-center gap-1 w-full justify-between">
        <button className={small} title="move left" disabled={index === 0} data-testid={`channel-left-${n}`} onClick={() => move(index)}>
          &lt;
        </button>
        <span className="text-[10px] text-zinc-500 tabular-nums">{n}</span>
        <button
          className={small}
          title="move right"
          disabled={index === count - 1}
          data-testid={`channel-right-${n}`}
          onClick={() => move(index + 2)}
        >
          &gt;
        </button>
        <button
          className="text-[10px] text-zinc-600 hover:text-red-400"
          title="remove channel"
          data-testid={`channel-remove-${n}`}
          onClick={() => void act(client.call("channel.remove", { n }))}
        >
          x
        </button>
      </div>
      <ChannelName channel={channel} />
      <StripInput n={n} />
      <button
        className="max-w-20 truncate text-[10px] text-amber-400/80 hover:text-amber-300 disabled:text-zinc-600"
        disabled={!instrument}
        title={sources ? `edit: ${sources}` : "nothing routed here"}
        data-testid={`strip-source-${n}`}
        onClick={() => instrument && select(instrument)}
      >
        {sources || "(empty)"}
      </button>
      <ParamKnob path={`mixer.${n}.pan`} label="pan" format={pan} />
      <div className="flex items-end gap-1">
        <Fader
          value={volume}
          defaultValue={1}
          onChange={(v) => void setParam(`mixer.${n}.volume`, v)}
          testId={`fader-${n}`}
        />
        <StereoMeter n={n} />
      </div>
      <div className="text-[10px] text-zinc-400 tabular-nums" data-testid={`volume-${n}`}>
        {pct(volume)}
      </div>
      <div className="flex gap-1">
        <Toggle label="M" on={mute} onClick={() => void setParam(`mixer.${n}.mute`, mute ? 0 : 1)} testId={`mute-${n}`} title="Mute" />
        <Toggle
          label="S"
          on={solo}
          onClick={() => void setParam(`mixer.${n}.solo`, solo ? 0 : 1)}
          activeClass="bg-sky-500 text-zinc-950 border-sky-400"
          testId={`solo-${n}`}
          title="Solo"
        />
      </div>
    </div>
  );
}

function AddControls() {
  const [types, setTypes] = useState<InstrumentTypeInfo[]>([]);
  const [kind, setKind] = useState<InstrumentType>("tb303");
  const connected = useApp((s) => s.connection === "open");
  useEffect(() => {
    if (connected) void act(client.call("instrument.types", {})).then((r) => r && setTypes(r.types));
  }, [connected]);
  const add = async () => {
    const info = await act(
      client.call("instrument.add", { type: kind, id: null, name: null, channel: null, no_channel: false }),
    );
    if (info) select(info.id);
  };
  const btn = "px-2 py-1 rounded border border-zinc-700 hover:border-zinc-500 bg-zinc-900 text-xs";
  return (
    <div className="flex flex-col gap-2 justify-end p-2 min-w-28">
      <select
        className="px-2 py-1 rounded bg-zinc-900 border border-zinc-700 text-xs text-zinc-200"
        value={kind}
        onChange={(e) => setKind(e.target.value as InstrumentType)}
        data-testid="add-instrument-type"
      >
        {types.map((t) => (
          <option key={t.type} value={t.type}>
            {t.label} ({t.type})
          </option>
        ))}
      </select>
      <button className={btn} onClick={() => void add()} data-testid="add-instrument">
        + instrument
      </button>
      <button className={btn} onClick={() => void act(client.call("channel.add", { name: null }))} data-testid="add-channel">
        + channel
      </button>
    </div>
  );
}

export function Console() {
  const channels = useApp((s) => s.snapshot?.graph.channels ?? []);
  const master = useApp((s) => s.snapshot?.params["mixer.master.volume"] ?? 0);
  return (
    <section className="flex flex-col gap-3 p-3 rounded-lg bg-zinc-900/50 border border-zinc-800" data-testid="console">
      <Transport />
      <div className="flex gap-2 overflow-x-auto">
        {channels.map((c, i) => (
          <Strip key={c.n} channel={c} index={i} count={channels.length} />
        ))}
        <div className="flex flex-col items-center justify-end gap-2 p-2 rounded bg-zinc-900 border border-zinc-700 min-w-20">
          <div className="text-xs font-semibold text-zinc-300">Master</div>
          <div className="flex items-end gap-1">
            <Fader value={master} onChange={(v) => void setParam("mixer.master.volume", v)} testId="fader-master" height={150} />
            <MasterMeters />
          </div>
          <div className="text-[10px] text-zinc-400 tabular-nums">{pct(master)}</div>
        </div>
        <AddControls />
      </div>
    </section>
  );
}
