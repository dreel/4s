# RFC 0001: Contribution gates

- Status: implemented
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-04
- Discussion: the PR that introduces this RFC

## Summary

Every PR passes six gates before merging: scope classification (with an
accepted RFC for architecture/UX changes), automated checks, agent-driven
validation evidence, an independent fresh-context agent review against the
principles, docs updates, and human approval. The process is written in
CONTRIBUTING.md and docs/gates.md, automated by `scripts/gates.sh`, and
enforced in CI and with CODEOWNERS.

## Motivation

4S is starting to receive outside contributions, most of them written with
coding agents. The design principles (agent-drivable, multiplayer, real-time
safe, validated through real interfaces) live in AGENTS.md and docs/, but
nothing required a change to respect them. Without gates, the codebase would
drift as volume grows. Agents can write code faster than humans can review
it, so the review itself needs to be partly automated, and independent of
the author.

## Design

See [docs/gates.md](../gates.md) for the full definitions. In short:

- G1 Scope: Fix / Extension / Architecture-UX classes; the last needs an
  accepted RFC (this directory).
- G2 Checks: `scripts/check.sh`, re-run authoritatively by CI on macOS.
- G3 Validation: commands and observed output through real interfaces.
- G4 Independent review: `scripts/review.sh` starts a new agent process with
  only the repo and the diff, following `docs/review/reviewer.md`. It emits
  `REVIEWED_SHA`, `DIFF_SHA256`, `CHANGE_CLASS`, and `VERDICT`. Maintainers
  can re-run it canonically in CI (label `agent-review`) using the reviewer
  instructions from `main`.
- G5 Docs: updated alongside behavior changes.
- G6 Human approval: branch protection plus CODEOWNERS on principles, RFCs,
  the protocol crate, CI, and the gate scripts.

## Impact on the principles

- Agent-drivable: the gates are themselves agent-runnable (one command).
- Multiplayer, real-time safety, validation: now checked explicitly on every
  PR by the reviewer.
- No runtime impact.

## Alternatives

- Human review only: does not scale with agent-written volume, and humans
  miss principle drift in large diffs.
- Review by the authoring agent: not independent; it shares the author's
  blind spots.
- CI-only agent review on every PR: most trustworthy, but costs API usage on
  every push and needs care with secrets on fork PRs. Kept as a
  maintainer-triggered option instead.

## Migration and compatibility

None for code. Contributors need an agent CLI to run G4 locally.

## Validation plan

The PR introducing this RFC is the first to pass the gates. Unit tests cover
the PR-body checker.

## Open questions

- Whether to run the canonical agent review automatically on every PR once
  volume and cost are understood. Resolved by
  [RFC 0003](0003-ci-agent-review.md): yes, with a Muse Contributor model.
- Issue templates and `docs/extending.md` recipes for the no-RFC paths.
