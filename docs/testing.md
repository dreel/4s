# Testing

Policy from [RFC 0002](rfcs/0002-testing-philosophy.md). In one line: **test
what users and agents actually do, end to end; unit-test only what is hard
to get right by inspection.**

## Default: end to end through real interfaces

User-visible behavior is tested the way it is used. Pick the harness that
matches the interface:

| Behavior | Harness | Where |
|----------|---------|-------|
| Anything reachable from the CLI or RPC | the `4s` CLI against a real headless daemon | `scripts/e2e-cli.sh` (`check "<desc>" "<expected substring>" <command>`) |
| UI behavior and UI <-> daemon sync | Playwright driving the real Electron app, checking the daemon over RPC (and vice versa) | `ui/e2e/*.spec.ts` |
| Sound: voices, mixing, timing | `4s render` offline render + analysis (peak, RMS, onset times vs. triggers) | CLI e2e, or engine render tests in `crates/engine` |
| MIDI controllers: input, feedback, hotplug | `virtual_block`, a virtual MIDI device in a separate process | `crates/daemon/examples/virtual_block.rs`, used by `scripts/e2e-cli.sh` |
| Daemon lifecycle | `4s daemon start/stop/status` with an isolated data dir; Electron lifecycle tests | `scripts/e2e-cli.sh`, `ui/e2e/lifecycle.spec.ts` |

Always use an isolated data dir and a random port (`FOURS_DATA_DIR`,
`--listen 127.0.0.1:0`), so tests never touch a daemon someone is running.

Mute and solo are a good example of the default. They are checked by
rendering with a track muted and asserting the hit disappears from the
*audio*, not by asserting a flag was stored. That test immediately found a
real bug: freshly loaded mixes leaked for ~10 ms.

## Unit tests: only where they pay

Write a unit test when either:

1. **The logic is complex and its output is not obvious**: DSP and filters,
   envelopes, sequencer timing and swing math, onset detection, parsers and
   regexes, migrations, LED paging and inversion. Examples in this repo:
   - `every_voice_sounds_and_decays`, `sequencer_fires_on_time`,
     `swing_delays_offbeats` (`crates/engine/src/engine.rs`)
   - the onset tests in `crates/engine/src/offline.rs`
   - `steps_round_trip` and the project fixture tests (`crates/protocol`)
   - `leds_show_steps_and_inverted_playhead` (`crates/daemon/src/controller.rs`)
   - the PR-description checker tests (`scripts/ci/check-pr-body.test.mjs`)
2. **An invariant would fail silently, far from its cause**:
   - `layout_matches_registry` (param index layout vs. paths)
   - `cli_covers_every_method` (every RPC has a CLI command)
   - the wire-format tests (JSON shapes the TypeScript client depends on)
   - `v1_fixture_loads` (old projects keep loading)

## Don't write change detectors

A change detector restates the implementation, so it fails whenever the code
changes, even when behavior didn't, and it rarely catches a real bug.
Examples:

- asserting each arm of a `match`/`switch`, or each entry of a lookup table
  (voice aliases, a default MIDI note map);
- testing a getter, a constructor, or trivial formatting (`5s`, `2m05s`);
- asserting a value you just set, through the same code path;
- duplicating a check an e2e test already makes.

Ask: *what realistic bug would this catch that the e2e suites would not?*
If there is no good answer, don't write it. The tests removed under RFC
0002 are examples: `led_diff`, `decode_default_map`, `uptime_format`,
`voice_parse`, `methods_unique`, `values_and_numbering`.

## Bug fixes

Reproduce the bug at the highest level that can express it, usually a CLI
or UI e2e check, a render, or a `virtual_block` scenario. Watch it fail,
then fix it. Add a unit test as well only if the root cause is complex
logic. Examples: the MIDI hotplug bug got a `virtual_block` e2e check; the
onset false positive got a render test with long tails.

## When no harness fits

Some behavior can't be expressed by today's harnesses: multi-daemon setups
(there is no bridge yet), real-hardware-only paths, and real-time-safety
properties that are invisible at the interfaces. Either add the harness or
explain in the PR why not. The human reviewer decides.

## Review

The independent reviewer (`docs/review/reviewer.md`) applies this policy.
Missing e2e coverage of user-visible behavior is **blocking**. A
change-detector test is a **suggestion to delete it**.
