# RFCs

> **Build phase ([RFC 0005](0005-build-phase.md)):** RFCs are optional. The
> process below is how they work when used, and how they will be required
> again once the project stabilizes.

An RFC (request for comments) is how 4S agrees on a big change *before* code
is written. Architecture / UX changes need one (see
[CONTRIBUTING.md](../../CONTRIBUTING.md#change-classes)): how the engine
routes audio, the real-time model, the protocol or sync model, lifecycle or
topology, the project format, the UI's overall structure, the core
principles, or the gates.

Adding or improving a drum voice, a new instrument type, a parameter, a
controller mapping, or fixing a bug does not need one (see
[extending.md](../extending.md)).

## Process

1. **Propose**: open a PR adding `docs/rfcs/NNNN-short-title.md` (next free
   number) from [0000-template.md](0000-template.md), with
   `Status: proposed`. An issue to discuss the idea first is welcome.
   Tick "Architecture / UX" in the PR template; a PR that only changes
   `docs/rfcs/` is recognized as a proposal, so `pr-gates` does not require
   an already-accepted RFC.
2. **Discuss**: on the PR. Revise the RFC; agents can help draft and analyze,
   but a human must approve.
3. **Accept or reject**: a maintainer (CODEOWNERS: @dreel) sets
   `Status: accepted` and merges, or `Status: rejected` and merges or
   closes. Rejected RFCs are worth keeping; they record why.
4. **Implement**: in one or more separate PRs, each linking the RFC and
   passing the gates. The `pr-gates` check verifies the linked RFC is
   `accepted` on `main`. If you opened the implementation PR before the RFC
   merged, re-run the check (or edit the PR description) once it has.
5. **Close out**: the final implementation PR sets `Status: implemented`
   and updates the design docs in `docs/` so they describe the new reality.
   The RFC stays as the record of why.

## Amendments

An accepted or implemented RFC can be amended: open a PR that only changes
`docs/rfcs/`, adding a dated amendment section and an `Amended:` line in the
header. The status does not change; merging the PR is the maintainer's
approval, as for the original. Where the amendment differs from the earlier
sections, the amendment wins; mark the superseded parts inline. Examples:
RFC 0003's spend guards (after implementation) and RFC 0004's maintainer
decisions (before implementation).

## Statuses

`proposed` -> `accepted` -> `implemented`, or `rejected` / `withdrawn`.

## Index

| RFC | Title | Status |
|-----|-------|--------|
| [0001](0001-contribution-gates.md) | Contribution gates | implemented |
| [0002](0002-testing-philosophy.md) | Testing philosophy | implemented |
| [0003](0003-ci-agent-review.md) | Automatic CI agent review (Muse Contributor tier) | implemented |
| [0004](0004-instruments-and-mixer.md) | Instruments and the channel mixer | implemented |
| [0005](0005-build-phase.md) | Build phase: suspend the RFC gate, lighten the review bar | implemented |
| [0006](0006-journal-and-undo.md) | Journal and per-user undo | implemented |
| [0007](0007-midi-bindings-and-clips.md) | MIDI bindings, seats, clips, and recording | accepted (phase 1 implemented) |
