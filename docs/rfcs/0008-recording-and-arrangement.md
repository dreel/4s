# RFC 0008: Recording and the arrangement

- Status: accepted (phases A, B1, and B2 implemented, C in progress; see "Implementation notes")
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-09
- Discussion: the PR that introduces this RFC.

## Summary

An Ableton-style workflow in three phases: **record** live notes into clips
(record quantize with strength, overdub/replace, a metronome, and a
count-in); a **song**: a clip pool per instrument and an arrangement
timeline that plays through or loops the song or a section; and the UI for
both, a **piano roll** for any clip and an **arrangement view** with time
left to right and one lane per track (instrument). This takes over RFC 0007's
phase 3 (recording). SMF import/export follows in a separate change.

## Motivation

Clips (RFC 0007) can only be entered as step grids or event text. Nothing
records, nothing clicks, and a project is one loop per instrument. Making
music means playing parts in, fixing their timing, and arranging them into a
song.

## Design

### Phase A: recording, metronome, count-in

- **Where a live note lands.** The engine reports each live (non-clip) note
  with the song tick the player *heard* when they played it:
  `Feedback::Live { slot, note, velocity, on, gate, tick, time }`. `tick` is
  fractional, counted in the unswung tick grid, with the output latency (from
  the audio backend's callback timestamps) taken off. It is negative during
  a count-in.
- **Takes.** `transport.record { arm: true }` starts a take into an
  instrument (default: the caller's seat focus). From a stop it plays after
  `count_in` bars of metronome. A take writes once per loop pass, one step
  before the loop's end (so pass 1 plays back in pass 2), and when it ends
  (`arm: false`, or stopping). Each write goes through `edit_clip` and is
  one journal entry, a `clip.update` marked `recorded`, so one pass is one
  undo step and a journal replay writes the same notes, owned by
  the user who armed.
- **Modes.** `overdub` adds; `replace` clears the part of the loop each
  pass covers, then adds what was played.
- **Record quantize.** A grid (`1/4` .. `1/32`, triplets) and a strength
  (0..1) moving each note's start toward the nearest grid line; a note that
  lands on the loop's end wraps to its start. `clip.quantize` gets the same
  `strength`. Lengths are kept.
- **Metronome.** A click in the engine (not an instrument), mixed after the
  master fader and its meter: `metronome.on`, `metronome.level` (globals,
  saved with the project, not undone). It always clicks during a count-in.
- **Agent loop.** `render.offline` takes `input` (notes timed in seconds)
  and `record` (settings), plays the notes sample-accurately, and runs the
  same take code over the render's feedback, returning the clip it would
  leave (`recorded`) without changing the project. `metronome: true`
  includes the click.
- RPC/CLI: `transport.record` -> `4s record [--off|--show] [--to] [--replace]
  [--quantize] [--strength] [--count-in] [--offset-ms]`; `4s metronome`;
  `4s render --input ... --record`; `4s clip quantize --strength`. Event
  `record`; snapshot `record`. Protocol 6.

### Phase B: song model, clip pool, loop range, piano roll

- Each instrument (a **track**) has a clip pool and an arrangement of
  placements `{clip, start, length, offset}`; a clip loops inside a longer
  placement.
- `transport.mode`: `pattern` (each track loops its selected clip, today's
  behavior) or `song`. Song loop: `song | range | off`; `transport.locate`.
- Engine: a preallocated clip table shared by slots and fixed placement
  arrays; per tick, the placement under the song tick plays its clip.
- Recording in song mode records into the placement under the playhead, or
  a new clip and placement that grows as you play.
- Project format v5 (each clip becomes clip 1 of its pool).
- Piano roll for any clip (drum rows for the 808), editing through an atomic
  `clip.update { remove, add }`.

### Phase C: arrangement view

Revised 2026-10-10 (before it was built): time runs **left to right**,
with **one lane per track** (= instrument), as Ableton's arrangement does;
not a column per mixer channel. A new `ui/src/components/Arrangement.tsx`:

- **Track headers** (left, sticky): the instrument's name, select (opens
  it in the editor), and **arm** = the seat's focus (`seat.focus`), which
  already routes MIDI input and sets the record target, as Ableton's arm
  does. No mute/solo here: they are per mixer channel, and an instrument
  can feed several (an instrument-level mute is a backlog item).
- **Ruler** (top): bars and beats; click to locate (`transport.locate`);
  a **loop brace** dragged to set `song.loop_start/end` with `song.loop`
  = 2, sent as one `batch` (one undo step); the song's end; a playhead
  that follows while playing.
- **Lanes**: placements as blocks with the clip's name and a small note
  preview (looped repeats marked). Drag to move (`song.move`), drag the
  right edge to resize (`song.place` at the same start; longer loops the
  clip), alt-drag to copy (`song.place`), Delete removes (`song.remove`),
  snapping to bar, beat, or off. Drag a clip from the track's pool (its
  clip bar) into its lane. Edits of several placements go as one `batch`.
  Moving a placement to another track (copying the clip into that pool) is
  out of scope.
