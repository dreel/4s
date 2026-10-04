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

- The bridge reads local MIDI devices and sends their input upstream as RPC
  events; it applies LED and state events coming back to the device.
- Real, bridged, and virtual (simulated) controllers look the same to the
  engine.
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
- Remote listening (collaborators not in the same room as the engine's audio
  out) requires audio streaming. Deferred.
