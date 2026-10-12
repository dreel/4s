# RFC 0009: One transport, clip launching, and a generic step view

- Status: accepted (direction approved by Sam in the project thread,
  2026-10-11: one transport that always plays the arrangement, plus
  Ableton-style clip launching in place of pattern mode; the details below
  are open to review on this PR)
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-12
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

### 1. One transport

- `song.mode` goes. Play always plays the arrangement from the locate
  point (`transport.locate`), with the existing loop settings: `song.loop`
  0 (stop at the song's end), 1 (the whole song), 2 (`song.loop_start` ..
  `song.loop_end`, bars). The arrangement view's playhead and loop brace
  always apply.
- An empty song plays on (silence, or launched clips) until stopped, as
  today.
- Playback without a loop stops at the song's end, unless recording (as
  today) or **any track is playing a launched clip** (a launched clip plays
  until stopped).
- **Loop this placement**: a UI action (and documented CLI recipe) that
  sets the brace to the bars a placement covers (rounded out to whole bars,
  since the loop parameters are bars) and turns the loop on, as one
  `batch` of `param.set`s. No new RPC: `4s song loop A B` already does it.

### 2. Clip launching

Each track (instrument) plays either its **arrangement** or a **launched
clip**:

- `clip.launch { instrument?, clip?, now? }` (default: the caller's focus,
  its selected clip) makes the track loop that clip, from the clip's start,
  instead of its placements.
- `song.back { instrument? }` returns one track to the arrangement, or
  every track when no instrument is named (Ableton's "back to
  arrangement").
- **Quantized.** While playing, a launch or a back waits for the next
  multiple of `launch.quantize` on the song timeline: a parameter, 0 none
  (the next tick), 1 a beat, 2 a bar (default), 3 two bars, 4 four bars.
  `now: true` ignores it. While stopped it takes effect at once, and every
  launched clip starts from its beginning when the transport starts
  (after any count-in).
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
  the undo doc. Deleting a launched clip sends its track back.
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

### 7. Devices (later phase, own RFC)

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
  song back`; `launch.quantize` is a parameter (`4s set`). The parity test
  covers them.
- **Real-time safety**: a launch is a small command; the engine keeps a
  pending switch per slot in preallocated state and resolves it on the
  tick it lands (`no_alloc` covers launching and coming back).
- **Close the loop**: `4s render` plays launched clips and the
  arrangement together, so launching, quantize, and back are verified
  from triggers in a render, and live through the CLI and Electron e2e.
- **Modularity**: note maps and declared expressions move instrument
  specifics out of the protocol and the UI (phase 2); devices follow.
- **Network transparency**: no impact; all state is in the daemon.

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
  `clip.launch`, `song.back`, `launch.quantize`; `Clip.length` and
  `ClipHeader.length` are numbers; `clip.length` takes a number;
  `TrackInfo` has `launched` and `pending`; `TransportState` and the
  `playhead` event have `clips`, and `step` is the song position in
  steps; `clip.select` no longer changes playback.
- **CLI**: `4s song mode` goes; `4s clip launch`, `4s song back`, `4s
  clip length <steps>` (no `--follow`); `4s set sequencer.length` is gone.
- **Journals**: recordings made before protocol 8 that set `song.mode` or
  `sequencer.length` no longer replay; the checked-in goldens are
  re-recorded.

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
2. Transport unification and clip launching (sections 1-4), with the v6
   migration, protocol 8, CLI, e2e, and UI (no pattern/song toggle, launch
   and back controls, loop this placement).
3. The generic step view, note maps, declared expressions, and the Block
   on clips (sections 5-6).
4. Devices: a follow-up RFC.

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
