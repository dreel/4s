# RFC 0002: Testing philosophy

- Status: implemented
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-04
- Discussion: the PR that introduces this RFC. Merging that PR is the
  maintainer's approval (status set to `accepted` in the same PR).

## Summary

Test what users and agents actually do, end to end, through the real
interfaces. Write unit tests only where they pay for themselves: complex
logic whose output is not obvious, and silent invariants. Do not write
"change detector" tests that just restate the implementation. The
independent reviewer enforces this.

## Motivation

Agents write tests readily, and without guidance they write many low-value
ones: a test per match arm, per getter, per lookup-table entry. Those tests
fail whenever the code changes, even when behavior did not, so they slow
down every change without catching bugs. Meanwhile the bugs that matter in
4S show up at the boundaries: a CLI command that parses wrong, a UI that
falls out of sync, audio that is silent or late, a MIDI device that never
appears. Only end-to-end tests through real interfaces catch those. The
MIDI hotplug bug was a good example: every unit test passed, and the virtual
MIDI device e2e test was what reproduced it.

## Design

### Default: end-to-end through real interfaces

User-visible behavior is tested the way it is used:

- `scripts/e2e-cli.sh`: the `4s` CLI against a real headless daemon.
- `ui/e2e`: Playwright driving the real Electron app against a real daemon,
  checking results over RPC and the other way round.
- `4s render`: offline audio with analysis (peak, RMS, onset times), so audio
  behavior is checked without speakers.
- `virtual_block`: a virtual MIDI device in a separate process, for
  controller input and output and hotplug.

### Unit tests only where they pay

1. **Complex logic with non-obvious output**: DSP (filters, envelopes,
   voices), sequencer timing and swing math, onset detection, parsers and
   regexes, migrations, LED/paging logic.
2. **Silent invariants** that would otherwise fail far from the cause: the
   parameter index layout vs. paths, CLI parity coverage, wire-format
   contracts consumed by TypeScript, project-format back-compat fixtures.

### Don't write change detectors

A test that restates a `match`/`switch` or lookup table, checks a getter or
trivial formatting, or would have to change whenever the implementation
changes even though behavior did not. If you cannot name a realistic bug
the test would catch, delete it.

### Bug fixes

Reproduce the bug at the highest level that can express it, usually e2e.
Add a unit test as well only if the root cause is complex logic.

### Enforcement

`docs/testing.md` holds the policy with examples. The independent reviewer
(`docs/review/reviewer.md`) treats missing e2e coverage of user-visible
behavior as **blocking**, and flags change-detector tests as a suggestion
to delete.

Scope of "missing e2e is blocking":

- It applies to behavior a user or agent can observe through the CLI, the
  UI, rendered audio, or MIDI.
- For DSP and sound changes, `4s render` analysis (or an engine-level render
  test) counts as end to end.
- Exceptions: behavior no existing harness can express. Examples are
  multi-daemon setups (no bridge yet), real-hardware-only paths, and
  real-time-safety properties that are invisible at the interfaces. For
  these, the PR either adds the harness or explains why not, and the human
  reviewer decides.

### Existing tests

The implementing PR removes these change detectors. Their behavior is
already covered end to end:

- `led_diff` and `decode_default_map` (`crates/daemon/src/controller.rs`):
  the `virtual_block` e2e covers pad input and LED output.
- `uptime_format` (`crates/cli/src/daemon_ctl.rs`): trivial formatting.
- `voice_parse` (`crates/protocol/src/types.rs`): an alias table; the CLI
  e2e uses ids, aliases, and numbers.
- `methods_unique` (`crates/protocol/src/api.rs`): duplicates would already
  break serde and the parity test.
- `values_and_numbering` (`crates/cli/src/main.rs`): percentages, negative
  values, and 1-based steps are covered in `scripts/e2e-cli.sh`.

Kept: DSP, timing, swing, onset detection, parsers and round trips,
migration fixtures, wire-format contracts, LED/playhead inversion and
paging, stale runtime-file handling, the PR-body checker tests, and
`cli_covers_every_method`.

The implementing PR also closes the e2e gap this reveals: mute/solo
audibility is only unit-tested, so it adds a CLI e2e check using
`4s render`.

### Docs the implementing PR updates

- New `docs/testing.md`, linked from the AGENTS.md docs index and the
  "How agents should work here" rules.
- `docs/review/reviewer.md` check 5 (Validation).
- `docs/gates.md` G3, and `CONTRIBUTING.md`.
- `docs/validation.md` ("By component", aligned with this policy).

## Impact on the principles

- Strengthens "Close the loop" (validation through real interfaces).
- Agent-drivable, multiplayer, real-time safety, modularity: no impact.
- No runtime impact.

## Alternatives

- Coverage targets: reward exactly the change-detector tests this RFC
  discourages.
- No policy: agent-written test suites drift toward many brittle unit tests.

## Migration and compatibility

None.

## Validation plan

The implementing PR passes the gates, with the pruned tests removed and the
e2e suites still covering the same behavior.

## Open questions

- Golden-render regression tests for audio (likely a good fit: end to end,
  and they catch unintended sound changes).
