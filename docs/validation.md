# Validation

## Principle: close the loop

Every component must be drivable and verifiable end-to-end by an agent, without
a human listening, looking, or pressing buttons. A change is not done until it
has been exercised through its real interface and the result observed.

## Running it

```
scripts/check.sh        # everything below, in order
cargo test              # Rust unit/integration tests
scripts/e2e-cli.sh      # headless 4sd driven only through the 4s CLI
cd ui && npm run test:e2e   # Electron app + real 4sd via Playwright
```

What exists today:

- **Engine** (`crates/engine`): every voice sounds and decays to silence;
  sequencer triggers land within 2 samples of the expected time; swing timing;
  mute/solo; offline render onsets match sequencer triggers, including dense
  mixed patterns and long tails that must not re-trigger.
- **Protocol** (`crates/protocol`): wire formats, project file round trip,
  v1 fixture loads, future versions rejected.
- **Daemon** (`crates/daemon`): Livid Block LED/page logic and MIDI decoding.
- **CLI** (`crates/cli`): every RPC method has a CLI command
  (`cli_covers_every_method`); 1-based numbering and value parsing.
- **CLI e2e** (`scripts/e2e-cli.sh`): ~39 checks covering the daemon
  lifecycle (start/status/stop/restart, single instance, runtime-file
  discovery, logs), params, patterns, virtual pads/knobs, transport events,
  offline render analysis, and project save/load. Uses an isolated data dir
  and random port.
- **UI e2e** (`ui/e2e`): UI edits verified over RPC, RPC edits verified in the
  UI, transport, Block mirror, project round trip, render; writes
  `ui/test-results/groove.png` for visual review. Lifecycle tests: the app
  starts and stops an owned daemon, an owned daemon exits when the app is
  killed, a detached daemon survives the app, and an existing daemon is never
  stopped.

Not yet: golden renders, spectral checks, multi-daemon tests (no bridge yet),
latency/jitter injection.

## By component

### Daemon

- Unit tests for DSP, sequencing, and state logic.
- Integration tests that start a daemon and drive it via the CLI/RPC, asserting
  on returned state and emitted events.

### Audio

- A **headless/offline render mode**: render N bars of a pattern to a WAV file
  faster than real time, no audio device needed.
- Assertions on rendered audio: not silent, onsets land on expected steps,
  levels within range, basic spectral checks (e.g. kick energy is low-frequency).
- Golden renders where useful, compared with a tolerance.

### Hardware / MIDI

- A **virtual controller** (simulated Livid Block) that can inject button and
  knob events, so controller flows are testable without the device.
- Controller output (LED state) is observable via the CLI.
- Real-device checks stay possible but are never the only way to validate.

### Multi-daemon

- Run an engine and one or more bridges on one machine over loopback, so
  remote and collaborative setups are testable without extra hardware.
- Inject latency, jitter, and disconnects between daemons; assert live input
  still lands on time (within the latency buffer), LEDs reconcile, and clients
  resync correctly after reconnect.

### UI

- Playwright (with Electron support) drives the app.
- Tests assert the UI reflects daemon state, and that UI actions produce the
  expected RPCs and resulting state changes.
- Screenshots are available for agents to inspect visually.

## Definition of done

- Tests pass.
- The change was exercised through its real interface (CLI, render, UI test).
- What was actually observed is reported, including failures.