- **Double-click a placement**: selects its clip and opens the piano roll
  (`PianoRoll.tsx`, also horizontal) in a panel below.
- Horizontal zoom and scroll; the transport keeps the pattern/song toggle.
- Expected to need no daemon changes; a gap that shows up becomes an RPC
  first, then CLI, then UI.
- Validation: Electron e2e drags (place, move, resize, copy, delete, loop
  brace, locate, arm), checking `song.get`/`state.get` and one undo per
  action; a screenshot at `ui/test-results/arrangement.png`.

## Impact on the principles

- **API first / parity:** every capability is an RPC with a CLI command
  before the UI uses it.
- **Real-time safety:** the click and count-in are fixed state in the
  engine; live-note positions are computed on the audio thread from
  existing clock state; takes are built on the control side
  (`no_alloc` covers live notes, the click, and a count-in).
- **Close the loop:** recording is verified deterministically through
  `render --input --record`, and live through the CLI and Electron e2e.

## Alternatives

- Session view (scenes) instead of an arrangement: the user asked for a
  song timeline that loops the song or a section.
- Writing each recorded note as it is released: live feedback, but one undo
  step per note; per-pass writes match Ableton's takes.
- Recording raw timing and quantizing non-destructively: more state for
  little gain while `clip.quantize` exists.

## Implementation notes (phase A)

- Record settings (mode, quantize, strength, count-in, offset) are session
  state shared by everyone on the engine, not saved in the project. One take
  runs at a time.
- Defaults: overdub, no quantize, strength 100%, count-in 1 bar.
- Notes played more than a 16th before the song's tick 0 (during the
  count-in) are not recorded; later ones are (and wrap or quantize onto the
  downbeat).
- `replace` clears a pass's span when the pass is written, so the old notes
  still play during that pass.
- `offset_ms` compensates extra input latency (MIDI interfaces, Bluetooth)
  on top of the measured output latency.
- The 4/4 click follows the sequencer's swing; a `transport.beats_per_bar`
  can come with the song model.
- A live note is placed at the start of the audio block its command is
  applied in, so it can land up to one buffer late (a few ms at typical
  buffer sizes) on top of the latency correction. `offset_ms` covers a
  steady error.
- The engine confirms `Play` (`Feedback::Started`); until then the daemon
  ignores steps and live notes, which still count from the previous start.
- Playing again during a take writes what was played (keys still held end
  there) and keeps recording from the new start; recording into another
  instrument ends the take first; removing the instrument (including by
  undo) ends the take without writing.
- Journal (RFC 0006): each write is a `clip.update { instrument, remove,
  add, recorded: true }` entry under the user who armed, with the seat of
  the connection that armed. A transport request that ends or restarts a
  take (`stop`, `play`, `record --off`, recording elsewhere) writes it
  before the request is journaled, so the take's entry comes first.
  `4s journal replay` sends `transport.record` without `arm` (replayed notes
  are not recorded again); the `clip.update` entries reproduce the takes.
