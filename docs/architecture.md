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
- **Synthesis**: instruments (808-style drums, a 303-style bass).
- **Mixer**: stereo channels (any number up to 32), levels, pan/balance,
  mute/solo, master bus; routes from instrument outputs to channels.
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

Implemented by RFC 0004 for instruments, channels, and routes:

- Nodes: instruments (`tr808`, `tb303`, added and removed at runtime), stereo
  mixer channels, the master bus, MIDI devices. Effects are future work.
- Edges: audio routing (instrument output -> channel -> master; an output is
  mono or stereo, an instrument has a main out and optionally direct outs)
  and control mappings (MIDI device -> instrument through a seat's note
  bindings, CC -> parameter through its CC maps; the Livid Block edits the
  seat's focused instrument). See "MIDI input and seats" below.
- Every parameter has a stable, human-readable path, e.g. `mixer.2.volume`,
  `drums.kick.decay`, `bass.cutoff`, `transport.tempo`. An instrument's id is
  the first segment of its paths. Paths are the shared vocabulary of the RPC
  API, CLI, UI, and controller mappings.
- The UI is a console (transport, channel strips, master) with the selected
  instrument's editor below it.

## Repo layout

```
crates/
  protocol/   # shared types: API (api! macro), events, state, project format
  engine/     # real-time engine: instruments, sequencer, mixer, offline render
  daemon/     # 4sd: core state, RPC server, audio output, MIDI, Livid Block
  cli/        # 4s: CLI client
ui/           # Electron + React + Tailwind; src/generated is codegen output
schema/       # generated JSON Schema
examples/     # example projects (demo.4s)
scripts/      # check.sh, e2e-cli.sh
docs/
```

## Daemon internals

- `Core` (`crates/daemon/src/core.rs`) holds all state behind one mutex. Every
  mutation updates state, pushes a `Command` to the engine, and broadcasts an
  event. Request handlers, MIDI input, and engine feedback all go through it.
- Threads: the audio thread (cpal callback or null pacer) owns the engine;
  a feedback thread drains engine feedback into `Core` (playhead, triggers,
  meters, LED refresh); a MIDI worker handles input (midir callbacks only
  enqueue); a scanner auto-connects Livid Blocks; a tokio runtime serves
  WebSocket connections.
- Offline renders copy state out of `Core` and run on a blocking task, never
  holding the lock.

## Decided

- RPC layer: JSON-RPC 2.0 over WebSocket, types defined in Rust and generated
  for TypeScript with `ts-rs`. See [rpc.md](rpc.md).
- Topology: one engine, hub-and-spoke bridges, clients always talk to the
  local daemon. See [topology.md](topology.md).
- Audio backend: `cpal` (CoreAudio on macOS; ALSA, JACK, or PipeWire on Linux,
  e.g. for a server with a line out).
- MIDI library: `midir` (CoreMIDI on macOS, ALSA on Linux; virtual ports are
  useful for testing).
- Project format: JSON directory bundle with versioned migrations. See
  [project-format.md](project-format.md).

## Open decisions

None currently.

## MIDI input and seats

RFC 0006 (phase 1). Every instrument takes the same input, note on/off
(`Instrument::note_on`, `note_off`); the 808 maps GM drum notes to voices,
the 303 keeps a note stack with last-note priority. MIDI is routed on the
control side, in `crates/daemon/src/core/seats.rs`, in three layers:

- **Hardware** (per machine, `<data-dir>/midi-devices.json`): physical port
  -> logical device name (`keys`), profile (`generic` or `livid_block`),
  and whether to auto-connect it.
- **Seats** (in the project): one performer's focus instrument, knob page,
  note bindings (`{device, channel, low, high, transpose, target}`), CC
  maps, and knobs that follow the focus. A device with no bindings plays
  the seat's focus.
- **Runtime**: which client sits in which seat. A client joins the one seat
  matching its user name; otherwise the UI asks (join, create, or ignore =
  a session-only seat). The engine host's devices use the host seat: a
  pinned seat (`midi.set_seat`), else the latest local client's seat, else
  the one matching the OS user.

Knobs pick up: a knob far from the parameter's value does nothing until it
passes it. Bridges will forward raw input with `midi.input`; the engine
resolves it, so bindings stay in one place.

