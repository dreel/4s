# RFC 0004: Instruments and the channel mixer

- Status: implemented
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-04
- Discussion: the PR that introduces this RFC. Merging it with
  `Status: accepted` is the maintainer's approval.
- Amended: 2026-10-04, maintainer decisions before implementation; see
  "Amendment: maintainer decisions" at the end. Where it differs from the
  sections above, the amendment wins.
- Implemented: in one PR (per the amendment). "Implementation notes" at the
  end records how the gaps the review left open were settled.

## Summary

Turn the fixed 8-voice drum engine into a set of **instruments** that can be
added and removed at runtime, each routed into a **multi-channel stereo
mixer** that becomes the app's central control point (transport, tempo, and
master live there). An instrument mixes its own voices internally and feeds
one channel by default; any voice can be broken out to a channel of its own
("normalled" direct outs, like the individual outputs on a hardware drum
machine). The existing kit becomes the first instrument type, `tr808`. A
TB-303-style bass synth, `tb303`, is the second, implemented as the last
step to prove the framework.

## Motivation

The engine is one kit hard-wired to eight mixer channels: `Voice`,
`NUM_TRACKS`, the parameter layout in `crates/engine/src/params.rs`, the
`mixer.1..8` paths, meters, the project file, and the UI all assume it.
Nothing else can make sound, and [extending.md](../extending.md) lists new
instrument types as needing exactly this RFC. Without it every new sound
source is a special case, the mixer is really "the 808's voice levels", and
there is nowhere for routing (and later effects) to attach.

What we want, from the user's point of view:

- A mixer of channel strips (name, pan, fader, meter, mute, solo) plus a
  master, always visible, with the transport above it.
- Add an instrument; it shows up on a new channel. Remove it again.
- The 808 is normally *one* channel ("Drums"), with per-voice level, pan, and
  mute in the 808's own panel. Breaking the kick out onto its own channel
  (and later into an effect) is one command.
- A 303 on another channel, sequenced from the same transport.

## Design

### Model

```
 808 "drums" panel                      Mixer
  kick  level pan mute [main]---+
  snare level pan mute [main]---+--> ch 1 "Drums" (stereo) --+
  ...                           |                            |
  c.hat level pan mute [ch 2]------> ch 2 "Hat"   (mono)  ---+--> master
                                                             |
 303 "bass" panel                                            |
  out ------------------------------> ch 3 "Bass"  (mono)  ---+
```

**Instrument.** An instance of a type (`tr808`, `tb303`) with an id chosen at
creation. The id is the first segment of the instrument's parameter paths.
The default 808 has id `drums`, so `drums.kick.tune` and friends keep their
meaning. Ids match `[a-z][a-z0-9_]*`. These ids are reserved: `transport`,
`sequencer`, `mixer`, `controller`. An instrument also has a display name.
A duplicate or reserved id is rejected with `invalid params`. If `id` is
omitted, the type's default is used (`drums` for `tr808`, `bass` for
`tb303`). If that is taken, a number is appended: `drums2`, `drums3`, and so
on.

**Outputs.** Every instrument has a `main` output. Multi-voice types also
expose one direct output per voice. An output is named by a *source* string:
`drums` is the 808's main out, `drums.kick` its kick direct out, `bass` the
303's main out.

**Normalled direct outs.** A voice's direct out, when unrouted, is part of the
instrument's main mix. Routing it to a channel removes it from the main mix,
like plugging a cable into a hardware individual-out jack. Unrouting puts it
back.

**Width.** Every output declares a width, mono or stereo:

| Output | Width | Why |
|--------|-------|-----|
| 808 main | stereo | the voices after their per-voice pan |
| 808 direct (per voice) | mono | the voice after its level and mute, before its pan |
| 303 main | mono | one oscillator, no stereo processing |

Future instruments with real stereo character (pads, a chorus) declare a
stereo main.

So a voice's `level` and `mute` always apply, whether it plays through the
main mix or a direct out; only its `pan` is bypassed on a direct out (the
channel pans it instead).

**Channel.** One channel is one stereo strip that accepts a source of either
width. It is never a pair of linked mono strips. Its `pan` parameter means
constant-power pan for a mono source and balance for a stereo source: same
path, same range -1..1, behavior chosen by the routed source's width. The
bus after the channel is always stereo.

Pan laws, so levels stay exactly as they are today:

- **Mono pan** (mono sources on a channel, and 808 voices into the 808's
  stereo main): today's constant-power law, `L = cos(a)`, `R = sin(a)`,
  `a = (pan + 1) * pi/4`. That is -3 dB per side at center.
- **Balance** (stereo sources): unity at center. Moving toward one side
  attenuates only the other side, linearly: `L *= min(1, 1 - pan)`,
  `R *= min(1, 1 + pan)`. At full left, R is silent.

Parameters, with today's squared taper and 10 ms smoothing:

| Path | Range | Default |
|------|-------|---------|
| `mixer.<n>.volume` | 0-1 (squared taper) | 1.0 (unity) |
| `mixer.<n>.pan` | -1..1 | 0 |
| `mixer.<n>.mute` / `.solo` | toggle | off |
| `mixer.master.volume` | 0-1 | 0.8 |

