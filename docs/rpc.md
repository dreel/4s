# RPC Layer

Status: decided. Details will be refined as we build.

## Decision

- **Protocol**: JSON-RPC 2.0.
- **Transport**: WebSocket, used by the CLI, the UI, and daemon-to-daemon
  links (see [topology.md](topology.md)). Listens on `127.0.0.1` by default;
  see "Remote operation" below. A Unix domain socket may be added later for
  local CLI use.
- **Server**: `jsonrpsee` (trait-based API definitions, WebSocket
  subscriptions). Fallback if it gets in the way: a small hand-rolled server on
  `tokio-tungstenite`.
- **Types**: defined once in Rust, generated for TypeScript with `ts-rs`.

## Single source of truth

- `crates/protocol` holds every request, response, and event type, using
  `serde`.
- The daemon and the CLI are both Rust and depend on `protocol` directly -- no
  codegen needed on that side.
- TypeScript types for the UI are generated from `protocol` with `ts-rs` into
  `ui/src/generated/`. Generated files are committed so diffs show up in review.
- `schemars` emits a JSON Schema of the whole API, served at runtime by the
  daemon and usable by any future non-Rust/TS client.

## API shape

### Typed structural RPCs

A small set of explicit methods for things that change the shape of the
system: adding/removing instruments, routing, pattern edits, transport
(play/stop/tempo), project load/save.

### Generic parameters

Most interaction is "set or read parameter X", so parameters are not individual
RPCs. Instead:

- `param.get {path}`
- `param.set {path, value}`
- `param.subscribe {paths}` -- push updates when values change

Paths are the stable names described in [architecture.md](architecture.md),
e.g. `mixer.3.volume`, `drums.kick.decay`.

### Parameter registry

The daemon exposes `describe`, returning every parameter with its path, type,
range, unit, and default. The CLI and UI discover parameters from it, so a new
synth parameter appears everywhere without new client code. The CLI uses the
live registry for validation and tab completion.

### Events

Subscriptions push state changes, playhead position, controller LED state, and
meters (roughly 30-60 Hz). JSON is fine at these rates; a binary side channel
for bulk data (waveforms, scopes) is deferred until needed.

## Parity guarantees

Enforced mechanically, not by convention:

- **Codegen check**: CI regenerates TS types and fails if they differ from what
  is committed.
- **Schema snapshot**: the daemon's JSON Schema is snapshot-tested, so any API
  change is visible in review.
- **Round-trip tests**: each message type is serialized in Rust, parsed in TS,
  and checked to come back unchanged.
- **CLI coverage**: a test asserts every RPC method has a corresponding CLI
  command.

## Remote operation

The engine may run on a different machine from its clients (see
[topology.md](topology.md)). The protocol must never assume a shared machine.

- **Listen address**: configurable. Default `127.0.0.1`; exposing on a network
  interface is an explicit opt-in.
- **Auth**: a token presented in a first `hello` message (browser WebSockets
  cannot set custom headers). Encryption initially via Tailscale or an SSH
  tunnel; native `wss://` later.
- **Version handshake**: `hello` exchanges protocol versions; a mismatch is
  refused or warned about.
- **Identity**: `hello` also carries a client id and display name. Events carry
  the origin of the change.
- **Resync**: subscriptions start with a full snapshot followed by
  sequence-numbered deltas, so a client can reconnect and catch up reliably.
- **Timestamps**: events carry engine-clock timestamps; a ping exchange lets
  remote daemons estimate clock offset.
- **Files**: projects, renders, and samples live on the engine host. The API
  either names engine-side paths explicitly or transfers contents. Clients
  never assume they can read the engine's disk.

## Alternatives considered

- **Protobuf + gRPC/Connect**: schema-first and language-neutral, but heavier
  tooling, awkward mapping to Rust enums, binary by default, and needs a proxy
  or Connect for Electron renderers. Revisit if we need clients in other
  languages.
- **rspc / specta-based RPC**: nice ergonomics but uneven maintenance and
  Tauri-oriented.
- **OSC as the primary API**: untyped. Planned instead as a future bridge that
  maps OSC addresses onto parameter paths (e.g. for TouchOSC or Max).
