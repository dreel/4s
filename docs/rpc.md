# RPC Layer

Status: implemented (protocol version 2, RFC 0004). Run `4s methods` for the live method list.

## Decision

- **Protocol**: JSON-RPC 2.0.
- **Transport**: WebSocket, used by the CLI, the UI, and (later) daemon-to-daemon
  links (see [topology.md](topology.md)). Listens on `127.0.0.1:4440` by
  default; see "Remote operation" below.
- **Server**: a small hand-rolled JSON-RPC layer on `tokio-tungstenite`
  (`crates/daemon/src/server.rs`). We started with `jsonrpsee` as the plan but
  went hand-rolled so that the API is one typed `Request` enum: that gives us
  generated TS method types and a mechanical CLI parity test for free (below),
  with less dependency surface.
- **Types**: defined once in Rust, generated for TypeScript with `ts-rs`.

## Single source of truth

- `crates/protocol` holds every request, response, event, state, and project
  type, using `serde`.
- Every method is declared exactly once, in the `api!` macro in
  `crates/protocol/src/api.rs`. The macro generates the `Request` enum, the
  `METHODS` list, and the TypeScript `Methods` map (method -> params/result).
- The daemon and the CLI depend on `protocol` directly.
- `cargo run -p fours-protocol --bin gen-bindings` writes:
  - `ui/src/generated/*.ts` -- one file per type, plus `methods.ts`.
  - `schema/4s.schema.json` -- JSON Schema for requests, events, snapshots,
    and project files. This is the language-neutral contract; any TS runtime
    validators (e.g. Zod) must be generated from it, never hand-written.
- Generated files are committed; `scripts/check.sh` regenerates them and fails
  if anything changed.

## Wire format

Request: `{"jsonrpc": "2.0", "id": 1, "method": "param.set", "params": {"path": "mixer.2.volume", "value": 0.35}}`

Events are notifications: `{"jsonrpc": "2.0", "method": "event", "params": {"seq": 42, "origin": "cli", "event": {"type": "param_changed", "path": "mixer.2.volume", "value": 0.35}}}`

Missing `params` are treated as `{}`. Error codes: -32700 parse, -32600 invalid
request, -32601 unknown method, -32602 invalid params, -32000 failed,
-32001 unauthorized.

## API shape

### Structural methods

Explicit methods for things that are not a single parameter: transport
(`transport.play/stop`), the instrument graph (`instrument.*`, `channel.*`,
`route.set`), pattern edits (`pattern.*`, drum steps and note steps),
auditioning (`voice.trigger`), the controller (`controller.*`), MIDI (`midi.*`),
undo and the journal (`history.*`, `journal.get`, `journal.export`), projects
(`project.*`, including `project.import` to load a project sent inline), rendering (`render.offline`), status (`engine.status`), and the
daemon itself (`daemon.info`, `daemon.shutdown`; see [lifecycle.md](lifecycle.md)).

### Generic parameters

Everything that is a value is a parameter with a stable path, e.g.
`transport.tempo`, `sequencer.length`, `drums.kick.decay`, `bass.cutoff`,
`mixer.2.volume`, `mixer.master.volume`. Three methods cover them all:

- `param.list {prefix?}` -- the registry: path, label, kind (continuous /
  integer / toggle) with range, default, unit.
- `param.get {path}`
- `param.set {path, value}` -- clamped to range; returns the applied value.

Clients build their controls from the registry, so new parameters appear in
the UI and CLI without client changes. See [engine.md](engine.md) for the
current parameter set.

### Events and sync

- `events.subscribe {types?}` starts `event` notifications (optionally
  filtered by type); `events.unsubscribe` stops them.
- Every event has a `seq`. `state.get` returns a snapshot with the current
  `seq`. Clients **subscribe first, then fetch the snapshot**, then apply only
  buffered events with `seq` greater than the snapshot's -- nothing is missed or
  applied twice. The UI does exactly this on every (re)connect.
- `reset` (project loaded/new), `graph` (instruments, channels, or routes
  changed, so parameters may have been added or removed), and `lagged`
  (subscriber fell behind) mean "refetch `state.get`" (and `param.list` for
  `graph` and `reset`).
- Pattern and trigger events name their instrument (`step_changed`,
  `pattern_changed`, `notes_changed`, `trigger`), so clients know which
  instrument an edit or hit belongs to.
- **Connection-scoped state: held notes.** `voice.note_on` holds a note for
  the calling connection until it sends `voice.note_off` for that note, or
  until the connection closes (the daemon then releases it). Other
  connections cannot release it. This is the only state tied to a
  connection; everything else is shared. A bridge must therefore track
  holders per local client (see [topology.md](topology.md)).
- High-rate events: `playhead` (per step), `trigger` (per hit), `meters`
  (~30 Hz, suppressed while silent). JSON is fine at these rates.

## Parity guarantees

Enforced mechanically:

- **CLI coverage**: `cli_covers_every_method` (in `crates/cli`) parses a list of
  real CLI commands, maps them to `Request`s, and fails if any method in
  `METHODS` has no dedicated CLI command. `4s call <method> <json>` additionally
  reaches any method generically.
- **Codegen check**: `scripts/check.sh` regenerates TS types and the schema and
  fails on any diff.
- **Wire-format tests** in `crates/protocol` pin the JSON shape of requests,
  events, and project files.
- **End-to-end**: `scripts/e2e-cli.sh` drives a real daemon only through the
  CLI; `ui/e2e` drives the real Electron app and verifies results over RPC (and
  vice versa).

## Remote operation

The engine may run on a different machine from its clients (see
[topology.md](topology.md)). The protocol never assumes a shared machine.

Implemented:

- **Listen address**: `4sd --listen ADDR`. Default `127.0.0.1:4440`; a
  non-loopback address without a token logs a warning.
- **Auth**: `4sd --token T` (or `FOURS_TOKEN`) requires `session.hello` with
  that token before any other call (browser WebSockets cannot set headers).
  Encryption: use Tailscale or an SSH tunnel for now; native `wss://` later.
- **Version handshake**: `session.hello` carries `protocol_version`; a
  mismatch is refused.
- **Identity**: `hello` carries a client name; events carry `origin` (client
  name, `midi:<port>`, or `engine`). `hello` may also carry a `user`, which
  owns the connection's undo history (default: the engine host's user); see
  [RFC 0006](rfcs/0006-journal-and-undo.md).
- **Journal**: every request that could change state is journaled with its
  user, origin, and changes (`journal.get`, `journal` events); MIDI input is
  journaled as its equivalent RPC.
- **Resync**: snapshot + sequence-numbered events, as above.
- **Files**: projects and renders are engine-side paths; relative paths
  resolve under the daemon's data dir.

Not yet implemented: engine-clock offset estimation (ping exchange) for
bridges, and the bridge role itself.

## Alternatives considered

- **jsonrpsee**: mature, but we get stronger typing and parity checks from a
  single `Request` enum, and the server is ~200 lines. Revisit if we need
  HTTP transport, batching, or other features it provides.
- **Protobuf + gRPC/Connect**: schema-first and language-neutral, but heavier
  tooling, awkward mapping to Rust enums, binary by default, and needs a proxy
  or Connect for Electron renderers. Revisit if we need clients in other
  languages.
- **rspc / specta-based RPC**: nice ergonomics but uneven maintenance and
  Tauri-oriented.
- **OSC as the primary API**: untyped. Planned instead as a future bridge that
  maps OSC addresses onto parameter paths (e.g. for TouchOSC or Max).
