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

It contains (format version 2, RFC 0004):

- The graph: `instruments` (`{id, type, name}`, in creation order),
  `channels` (`{n, name}`, in display order), and `routes`
  (`source -> channel number`, e.g. `"drums": 1`, `"drums.kick": 3`).
  Effects come later.
- Parameter values as a `path -> value` map (e.g. `"mixer.2.volume": 0.7`,
  `"bass.cutoff": 0.3`).
- Patterns per instrument id: drum step strings per voice, or a note string
  (and later, arrangement).
- The controller: target instrument, knob mode, follow.
- Transport: tempo, swing (as parameters).

```json
"patterns": {
  "bass": "C2! - C2 D#2~ G2 - C2 -",
  "drums": { "kick": "X---x---X---x---", "snare": "----x-------x---" }
}
```

A project is applied all or nothing: it is validated (ids, channel numbers,
pool limits of 16 instruments and 32 channels) before anything changes.
`examples/demo.4s` is a complete example.

## Git-friendly

- Pretty-printed with a stable key order, so diffs are minimal.
- Patterns stored compactly and readably, e.g.
  `"kick": "x---x---x---x---"` or `"bass": "C2 - D#2~ G1"`, so changing a beat
  produces a readable diff.

## Versioning

- A top-level integer `format_version`.
- Migrations are Rust functions `vN -> vN+1` operating on `serde_json::Value`,
  applied in sequence on load. Saving always writes the latest version.
- A fixture project for every supported version lives in the test suite; CI
  checks they all load.
- Version 1 (the fixed 8-track kit) was dropped with RFC 0004 without a
  migration, by the maintainer's decision while the project had no other
  users: loading a v1 file fails with a clear error. From version 2 on,
  format changes come with migrations.
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

- A plain name saves under the daemon's data dir:
  `project.save {"path": "beat1"}` -> `~/.4s/projects/beat1.4s/project.json`
  (data dir default `~/.4s`, see [lifecycle.md](lifecycle.md)).
- Absolute paths are used as-is (`.4s` is appended if missing).
- `project.list` lists bundles in `<data-dir>/projects/`.
- The UI's Project panel shows the full path as a tooltip on the project name,
  and a "show in Finder" (Explorer on Windows) button that opens the folder
  with the bundle selected. `4s project reveal` does the same from the CLI
  (`--no-open` just prints the path). Both are local desktop actions, only
  available when the daemon runs on this machine.

## Presets

Instrument and pattern presets use the same format, as subsets of a project
(e.g. one instrument's params, or one pattern), with their own
`format_version`.

## Out of scope for now

- Undo history (separate concern from saved state).
- Large-project storage (e.g. SQLite); revisit if JSON becomes a bottleneck.
