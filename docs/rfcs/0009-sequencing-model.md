# RFC 0009: One transport, clip launching, and a generic step view

- Status: accepted (phases 1 and 2 implemented; see "Implementation notes")
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-11
- Discussion: the PR that introduces this RFC.

## Summary

4S has two ways to play: pattern mode (each track loops its selected clip)
and song mode (the arrangement). This RFC replaces them with **one
transport that always plays the arrangement**, plus **clip launching**:
any track can be told to loop one of its clips instead of following the
arrangement, starting on the next bar, until it goes **back to the
arrangement**. The selected clip becomes only what editors and the Block
edit. Clips get **explicit lengths** (no more `sequencer.length`). The
drum and bass step editors become **one generic step view** over any clip,
with rows from the instrument's **note map** (drum voices) or a pitch
range, and extra rows for **note expressions** an instrument declares
(the 303's accent and slide). The Livid Block edits the **armed track's
playing clip**. **Devices** (note FX, audio inserts, sends, modulators,
automation lanes) are a later phase with their own RFC.

## Motivation

Pressing play in pattern mode ignores the arrangement, the loop brace, and
the playhead in the arrangement view; switching to song mode makes the step
editors edit a clip that may not be playing. Two playback models run
side by side in the engine, the recorder, take progress, and the UI. The
selected clip does double duty (edit target and what pattern mode plays),
so opening a clip to look at it changes what you hear. Step-grid globals
(`sequencer.length`, `Feedback::Step`'s step within it) and 303 semantics
baked into the protocol (accent = velocity >= 121, slide = a note held past
the next step) make every new instrument a special case, and the Block only
works on drum machines.

What pattern mode was good for, looping a few clips while you build them
up, is what Ableton's session view does with clip launching, and it can
sit on top of the arrangement instead of beside it.

## Design

Sections 1-4 are phase 1; sections 5-6 phase 2; section 7 phase 3, with
its own RFC.

### 1. One transport

- `song.mode` goes. Play always plays the arrangement from the locate
  point (`transport.locate`), with the existing loop settings: `song.loop`
  0 (stop at the song's end), 1 (the whole song), 2 (`song.loop_start` ..
  `song.loop_end`, bars). The arrangement view's playhead and loop brace
  always apply.
- An empty song plays on (silence, or launched clips) until stopped, as
  today. Playing from past a bar loop's end goes on without looping or
  stopping, as today: the stop rule below applies only without a loop.
- Playback without a loop stops at the song's end, unless recording (as
  today) or **any track is playing a launched clip** (a launched clip plays
  until stopped). A return to the arrangement that is still waiting for
  its boundary counts as launched: past the song's end, the transport stops
  when the last one lands.
- **Loop this placement**: a UI action (and documented CLI recipe) that
  sets the brace to the bars a placement covers (rounded out to whole bars,
  since the loop parameters are bars) and turns the loop on, as one
  `batch` of `param.set`s. No new RPC: `4s song loop A B` already does it.

### 2. Clip launching

Each track (instrument) plays either its **arrangement** or a **launched
clip**:

- `clip.launch { instrument?, clip?, now? }` (default: the caller's focus,
  its selected clip) makes the track loop that clip, from the clip's start,
  instead of its placements. The selected clip is per track, shared by
  everyone, and the journal records the instrument and clip a launch
  resolved to, so a replay launches the same clip whatever the replaying
  client's focus.
- `song.back { instrument? }` returns one track to the arrangement, or
  every track when no instrument is named (Ableton's "back to
  arrangement").
- **Quantized.** While playing, a launch or a back waits for the next
  multiple of `transport.launch_quantize` on the song timeline: a
  parameter, 0 none (the next tick), 1 a beat, 2 a bar (default), 3 two
  bars, 4 four bars.
  `now: true` ignores it. While stopped it takes effect at once, and every
  launched clip starts from its beginning when the transport starts
  (after any count-in). The boundary is counted on the song position:
  a switch lands on the first tick whose song position is a multiple of
  the quantum. Loops start and end on whole bars, so with beat or bar
  quantize a wrap lands on a boundary; a two- or four-bar quantum with a
  loop that is not a multiple of it lands on the next song position that
  is (so it can take more than one pass, as in Ableton with a short loop).
  A locate lands waiting switches on the first boundary at or after the
  new position.
- A launched clip loops at its own length, counted from the tick it
  started, whatever the arrangement does: a song loop wrapping or a locate
  does not move it. Coming back, the track picks up the arrangement where
  the song position is (a placement already under way plays from that
  point). A switch ends the notes the track's clips were playing.
- **State.** `TrackInfo` gains `launched` (the clip id, or null for the
  arrangement) and `pending` (true while a launch or back is waiting for
  its boundary). The engine reports when a switch lands
  (`Feedback::Launched`), and the daemon emits `track` then.
- **Saved, journaled, not undone.** `launched` is saved in the project
  (so a project reopens sounding as it was saved) and every launch is a
  journaled request (so a replay reproduces it), but launching is a
  performance action like the transport and the metronome: it is not in
  the undo doc. Deleting a launched clip sends its track back; undoing the
  delete brings the clip back but does not launch it again (launch it
  again by hand), which keeps undo out of performance state entirely.
- `clip.select` no longer affects playback: the selected clip is only
  the edit target (step views, the piano roll panel, the Block, and the
  `clip.*` defaults).
- A new project launches its drum machine's first clip, so a fresh
  daemon plays a beat on play, as pattern mode did. `instrument.add` does
  not launch anything (see Open questions).

### 3. Recording

A take into a track that is playing a launched clip records into that
clip, looping at its length from where it started (the old pattern-mode
path, offset by the launch's start); otherwise it records into the
arrangement (the old song-mode path: the placement under each note, or a
clip the take makes). When the track switches during a take (a launch or
back lands), the take writes what was played and goes on into the new
target, as a locate does today.

### 4. Explicit clip lengths

- Every clip has a length in ticks (`1..MAX_CLIP_TICKS`); `Clip.length`
  and `ClipHeader.length` are numbers, not nullable. `clip.length` takes a
  length; `clip.new` defaults to one bar.
- `sequencer.length` goes. Step views (`pattern.*`) show the selected
  clip's length in whole steps (rounded up), and their results report it.
- Swing stays a global (per 16th, on the song timeline).
- The playhead event and the snapshot's transport give the song position
  and, per track, the clip it is playing and the tick in that clip
  (`clips`), so editors draw a playhead without knowing how the track got
  there. `step` is the song position in steps.

### 5. One step view (phase 2)

- `DrumEditor` and `BassEditor` become one `StepView` over any clip: a
  grid of steps at a resolution (1/16 default, 1/8, 1/32, triplets) over
  the clip's length, scrolling and paging like the piano roll.
- **Rows** come from the instrument: a **note map** for drum machines (the
  808's voices: name, short name, note; later sampler pads), or a pitch
  range for melodic instruments, one row per pitch (with a per-step note
  column for monophonic instruments as the bass editor has now).
- **Note expressions** an instrument declares get a row of toggles under
  the grid: the 303 declares `accent` (velocity at or above a threshold)
  and `slide` (the note held into the next one). The protocol describes
  them (`instrument.types` and the graph), so the step view, the Block,
  and the CLI read them instead of hard-coding 808 and 303 rules. The step
  RPCs (`pattern.*`) become one generic set over a clip's grid; the old
  methods stay as thin aliases until the UI no longer uses them.
- The step view edits the selected clip and shows the playhead where that
  clip is playing.

### 6. The Block on clips (phase 2)

- The grid edits the **armed track's playing clip**: the launched clip, or
  the placement's clip under the playhead, falling back to the selected
  clip when the track plays nothing there. Pages follow the playhead.
- Drum machines show one voice per row (as now); melodic instruments show
  8 pitch rows around the clip's notes, the knobs' last page moving them.
- A **pad mode** (a shifted page or a dedicated button) shows each
  track's clips as pads: press to launch, the track's row end to go back
  to the arrangement.

### 7. Devices (phase 3, own RFC)

Recorded here so the clip work does not paint itself into a corner:

- A track becomes a chain: note input (clips, live MIDI) -> note FX ->
  one instrument -> audio FX inserts -> its mixer channel, plus sends to
  buses.
- Modulators (LFO banks, envelopes, macros) route to any parameter path
  with a depth, evaluated per block on the audio thread.
- Devices get typed ports (notes, audio, control) and stable ids, so a
  full modular graph later is a UI and scheduling change, not a new model.
- Clips gain automation lanes (parameter path -> breakpoints), played like
  note events.

This RFC changes none of that; it only keeps instruments behind the
`Instrument` trait and parameters addressable by path.

## Impact on the principles

- **Rust daemon owns everything real / thin UI**: launching, quantizing,
  and switching happen in the engine and daemon; the UI only sends
  `clip.launch` and `song.back` and draws `launched`, `pending`, and the
  playhead's `clips`.
- **API parity**: `clip.launch` -> `4s clip launch`, `song.back` -> `4s
  song back`; `transport.launch_quantize` is a parameter (`4s set`). The
  parity test covers them.
- **Real-time safety**: a launch is a small command; the engine keeps a
  pending switch per slot in preallocated state and resolves it on the
  tick it lands (`no_alloc` covers launching and coming back).
- **Close the loop**: `4s render` plays launched clips and the
  arrangement together, so launching, quantize, and back are verified
  from triggers in a render, and live through the CLI and Electron e2e.
- **Modularity**: note maps and declared expressions move instrument
  specifics out of the protocol and the UI (phase 2); devices follow.
- **Network transparency**: all state is in the daemon. `launched` and
  `pending` are in each `TrackInfo` and the playhead's `clips` in
  `TransportState`, so a client that reconnects gets them from
  `state.get` like everything else.

## Alternatives

- **Keep pattern mode and fix its edges** (option B in the thread):
  keeps two playback models and the selected clip's double duty.
- **A step sequencer device** that plays outside the arrangement
  (option D): pattern mode again under another name, as the main way to
  make loops.
- **Scenes** (rows of clips launched together): useful later on top of
  per-track launching; not needed to replace pattern mode.
- **Launching makes undo steps**: Ableton does not undo launches, and a
  performance would fill the undo stack.

## Migration and compatibility

- **Project format v6**: drops `song.mode` and `sequencer.length` from
  `params`. A project saved in pattern mode (`song.mode` 0 or absent)
  launches each track's selected clip, so it sounds as it did (an
  instrument with no track gets one, with its empty clip launched). A
  clip without a length gets the project's `sequencer.length` (default
  16 steps) in ticks. Tracks gain `launched`. Projects saved in song mode
  launch nothing.
- **Protocol 8**: `song.mode` and `sequencer.length` are gone;
  `clip.launch`, `song.back`, `transport.launch_quantize`; `Clip.length` and
  `ClipHeader.length` are numbers; `clip.length` takes a number;
  `TrackInfo` has `launched` and `pending`; `TransportState` and the
  `playhead` event have `clips`, and `step` is the song position in
  steps; `clip.select` no longer changes playback.
- **CLI**: `4s song mode` goes; `4s clip launch`, `4s song back`, `4s
  clip length <steps>` (no `--follow`); `4s set sequencer.length` is gone.
- **Journals**: recordings made before protocol 8 that set `song.mode` or
  `sequencer.length` no longer replay; the checked-in goldens are
  re-recorded. Parameters are saved, journaled, and bound (MIDI CC maps,
  knob pages) by path, never by the engine's global index, so removing two
  globals needs no other migration.

## Validation plan

- Engine tests: a launched clip loops from its start while the
  arrangement plays the other track; a launch while playing waits for the
  bar; back returns mid-placement; no song end while launched.
- `scripts/e2e-cli.sh`: `4s clip launch` / `4s song back` change what
  `4s render` triggers; quantize from `render --input` timing; recording
  into a launched clip vs the arrangement; loading a v5 pattern-mode
  project plays the same triggers as before.
- Electron e2e: launch and back buttons, the pending state, loop this
  placement, no pattern/song toggle; one undo step for loop-this.
- Phase 2: the step view on both instruments from the same component;
  the Block's grid on a 303 clip via `virtual_block`.

## Implementation plan

1. This RFC.
2. Phase 1: transport unification and clip launching (sections 1-4), with
   the v6 migration, protocol 8, CLI, e2e, and UI (no pattern/song toggle,
   launch and back controls, loop this placement).
3. Phase 2: the generic step view, note maps, declared expressions, and
   the Block on clips (sections 5-6).
4. Phase 3: devices, in a follow-up RFC.

## Open questions

- Should `instrument.add` launch the new track's first clip, so a new
  instrument sounds on play straight away? This RFC says no (a new track
  follows the arrangement, which is empty for it), and the UI shows a
  launch button on every clip; a fresh project launches its drums.
- Should launching while stopped start the transport, as in Ableton? This
  RFC says no: the transport has one control, and launching while stopped
  only sets what plays.
- A "stop clip" (a track silent, neither arrangement nor a clip) is left
  out until someone needs it.

## Implementation notes (phase 1: one transport and clip launching)

Where these differ from the sections above, these win:

- **Undo and launches.** `launched:<id>` is an undo key, but `clip.launch`
  and `song.back` do not build the doc (`changes_doc`), so launching never
  makes an undo step. Changes other requests make to it are undone like
  any key: undoing the removal of an instrument, or the deletion of a
  launched clip, launches it again. (Section 2 said such an undo would not
  relaunch; with it, undoing a removal left a silent instrument.) Inside a
  `batch` the doc is built for the whole batch, so a launch there is part
  of the batch's undo step.
- **Parameters and state.** `transport.launch_quantize` (the path is under
  `transport.`, a reserved id, so no instrument can shadow it) is an
  ordinary parameter: saved and undone. Removing an instrument resets its
  engine slot (launch and pending switch included) and drops its track; a
  project load or new sets every track's launch from the file (the
  default project launches its drums). The daemon
  keeps each track's `launched` (asked for) and what the engine plays
  (`Feedback::Launched`: clip and start tick); `pending` is the two
  differing while playing. Play and stop settle every track at once, as
  the engine does.
- **Playheads.** `TransportState.clips` and the `playhead` event's `clips`
  map each playing track to `{clip, tick}`; editors draw the playhead only
  when the track plays their (selected) clip. The Block grid edits the
  selected clip until phase 2, and shows its playhead the same way.
- **Recording.** A take's target is what its track plays at that moment:
  `TakeTarget::Clip { clip, length, start }` for a launched clip (start:
  the tick it started), else the arrangement. When the track switches
  during a take, the take writes what was played up to the switch and goes
  on into the new target. A render (`render --record`) records into the
  launched clip from tick 0, or the arrangement.
- **Project format v6, protocol 8** as in "Migration". The default project
  launches its drums' clip; `instrument.add` launches nothing (the open
  question stands). Clip lengths are always saved; step patterns are used
  only for clips a whole number of steps long.
