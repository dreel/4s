# Architecture

```
  Livid Block / MIDI gear
           |
           v
  +-------------------+        RPC + events        +------------------+
  |   4sd (daemon)    | <------------------------> |   4s (CLI)       |
  |   Rust            |                            |   agents, scripts|
  |                   | <------------------------> +------------------+
  |  audio engine     |                            +------------------+
  |  sequencer/clock  | <------------------------> |  Electron UI     |
  |  synth voices     |                            |  React+Tailwind  |
  |  mixer            |                            +------------------+
  |  MIDI/controllers |
  |  state/persistence|
  +-------------------+
           |
           v
     audio output
```

This shows the solo setup, with everything on one machine. The daemon can
also run on a remote server, with a bridge daemon on each laptop handling
local controllers. See [topology.md](topology.md).

## Components

### Daemon (`4sd`, working name)

The core Rust binary and the single source of truth. It owns:

- **Audio engine**: real-time audio output, the processing graph.
- **Sequencer and clock**: transport, tempo, patterns, timing.
- **Synthesis**: instrument voices (starting with 808-style drums).
- **Mixer**: channels, levels, pan, mute/solo, master bus.
- **MIDI and controller I/O**: including the Livid Block (input and LED output).
- **State and persistence**: projects, patterns, settings.

The real-time audio thread must stay lock-free and allocation-free. Control
changes reach it through message queues; it reports back the same way.

The same binary runs in one of two roles: **engine** (everything above) or
**bridge** (local devices plus a proxy to a remote engine, no audio). See
[topology.md](topology.md).

### CLI (`4s`, working name)

A thin client of the daemon's RPC API, and the primary interface for agents.
It can call any RPC, read state, and stream events. See
[api-parity.md](api-parity.md).

### UI (Electron)

A thin presentation layer in TypeScript with React and Tailwind, chosen so we
can iterate on UX quickly with standard open-source tooling. It holds no
business logic: it renders daemon state, subscribes to events, and sends RPCs.

## Data flow

- Clients (CLI, UI) send **commands** as RPCs.
- The daemon applies them and publishes **state-change events**.
- All clients subscribe to events, so they stay in sync no matter where a change
  came from (a knob turn on the Block, a CLI command, a UI click).

## Modular graph model

- Nodes: instruments, effects, mixer channels, master bus, MIDI devices.
- Edges: audio routing (instrument -> channel -> master) and control mappings
  (controller input -> parameter).
- Every parameter has a stable, human-readable path, e.g. `mixer.3.volume`,
  `drums.kick.decay`, `transport.tempo`. Paths are the shared vocabulary of the
  RPC API, CLI, UI, and controller mappings.

## Proposed repo layout (not yet created)

```
crates/
  daemon/     # 4sd
  cli/        # 4s
  protocol/   # shared RPC types
ui/           # Electron app
docs/
```

## Decided

- RPC layer: JSON-RPC 2.0 over WebSocket, types defined in Rust and generated
  for TypeScript with `ts-rs`. See [rpc.md](rpc.md).
- Topology: one engine, hub-and-spoke bridges, clients always talk to the
  local daemon. See [topology.md](topology.md).

## Open decisions

To be settled when we start building:

- Audio backend (e.g. `cpal`) and MIDI library (e.g. `midir`).
- Project file format.
