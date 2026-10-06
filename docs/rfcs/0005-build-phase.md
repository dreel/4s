# RFC 0005: Build phase -- suspend the RFC gate, lighten the review bar

- Status: implemented
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-05
- Discussion: the PR that introduces this RFC. The maintainer chose to land
  it as one PR (RFC and change together), merged over the old rules' checks.
- Amends: [RFC 0001](0001-contribution-gates.md) (G1) and the reviewer
  instructions.

## Summary

4S is in a **build phase**: the goal is to reach a rough shape quickly with
big changes, then clean up, stabilize, and bring the heavier gates back.
Until then, the RFC gate is suspended and the independent review blocks only
on real problems. The cheap gates stay.

## Motivation

The RFC gate and a strict review cost far more than they return this early.
RFC 0004 needed one RFC PR and one amendment PR, plus many review rounds on
the implementation, most of them about spec and process completeness rather
than bugs. The project has one maintainer and no users to protect from churn
yet.

## Design

Suspended:

- **G1's RFC requirement.** Architecture / UX changes no longer need an
  accepted RFC. `scripts/ci/check-pr-body.mjs` has `RFC_GATE = false`; the
  change class tick is still required. RFCs remain welcome as design notes
  for big changes, but are optional.
- **The `needs-rfc` verdict.** The reviewer classifies the change but does
  not ask for an RFC.

Kept:

- G2 `scripts/check.sh` (tests, codegen, CLI and Electron e2e), G3
  validation evidence, G4 independent review, G5 docs, G6 maintainer merge.

Lighter review bar (in [reviewer.md](../review/reviewer.md)). **Blocking**
only for:

- correctness bugs;
- audio-thread safety: locks, allocation, frees, or I/O on the audio path;
- a user-facing capability with no RPC and CLI command;
- no end-to-end test of the change's main behavior;
- generated code edited by hand or out of date.

Everything else is a **suggestion**: docs gaps, process, spec completeness,
coverage of edge paths, style.

## Restoring the gates

When the maintainer declares the stabilize phase, revert the commit that
introduced this RFC (or set `RFC_GATE = true` and restore the reviewer's
change-class and verdict rules by hand, and delete the "build phase" test in
`scripts/ci/check-pr-body.test.mjs`), then set this RFC's status to
`withdrawn` with a note. The tests for the RFC gate itself still run with
the gate switched on, so they need no changes.

## Impact on the principles

- **Agent-drivable / API parity**, **real-time safety**, and
  **validation** stay blocking in review.
- **Multiplayer / network transparency** and **modularity**: still reviewed,
  but findings are suggestions unless they are correctness bugs.
- **Docs evolve with the code** (principle 7): this RFC is the record.

## Alternatives

- Keep the gates: too slow for the current phase.
- Drop the review entirely: it caught real bugs during RFC 0004 (a stuck
  note path, an audio-thread drop, a pattern-edit race), so keep it, lighter.
