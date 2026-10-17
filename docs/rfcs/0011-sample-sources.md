# RFC 0011: Sample sources: formats, regions, and a decode cache

- Status: accepted (phases 1 and 2 implemented; slicing to pads later)
- Author: Claude, with Sam
- Created: 2026-10-17
- Discussion: project thread

## Summary

The sampler reads the common audio formats (WAV, AIFF, FLAC, MP3, AAC/M4A,
ALAC, Ogg Vorbis) and a pad plays a region of a file, so one drum hit can
come from a whole song. Decoded files are kept in a cache keyed by their
contents with a memory budget; pads hold copies of just their regions.
Files of any size the engine takes are sent in chunks. A saved bundle
stores each region as a WAV, with where it came from.

## Motivation

Samples come as MP3s and FLACs, and the sound you want is often a slice of
something longer. Decoding a whole song for each pad (or each edit) would
be slow, and keeping a decoded song per pad wasteful.

## Design

- **Decoding**: symphonia (pure Rust, no system libraries) decodes; the
  first two channels are kept (mono doubled); rubato resamples to the
  engine's rate (windowed sinc), once, off the audio thread. A source is at
  most 20 minutes; a region at most 60 s (the old limit, now per pad).
- **Regions**: a pad is `{path, start, end}` in seconds of the file
  (`sampler.load` gains `start`/`end`; default: all of a file up to 60 s).
  The pad's `start`/`end` parameters stay as trims within the region (and
  can still be modulated and automated).
- **Cache** (`crates/daemon/src/core/sources.rs`): decoded sources by a
  hash of the file's bytes (a path's size and modification time remember
  its hash, so an unchanged file is not read again), least recently used
  dropped past a budget (`4sd --sample-cache-mb`, default 512), decoded
  again when needed. A pad gets a copy of its region, so a song is not
  pinned in memory for one hit; the engine is unchanged.
- **Overview**: a source keeps min/max per 256 frames; `sample.peaks`
  returns any span in up to 4096 bins (finer than a bin: from the audio).
- **Uploads**: `sample.upload {upload, name, offset, data, done}` sends a
  file in chunks of at most 11 MB (a WebSocket frame), up to 256 MB; the
  last chunk is checked (it must decode) and the file stored under
  `<data dir>/samples/` (sending the same file again keeps one copy).
  `sampler.load {upload}` loads it and is journaled as the load of the
  stored path; `sample.info` and `sample.peaks` take `upload:<id>` as a
  path. `sampler.upload` is gone (protocol 10).
- **Bundles** (project format 8): a pad's entry is a path (all of a file,
  as in v7) or `{path, start?, end?, origin?}`. Saving copies all of a WAV
  file as before; a region, or another format, is written as a 32-bit
  float WAV of what the pad plays, with `origin` (the file and region it
  was cut from) so the editor can widen it while that file exists.
- **Undo**: `sample:<id>.<pad>` -> `{path, start, end, origin?}`.
- **CLI**: `4s sampler load 1 song.mp3 --from 1:23.40 --to 1:23.65`;
  `4s sample info|peaks|upload <file>`.

## Impact on the principles

- **API parity**: every capability is an RPC with a CLI command
  (`cli_covers_every_method`); the UI's pad loader uses the same upload.
- **Network transparency**: files go to the engine host in chunks; the
  waveform is computed there and sent as a few thousand numbers.
- **Real-time safety**: decoding, resampling, and region copies happen on
  the control side; the audio thread still gets one buffer per pad.
- **Validation**: unit tests decode MP3, FLAC, and AAC fixtures
  (`tests/fixtures/audio`) and check the cache; the CLI e2e loads regions,
  renders them, uploads in chunks, saves and reloads bundles.
- **Modularity**: no new node types; the sampler's parameters are as before.

## Alternatives

- Cutting files in the editor (destructive): loses the rest of the song
  for later edits.
- Pads sharing one decoded song (zero copy): keeps whole songs in memory as
  long as any pad uses them; copies of short regions are cheap.
- Copying whole sources into bundles: always re-editable, but a 50 MB FLAC
  for one kick.
- A disk cache of decodes or streaming from disk: not needed at these sizes.

## Migration and compatibility

Format 8 reads v7 sample paths as all of the file. Protocol 10 removes
`sampler.upload` and changes `sampler.load`'s `path` to optional.

## Implementation plan

1. Formats, regions, cache, chunked uploads, bundles, CLI (this PR).
2. The sample editor in the UI: waveform with zoom and scroll, select a
   region, audition it through the engine, assign it to a pad.
3. Later: slice a region to pads by its transients.

## Implementation notes (phase 2: the editor)

- `sample.play {path, start?, end?}` auditions a region once through the
  engine and `sample.stop` stops it: the engine plays it after the master
  (like the metronome's click: not in the mix, not on the meters, not in a
  render), faded in and out over 128 frames, and hands the buffer back when
  replaced (`Command::Preview`). Neither is journaled or saved. CLI:
  `4s sample play <file> --from --to`, `4s sample stop`.
- UI (`SampleEditor.tsx`, under the selected pad's knobs in the sampler):
  the pad's file as a waveform from `sample.peaks` (a bin a pixel,
  refetched as the view moves), opened on the file the pad was cut from
  when the engine host still has it (a part of a bundle copy maps to that
  part of its origin), else on the pad's own file. Drag to select, drag an
  edge to move it, or type the times; wheel zooms around the pointer,
  shift-wheel scrolls; `all` and `selection` zoom. `play`/`stop` audition
  the selection (a cursor follows it); `apply to pad` loads it into the pad
  (`sampler.load` with the region, one undo step).
- Not built: snapping to zero crossings or transients, and slicing a
  region to pads (next).
