# RFC 0007: MIDI bindings, seats, clips, and recording

- Status: accepted (phases 1 and 2 implemented; see "Implementation notes")
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-07
- Discussion: the PR that introduces this RFC. Merging it with
  `Status: accepted` is the maintainer's approval.

## Summary

Give every instrument one input vocabulary (note on/off), and route MIDI
input to instruments through **bindings** instead of hard-wired device
kinds. Bindings name **logical devices** ("keys", "pads") and belong to a
**seat** (one performer's setup) saved in the project; each machine maps its
physical ports to logical device names. Move sequencing out of instruments
into **clips** of timed note events on a tick clock, played by the
sequencer into any instrument. With live input and the sequencer speaking
the same events, add **recording** (overdub/replace, quantize) and Standard
MIDI File import/export.

## Motivation

Today each MIDI path assumes one instrument type:

- `DeviceKind` decides behavior per port: `livid_block` edits the 808
  pattern of `controller.target`, `generic_drums` triggers 808 voices from
  GM notes, `keyboard` plays a `tb303` (the first one unless set). Dispatch
  is a `match` in `Core::handle_midi`.
- There is no channel filtering (except the Block's map), one target per
  port, and no CC mapping outside the Block's fixed `KnobMode`.
- The daemon tracks held notes per instrument slot, which assumes a
  monophonic note instrument.
- Patterns live inside instruments (`set_drum_step`, `set_notes`,
  `on_step`): the 808 owns an 8x64 level grid, the 303 a `NoteStep[64]`.
  A new instrument type must invent its own pattern format, RPCs, CLI, and
  UI editor. One global `sequencer.length` applies to every pattern.
- Nothing records.

Every new controller (a pad box, a second keyboard, a knob box) and every
new instrument type multiplies these special cases. Multiplayer
([topology.md](../topology.md)) makes it worse: setups differ per laptop,
yet a performer wants their setup saved with the project.

## Design

### 1. One input vocabulary

Every instrument accepts:

```
note_on(note: u8, velocity: f32)
note_off(note: u8)
```

(`all_notes_off` was dropped in phase 1: nothing needed it.)

(Pitch bend and per-note expression can follow; not in scope.)

Voice allocation and note priority belong to the instrument:

- `tr808`: notes select voices through the GM drum map (`Voice::gm_note`).
  `note_off` is ignored (drums are one-shot).
- `tb303`: monophonic with a note stack and last-note priority. A
  `note_on` while another note is held glides (legato); releasing the top
  note returns to the one below. Velocity >= 0.95 is accent (as today).
- Future polyphonic instruments allocate voices themselves.

`voice.trigger` becomes a convenience that sends the voice's GM note.
`voice.note_on` / `voice.note_off` keep their names and holder semantics,
but holders now hold `(instrument, note)` pairs instead of one note per
instrument slot, so polyphonic instruments and several players work.
`Instrument::trigger` and the per-slot `held_notes` go away.

### 2. Hardware, seats, and runtime

Three layers, split by what each describes:

| Layer | Holds | Stored |
|---|---|---|
| Hardware | physical port -> logical device name; surface profile; Block note/LED map | per machine: `<data-dir>/midi-devices.json` on the daemon that owns the port (solo daemon or bridge) |
| Seat | one performer's bindings, CC maps, and focus | project file |
| Runtime | which connected client sits in which seat | engine memory |

**Hardware.** When a port appears, the daemon that owns it looks it up in
`midi-devices.json`:

```json
{
  "devices": {
    "Arturia KeyStep 37": { "name": "keys" },
    "Block":              { "name": "block", "profile": "livid_block" }
  }
}
```

An unknown port gets a name derived from its port name (lowercased,
`[a-z0-9_]`, e.g. `arturia_keystep_37`) and is written back so the user can
rename it (`4s midi rename <port> keys`). A port containing "block" gets the
`livid_block` profile, as autoconnect does today. `livid-block.json` (the
note/LED map) stays as the profile's hardware map.

**Seats.** A seat is a named performer setup in the project:

```json
"seats": {
  "sam": {
    "focus": "bass",
    "bindings": [
      { "device": "pads", "channel": 10, "map": "gm_drums", "target": "drums" },
      { "device": "keys", "notes": "C1..B2", "target": "bass" },
      { "device": "keys", "notes": "C3..C8", "transpose": -12, "target": "focus" }
    ],
    "cc": [
      { "device": "knobs", "channel": null, "cc": 21, "param": "bass.cutoff" }
    ]
  }
}
```

Binding fields:

- `device`: logical device name.
- `channel`: 1-16, or absent for any channel.
- `notes`: inclusive range, absent for all notes.
- `transpose`: semitones, default 0.
- `map`: `identity` (default) or `gm_drums` (passes GM notes through
  unchanged; the field documents intent and lets a non-GM pad layout be
  remapped later).
- `target`: an instrument id, or `focus` (the seat's focused instrument).

A message goes to every binding that matches, so layers and splits both
work. A device with no binding in the seat plays the seat's focus on any
channel: plugging in a keyboard just works.

CC maps bind `(device, channel?, cc)` to a parameter path, scaled over the
parameter's range (as `controller.knob` scales today). `midi.learn <path>`
binds the next CC that moves on any of the seat's devices.

**Focus.** Each seat has a focused instrument, the target of `focus`
bindings and of a surface like the Block. It replaces
`controller.target`, which today can only be a `tr808`.

**Runtime.** A client joins a seat when it connects:

- If exactly one seat in the project matches the client's display name
  (case-insensitive), the client joins it automatically.
- Otherwise (no match, or several candidates) the client is not seated
  yet. `session.hello` returns `seat: null` plus the seat list, and the
  UI asks: join an existing seat, create a new seat (saved in the
  project, named after the client by default), or ignore.
- The "create" and "ignore" choices are always offered, even after an
  automatic join (`seat.claim`, `seat.create`, `seat.leave`).
- **Ignore** puts the client in a session-only seat: zero-config defaults
  (devices play its focus), never saved to the project.
- The CLI never prompts: `--seat <name>`, `--new-seat`, or
  `--no-seat`; without them it auto-joins on a unique match and
  otherwise stays unseated (`4s seat claim` later).
- In solo use with a fresh project, the host user's seat is created and
  the local client joins it (see "Implementation notes").

A bridge claims seats for its local clients. Two clients can share a
seat. A seat in the project with no one in it does nothing.

**Where bindings are resolved.** The engine. A bridge forwards raw input
upstream with an engine-time timestamp:

```
midi.input { device: "keys", seat: "sam", data: [0x90, 60, 100], time }
```

and applies LED output coming back for surfaces. A solo daemon calls the
same handler directly. This keeps one source of truth, lets every client
see every seat's bindings, and gives recording exact engine timing. The
cost (raw CC streams over the network) is small next to audio and events.

**Remote input latency.** Input from a bridge plays at its timestamp
plus a fixed delay, so network jitter is absorbed instead of heard:

- Default 20 ms, set per bridge (`4sd --input-latency <ms>`, 0..100) and
  shown in `seat.list`. Local input (solo daemon, or a device on the
  engine host) has no added delay.
- An event that arrives later than its slot plays immediately and is
  counted (`engine.status` reports late input), so a user can tell when
  to raise the delay.
- Recording uses the event's timestamp, not when it played, so takes are
  placed correctly whatever the delay.
- Later, the default can follow measured jitter (for example p99 from the
  clock-sync pings, clamped to 5..50 ms); a fixed value comes first
  because it is predictable to play against.

**Knob pages.** Each instrument type declares named pages of up to 8
parameters each (paths relative to the instance):

- `tr808`: `volume`, `tune`, `decay`, `tone`, each one parameter per
  voice (today's `KnobMode` values).
- `tb303`: `main` (cutoff, resonance, env mod, decay, accent, tune,
  waveform).

A device (or a range of its CCs) can be set to **follow focus**: its
knobs control the current page of the seat's focused instrument, so
selecting the bass turns them into bass knobs. Fixed CC maps sit beside
this for knobs pinned to one parameter. `KnobMode` becomes the page
(`controller.set_mode --knob-mode` -> `seat.page`), and pages show up in
`instrument.types` so the UI and CLI can list them.

**Pots and pickup.** Phase 1 supports absolute knobs (0-127). Pickup is on
by default: after a parameter changes elsewhere (UI, CLI, another seat, a
page or focus switch), a knob does nothing until it passes the current
value, so it never jumps the sound. Each CC map and follow-focus range has
`mode: "absolute"` and `pickup: true|false`; relative (endless encoder)
modes can be added to `mode` later without a format change.

**Surfaces.** The Block profile stays special: its grid edits the focus
instrument's clip (section 3) and its knobs follow focus by default.

### 3. Clips on a tick clock

**Clock.** The sequencer counts ticks at 96 PPQ (24 per 16th). Swing stays
a property of the tick-to-sample map (offbeat 16ths delayed), so existing
step patterns sound identical.

**Clips.** A clip is a sorted list of events plus a length:

```rust
struct ClipEvent { tick: u32, len: u32, note: u8, velocity: u8 }
struct Clip { length: u32 /* ticks */, events: Vec<ClipEvent> }
```

There is one clip per instrument for now (a **track**), with its own
length, so polymeters are possible. `sequencer.length` remains as the
default length for new clips and the length the step editors show.

**Playback.** On the audio thread the sequencer walks each track's clip
and emits `note_on` / `note_off` at the exact sample inside the block.
Clips are built on the control side as fixed-capacity arrays (cap, e.g.,
1024 events) and swapped in through the command ring; the old clip comes
back and is dropped on the control side, as instruments are today.

**Instruments lose sequencing.** `set_drum_step`, `set_drum_track`,
`set_notes`, and `on_step` are removed from `Instrument`. A new instrument
type only implements sound.

**Step editors are views over clips.** The existing pattern RPCs keep
working as editors:

- Drum grid: a step on track `voice` = an event at `step * 24` on the
  voice's GM note, `len` 1 step, velocity 100 (on) or 127 (accent).
- Note steps (303): a step = an event at `step * 24`, `len` half a step;
  accent = velocity 127; slide = `len` extends past the next event's start,
  which the 303 plays as a glide (legato), the same as real 303s do over
  MIDI.
- `x---X---` and `C2 C2! D#2~ -` strings stay as CLI/project shorthand.

New clip RPCs work with events directly: `clip.get`, `clip.set`,
`clip.add`, `clip.remove`, `clip.length`, `clip.clear`,
`clip.quantize`.

### 4. Recording

```
transport.record { arm: bool, track?: id, mode: overdub|replace,
                   quantize: off|1/16|1/8|..., count_in: bars }
```

- The armed track defaults to the recording seat's focus.
- Live notes already pass through the engine. The engine reports, in
  `Feedback`, the clip tick at which each live note sounded (the existing
  `Trigger` feedback gains `tick` and `note_off`), and the daemon writes
  events from that, minus the output latency, so notes land where the
  player heard them.
- Recording loops at the clip length. `replace` clears the region it
  passes over; `overdub` adds. Each pass (take) is one undo step
  (`clip.undo`).
- `clip.export` / `clip.import` read and write Standard MIDI Files (crate
  `midly`), on the engine host like `4s render`.

### RPC / CLI summary

| RPC | CLI |
|---|---|
| `midi.ports` (now lists device names) | `4s midi ports` |
| `midi.rename` | `4s midi rename <device> <name>` |
| `midi.set_seat` | `4s midi seat [<seat>]` |
| `midi.input` | `4s midi send <device> <hex bytes...>` |
| `seat.list/claim/create/leave/remove` | `4s seat [list/claim/create/leave/rm]` |
| `seat.bind`, `seat.unbind` | `4s bind <device> ...`, `4s unbind <n>` |
| `seat.map_cc/unmap_cc/learn_cc` | `4s cc map/unmap/learn` |
| `seat.focus` | `4s focus <id>` |
| `seat.page`, `seat.follow_knobs` | `4s knobs page <name>`, `4s knobs follow <device> [ccs]` |
| `clip.*` | `4s clip ...` |
| `transport.record` | `4s record ...` |

`DeviceKind::{generic_drums, keyboard}` and `MidiConnection.instrument`
are removed; `midi.connect` keeps opening ports. `controller.set_mode
--target` becomes `seat.focus`.

## Impact on the principles

- **Agent-drivable / API parity**: every new capability is an RPC with a
  CLI command. `midi.input` lets an agent play any logical device without
  hardware; the existing `voice.note_on` path remains.
- **Multiplayer / network transparency**: seats are the multiplayer model
  for input. Hardware mapping stays on the machine that owns the port;
  bindings travel with the project; the engine resolves everything.
  Updates `topology.md` ("Controller bridge": raw input is forwarded, not
  pre-resolved).
- **Real-time safety**: clips are preallocated and swapped like
  instruments. Note stacks are fixed-size arrays inside instruments.
  Binding resolution happens on the control thread.
- **Validation**: virtual MIDI keyboard ports and `midi.input`, `4s render`
  onsets and triggers, project round-trips, SMF round-trips (below).
- **Modularity**: instruments become pure sound nodes with one input
  vocabulary; the sequencer, MIDI, and recording are sources of note
  events. New instrument types need no sequencing code.

## Alternatives

- **Keep per-instrument patterns, generalize steps** (any notes per step,
  plus velocity and length). Smaller change, but recording must be
  quantized and every instrument still owns its sequencing.
- **All bindings in the project, keyed by port name.** Simple, but port
  names differ per machine and per collaborator, so projects need
  re-binding when opened elsewhere.
- **All bindings per machine.** Loses your setup when a project moves.
- **Resolve bindings at the bridge** (send `voice.note_on` / `param.set`
  upstream). Lighter on the network, but bindings live outside the engine,
  other clients cannot see them, and recording timing is less exact.
- **Multiple clips per track with pattern switching now.** Deferred; the
  clip model allows it later without a format break.

## Migration and compatibility

- `PROJECT_FORMAT_VERSION` 2 -> 3 with a migration: drum step strings and
  note strings become clip events (as defined above, phase 2);
  `controller.target` and `knob_mode` are dropped (phase 1: seats start
  focused on the first instrument); `seats` added. v2 projects load and
  render identically.
- `PROTOCOL_VERSION` 2 -> 3: `DeviceKind` variants, `MidiConnection`,
  `ControllerState.target`, `PatternData` (gains clips) change.
- `livid-block.json` unchanged.
- Docs: engine.md, architecture.md, topology.md, extending.md (new
  instrument recipe no longer covers patterns), hardware/livid-block.md,
  project-format.md.

## Phasing

Each phase is one PR, verified on its own:

1. Note-input vocabulary, hardware names, seats, bindings, CC maps and
   learn, focus. Replaces `DeviceKind` dispatch. Project v3 adds `seats`.
2. Tick clock and clips; patterns move out of instruments; step editors
   become clip views; Block grid edits the focus clip; v2 -> v3 pattern
   migration.
3. Recording (arm, overdub/replace, quantize, latency compensation, undo)
   and SMF import/export.
4. Later: several clips per track and pattern switching/chains, MIDI-out
   instruments (external synths as sequencer targets), MIDI clock sync.

## Validation plan

- A virtual MIDI keyboard (like `virtual_block`) sending on several
  channels; `4s render` triggers show each binding reaching the right
  instrument, including splits, transpose, and focus.
- `cc learn`, then a CC from the virtual device changes the parameter
  (`4s param get`).
- Two CLI clients claiming different seats with different focus play
  different instruments.
- Every v2 project in the tests loads as v3 and renders the same onsets
  and trigger times.
- Recording: scripted `midi.input` events during play, then `4s clip get`
  shows events at the expected ticks (quantized and not); SMF export then
  import round-trips.

## Open questions

- None blocking. Decided during review: seat joining (section 2,
  "Runtime"), remote input latency (20 ms default), knob pages replacing
  `KnobMode`, absolute knobs with pickup first.

## Implementation notes (phase 1)

Built as designed, with these decisions made along the way (where they
differ from the sections above, these win):

- **RPC names**: every seat edit is a `seat.*` method taking an optional
  `seat` (default: the caller's); the table above lists the final names.
  The CLI's global `--user`, `--seat`, `--new-seat`, and `--no-seat` flags
  set how a command is seated.
- **No `map` field** on bindings: GM drum notes already reach the 808 as
  they are, so `identity` was the only behavior. It can be added when a
  non-GM pad layout needs remapping.
- **Who joins what**: a client joins the one seat matching its `user`
  (exactly, any case, or as a slug). In a project with no saved seats other
  than the host's untouched one, and nobody seated yet, the first user gets
  a new seat without being asked. Otherwise `choose_seat` is set and the UI
  shows a chooser (join, create, ignore). After a project load, clients
  whose seat is gone are seated again by the same rule.
- **Host seat** (the engine host's own devices): pinned with
  `midi.set_seat`, else the seat of the latest local (loopback) client of
  the daemon's own user that was not told a seat (`--seat`), or that chose
  one itself (`seat.claim`/`seat.create`), else the seat matching the daemon's OS user (`FOURS_USER`
  overrides it), else a new seat for that user in a project without seats,
  else a session-only `local` seat. Session-only seats nobody sits in are
  dropped.
- **Saving**: seats with nothing set are not written; they come back by
  themselves for whoever uses the project.
- **Device names**: `midi-devices.json` also records `auto_connect`: a port
  connected by hand is reconnected automatically when it reappears (unless
  `--no-midi`), until it is disconnected by hand.
- **Pickup** applies to MIDI knobs (CC maps, following knobs, the Block's
  knobs). `controller.knob` (virtual knobs, scripts) sets the value
  directly.
- **Reserved id**: `focus` is now a reserved instrument id (it names the
  seat's focus in bindings). A project with an instrument called `focus`
  fails to load with "instrument id 'focus' is reserved"; rename it in the
  file. No migration, as no project is known to use it.
- **Device names**: connecting a port with a name that a now-absent port
  had moves the name to the new port (a replacement keyboard keeps `keys`).
- **Journal and undo** (RFC 0006, merged alongside): device input is
  journaled as `midi.input`, and saved seat configs are undoable doc keys
  (`seat:<name>`); see RFC 0006's amendment. The journal's `user` and the
  seat-matching `user` are the same `session.hello` field.
- **Numbering**: drafted as RFC 0006; renumbered 0007 because the journal
  RFC took 0006 first.
- **Not built yet**: the bridge itself, so the remote input delay exists
  only as the design above.

## Implementation notes (phase 2)

Clips on a tick clock, as designed in section 3, with these details (where
they differ from the sections above, these win):

- **Clock**: 96 PPQ, 24 ticks per step. The engine fires every tick (notes
  ending there are released first, then events starting there play), so
  timing is sample-accurate to the tick. Swing is applied per step pair as
  before, counted from the pattern's first step.
- **No clip boxes**: each slot's clip is a `Vec` preallocated to
  `MAX_EVENTS` (1024) in `Engine::new`, edited by `ClearClip`, `AddEvent`,
  `RemoveEvent`, and `SetClipLength` commands, so nothing is allocated or
  freed on the audio thread (`no_alloc` fills a clip to capacity). The
  command ring grew from 4096 to 32768 commands, so loading a project with
  every clip full fits in one go.
- **Step views** (`crates/protocol/src/clip.rs`): drum step = event on the
  step's first tick at the voice's GM note, one step long, velocity 89 (the
  0.7 steps always played at) or 127 (accent; 121 and up reads as accent,
  the 0.95 at which the 303 plays one);
  303 step = half a step long, slide = 25 ticks (one tick past the next
  step). Renders of existing patterns are unchanged (the e2e level and
  onset checks still pass). `pattern.*` edits replace only the view's
  events, so notes off the grid survive step edits.
- **Same note twice**: when a clip note ends while a later clip note on the
  same pitch still sounds, no note-off is sent, so overlaps never cut a note
  short. Quantize wraps a note that would land on the loop's end to its
  start. The Block grid shows the focus clip's own length.
- **Events**: every clip edit emits `clip_changed`, plus the step view's
  `step_changed` / `pattern_changed` / `notes_changed` so editors that only
  know steps keep working. The UI notes how many events are off the grid
  and a clip's own length.
- **Undo keys**: `event:<inst>.<tick>.<note>` and `clip:<inst>` replace
  `step:` and `note:` (RFC 0006 amendment).
- **Project format v4**: an instrument's clip is saved as a step pattern
  when that says exactly the same thing (and the clip follows
  `sequencer.length`), else under `clips` as `tick:note:len:vel` tokens.
- **RPC/CLI**: `clip.get/set/add/remove/length/clear/quantize`, `4s clip
  show/set/add/rm/length/clear/quantize`; the default instrument is the
  caller's seat focus. Protocol version 4.
- **Not in phase 2**: a piano-roll editor in the UI (the CLI and RPC edit
  any event; the step editors edit the grid).

## Amendment (2026-10-09): recording moves to RFC 0008

Phase 3's recording (arm, overdub/replace, quantize, latency compensation,
undo per pass) is designed and built in
[RFC 0008](0008-recording-and-arrangement.md), with a metronome and
count-in, as the first phase of the song/arrangement work. SMF
import/export remains to do.

## Implementation notes (device models)

Added with the Akai MPK mini IV, the first controller with several ports
and endless knobs:

- **Models** are data (`crates/daemon/devices/*.json`, embedded): `match`
  on the port name, `ports` (name part -> logical device name or `ignore`;
  a controller can be several devices, e.g. `mpk` and `mpk_daw`), a
  `profile`, and a default `layout` (a `SeatConfig` naming the model's
  ports). The Livid Block is a model too (profile `livid_block`). Ports of
  known models connect automatically; `midi-devices.json` records each
  port's `model` and `role`.
- **Default layout**: a device with no bindings, CC maps, or knob entries
  of its own in the seat uses its model's layout for its role (renamed to
  the device); without a model it plays the focus as before.
  `seat.apply_layout` (`4s midi layout <device> --apply`, the UI's "edit")
  copies it into the seat for every connected port of the model, as an
  undoable seat edit.
- **General additions**: `NoteBinding.remap` (output notes from the range's
  low end), `@<type>` targets and `a|b` fallbacks (the MPK's keys play
  `@tb303|focus`), `focus.<param>` CC maps (skipped when the
  focus lacks the parameter), `CcMode::Relative` for endless encoders (200
  steps per range, no pickup needed), and `SeatConfig.pitch_bend` (default
  the focus; +/-2 semitones; `Instrument::pitch_bend`, the 303 bends).
- Pitch bend is not journaled (see the RFC 0006 amendment). Protocol 5.
- Not yet: pad LEDs, display, and buttons over the DAW Port (no documented
  protocol); MIDI out to the Din Port.