- The global loop keeps its own position when `sequencer.length` changes
  while playing (#24), but a take maps song ticks onto the loop by the
  current length; a length change during a take can place notes off
  (see docs/backlog.md).
- Live take progress (notes drawn while recording) waits for the piano roll
  (phase B).

## Implementation notes (phase B1: the song model)

Phase B is two PRs: B1 (this) is the model, engine, RPC/CLI, and a
minimal UI; B2 is the piano roll. Where these differ from the sections
above, these win:

- **Song settings are parameters**: `song.mode` (pattern/song),
  `song.loop` (0 off, 1 the song, 2 bars), `song.loop_start` and
  `song.loop_end` (bars, end exclusive). They save, undo, and show in
  `param.list` like any other; the engine reads them directly. Only the
  start point is a request (`transport.locate`), session state like the
  transport.
- **Clip pools**: clip ids are 1, 2, ... per instrument (a new clip takes
  one past the highest); names default to the id. A track always keeps one
  clip; deleting a clip deletes its placements. `clip.*` requests take an
  optional `clip` (default: the selected one), so the step editors, the
  Block grid, and existing scripts edit the selected clip.
- **Placements** don't overlap: `song.place` and `song.move` cut what the
  new placement lands on (the right part keeps playing from where it was,
  by its offset). A placement longer than its clip loops it.
- **Loops**: a bar range loops when the playhead reaches its end, so
  playing from past it goes on; the whole song loops from anywhere past its
  end. Without a loop, playback stops at the song's end, except while
  recording; a take that ends past the song's end leaves it playing on
  (silence) until stopped, as playing from past a bar loop does.
- **Recording in song mode** writes each note into the clip of the
  placement under it (at its local tick, quantized at that clip's length).
  Notes with nothing under them go into a clip the take makes: whole bars
  around them, kept inside the gap between placements, growing as the take
  goes on past it. With a loop, each pass is written a step before the
  loop's end; without one, when the take ends. A write is journaled as the
  requests that make it, in one `batch` (`clip.new`, `clip.update`,
  `song.place`, ...), so it replays and undoes in one step.
- **`batch`** is a general request: several requests as one journal entry
  and one undo step (stops at the first error; not atomic). It is also
  how the UI can make multi-request actions undo in one go.
- **Engine**: one preallocated clip table (`MAX_CLIPS` = 256) shared by
  all slots, `MAX_PLACEMENTS` = 256 per slot, edited by small commands;
  the command ring holds a full project load (about 270k commands).
  `Feedback::Step` and `Live` carry the song position.
- **Journal keys** add the clip id (`clip:`, `event:`, `selected:`,
  `place:`; see the RFC 0006 amendment). The basic-session fixture was
  re-accepted for the new keys.
- **Project format v5** (`tracks`), protocol 7.
- **UI (B1)**: a clip bar in the editor (select, new, duplicate, delete,
  rename), and pattern/song mode, loop, and start bar in the transport.
  Placing clips in the UI comes with the arrangement view (phase C).

## Implementation notes (phase B2: the piano roll)

- A **piano roll** for any clip (the editor's "piano roll" view, beside
  the step editor): rows are pitches (C1..C5, widened to fit; for the 808,
  its voices and any other notes in the clip), columns ticks on a snap grid
  (1/4..1/32, triplets, off) with a bar/beat ruler and zoom. Click to add,
  drag to move (time and pitch), drag the right edge to resize, alt-drag to
  copy, shift-click and box to select, Delete to remove, a velocity lane,
  the clip's own length, and quantizing the selection (or every note).
  Every edit is one `clip.update`, so one undo step.
- `clip.quantize` takes `events`: only those notes.
- **Take progress**: the `take_notes` event sends the notes a take has
  recorded but not written yet (where they will go, unquantized; held
  notes as long as they are so far), and an empty list once they are
  written; the piano roll draws them faded. In song mode, notes with no
  placement under them are not sent (they make a clip when written).
- The playhead follows the clip where it plays: the selected clip in
  pattern mode, the placement under the song position in song mode.

## Implementation notes (phase C, step 1: drawing the song)

- `Arrangement.tsx` sits between the console and the editor and draws the
  snapshot's tracks; it sends only `transport.locate` (ruler click,
  snapped to bar, beat, or off) and `seat.focus` (arm). Editing
  placements, the loop brace, and the piano roll panel come next.
- Placements show the clip's name and its notes, repeated where the
  placement loops the clip (`(r + offset) % clip length`), with each
  repeat's start dashed. The placement under the playhead is ringed.
- The ruler shows bars, the loop range (`song.loop` = 2), the song's end
  (the end of the last placement, as `song.get`'s `length`), and the start
  point; the playhead shows in song mode while playing.
- Zoom is pixels per bar (50% to 400%); the view is 16 bars, or 4 past the
  furthest of the song's end, the start point, and the loop end.
