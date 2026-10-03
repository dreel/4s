# Project Format

Status: decided. Details will be refined as we build.

## Layout

A project is a directory bundle:

```
mysong.4s/
  project.json    # the project state
  samples/        # later: imported audio
  renders/        # later: exported audio
  autosave/       # later: crash recovery snapshots
```

## Contents

`project.json` is the engine's state snapshot, serialized with the same
`protocol` types the RPC layer uses (see [rpc.md](rpc.md)). Saving writes the
snapshot; loading applies it. One set of types covers the API and the file, and
agents can generate or edit projects directly.

It contains:

- The graph: instruments, effects, routing, mixer channels.
- Parameter values as a `path -> value` map (e.g. `"mixer.3.volume": 0.35`).
- Patterns (and later, arrangement).
- Controller mappings.
- Transport: tempo, swing, time signature.

## Git-friendly

- Pretty-printed with a stable key order, so diffs are minimal.
- Patterns stored compactly and readably, e.g.
  `"kick": "x---x---x---x---"`, so changing a beat produces a readable diff.

## Versioning

- A top-level integer `format_version`.
- Migrations are Rust functions `vN -> vN+1` operating on `serde_json::Value`,
  applied in sequence on load. Saving always writes the latest version.
- A fixture project for every past version lives in the test suite; CI checks
  they all load.
- Parameters are stored by path, so added params take their defaults from the
  parameter registry. Unknown paths are warned about (whether they are kept or
  dropped is decided when building).

## Schema

- `schemars` emits a JSON Schema for the project file, from the same pipeline
  as the RPC schema. It is snapshot-tested so format changes show up in review.
- Rust is the source of truth. TypeScript runtime validators (e.g. Zod), if we
  want them in the UI, are generated from the JSON Schema, never hand-written.

## Location

Projects live on the engine host. Load and save go through RPC using
engine-side paths; clients never assume they can read the engine's disk (see
[topology.md](topology.md)).

## Presets

Instrument and pattern presets use the same format, as subsets of a project
(e.g. one instrument's params, or one pattern), with their own
`format_version`.

## Out of scope for now

- Undo history (separate concern from saved state).
- Large-project storage (e.g. SQLite); revisit if JSON becomes a bottleneck.
