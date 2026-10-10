<!--
Thanks for contributing! Before opening this PR, read CONTRIBUTING.md and run:

    scripts/gates.sh

then paste .gates/report.md into the "Gate report" section below.
The pr-gates check reads this description; keep the headings.
-->

## Summary

<!-- What does this change do, and why? -->

## Change class

<!-- Tick exactly one. See CONTRIBUTING.md#change-classes. -->
- [ ] Fix (bug fix, test, docs, behavior-neutral refactor)
- [ ] Extension (instrument, controller mapping, parameter, CLI command + RPC + UI that follows existing patterns)
- [ ] Architecture / UX (RFC optional during the build phase, RFC 0005)

RFC: <!-- Optional during the build phase (RFC 0005): link one if it exists, e.g. docs/rfcs/0004-instruments-and-mixer.md, or write "none". -->

## Validation

<!--
G3: show the change working through its real interface, driven by an agent.
Commands and the output you observed: 4s CLI transcripts, `4s render`
results, UI test screenshots, virtual_block input/output. For bug fixes,
the reproduction before and after.
-->

## Docs

<!-- G5: docs/AGENTS.md/README updated, or "no behavior or design change". -->

## Follow-ups

<!-- Anything noticed but not done here goes in docs/backlog.md (in this PR); list or link those entries, or "none". -->

## Gate report

<!-- Paste .gates/report.md from scripts/gates.sh here (G2 checks + G4 independent review). -->

## Checklist

- [ ] I ran `scripts/gates.sh` on the final commit of this PR, and the review covers the head commit.
- [ ] The independent review ran in a fresh agent session with no context from the authoring session.
- [ ] Every new capability is reachable over RPC and the CLI, not only the UI.