- **CLI**: `4s clip launch [instrument] [clip] [--now]`, `4s song back
  [instrument] [--now]`; `4s clip list` and `4s song` say what each track
  plays and whether it is waiting for its boundary; `4s watch --type
  playhead` shows each track's clip and step.
- **UI**: the transport loses the pattern/song toggle and the global
  length; it gains "back to song" (every track) and the launch quantize.
  The editor's clip bar launches the selected clip and goes back, and the
  selected clip's length is set there in steps (8, 16, 32, 64). In the
  arrangement each pool clip has a launch triangle, the launched clip and
  its lane show it (pulsing while it waits), the header has "back", and
  "loop" (or L) sets the loop brace over the selected placements.

## Implementation notes (phase 2: the step view and the Block on clips)

- **Layouts** are declared in `crates/protocol/src/layout.rs`
  (`InstrumentType::layout`) and sent with every instrument
  (`InstrumentInfo.layout`, `instrument.types`): named `rows`, a pitch
  range (`low`, `high`), `mono`, a new note's `gate` (fraction of a step)
  and `velocity`, and `expressions` (`velocity` kind with a threshold and
  the values it writes, or `overlap`). `step_cells` and `set_step` there
  are the one reading and editing of a step, used by the daemon and the
  Block; the old `pattern.*` methods still work on 16ths and agree with
  them (a unit test checks the declared layouts write the same events).
- **RPC**: `step.get { instrument?, clip?, resolution? } -> StepGrid` and
  `step.set { instrument?, clip?, resolution?, step, note?, on?,
  expressions? } -> StepGrid`. CLI: `4s step [instrument] [step] [note]
  [--off] [--set x] [--clear x] [--res 1/8] [--clip n]`.
- **UI**: `StepView.tsx` replaces the drum and bass step grids: rows for
  named notes (the 808 panel puts audition, mute, and output beside each),
  a column per step with a note and a toggle per expression for mono
  instruments, and a row per pitch otherwise; a resolution select (1/8,
  1/16, 1/16t, 1/32). The instrument panels keep their knobs.
- **The Block**: `ControllerState` gains `grid` (`steps` or `launch`),
  `clip` (the clip the step grid edits), `rows` (each row's label),
  `length`, and `base_note`; `controller.set_mode` takes `grid` and
  `base_note`. The mode is session state (not saved), like the page. No
  hardware button switches the mode or moves the pitch rows yet; the knobs
  keep the focus's knob page.

