# 4S -- Agent Guide

4S (Sam's Sequencer and Synthesizer Set) is a modular electronic music suite.
The first milestone is a TR-808-style drum sequencer played from a Livid Block
controller (8x8 LED buttons + 8 knobs). The system is built so that both humans
and agents can drive and verify every part of it.

## Core principles

1. **Rust daemon owns everything real.** Audio, sequencing, synthesis, mixing,
   MIDI/controller I/O, and state all live in the daemon.
2. **The UI is thin.** An Electron app (React + Tailwind) renders daemon state
   and sends commands. No business logic in the UI.
3. **One API, two clients.** Every UI action is an RPC that is also available
   from the CLI, 1:1. The CLI is the primary agentic interface.
4. **Close the loop.** Every component must be drivable and verifiable end-to-end
   by an agent: headless audio renders, a virtual controller, scripted UI tests.
5. **Modular by design.** Instruments, mixer channels, effects, and MIDI devices
   are nodes in a graph; every parameter has a stable, addressable path.
6. **Network-transparent.** Never assume clients, controllers, or files share
   a machine with the engine. See [topology](docs/topology.md).
7. **Docs evolve with the code.** These principles are a starting point; update
   them as we learn.

## Docs

- [Vision](docs/vision.md) -- what 4S is, near-term goal, longer-term direction
- [Architecture](docs/architecture.md) -- daemon / CLI / UI split, graph model
- [API parity](docs/api-parity.md) -- the 1:1 RPC <-> CLI <-> UI rule
- [RPC](docs/rpc.md) -- JSON-RPC over WebSocket, shared types, ts-rs codegen
- [Project format](docs/project-format.md) -- JSON bundle, versioning, migrations
- [Engine](docs/engine.md) -- voices, sequencer, mixer, parameter list
- [Topology](docs/topology.md) -- engine/bridge roles, remote and collaborative setups
- [Validation](docs/validation.md) -- loop-closing, agent-driven testing
- [Livid Block](docs/hardware/livid-block.md) -- controller notes and mapping

## Build, run, verify

```
cargo build                                  # target/debug/4sd and 4s
target/debug/4sd                             # daemon on ws://127.0.0.1:4440 (default audio device)
target/debug/4sd --no-audio --no-midi \
  --listen 127.0.0.1:0 --data-dir /tmp/4s    # headless, random port (prints its URL)
FOURS_URL=ws://... target/debug/4s state     # drive it; `4s --help`, `4s methods`
(cd ui && npm install && npm start)          # Electron UI (FOURS_URL selects the daemon)
scripts/check.sh                             # all tests: Rust, codegen, CLI e2e, Electron e2e
```

- Changing anything in `crates/protocol`: run
  `cargo run -p fours-protocol --bin gen-bindings` and commit the generated
  `ui/src/generated/` and `schema/` output.
- Adding an RPC method: add it to the `api!` macro in
  `crates/protocol/src/api.rs`, handle it in `crates/daemon/src/core.rs`, add a
  CLI command (the `cli_covers_every_method` test enforces this), then UI.
- `4s render` writes a WAV on the engine host and reports peak, RMS, detected
  onsets, and sequencer triggers -- use it to verify audio changes without
  listening. `ui/test-results/groove.png` (from the Electron e2e) shows the UI.

## How agents should work here

- **Do not claim done until verified.** Exercise changes through their real
  interface (CLI against a running daemon, a headless render, a UI test) and
  report what you actually observed.
- **API first.** New features start as an RPC, then get a CLI command, then UI.
  Never add a capability that only the UI can reach.
- **Keep the audio thread real-time safe.** No locks, allocations, or I/O on the
  audio callback path.
- **Update docs/** when a change alters a principle or a design decision, and
  keep this index in sync.
