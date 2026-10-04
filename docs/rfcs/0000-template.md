# RFC NNNN: Title

- Status: proposed
- Author: your name / handle
- Created: YYYY-MM-DD
- Discussion: link to issue or PR

## Summary

One paragraph: what changes, in plain terms.

## Motivation

What problem this solves and for whom. What happens if we don't do it.

## Design

The proposed design, in enough detail that someone else could implement it.
Include RPC methods or events added or changed, parameters, data formats,
threads, and UI structure as relevant. Diagrams welcome (ASCII is fine).

## Impact on the principles

How the design keeps each principle from [AGENTS.md](../../AGENTS.md). Say
"no impact" where true, but address each one.

- **Agent-drivable / API parity**: Is everything reachable over RPC and the
  CLI?
- **Multiplayer / network transparency**: Does it work with several clients
  and a remote engine?
- **Real-time safety**: What runs on the audio thread? Any locks or
  allocations?
- **Validation**: How will an agent verify it works through real interfaces?
- **Modularity**: Does it fit the node/parameter-path model?

## Alternatives

Other designs considered and why not.

## Migration and compatibility

Effects on existing projects, protocol clients, and docs. Version bumps
(`PROTOCOL_VERSION`, `PROJECT_FORMAT_VERSION`) and migrations.

## Validation plan

Tests and agent-driven checks that will prove the implementation.

## Open questions

What is still undecided.
