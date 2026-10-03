# Validation

## Principle: close the loop

Every component must be drivable and verifiable end-to-end by an agent, without
a human listening, looking, or pressing buttons. A change is not done until it
has been exercised through its real interface and the result observed.

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
