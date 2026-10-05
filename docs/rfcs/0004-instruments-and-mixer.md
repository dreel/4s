# RFC 0004: Instruments and the channel mixer

- Status: proposed
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-04
- Discussion: the PR that introduces this RFC. Merging it with
  `Status: accepted` is the maintainer's approval.

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
| 808 direct (per voice) | mono | the voice before pan |
| 303 main | mono | one oscillator, no stereo processing |

Future instruments with real stereo character (pads, a chorus) declare a
stereo main.

**Channel.** One channel is one stereo strip that accepts a source of either
width. It is never a pair of linked mono strips. Its `pan` parameter means
constant-power pan for a mono source and balance for a stereo source: same
path, same range -1..1, behavior chosen by the routed source's width. The
bus after the channel is always stereo. Parameters, with today's taper and
10 ms smoothing:

| Path | Range | Default |
|------|-------|---------|
| `mixer.<n>.volume` | 0-1 (squared taper) | 0.8 |
| `mixer.<n>.pan` | -1..1 | 0 |
| `mixer.<n>.mute` / `.solo` | toggle | off |
| `mixer.master.volume` | 0-1 | 0.8 |

`n` is a stable channel number assigned at creation and never renumbered when
another channel is removed, so paths in projects and scripts stay valid.
Display order is a separate list. Solo works across channels as today. A
channel also has a name (structured state, not a parameter).

**Routing.** A map from source to channel number, kept as structured state
next to the instrument and channel lists. It is not a float parameter. Each
source feeds at most one channel; a channel may be fed by several sources
(they are summed). This is the routing graph; sends, buses, and inserts are
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
- **Meters** are per channel (L and R peaks) plus master.

### API (RPC -> CLI -> UI)

New methods, each with a CLI command (`cli_covers_every_method`) and UI:

| Method | Params | Notes |
|--------|--------|-------|
| `instrument.types` | - | available types, their outputs and params |
| `instrument.list` | - | instances: id, type, name, outputs |
| `instrument.add` | `{type, id?, name?, channel?}` | by default creates a channel and routes `main` to it |
| `instrument.remove` | `{id}` | unroutes all its sources |
| `channel.add` | `{name?}` | returns the new `n` |
| `channel.remove` | `{n}` | unroutes sources that fed it |
| `channel.rename` | `{n, name}` | |
| `route.set` | `{source, channel: n \| null}` | null = unroute (back into the main mix for direct outs) |
| `pattern.get_notes` / `pattern.set_notes` | `{instrument, notes}` | note patterns, string or structured |

Changed:

- `pattern.*` and `voice.trigger` gain an `instrument` field, defaulting to
  the first `tr808` so existing scripts keep working.
- `Snapshot` gains `graph: { instruments, channels, routes }`; `pattern`
  becomes per instrument.
- New event `graph` on any instrument/channel/route change. Clients refetch
  `param.list` when they receive it, since the registry may have changed.
- `Meters` carries per-channel L/R peaks keyed by channel number.
- Render results (`RenderTrigger`) gain the instrument id. `4s render`
  reports triggers per instrument.
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
- Volume knob mode now controls `<target>.<voice>.level` (the 808's internal
  mix) rather than `mixer.N.volume`.

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
- **Per-instrument clocks.** More flexible (polymeter), but not needed yet;
  per-instrument length is listed as an open question.

## Migration and compatibility

- `PROJECT_FORMAT_VERSION` 1 -> 2. `MIGRATIONS[0]` (v1 -> v2):
  - `instruments = [{drums, tr808, "Drums"}]`, `channels = [{1, "Drums"}]`,
    `routes = {drums: 1}`, `controller.target = "drums"`.
  - `mixer.N.volume` -> `drums.<voice N>.level`, `mixer.N.pan` ->
    `drums.<voice N>.pan`, `mixer.N.mute` -> `drums.<voice N>.mute`.
  - If any `mixer.N.solo` is on, every non-soloed voice gets
    `drums.<voice>.mute = 1` (and soloed voices keep their own mute).
  - The old `mixer.N.*` paths are removed; `mixer.1.*` then takes defaults
    as the new Drums channel. `mixer.master.volume` is unchanged.
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
  recipe (implementing a type becomes an Extension).

## Implementation plan

Separate PRs, each linking this RFC, each under ~800 changed lines
(excluding generated code):

1. **Engine framework.** `Instrument` trait, block rendering, slot table,
   channel pool, routing, add/remove through the rings, 808 ported. Wired to
   reproduce today's behavior exactly (eight direct outs on channels 1-8),
   so existing tests and renders prove the refactor.
2. **Graph API.** Protocol and `Core` state for instruments, channels, and
   routes; the new RPCs and CLI commands; dynamic registry; 808 internal mix
   and normalled outs; widths and pan/balance; controller target;
   `PROTOCOL_VERSION` 2.
3. **Project v2.** Format, migration, fixtures, and the switch to the new
   default shape (one Drums channel).
4. **UI.** Console + editor layout, channel strips, "+ instrument", 808
   output selector; Playwright tests.
5. **TB-303.** DSP, note patterns, `pattern.*_notes` RPC/CLI, 303 editor.
   Sets this RFC to `implemented` and updates the docs listed above.

## Validation plan

- **Engine tests** (silent invariants and non-obvious DSP only):
  - `tb303` sounds and decays at every waveform, does not exceed 1.0, and a
    slid note does not retrigger the envelope.
  - A direct out routed to a channel leaves the instrument's main mix (and
    returns when unrouted).
  - A stereo source on a centered channel keeps its image; full-left balance
    silences the right side. A mono source pans with constant power.
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
- **Migration**: the v1 fixture loads into the v2 shape (asserted
  structurally) and renders the same onsets as before migration.
- **Playwright** (`ui/e2e/`): add an instrument from the console and see its
  strip; mute/solo a strip and check over RPC; change routing over RPC and
  see the UI update. Review the `groove.png` screenshot for the new layout.
- **Virtual Block**: knobs in volume mode move `drums.<voice>.level`;
  pads still edit the 808 pattern.

## Open questions

- Does removing an instrument also remove channels that only it fed?
  (Proposed default: yes, unless `--keep-channels`.)
- Playing the 303 from a MIDI keyboard, and whether the Livid Block gets a
  note-pattern mode for it.
- Per-instrument pattern length versus the shared `sequencer.length`.
- Whether a stereo channel needs a width / mono-sum control.
- Channel limits (16 instruments, 32 channels): enough, and should channels
  get insert slots now or with the effects RFC?
