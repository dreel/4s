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
   Instruments (808 drums, 303 bass) are added at runtime and routed into
   stereo mixer channels (RFC 0004); new instrument types are Extensions.
   Effects and buses still need an RFC. See [extending](docs/extending.md).
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
- [Lifecycle](docs/lifecycle.md) -- starting/stopping the daemon; app, dev, and remote modes
- [Topology](docs/topology.md) -- engine/bridge roles, remote and collaborative setups
- [Extending](docs/extending.md) -- recipes: drum voices, parameters, controllers, RPC/CLI/UI features
- [Contributing](CONTRIBUTING.md) and [Gates](docs/gates.md) -- change classes, RFCs, and what every PR must prove
- [RFCs](docs/rfcs/README.md) -- how big changes get approved before code is written
- [Testing](docs/testing.md) -- e2e first; which unit tests are worth writing (RFC 0002)
- [Validation](docs/validation.md) -- loop-closing, agent-driven testing
- [Livid Block](docs/hardware/livid-block.md) -- controller notes and mapping

## Build, run, verify

```
scripts/dev.sh                               # build + daemon + UI with hot reload (humans)
scripts/dev.sh --no-ui --headless            # build + current daemon only

cargo build                                  # target/debug/4sd and 4s
target/debug/4s daemon start                 # background daemon on ws://127.0.0.1:4440
target/debug/4s state                        # drive it; `4s --help`, `4s methods`
target/debug/4s daemon stop

# Isolated headless daemon for agent work (does not touch ~/.4s or port 4440):
export FOURS_DATA_DIR=$(mktemp -d)
target/debug/4s daemon start --no-audio --no-midi --listen 127.0.0.1:0
target/debug/4s state                        # found via the runtime file in FOURS_DATA_DIR
target/debug/4s daemon stop

(cd ui && npm install && npm start)          # Electron UI; starts a daemon if none is running
scripts/check.sh                             # all tests: Rust, codegen, CLI e2e, Electron e2e
```

- Prefer an isolated data dir and random port when testing, so you never
  touch a daemon the user is running on the default port.

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
- **Test end to end, not implementation details.** Cover new behavior through
  the CLI/UI/render/virtual-MIDI harnesses; unit-test only complex logic and
  silent invariants; never write change-detector tests. See
  [docs/testing.md](docs/testing.md).
- **API first.** New features start as an RPC, then get a CLI command, then UI.
  Never add a capability that only the UI can reach.
- **Keep the audio thread real-time safe.** No locks, allocations, or I/O on the
  audio callback path.
- **Update docs/** when a change alters a principle or a design decision, and
  keep this index in sync.

## Before you open a PR

1. **Classify the change** (see [CONTRIBUTING.md](CONTRIBUTING.md#change-classes)):
   Fix, Extension, or Architecture / UX. If it is Architecture / UX -- audio
   routing or processing model, real-time model, protocol or sync model,
   lifecycle or topology, project format, the UI's overall structure, the
   principles, or the gates -- **stop and draft an RFC** from
   `docs/rfcs/0000-template.md` for a human to approve. Do not implement it
   first.
2. **Validate through real interfaces** and keep the commands and output
   (CLI against a daemon in an isolated data dir, `4s render`, UI tests,
   `virtual_block`).
3. **Commit, then run `scripts/gates.sh`.** Fix what it reports and re-run
   until it passes. Do not edit the review output.
4. **Open the PR with the template**, pasting `.gates/report.md` and your
   validation evidence.

## If you are the independent reviewer

Follow [docs/review/reviewer.md](docs/review/reviewer.md) only. You have no
context from the author by design. Review the given diff against the
principles and docs, stay read-only, and end with the required
`REVIEWED_SHA` / `DIFF_SHA256` / `CHANGE_CLASS` / `VERDICT` lines.
