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

It contains (format version 5; RFC 0004, RFC 0007, RFC 0008):

- The graph: `instruments` (`{id, type, name}`, in creation order),
  `channels` (`{n, name}`, in display order), and `routes`
  (`source -> channel number`, e.g. `"drums": 1`, `"drums.kick": 3`).
  Effects come later.
- Parameter values as a `path -> value` map (e.g. `"mixer.2.volume": 0.7`,
  `"bass.cutoff": 0.3`).
- Each instrument's track (RFC 0008) under `tracks`: its clip pool by
  id, the `selected` clip (what pattern mode plays), and its `arrangement`
  (placements `{clip, start, length, offset}` in ticks, 384 a bar). Each
  clip is in the most readable form that says exactly the same thing: a
  `pattern` (drum step strings per voice, or a note string) or `events`
  (`tick:note:len:vel` tokens, see `format_events`), with an optional own
  `length` in ticks and a `name` when it is not its id. An instrument with
  one empty clip is left out.
- The controller: whether the grid page follows the playhead.
- Seats (RFC 0007), by name: `focus`, `knob_page`, note `bindings`, `cc`
  maps, and `knobs` that follow the focus. Seats with nothing set are left
  out. Port names are not here: they belong to each machine
  (`<data-dir>/midi-devices.json`), and bindings use logical device names.
- Transport: tempo, swing (as parameters).

```json
"tracks": {
  "bass": { "selected": 1, "clips": { "1": { "pattern": "C2! - C2 D#2~ G2 - C2 -" } } },
  "drums": {
    "selected": 2,
    "clips": {
      "1": { "pattern": { "kick": "X---x---X---x---", "snare": "----x-------x---" } },
      "2": { "name": "Fill", "length": 72, "events": "0:C3:12:89 36:D#3:6:100" }
    },
    "arrangement": [
      { "clip": 1, "start": 0, "length": 1536, "offset": 0 },
      { "clip": 2, "start": 1536, "length": 384, "offset": 0 }
    ]
  }
}
```

Song mode and its loop are parameters (`song.mode`, `song.loop`,
`song.loop_start`, `song.loop_end`), saved with the others.

A project is applied all or nothing: it is validated (ids, channel numbers,
pool limits of 16 instruments, 32 channels, 256 clips, and 256 placements
a track) before anything changes.
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
- v2 -> v3 (RFC 0007) drops the controller's `target` and `knob_mode` (now
  each seat's focus and knob page) and adds `seats`. v3 -> v4 adds `clips`
  (step patterns load unchanged as clips). v4 -> v5 (RFC 0008) moves each
  instrument's pattern or clip into `tracks` as clip 1 of its pool,
  selected, with no arrangement. `focus` became a
  reserved instrument id; a project with an instrument called `focus` must
  be renamed by hand.
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
