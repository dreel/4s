// Seats (RFC 0007): which performer setup this client plays in. The daemon
// seats a client automatically when exactly one seat matches its user;
// otherwise this chooser asks: join a seat, create one, or ignore (a seat
// for this session only). It can always be reopened from the header.

import { useState } from "react";
import { act, app, client, launch, mySeat, useApp } from "../store";

const btn = "px-2 py-1 rounded border border-zinc-700 hover:border-zinc-500 bg-zinc-900 disabled:opacity-40";
const input = "px-2 py-1 rounded bg-zinc-900 border border-zinc-700 text-zinc-200 min-w-0";

export function SeatBadge() {
  const seat = useApp((s) => mySeat(s));
  const host = useApp((s) => s.snapshot?.seats.host);
  return (
    <button
      className="text-xs text-zinc-400 hover:text-zinc-100"
      data-testid="seat"
      data-seat={seat?.name ?? ""}
      title={host ? `this engine's MIDI devices play in seat ${host}` : ""}
      onClick={() => app.set({ choosingSeat: true })}
    >
      seat: <span className={seat ? "text-zinc-200" : "text-amber-400"}>{seat?.name ?? "none"}</span>
      {seat && !seat.saved ? " (session only)" : ""}
    </button>
  );
}

export function SeatChooser() {
  const open = useApp((s) => s.choosingSeat && s.snapshot !== null);
  const seats = useApp((s) => s.snapshot?.seats.seats ?? []);
  const current = useApp((s) => mySeat(s)?.name ?? null);
  const [name, setName] = useState("");
  if (!open) return null;
  const done = (r: unknown) => {
    if (r !== undefined) app.set({ choosingSeat: false });
  };
  const join = async (seat: string) => done(await act(client.call("seat.claim", { name: seat })));
  const create = async (saved: boolean) =>
    done(await act(client.call("seat.create", { name: saved && name.trim() ? name.trim() : null, saved })));
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60" data-testid="seat-chooser">
      <div className="flex flex-col gap-3 w-96 p-4 rounded-lg bg-zinc-900 border border-zinc-700 text-sm">
        <div className="text-zinc-200">Choose your seat</div>
        <div className="text-xs text-zinc-500">
          A seat holds one performer's focus, MIDI bindings, and CC maps, saved in the project.
          {launch.user ? ` No seat matches "${launch.user}" unambiguously.` : ""}
        </div>
        <div className="flex flex-col gap-1">
          {seats.length === 0 && <div className="text-zinc-500 text-xs">no seats yet</div>}
          {seats.map((s) => (
            <div key={s.name} className="flex items-center justify-between gap-2">
              <span className="truncate">
                {s.name}
                <span className="text-zinc-500 text-xs">
                  {" "}
                  {s.occupants.length ? s.occupants.join(", ") : "empty"}
                  {s.saved ? "" : ", session only"}
                </span>
              </span>
              <button
                className={btn}
                disabled={s.name === current}
                data-testid={`seat-join-${s.name}`}
                onClick={() => void join(s.name)}
              >
                {s.name === current ? "yours" : "join"}
              </button>
            </div>
          ))}
        </div>
        <div className="flex gap-1">
          <input
            className={`${input} flex-1`}
            placeholder={launch.user ? `new seat (default: ${launch.user})` : "new seat name"}
            value={name}
            onChange={(e) => setName(e.target.value)}
            data-testid="seat-name"
          />
          <button className={btn} onClick={() => void create(true)} data-testid="seat-create">
            create
          </button>
        </div>
        <div className="flex justify-between">
          <button className={btn} onClick={() => void create(false)} data-testid="seat-ignore" title="a seat for this session only">
            ignore
          </button>
          {current && (
            <button className={btn} onClick={() => app.set({ choosingSeat: false })} data-testid="seat-cancel">
              cancel
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

/** This client's seat: focus, bindings, and CC maps, with remove buttons. */
export function SeatPanel() {
  const seat = useApp((s) => mySeat(s));
  if (!seat) return null;
  const c = seat.config;
  const unbind = (index: number) => void act(client.call("seat.unbind", { seat: seat.name, index }));
  const unmap = (device: string, cc: number, channel: number | null) =>
    void act(client.call("seat.unmap_cc", { seat: seat.name, device, cc, channel }));
  return (
    <div className="flex flex-col gap-0.5 text-zinc-400" data-testid="seat-panel">
      <div>
        seat <span className="text-zinc-200">{seat.name}</span>, focus {c.focus ?? "first instrument"}
        {seat.learning ? ` - move a knob to map ${seat.learning}` : ""}
      </div>
      {c.bindings.map((b, i) => (
        <div key={`b${i}`} className="flex justify-between gap-2" data-testid={`binding-${i}`}>
          <span className="truncate">
            {b.device} {b.channel ? `ch ${b.channel}` : "any ch"}
            {b.low !== null || b.high !== null ? ` ${b.low ?? 0}..${b.high ?? 127}` : ""}
            {b.transpose ? ` ${b.transpose > 0 ? "+" : ""}${b.transpose} st` : ""} {"->"} {b.target}
          </span>
          <button className="text-zinc-500 hover:text-zinc-200" onClick={() => unbind(i)} title="remove binding">
            x
          </button>
        </div>
      ))}
      {c.cc.map((m) => (
        <div key={`${m.device}/${m.channel}/${m.cc}`} className="flex justify-between gap-2">
          <span className="truncate">
            {m.device} cc {m.cc} {"->"} {m.param}
          </span>
          <button className="text-zinc-500 hover:text-zinc-200" onClick={() => unmap(m.device, m.cc, m.channel)} title="remove CC map">
            x
          </button>
        </div>
      ))}
    </div>
  );
}
