# Topology

4S must work on one laptop, with the engine on a remote server, and eventually
with several collaborators connected to one shared engine. This doc describes
how daemons, clients, and controllers are arranged.

## One binary, two roles

`4sd` runs in one of two roles:

- **Engine**: audio output, sequencer clock, synthesis, mixer, and the
  authoritative state. Exactly one per session.
- **Bridge**: owns local devices (Livid Block, other MIDI gear) and proxies
  RPCs and events to/from an upstream engine. No audio.

Solo use is a single daemon in the engine role. Remote and collaborative use
is an engine on the server plus a bridge on each laptop.

```
Solo (one laptop)

  [UI] [CLI] --localhost--> [4sd engine] --> audio out
                                 ^
                            [Livid Block]

Remote engine / collaboration

  Laptop A                                      Server
  [UI] [CLI] --localhost--> [4sd bridge] --+
                                 ^         |
                            [Livid Block]  |
                                           +--network--> [4sd engine] --> audio out
  Laptop B                                 |
  [UI] [CLI] --localhost--> [4sd bridge] --+
                                 ^
                            [MIDI gear]
```

Lifecycle: a remote engine is managed by whoever runs the server; local
clients use `external` mode and never start or stop it. See
[lifecycle.md](lifecycle.md).

## Hub and spoke, not mesh

- The engine is the single source of truth and the single clock.
- Bridges talk only to the engine. When collaborators "talk to each other",
  it is through the engine.
- One writer means no distributed state merging (no CRDTs), one timeline, and
  simple reasoning. Peer-to-peer can be revisited if a real need appears.
- Daemon-to-daemon traffic uses the same JSON-RPC protocol as everything else
  (see [rpc.md](rpc.md)). A bridge is just another client of the engine, built
  on the shared `protocol` crate.

## Clients always talk to the local daemon

- The Electron UI and the CLI connect to `localhost`. In the bridge role, the
  local daemon forwards their RPCs upstream and relays events back.
- This gives clients one connection point, keeps local devices local, handles
  auth and reconnect to the remote engine once (in Rust), and keeps UI code
  identical in solo and remote setups.
- The CLI can still target a remote daemon directly (e.g. a `--host` flag) for
  debugging.

## Controller bridge

- The bridge reads local MIDI devices and forwards their raw input upstream
  with `midi.input {device, data, seat}` (RFC 0007): the logical device
  name from its own `midi-devices.json`, and the seat of its user. The
  engine resolves the seat's bindings and CC maps, so bindings live in one
  place (the project) and everyone sees them. The bridge applies LED and
  state events coming back to the device.
- **Seats** (RFC 0007): each performer's focus, bindings, and CC maps,
  saved in the project. A client joins the seat matching its user name if
  exactly one does; otherwise its UI asks (join, create, or ignore for a
  session-only seat). Remote input plays a fixed delay (default 20 ms)
  after its timestamp to absorb jitter; recording uses the timestamp. (The
  bridge and the delay are not built yet.)
- Real, bridged, and virtual (simulated) controllers look the same to the
  engine.
- **Held notes** (`voice.note_on` / `voice.note_off`, RFC 0004) belong to
  the connection that started them, and the engine releases them when that
  connection closes. A bridge is one upstream connection for all its local
  clients and keyboards. Notes started through `midi.input` are held per
  (connection, device), and a device's note-off releases them; the bridge
  sends a note-off upstream for a device that is unplugged. If the bridge
  drops, the engine releases all its notes.
- **Local feedback**: the bridge lights LEDs immediately on press and then
  reconciles with authoritative state, so the device feels instant despite
  network latency.

## Timing

- Sequencer playback runs on the engine and is unaffected by the network.
- Bridges estimate their clock offset to the engine (ping/timestamp exchange).
- Live input is timestamped at the bridge in engine time. The engine schedules
  it with a small fixed latency buffer so network jitter is absorbed rather
  than heard.
- Events carry engine timestamps so UIs and LEDs can animate the playhead
  smoothly instead of stepping on message arrival.
- Ableton Link is an option for tempo/phase sync with external software.

## Collaboration

- Every connection has an identity (client id + display name). Events carry
  their origin so UIs can show who changed what.
- Conflicts: the engine applies commands in arrival order (last write wins).
  Ownership (e.g. a collaborator claims a track) is deferred until needed.
- Undo is per user and selective ([RFC 0006](rfcs/0006-journal-and-undo.md)):
  each user (`user` in `session.hello`; default the engine host's user)
  undoes only their own changes, and keys someone else changed since are
  kept and reported as skipped.
- The engine journals every request that could change state (who, when, the
  request, and what it changed), in memory and under `<data_dir>/journal/`.
- Remote listening (collaborators not in the same room as the engine's audio
  out) requires audio streaming. Deferred.