The channel fader now defaults to unity (it was 0.8 for the per-voice
strips). Level setting moves into the instrument (`drums.<voice>.level`
defaults to 0.8, today's per-voice default). So the default chain is
voice -> level 0.8 -> constant-power pan -> balance at unity -> fader at
unity -> master: the same gain as today's voice -> fader 0.8 ->
constant-power pan -> master.

**Channel numbers.** `n` is the channel's slot in the engine's channel pool,
1..`MAX_CHANNELS`. `channel.add` takes the lowest free `n`. Channels are
never renumbered when another is removed, so paths in projects and scripts
stay valid while the channel exists. A removed `n` can be reused by a later
`channel.add`. Removing a channel resets its parameters to defaults, so a
reused `n` starts clean, but a script that still holds the old `n` will
address the new channel. We accept that, as DAWs do with track numbers.
Projects store `n` explicitly, and gaps load into the same slots. Display
order is creation order (the order of `channels` in the project).
Reordering is out of scope for this RFC; it would add a `channel.move`
RPC and CLI command later. Solo works across channels as today. A channel
also has a name (structured state, not a parameter).

**Routing.** A map from source to channel number, kept as structured state
next to the instrument and channel lists. It is not a float parameter. Each
source feeds at most one channel; a channel may be fed by several sources.
Each source gets its own pan law (mono pan or balance, by its own width),
and the results are summed in stereo before the fader. So a mono and a
stereo source can share a channel, and the channel's one `pan` knob moves
both. This is the routing graph; sends, buses, and inserts are
a later RFC and will attach to channels.

**808 internal mix.** New per-voice parameters in the instrument panel:

| Path | Range | Default |
|------|-------|---------|
| `drums.<voice>.level` | 0-1 (squared taper) | 0.8 |
| `drums.<voice>.pan` | -1..1 | 0 |
| `drums.<voice>.mute` | toggle | off |

The smoothers that live in `Channel` in `engine.rs` today move into the 808
for these. `tune`, `decay`, and `tone` are unchanged.

### Sequencing

- One shared clock: `transport.tempo`, `transport.swing`, and
  `sequencer.length` stay global. The engine fires steps sample-accurately as
  today and hands each step to every instrument, which plays its own pattern.
- Patterns are per instrument and kind-specific:
  - **Drum pattern** (`tr808`): step levels per voice, 0/1/2, exactly as now.
  - **Note pattern** (`tb303`): per step either a rest or
    `{note, accent, slide}`. String form for the CLI and project files,
    space-separated tokens: `C2` note, `C2!` accent, `C2~` slide into the
    next step, `C2!~` both, `-` rest. Example: `"C2 C2! D#2~ - G1 - C3 -"`.
    Notes use scientific pitch with C4 = MIDI 60, so `C2` is MIDI 36.
    Sharps only (`#`); `b` flats are accepted on input and written as
    sharps. The range is C0-C8 (MIDI 12-108). As with drum patterns, the
    pattern always holds 64 steps and `sequencer.length` sets how many
    play. A shorter string is padded with rests; more than 64 tokens is an
    error.
- Still one pattern per instrument. Pattern banks and song arrangement are out
  of scope.

### Real-time engine

- An `Instrument` trait in `crates/engine` covers what the engine needs from
  any instrument: `outputs()` (names and widths), `on_step(step)`,
  `trigger(voice, velocity)` / `note_on` / `note_off`, `set_param(index,
  value)`, `set_pattern(...)`, and `render(&mut OutputBuffers, frames)`.
  Instruments render whole sub-blocks into preallocated per-output buffers
  instead of being called once per sample. The engine already splits blocks
  at step boundaries; it also caps sub-blocks at a fixed maximum (e.g. 256
  frames) so buffers can be sized up front.
- **Fixed pools, no audio-thread allocation.** The engine holds a slot table
  of `MAX_INSTRUMENTS` (16) and a channel pool of `MAX_CHANNELS` (32), all
  allocated at startup. Output buffers are allocated by width (1 or 2
  lanes) when the instrument is built.
- **Adding and removing.** The control side builds the `Box<dyn Instrument>`
  (with its buffers) and sends it through the command ring as
  `Command::AddInstrument { slot, instrument }`. `RemoveInstrument { slot }`
  takes it out of the table and pushes the box onto a new return ring. The
  feedback thread drains that ring and drops the box. `Command` is no longer
  `Copy`. Nothing is allocated or freed on the audio thread.
- **Structural commands never drop silently.** Today `Core::send` logs and
  drops a command when the ring is full. For add, remove, and route
  commands, a failed push fails the RPC and rolls back `Core`'s state. The
  box comes back in `PushError::Full` and is dropped on the control side,
  so `Core` and the engine never disagree about the graph.
- **Pattern commands are fixed size.** A note pattern goes to the engine
  as `[NoteStep; MAX_STEPS]` (a small `Copy` struct per step), like the
  drum `SetTrack`. It is never a `Vec`, so applying it neither allocates
  nor frees.
- **The return push cannot fail.** Slot ids and returning boxes are
  tracked separately. A slot is free again as soon as its
  `RemoveInstrument` is pushed. The engine applies commands in order, so a
  later `AddInstrument` to the same slot always finds it empty. The return
  ring holds `RETURN_CAPACITY` boxes (64, four times `MAX_INSTRUMENTS`).
  `Core` counts boxes in flight: up when it pushes a remove, down when the
  feedback thread drops a returned box. It never pushes a remove that would
  take the count past the capacity, so the engine's push always has room.
- **Project load and `project.new` are all-or-nothing.** They replace the
  whole graph in one go under the `Core` lock: removes for the old
  instruments, then adds for the new ones. Before pushing anything, `Core`
  checks that the command ring has room for every command (`rtrb`'s
  `slots()`) and that the in-flight count leaves room for every remove. If
  not, the load fails with a clear error and nothing changes. Otherwise
  every push succeeds. Reloading a project with 16 instruments needs only
  16 in-flight boxes, well under the capacity.
- **Full pools.** `instrument.add` with all `MAX_INSTRUMENTS` slots used,
  and `channel.add` (or an `instrument.add` that needs new channels) with
  all `MAX_CHANNELS` used, fail with `invalid params` naming the limit
  ("at most 32 channels"). A project with more instruments or channels than
  the pools hold fails to load, as part of the same all-or-nothing check.
- **No engine rebuild in this RFC.** The live `RtEngine` is moved onto the
  audio thread once at startup (the cpal callback or the null pacer) and
  stays there, so the control side never gets it back. If the audio thread
  stalls (a device stops calling back), removed instruments stop coming
  back, and the in-flight count reaches the capacity. Removes (and loads)
  then fail with an error that names the cause ("N removed instruments are
  still waiting to be returned by the audio engine"). A daemon restart clears it. Recovering from a lost device, and
  rebuilding the engine, are left to a later lifecycle RFC.
- **Startup.** The engine starts in the same default shape `Core` starts
  with (one `drums` 808). A loaded project is applied through the same
  commands as live edits.
- **Offline renders** (`render.offline`) build their engine the same way,
  from a copy of `Core`'s graph, parameters, and per-instrument patterns.
  So `4s render` hears exactly what is configured: routing, mute/solo, and
  every instrument.
- **Parameter addressing.** The flat `ParamId` array splits by owner. The
  control side resolves a path to
  `Global(idx) | Instrument { slot, idx } | Channel { n, idx }` and sends
  that. Each instrument type supplies its own parameter descriptions. The
  registry (`param.list`) becomes dynamic: globals, plus every active
  instrument's parameters under its id, plus every active channel's.
- **Mixing.** Per sub-block: each instrument renders its outputs; each
  routed output is summed into its channel's input; unrouted direct outs are
  folded into their instrument's main mix (the instrument does this
  internally, from a routed/unrouted flag the engine sets via a command).
  Then each channel applies pan or balance (from the routed width), gain,
  and mute/solo, and the result is summed into the master. The master keeps
  its soft clipper.
- **Meters** are per channel (L and R peaks) plus master. Channel meters
  read post-fader, post-pan/balance, and post-mute/solo, so they show what
  the channel sends to the master. The L/R render stats measure the same
  point (the master output).

### API (RPC -> CLI -> UI)

New methods, each with a CLI command (`cli_covers_every_method`) and UI:

| Method | Params | Notes |
|--------|--------|-------|
| `instrument.types` | - | available types, their outputs and params |
| `instrument.list` | - | instances: id, type, name, outputs |
| `instrument.add` | `{type, id?, name?, channel?}` | `channel`: omitted = create a new channel named after the instrument and route `main` to it; a number = route `main` to that existing channel; `null` = leave `main` unrouted, as in `route.set` (CLI: `--channel N`, `--no-channel`) |
| `instrument.remove` | `{id, keep_channels?}` | unroutes all its sources, and removes the channels they fed that are left with no source, unless `keep_channels` (CLI: `--keep-channels`). Channels that were already empty are untouched. |
| `channel.add` | `{name?}` | returns the new `n` |
| `channel.remove` | `{n}` | unroutes sources that fed it |
| `channel.rename` | `{n, name}` | |
| `route.set` | `{source, channel: n \| null}` | null = unroute (back into the main mix for direct outs) |
| `pattern.get_notes` / `pattern.set_notes` | `{instrument, notes}` | note patterns, string or structured (superseded: see the amendment's note-pattern API) |

Changed:

- (Defaulting superseded by the amendment: note calls default to the first
  `tb303`.) `pattern.*` and `voice.trigger` gain an `instrument` field, defaulting to
  the first `tr808` so existing scripts keep working. "First" means
  creation order, the order `instrument.list` returns. The controller
  retargeting rule uses the same order. The default is resolved at call
  time. If there is no `tr808`, a call without
  `instrument` fails with `invalid params`, and the error lists the
  instruments that exist.
- `Snapshot` gains `graph: { instruments, channels, routes }`; `pattern`
  becomes per instrument.
- New event `graph` on any instrument/channel/route change. When a client
  receives it, it refetches `state.get` and `param.list`, as it does for
  `reset`, since both the registry and parameter values may have changed
  (for example, a removed and re-added `bass` starts at defaults).
  Structural changes are rare, so a full refetch is cheap enough.
- Events that carry pattern or trigger data gain an `instrument` field, so
  every client knows which instrument an edit or hit belongs to:
  `StepChanged { instrument, voice, step, level }` and
  `PatternChanged { instrument, voice, steps }`.
- (Payload per the amendment: `NotesChanged { instrument, steps }`.) New
  event `NotesChanged { instrument, notes }` for note-pattern edits
  (the whole pattern; it is at most 64 steps).
- `Trigger` becomes `{ instrument, voice?, note?, velocity, time }`: a drum
  hit sets `voice`, a 303 note sets `note` (MIDI number). `velocity` is 1.0
  for an accented 303 step and 0.7 otherwise, as for drums.
- `Meters` carries per-channel L/R peaks keyed by channel number.
- Render results (`RenderTrigger`) gain the instrument id. `4s render`
  reports triggers per instrument, and peak and RMS for the left and right
  channels separately (as well as combined), so pan and balance can be
  checked end to end.
- `PROTOCOL_VERSION` 1 -> 2.

CLI:

```
4s instrument types | list
4s instrument add tb303 [--id bass] [--name Bass] [--no-channel]
4s instrument rm bass
4s channel add [--name Hat] | rm 2 | rename 2 "Hats"
4s route drums.closed_hat 2        # break the hat out to channel 2
4s route drums.closed_hat none     # back into the 808's main mix
4s mixer                           # channels: name, sources, volume, pan, mute, solo
4s notes bass "C2 C2! D#2~ - G1"   # set; `4s notes bass` prints
```

### Controllers

- The Livid Block and `GenericDrums` devices target one drum instrument:
  `controller.target` (default: the first `tr808`; settable via
  `controller.set_mode` and `4s controller mode --target`). Grid rows stay the
  808's eight voices.
- If the target instrument is removed, the controller retargets to the
  first remaining `tr808`. If there is none, the target becomes null: the
  grid goes dark, and pads and knobs do nothing. Either way, `Core` emits a
  `controller` event. Adding a `tr808` while the target is null makes it
  the target.
- Bridges light LEDs locally on press ([topology.md](../topology.md)). The
  target is part of `ControllerState`, so it reaches bridges in the snapshot
  and in `controller` events, and the target's pattern reaches them through
  the snapshot and the instrument-tagged `StepChanged`/`PatternChanged`
  events. A bridge has everything it needs locally.
- (Superseded: no steps 2-3.) In steps 2-3, volume knob mode controls the `mixer.N.volume` of the
  channel the target's voice is routed to (nothing, if it is unrouted).
- From step 4 (when the parameter exists), volume knob mode controls
  `<target>.<voice>.level` (the 808's internal mix) rather than
  `mixer.N.volume`.

### UI

The main window becomes **console + editor**:

- **Console** (top, always visible): the transport bar (play/stop, playhead,
  tempo, swing, length -- moved from `Transport.tsx`), one strip per channel
  (name, source label, pan, fader, L/R meter, mute, solo), the master strip,
  and a "+ instrument" control. Clicking a strip selects the instrument that
  feeds it.
- **Editor** (below): the selected instrument's panel. For the 808 this is
  today's `Sequencer` grid, per-voice tune/decay/tone and level/pan/mute,
  a per-voice output selector (main or a channel), and `BlockMirror`. For
  the 303 it is a note-step editor (note, accent, slide per step) and its
  knobs.
- Every control remains a `ParamKnob` or an RPC call; the UI holds no routing
  or mixing logic.

### TB-303 instrument (`tb303`)

- Monophonic. Oscillator: band-limited (PolyBLEP) saw or square. Filter: a
  4-pole resonant lowpass (ladder-style, 24 dB/oct) with an envelope that
  decays from the env-mod depth. VCA: a fast attack with a fixed decay,
  gated at about half a step.
- Accent: raises the VCA level and the filter envelope; consecutive accents
  build up the characteristic sweep (a smoothed accent capacitor).
- Slide: the note glides into the next step's pitch (~60 ms) and the gate is
  held (no retrigger), as on the original.
- Parameters, under the instance id (default `bass`):

| Path | Range | Default |
|------|-------|---------|
| `bass.tune` | -12..12 semitones | 0 |
| `bass.waveform` | toggle (off saw, on square) | off |
| `bass.cutoff` | 0-1 (exponential, ~40 Hz-10 kHz) | 0.4 |
| `bass.resonance` | 0-1 | 0.5 |
| `bass.env_mod` | 0-1 | 0.5 |
| `bass.decay` | 0-1 (~0.2-2 s) | 0.4 |
| `bass.accent` | 0-1 | 0.5 |

Level, pan, mute, and solo live on its mixer channel.

### Project format v2

```json
{
  "format_version": 2,
  "instruments": [
    { "id": "drums", "type": "tr808", "name": "Drums" },
    { "id": "bass", "type": "tb303", "name": "Bass" }
  ],
  "channels": [
    { "n": 1, "name": "Drums" },
    { "n": 3, "name": "Bass" }
  ],
  "routes": { "drums": 1, "bass": 3 },
  "params": { "drums.kick.level": 0.8, "mixer.1.volume": 0.8, "...": 0 },
  "patterns": {
    "drums": { "kick": "X---x---X---x---", "...": "" },
    "bass": "C2 C2! D#2~ - G1 - C3 - C2 - - - G1 G1 A#1 -"
  },
  "controller": { "knob_mode": "volume", "follow": true, "target": "drums" }
}
```

Channel order in `channels` is the display order. Parameters remain a flat
`path -> value` map, so new parameters still take registry defaults.

## Impact on the principles

- **Agent-drivable / API parity**: every new capability (instruments,
  channels, routes, note patterns, controller target) is an RPC with a CLI
  command first; the UI only calls them. `4s mixer` and `4s instrument list`
  make the graph readable from the CLI.
- **Multiplayer / network transparency**: graph changes go through `Core`,
  emit a `graph` event, and are part of the snapshot, so every client
  (local, bridged, remote) stays in sync. Nothing assumes a shared machine.
- **Real-time safety**: instruments are built and dropped off the audio
  thread and passed through lock-free rings; slot, channel, and buffer pools
  are preallocated; the audio thread only indexes into fixed tables. This is
  the main risk and gets its own test (below).
- **Validation**: `4s render` gains per-instrument triggers and still reports
  peak, RMS, and onsets, so routing, mute/solo, and the 303 are checkable
  without speakers. CLI e2e, Playwright, and `virtual_block` cover the rest.
- **Modularity**: this is the first real step toward the node graph in
  [architecture.md](../architecture.md): instruments and channels become
  nodes with stable parameter paths, and routes become edges. Effects,
  sends, and buses can attach to channels later without changing this
  shape.

## Alternatives

- **Always multi-out (one channel per voice, grouped in the UI).** Exact and
  simple to route, but the common case (the kit as one sound) would need a
  group/fold UI concept, and per-voice balance would live on the global
  mixer. Normalled outs give the common case for free and the breakout when
  wanted.
- **One channel per instrument, no breakout.** Simplest, but rules out
  processing one drum (e.g. the kick) separately, which effects will need.
- **Linked mono channel pairs for stereo sources.** Classic on hardware
  desks, but doubles the strips and parameters for every stereo source.
  One stereo strip with pan/balance is what DAWs do.
- **Routing as float parameters (`drums.kick.out = 2`).** Fits the existing
  param path, but routing is structural (it changes the registry and the
  engine graph) and integers-as-channel-references break when channels are
  removed. Structured state with its own event is clearer.
- **`instruments.<id>.<param>` paths** (as [extending.md](../extending.md)
  first suggested). This avoids collisions without a reserved list, but it
  breaks every `drums.*` path in projects, scripts, and controller maps for
  no gain. Bare `<id>.<param>` keeps them. The cost is that the reserved
  list (`transport`, `sequencer`, `mixer`, `controller`) must grow with any
  future top-level namespace. Adding one is an RFC-level change anyway.
- **Per-instrument clocks.** More flexible (polymeter), but not needed yet;
  per-instrument length is listed as an open question.

## Migration and compatibility

> Superseded by the amendment (2026-10-04): no v1 migration; v1 projects are rejected. Only the protocol and docs notes below still apply.

- `PROJECT_FORMAT_VERSION` 1 -> 2. `MIGRATIONS[0]` (v1 -> v2):
  - `instruments = [{drums, tr808, "Drums"}]`, `channels = [{1, "Drums"}]`,
    `routes = {drums: 1}`, `controller.target = "drums"`.
  - `mixer.N.volume` -> `drums.<voice N>.level`, `mixer.N.pan` ->
    `drums.<voice N>.pan`, `mixer.N.mute` -> `drums.<voice N>.mute`.
  - If any `mixer.N.solo` is on, every non-soloed voice gets
    `drums.<voice>.mute = 1` (and soloed voices keep their own mute). This
    is deliberately lossy. The project sounds the same, but it is no longer
    "soloed": turning a voice's mute off is how to bring it back. Step 4
    documents this in [project-format.md](../project-format.md).
  - The old `mixer.N.*` paths are removed; `mixer.1.*` then takes defaults
    as the new Drums channel (fader at unity, pan centered).
    `mixer.master.volume` is unchanged. With the pan laws above, the
    migrated project renders at the same level and stereo image as before.
  - `patterns` moves under `patterns.drums`.
  The v1 fixture keeps loading; a v2 fixture is added.
- The default for new projects (and `project.new`) is the same shape: one
  808 on channel 1. Adding a 303 is a command.
- Protocol: `PROTOCOL_VERSION` 1 -> 2. The bundled CLI and UI move together.
  Scripts that set `mixer.N.volume` for drum voices must use
  `drums.<voice>.level` instead; `pattern.*` calls without `instrument`
  keep working.
- Docs to update as it lands: [engine.md](../engine.md),
  [architecture.md](../architecture.md),
  [project-format.md](../project-format.md), [rpc.md](../rpc.md),
  [livid-block.md](../hardware/livid-block.md), and
  [extending.md](../extending.md), which gains an "add an instrument"
  recipe (implementing a type becomes an Extension). For the same reason,
  update the parts of [AGENTS.md](../../AGENTS.md) (principle 5),
  [CONTRIBUTING.md](../../CONTRIBUTING.md) (change classes), and
  [rfcs/README.md](README.md) that say new instrument types need an RFC.

## Implementation plan

> Superseded by the amendment (2026-10-04): it all lands in one PR, with no size limit and no interim rules between steps.

Separate PRs, each linking this RFC, each under ~800 changed lines
(excluding generated code):

1. **Engine framework.** `Instrument` trait, block rendering, slot table,
   channel pool, routing, add/remove through the rings, 808 ported. No
   protocol change. Wired to reproduce today exactly: eight direct outs on
   channels 1-8, voice level fixed at unity, and `mixer.N.*` still meaning
   those channels. Existing tests and renders prove the refactor.
2. **Graph API.** Protocol and `Core` state for instruments, channels, and
   routes; `instrument.*`, `channel.*`, `route.set`, and their CLI commands
   (including `4s mixer`); the dynamic registry; the `graph` event; the
   `instrument` field on requests and events; `PROTOCOL_VERSION` 2. Only
   the `tr808` type exists.
3. **Widths and controller target.** Output widths, L/R render stats,
   controller target and retargeting. Every source is still mono, so only
   mono pan is exercised; levels are unchanged.
4. **Internal mix and project v2.** `drums.<voice>.level/pan/mute`,
   normalled outs, the stereo 808 main out and balance, volume knob mode on
   `level`, the new default shape (one Drums channel at unity),
   project format v2, migration, and fixtures, all in one step. That way
   the new mix and the format that saves it, and the new defaults and the
   migration that keeps old projects sounding the same, always ship
   together.
5. **UI.** Console + editor layout, channel strips, "+ instrument", 808
   output selector; Playwright tests.
6. **TB-303 engine and API.** DSP, note patterns, `pattern.*_notes` RPC,
   `NotesChanged`, and `4s notes`, with render coverage.
7. **TB-303 editor.** The 303 panel in the UI. Sets this RFC to
   `implemented` and updates the docs listed above.

Between steps, nothing is silently lost or changed:

- **Fader default.** It stays 0.8 for every channel (including ones made
  by `channel.add`) through step 3. Step 4 makes it unity, together with
  the internal mix and the migration.
- **Routing in steps 2-3.** There is no main mix before step 4, so a
  `tr808`'s sources are its eight direct outs. `instrument.add tr808` (the
  default `channel`) creates one channel per voice and routes each direct
  out to it, which is the step-1 shape. `route.set <direct out> null`, and
  `instrument.add` with `channel: null` or a number, are rejected with
  `invalid params` ("needs the instrument main mix, RFC 0004 step 4").
  Nothing goes silent unexpectedly.
- **Saving in steps 2-3.** The project format is still v1. `project.save`
  fails with a clear error ("saving instruments, channels, and routes needs
  project format v2, RFC 0004 step 4") if the graph differs from the
  default (one `drums` 808, channels 1-8, eight direct outs, default
  channel names; a `channel.rename` counts as a change). So a v1 file
  never holds state it cannot represent. The `drums.<voice>.level/pan/mute`
  parameters do not exist until step 4, so a v1 file never holds them.
- **Loading in steps 2-3.** `project.load` of a v1 file, and `project.new`,
  reset the graph to that default, then apply the file as today.

If a step still runs past ~800 lines, it splits further. The order is
fixed: each step builds on the one before.

## Validation plan

- **Engine tests** (silent invariants and non-obvious DSP only; routing and
  pan/balance are covered end to end below, so they get no unit tests):
  - `tb303` sounds and decays at every waveform, does not exceed 1.0, and a
    slid note does not retrigger the envelope.
  - Adding and removing instruments while rendering, under an
    allocation-counting global allocator on the render path, allocates
    nothing.
- **CLI e2e** (`scripts/e2e-cli.sh`, isolated daemon):
  - `4s instrument add tb303 --id bass`, set notes, `4s render`: onsets land
    on the note steps, triggers reported for `bass`.
  - Mute/solo/fader on the bass channel change RMS as expected.
  - `4s route drums.kick <new channel>`, then mute that channel: kick onsets
    disappear while the rest of the kit remains.
  - `4s instrument rm bass`: removed from `instrument.list`, its params gone
    from `4s params`, render unaffected for the drums.
  - `4s route drums.kick none`: the kick returns to the Drums channel
    (muting channel 1 silences it again).
  - `4s channel add --name X`, `rename`, `rm`: `4s mixer` shows each change,
    and a removed `n` reused by the next `add` starts at defaults.
  - `4s instrument types` lists `tr808` (and `tb303` from step 6;
    superseded: both, as everything lands at once).
  - `4s mixer` output shows names, sources, and levels.
  - Pan and balance from the L/R render stats: a mono channel panned hard
    left has near-zero right RMS; a stereo channel (the Drums main) at
    balance -1 has near-zero right RMS, and at center matches the RMS of
    the same pattern rendered before the balance change.
  - `4s controller mode --target`, and removing the target: the controller
    retargets or goes null, as specified.
  - Calls without `instrument` after removing every `tr808` fail with the
    documented error.
  - With two subscribed clients, a step edit on one 808 and a note edit on
    the 303 arrive at the other client as `step_changed` / `notes_changed`
    with the right `instrument`, and `4s events` shows `trigger` events
    with `instrument` and `voice` or `note`.
  - (Superseded: no steps 2-3.) In steps 2-3, `project.save` with a non-default graph fails with the
    documented error, and with the default graph it still writes v1.
- **Migration** (superseded by the amendment's replacement checks): the v1 fixture loads into the v2 shape (asserted
  structurally). It also loads through a running daemon (`4s project load`),
  and `4s render` reports the same onsets, and peak and RMS within 0.1 dB of
  the values recorded before the migration.
- **Playwright** (`ui/e2e/`): add an instrument from the console and see its
  strip; mute/solo a strip and check over RPC; change routing over RPC and
  see the UI update. Review the `groove.png` screenshot for the new layout.
- **Virtual Block**: knobs in volume mode move `drums.<voice>.level`;
  pads still edit the 808 pattern.

## Open questions

None. The amendment below resolved them.

## Amendment: maintainer decisions (2026-10-04)

Decided by the maintainer (@dreel) after acceptance and before
implementation. Merging this amendment is the approval.

### No backward compatibility for v1 projects

The project is days old and has a single user, so existing project files can
be discarded. Instead of the v1 -> v2 migration:

- Project format version 2 is the oldest supported version. Loading a v1
  file fails with a clear error ("projects from before RFC 0004 ... are no
  longer supported; create a new project").
- The v1 fixture is replaced by a v2 fixture. `docs/project-format.md` says
  that v1 was dropped deliberately, and that migrations are required again
  from version 2 on.
- The "Migration and compatibility" fold of `mixer.N.*` into
  `drums.<voice>.*` (with its lossy-solo note) and the migration checks in
  the validation plan no longer apply. The new defaults still reproduce the
  old kit's levels: voice level 0.8, channel fader at unity. Replacement
  checks:
  - a CLI e2e check that `4s project load` of a v1 file fails with the
    documented error;
  - a CLI e2e check that a fixed groove, rendered with the default project,
    matches the peak and RMS that `main` rendered for it before this RFC
    (recorded in the test) within 0.1 dB.
- Protocol clients are not kept compatible either: `PROTOCOL_VERSION` 2 is a
  clean break, and the bundled CLI and UI move with it.
- Docs that cite v1 compatibility are updated with the implementation:
  `docs/project-format.md` (fixtures for every *supported* version, and v1
  dropped deliberately), `docs/testing.md` (a v2 fixture test replaces
  `v1_fixture_loads`), and `docs/validation.md` (the v2 fixture loads; v1
  and future versions are rejected).

### One implementation PR, and no PR size guideline

- The seven implementation steps land as one PR, so the interim rules for
  steps 2-3 (save refusing a non-default graph, routing limits before the
  main mix, per-step fader defaults, volume knobs on routed channels) are
  not needed, and neither are their validation checks (including the
  `project.save` refusal check).
- The ~800-line guideline in CONTRIBUTING.md's "Keep PRs reviewable" is
  removed: a PR can be as large as the change needs, as long as the gates
  pass. This is guidance, not one of the gates in the gate table, and the
  implementation PR makes the edit.

### Open questions, resolved

- A MIDI **keyboard** device kind plays a note instrument. A Livid Block
  note-pattern mode is out of scope. Design:
  - API: a new `DeviceKind::Keyboard` (`keyboard` on the wire), and an
    optional `instrument` on `midi.connect` params and on `MidiConnection`.
    `instrument` is only accepted for `kind: keyboard`; it must name a
    `tb303`. CLI: `4s midi connect <port> --kind keyboard
    [--instrument bass]`; the UI's MIDI panel offers the same choice.
  - State: the connection, including its `instrument`, is part of the
    snapshot's `midi` list and of `midi` events, so every client and bridge
    sees which instrument a keyboard plays.
  - RPC (so bridges, scripts, and the CLI can play held notes too, as
    `controller.press` does for the Block): `voice.note_on {instrument?,
    note, velocity?}` and `voice.note_off {instrument?, note}` -> `Empty`.
    The keyboard handler goes through the same code path. A note-on (from
    either path) emits a `trigger` event with `note` and `velocity`; a
    note-off emits no event (the `trigger` event stays "a note started").
  - Holders: a held note belongs to whoever started it. For a keyboard
    that is the keyboard (`midi:<port>`); for an RPC caller it is its
    connection (`name#id`, the `client_id` from `session.hello`, unique
    per connection), not its client name, so two `cli` or `ui` clients
    never release each other's notes. A holder's notes are released when
    it goes away: a keyboard disconnecting or being unplugged, or a client
    connection closing (so a crashed script or bridge cannot leave the 303
    droning). A persistent client (the UI, a bridge) holds a note between
    `note_on` and `note_off`; the CLI holds one for a duration in a single
    command, `4s key C2 [--for SECONDS (default 1)] [--instrument bass]
    [--velocity 0.8]`,
    since each CLI command is its own connection.
  - Through a bridge (`docs/topology.md`; the bridge role is not built
    yet, so this is its contract): the engine only sees the bridge's
    upstream connection as the holder. The bridge therefore tracks holders
    per local client connection and per local keyboard, forwards
    `voice.note_on` / `voice.note_off`, and sends `voice.note_off` upstream
    for a local client's (or keyboard's) held notes when it disconnects or
    is unplugged. If the bridge itself drops, the engine releases all its
    notes, so nothing is left droning.
  - A note-off without `instrument` goes to the instrument the holder
    started that note on, not to "the first `tb303`" looked up again, so
    adding or removing a `tb303` in between cannot leave a note stuck.
  - Takeover: each instrument has one held-note entry. A newer note-on
    replaces it (the earlier holder's later note-off or disconnect is then
    a no-op and cannot cut the newer note). A sequenced note on a later
    step takes the voice over from a held key: from then on the key's
    note-off does nothing, and the pattern's rests and stop cut the voice
    again.
  - An audition (`voice.trigger {note}`, gated for half a step) plays
    through the same voice: it takes over from a held key like a
    sequenced step does (the key's later note-off is then a no-op).
  - Behavior: a note-on starts a note with no gate: the VCA sustains while
    it is held (it does not decay at the gated rate) and releases (~10 ms)
    on note-off. Only the holder's note-off for that same note releases
    it. Each instrument plays one note at a time: a newer note-on takes
    over (gliding, as playing legato on a 303), and releasing the newer
    key releases the voice even if an older key is still down (no
    last-note priority). A note held by a keyboard is released if that
    keyboard disconnects or is unplugged. While a note is held, the
    instrument's own pattern does not cut it on rest steps or on transport
    stop; a sequenced note on a later step takes over.
  - Velocity: MIDI velocity / 127 (0..1) is the note's velocity, reported
    as `trigger.velocity`; a MIDI note-on with velocity 0 is a note-off.
    `velocity` defaults to 1.0 for `voice.note_on` and `voice.trigger
    {note}`, so an audition (`4s trigger --note C2`, `4s key C2`) plays
    accented by default, deliberately, as `4s trigger kick` plays a drum
    at full velocity; pass `velocity` below 0.95 for an unaccented note.
    For every played (not sequenced) note, from MIDI or RPC, 0.95 and up
    plays accented. Sequenced steps keep the original
    velocities (1.0 accented, 0.7 otherwise).
  - Fallbacks, mirroring the controller target: with no `instrument`, a
    keyboard plays the first `tb303` at the time of each note. If its
    instrument is removed, the connection's `instrument` is cleared (a
    `midi` event is emitted) and it falls back the same way. With no
    `tb303` at all, notes do nothing.
  - Validation, CLI e2e: with `virtual_block` (started under a name
    without "block", so it is not auto-connected as a Livid Block, and
    connected with `--kind keyboard`) sending raw note on/off, a
    `trigger` event with `instrument` and `note`; held keys sustain and
    release on note-off (per-channel meters); disconnecting releases a held
    note. `4s key` holds then releases; another connection's
    `voice.note_off` does not release it; a client that disconnects
    releases its notes. `--instrument` is rejected for non-keyboard devices
    and non-`tb303` instruments; removing a keyboard's instrument clears it
    (with a `midi` event). `4s note` and `4s trigger --note` work, and
    `voice.trigger` with both or neither of `voice`/`note` is
    `invalid params`. Playwright: the MIDI panel connects a virtual port as
    a keyboard playing a chosen `tb303`.
- `sequencer.length` stays shared by every instrument.
- No stereo width or mono-sum control for now.
- Note-pattern and audition API, added to the API table:
  - `pattern.get_notes {instrument?}` / `pattern.set_notes {instrument?,
    steps}` -> `{instrument, length, steps}`, emitting `notes_changed
    {instrument, steps}` (all 64 steps);
    steps are structured `{note: n | null, accent, slide}` (the string form
    is for the CLI and project files).
  - `pattern.set_note {instrument?, step, note: {note, accent, slide}}` ->
    the same result and event: one step at a time, so quick edits and
    other clients never overwrite each other with a stale whole pattern.
    CLI `4s note <instrument> <step> <token>` (`C2`, `D#2!~`, `-`).
  - `voice.trigger {instrument?, voice?, note?, velocity?}`: exactly one of
    `voice` (drums) or `note` (a note gated for half a step); both or
    neither is `invalid params`. CLI `4s trigger kick`, `4s trigger --note
    C2`.
- Calls without `instrument` keep defaulting, with no plan to deprecate
  that. The default is the first instrument, in creation order, of the
  exact type the call needs: drum calls (`pattern.get/set/set_step/
  toggle_step/clear`, `voice.trigger {voice}`) take the first `tr808`; note
  calls (`pattern.get_notes/set_notes/set_note`, `voice.trigger {note}`)
  take the first `tb303`, so `instrument` is optional for them too. With
  no instrument of that type, the call fails with `invalid params` naming
  the type and listing the instruments that exist. (The CLI `4s notes` and
  `4s note` still name the instrument explicitly.) CLI e2e covers the
  error.
- Limits stay at 16 instruments and 32 channels. Channel inserts wait for
  the effects RFC.
- Removing an instrument also removes the channels its outputs fed if they
  are left empty, unless `keep_channels` is set (as proposed).

## Implementation notes

How the implementation settled details the design left open:

- `instrument.add` takes `channel?: n` and `no_channel: bool` (two plain
  fields) rather than a null-vs-omitted union.
- `RenderTrigger` is `{time, step, instrument, voice?, note?, velocity}`,
  matching the `trigger` event. `RenderResult` adds `left`/`right`
  `{peak, rms}`.
- `KnobMode::param_path(target, track)`: knob paths are
  `<target>.<voice>.{level,tune,decay,tone}`; `knob_params` is empty with no
  target. `controller.set_mode {target}` sets a target (a `tr808`); there is
  no explicit clear, since the target only goes null when no `tr808` exists.
- `voice.trigger` takes `voice` (drums) or `note` (note instruments, gated
  for half a step), so the 303 editor can audition.
- The snapshot carries `patterns: [{instrument, pattern}]`, where `pattern`
  is `{kind: "drums", tracks}` or `{kind: "notes", steps}`.
- New `pattern.set_note {instrument?, step, note}` alongside
  `pattern.set_notes`, found by the UI e2e: editing one step by sending the
  whole pattern lost quick successive edits (and would clobber another
  client's). The UI edits one step at a time and applies it optimistically.
  CLI: `4s note <id> <step> <token>`.
- A held keyboard note is tracked per instrument together with the
  keyboard (input port) that started it: only that keyboard's note-off
  releases it, and unplugging or disconnecting that keyboard releases it.
  Removing an instrument clears any keyboard set to play it (it falls back
  to the first `tb303`).
- Default channel names: an instrument's new channel takes the instrument's
  name ("Drums", "Bass", "Drums 2"); `channel.add` without a name gives
  "Ch N".
- Output buffers are always 2-lane (mono outputs use the left lane) rather
  than allocated by width; simpler, and the memory is negligible.
- Project loads check the exact number of engine commands they will send
  before changing anything, so a load is all or nothing.
