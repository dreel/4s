# API Parity

## The rule

Every action available in the UI is an RPC to the daemon, and every RPC is
reachable from the CLI. There are no UI-only capabilities. The mapping is 1:1.

Example -- "set mixer channel 3 volume to 35%":

| Surface | Form |
|---------|------|
| UI      | drag channel 3 fader to 35% |
| RPC     | `param.set {"path": "mixer.2.volume", "value": 0.35}` |
| CLI     | `4s set mixer.2.volume 0.35` |

(Exact names and syntax are illustrative until the API is built.)

See [rpc.md](rpc.md) for the protocol and how parity is enforced
mechanically (shared types, codegen checks, CLI coverage tests).

## Exception: local desktop actions

A few UI actions act on the user's own machine rather than on daemon state --
currently "show in Finder" for the project bundle. These are not RPCs (the
daemon may be remote, and the action is meaningless there). The CLI still
mirrors them as local commands (`4s project reveal`), so agents and humans keep
the same capabilities.

## Why

- **Agents can do anything a human can.** The CLI is the agentic interface, so
  an agent can build patterns, tweak sounds, and mix.
- **Testability.** Any UI flow can be reproduced and verified from the CLI.
- **Thin UI.** Keeping logic behind the API keeps the UI swappable and cheap to
  iterate on.

## CLI expectations

- Commands for both writing (`set`, `trigger`, ...) and reading (`get`, `list`).
- `watch` to stream state-change events.
- Machine-readable output (`--json`) on every command, human-friendly by
  default.
- Clear exit codes and error messages.

## Adding a feature

1. Define the RPC (and any events it emits) in the shared protocol.
2. Implement it in the daemon, with tests.
3. Expose it in the CLI and verify end-to-end against a running daemon.
4. Add the UI on top.
