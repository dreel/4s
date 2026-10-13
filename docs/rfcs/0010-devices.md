# RFC 0010: Devices: effects, buses, modulators, a sampler, automation

- Status: accepted (phases 2 to 5 implemented; Sam asked to keep building and
  review the stack later, so the choices marked "default" are reversible
  and open to review; see "Implementation notes")
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-11
- Discussion: the PR that introduces this RFC. Follows RFC 0004
  (instruments, channels, routes) and RFC 0009 (one transport, clip
  launching, note layouts), whose section 7 sketched this phase.

## Summary

Instruments stop being the only kind of node. A **device registry**
describes every kind of device the engine can run: instruments (808, 303,
and a new **sampler**), **audio effects** (a filter, a delay, a reverb to
start), and **modulators** (an LFO bank). Audio effects sit in **insert
chains** on mixer channels and the master. **Return buses** are channels
fed by per-channel **sends**. Modulators drive any parameter path through
**modulation routes** with a depth, evaluated per audio block. Clips gain
**automation lanes** (parameter path -> breakpoints) that play with their
notes. Every device has a stable id, typed ports, and parameters under its
id, so the modular graph the UI shows later is a change of view and
scheduling, not of model.

## Motivation

Sam wants samplers, FX panels, LFO banks, and other devices that connect
to each other, sequenced from the piano roll and the arrangement. Today an
instrument goes straight to a mixer channel; there is nowhere to put a
delay, no way to share a reverb, nothing that moves a parameter over time
except a hand or a CC, and the only sound sources are two synthesized
instruments.

## Design

### 1. Devices and the registry

- A **device** is a node with an id (the first segment of its parameter
  paths, from the same namespace as instruments: `drums`, `delay`,
  `lfo`), a type, a name, typed **ports**, and parameters.
- **Kinds**: `instrument` (notes in, audio out), `audio_effect` (audio in,
  audio out), `modulator` (modulation out), and later `note_effect` (notes
  in, notes out; an arpeggiator, a chord or scale device), which this RFC
  only reserves.
- **Ports** are declared per type: `audio` (mono or stereo), `notes`,
  `modulation`. In this RFC the connections are fixed by where a device
  sits (an instrument's notes come from its track's clips; an effect's
  audio comes from the chain before it; a modulator's outputs go through
  modulation routes), so ports are descriptive. A free patching graph
  later connects ports directly, without changing devices.
- **The registry** (`device.types`) lists every type with its kind, label,
  default id, ports, parameters, knob pages, and (instruments) note
  layout. `InstrumentType` stays for instruments on the wire (`"tr808"`,
  `"tb303"`, `"sampler"`), and a `DeviceType` covers every type, so
  adding a device type is an Extension as adding an instrument is now.
- Engine side, an `AudioEffect` trait (stereo in place: `process(&mut
  [f32], frames)`, `set_param`, `snap`, `reset`) and a `Modulator` trait
  (`advance(frames, tempo, song position)` -> its outputs) join
  `Instrument`. All three are built on the control side, sent in through
  the command ring, and handed back to be dropped, as instruments are
  (RFC 0004). Preallocated tables: `MAX_EFFECTS` = 64, `MAX_MODULATORS` =
  8.

### 2. Insert chains

- Every mixer channel and the master has an ordered **insert chain** of
  audio effects, processed on the channel's summed input before its fader
  (pre-fader inserts, as on a console strip). The master's chain runs
  before the master fader and soft clipper.
- RPC: `device.add { type, id?, name?, channel, position? }` (channel 0 is
  the master: a new convention, since channels count from 1 and the
  master's parameters are `mixer.master.*`; the CLI says `--on master`;
  default position: the end), `device.remove { id }`,
  `device.move { id, channel, position }`, `device.list`. Every effect has
  `<id>.bypass` (a toggle) and `<id>.mix` (dry/wet) parameters besides its
  own.
- Instruments keep `instrument.add` and their routes (RFC 0004); a
  device's `channel` placement is its route.
- First effects:
  - `filter`: a state-variable filter (low, band, high pass), cutoff,
    resonance; stereo.
  - `delay`: tempo-synced or free stereo delay with feedback, ping-pong,
    and a damping low-pass; the buffer (2 s at the engine rate) is
    allocated when the device is built, and a longer synced time (a
    dotted half at a slow tempo) is clamped to it.
  - `reverb`: a small Freeverb-style network (comb and all-pass filters),
    size, damping, width.

### 3. Return buses and sends

- A channel is a `track` channel (fed by instrument outputs) or a
  `return` channel (`channel.add { kind: "return" }`), fed only by sends.
- Every track channel has a send level per return channel:
  `mixer.<n>.send.<m>` (0..1, default 0), post-fader (default; a pre-fader
  option can come later as `mixer.<n>.send.<m>.pre`), so a muted (or
  soloed-away) track sends nothing.
- Returns are mixed after every track channel, so a send can never feed
  itself; returns do not send (no return-to-return routing in this phase).
  A return has its own insert chain (the reverb usually lives there),
  fader, pan, mute, and solo. Solo on a track keeps the returns audible,
  as consoles do; soloing a return plays just that return (the tracks keep
  sending, their dry signal leaves the master).

### 4. Modulators and modulation routes

- A modulator device has outputs `<id>.<k>` (an LFO bank's LFOs:
  `lfo.1` .. `lfo.4`), each -1..1 (or 0..1 for unipolar sources).
- The `lfo` type: four LFOs, each with shape (sine, triangle, saw up, saw
  down, square, sample and hold), rate (Hz, or synced to the transport:
  1/16 .. 4 bars), phase, and a retrigger mode (free, or restart on play).
  Synced LFOs follow the song position, so a render is deterministic.
- A **modulation route** `{source: "lfo.1", target: "bass.cutoff",
  depth: -1..1}` moves the target by `depth x` the target's range around
  the value it is set to (its **base**), clamped to its range. Several
  routes on one target add up. Only continuous parameters can be targets
  (a toggle, an integer, or a `bypass` is refused). Removing a source or
  a target (its device or instrument) drops its routes, in the same
  change, so undo brings them back and clients get the new graph.
- RPC: `mod.route { source, target, depth }` (adds, or sets the depth of
  an existing route), `mod.unroute { source, target }`; the graph lists
  `mod_routes`. A route's depth is also a parameter for CC maps and
  automation: no (default: routes are graph state; their depth is set by
  `mod.route`), to keep parameter lists stable.
- Engine: a fixed route table (`MAX_MOD_ROUTES` = 64), filled by small
  commands (`SetModRoute { index, source, target, depth }`, `ClearModRoute`)
  that carry no heap data. Each block, every
  modulator advances, and every route's target is set to its base plus
  the routes' sum, through the same `set_param` path as any change (so
  instruments and effects need nothing new). `SetParam` on a modulated
  target updates its base. Parameter smoothing inside each device keeps
  block-rate steps inaudible. Real-time safe: no allocation, no locks.
- What the user sets stays the base: `param.get` and the journal show the
  base, never the modulated value; `state.get` adds the live modulated
  values of modulated targets (`modulated`), refreshed with the meters.

### 5. The sampler instrument

- `sampler`: up to 16 **pads** (zones), each a sample with a root note (it
  plays at its own pitch there and transposes from it), the note that
  plays it, gain, pan, tune, start and end points, one-shot or gated with
  an attack and release, and a choke group. Polyphonic (16 voices),
  linear interpolation.
- Note layout (RFC 0009): named rows, one per loaded pad (`Pad 1` or the
  sample's name), on GM drum notes from 36 by default, so the step view
  and the Block treat it as a drum machine; a pad can instead span a key
  range for melodic sampling (the layout then is a pitch range).
- Samples are decoded on the control side (WAV via `hound`; mono or
  stereo, any rate, resampled to the engine rate on load) and sent to the
  engine as `Arc<[f32]>`-like preallocated buffers inside a command; the
  audio thread never decodes or allocates, and a replaced sample is
  handed back to be dropped.
- **Storage**: `sampler.load { instrument, pad, path }` reads a file on
  the engine host, with the same rule as project paths: relative paths
  under the data dir, absolute paths as given (the engine host's own
  files, which the daemon's user can already read).
  Saving a project copies each sample into the bundle
  (`<name>.4s/samples/<instrument>-<pad>-<file>.wav`) and stores the
  relative path, so a bundle is self-contained and moves between
  machines. `sampler.upload { instrument, pad, name, data }` (base64 WAV,
  at most 16 MB decoded, within the server's message limit) serves remote
  clients (network transparency); the CLI uses it when the file is local
  to the CLI and the engine is remote.

### 6. Automation lanes

- A clip gains **lanes**: `{path, points: [{tick, value}]}`, values in the
  parameter's own units, linear between points; before the first point
  and after the last, the nearest point's value.
- While a clip plays (launched or placed), each of its lanes overrides its
  parameter in the engine, evaluated at block rate from the song position
  (so renders are deterministic). The daemon's value of the parameter
  stays the one set by hand: it is what `param.get` returns, what is
  saved, and what undo and the journal see. When no playing clip has a
  lane for it, the parameter goes back to that value (default; keeping
  the last automated value is the alternative, but would make the engine,
  not the daemon, own a saved value). `state.get` shows the live values
  of automated parameters (`automated`), at meter rate.
- RPC: `clip.lane.set { instrument?, clip?, path, points }`,
  `clip.lane.remove { instrument?, clip?, path }`; lanes are in `Clip`
  and in the project file, and undo per lane (`lane:<id>.<n>.<path>`).
- UI: the piano roll gets lanes below the velocity lane: pick a
  parameter, click to add points, drag to move, Delete to remove.

### 7. The graph, the project, the UI

- `Graph` gains `devices` (effects and modulators: id, type, name, kind,
  channel, position) and `mod_routes`; `ChannelInfo` gains `kind`.
- Project format v7: `devices` (with placement), channel kinds, mod
  routes, sampler pads (sample paths inside the bundle), clip lanes.
  Older projects load unchanged (no devices, track channels).
- Protocol 9.
- UI: each console strip shows its inserts (add, reorder by drag, bypass,
  remove) and its sends to returns; clicking an insert opens its panel
  (generic: one `ParamKnob` per parameter, grouped by its knob pages)
  in the editor area. A "modulators" rack lists LFO banks; assigning is
  a "map" mode: arm an LFO output, click any knob in the UI to route it,
  then drag the depth ring on that knob.
- The Block: the knobs follow the focused device's knob page, so
  focusing an effect or a modulator (`seat.focus` takes any device id)
  puts its parameters on the knobs.

## Impact on the principles

- **Daemon owns everything / thin UI**: devices, chains, sends, routes,
  modulation, and automation are daemon and engine state; the UI renders
  and sends RPCs.
- **API parity**: every capability above is an RPC with a CLI command
  (`4s device add|rm|mv|list|types`, `4s send`, `4s mod route|unroute`,
  `4s sampler load`, `4s clip lane`), checked by the parity test.
- **Real-time safety**: effects, modulators, and samples are built and
  decoded on the control side and dropped there; the engine's tables and
  route lists are preallocated; `no_alloc` covers inserts, sends,
  modulation, sample playback, and automation.
- **Close the loop**: `4s render` reports peaks and onsets per channel
  and the modulated values it applied, so effects (a delay's echoes as
  onsets, a filter's level drop), sends, LFOs, sampler hits, and lanes are
  verified without listening.
- **Modularity**: every device is a node with ports and addressable
  parameters; a patching graph later wires ports directly.
- **Network transparency**: samples load from the engine host or upload
  over RPC, and live in the project bundle.

## Alternatives

- **Inserts per instrument instead of per channel.** An 808 can feed
  several channels (direct outs), so effects on the instrument would not
  reach a voice routed elsewhere; channel inserts are what a console has.
  A per-instrument chain can be added later as a device chain between the
  instrument and its routes.
- **A free modular graph now.** More flexible, but needs graph
  scheduling, cycle handling, and a patching UI before it is useful;
  ports make it possible later.
- **Modulation as automation of the base value.** Simpler, but every LFO
  would rewrite the saved value and fill the journal.
- **Streaming samples from disk.** Unneeded at drum-sample sizes; loading
  into memory keeps the audio thread simple.

## Migration and compatibility

- Project format v7 (additive: v6 projects load as projects with no
  devices and track channels). Protocol 9.
- `instrument.types` stays (instruments only); `device.types` lists all.
- `seat.focus` accepts any device id (an instrument as before).

## Validation plan

- Engine tests: each effect processes without exceeding 1.0 and decays;
  a delay's echo arrives at its time; an insert's bypass passes audio
  through unchanged; a send reaches its return after the fader; an LFO
  moves a target around its base and back; a sampler pad plays its
  sample at the root note and an octave up at twice the rate; a lane
  sets its parameter as its clip plays. `no_alloc` with all of them.
- CLI e2e: add, move, bypass, and remove effects; render onsets from a
  delay; a send to a return with a reverb raising the return's level;
  `mod route` changing rendered levels; load a WAV into a sampler, render
  its hits, save and reload a bundle with the sample inside; lanes
  changing a rendered parameter.
- Electron e2e: inserts on a strip, an effect's panel, sends, map mode for
  modulation, sampler pads, and a lane in the piano roll.

## Implementation plan

1. This RFC.
2. The device registry, insert chains, and the filter, delay, and reverb
   (sections 1-2), project v7, protocol 9, CLI, UI.
3. Return buses and sends (section 3).
4. Modulators and modulation routes (section 4).
5. The sampler (section 5).
6. Automation lanes (section 6).

## Open questions

- Sends post-fader by default (as most consoles), with pre-fader later.
- A `virtual_block` check that `seat.focus` on an effect or a modulator
  puts its knob page on the Block's knobs comes with that change.
- Automation returns to the value set by hand when its clip stops
  (default), or leaves the last automated value.
- Modulation depth as graph state (default) or as a parameter (so CCs and
  lanes can move it).
- Should `instrument.add` grow into `device.add` for every kind? This RFC
  keeps both: instruments have routes and tracks, effects have
  placements.

## Implementation notes (phase 2: the registry and insert chains)

- `DeviceType` (`crates/protocol/src/device.rs`) has the effects; the
  registry (`device.types`) lists instruments and effects with their
  ports, parameters, knob pages, and (instruments) note layouts. Modulators
  join `DeviceType` in phase 4.
- `device.add { type, id?, name?, channel, position? }`, `device.remove`,
  `device.move`, `device.list`; the graph's `devices` lists effects by
  chain (the master, channel 0, first) and place. Ids share the
  instruments' namespace. Removing a channel removes its inserts (undo
  brings both back: `device:<id>` and `chain:<n>` undo keys).
- Engine: `AudioEffect` (`crates/engine/src/effect.rs`), 64 effect slots,
  a chain per channel and the master (8 inserts each), a shared dry
  buffer; `bypass` and `mix` are applied by the engine around each
  effect. The return ring carries `Returned::{Instrument, Effect}`.
- Offline renders include effects (`RenderSpec.effects`, `chains`).
- Project format v7 (`devices`), protocol 9.
- CLI: `4s device types|list|add|rm|mv` (`--on <n|master>`, `--at`);
  `4s mixer` shows each chain.
- UI: each strip and the master list their inserts under the name, with
  a "+ fx" select; a chip opens the device panel below the console (one
  knob per parameter from the registry, bypass, earlier/later, remove).
- Not yet: per-channel peaks in `4s render` (the CLI e2e checks a delay by
  its onsets instead); `seat.focus` on an effect (knobs for effects).

## Implementation notes (phase 3: return buses and sends)

- `ChannelInfo.kind` (`track` or `return`), `channel.add { kind }`; a
  return takes no instrument (`route.set`, `instrument.add --channel`, and
  project routes to it are refused or ignored), and new instruments skip
  returns when they look for an empty channel.
- Sends are parameters, `mixer.<n>.send.<m>` (0..1), one per track
  channel and return, added and removed with the graph; post-fader (a
  muted track sends nothing), ramped across each block, into the return's
  balance. Returns are mixed after every track channel and are not
  silenced by another channel's solo.
- Undo: a return is `channel:<n>` plus `return:<n>`; its sends are
  parameters, so removing a return and undoing it brings them back.
- CLI: `4s channel add --return`, `4s send <from> <to> <level>`; `4s
  mixer` shows each return's sends.
- UI: "+ return" on the console; a return strip shows `return` instead of
  an input; every track strip has a send knob per return.

## Implementation notes (phase 4: modulators and routes)

- `DeviceType::Lfo` (kind `modulator`); `device.add { type: "lfo" }` with
  no `channel` puts it in the rack (`DeviceInfo.channel` is null for
  modulators, `position` its place in the rack). Its parameters are
  `<id>.<k>.shape|sync|rate|division|phase`, a knob page per LFO.
- `mod.route { source, target, depth }` and `mod.unroute`; the graph's
  `mod_routes`. Targets must be continuous parameters; routes go when
  their modulator or target goes (in the same change, so undo brings them
  back; undo key `mod:<source>><target>` -> depth). A route's depth is
  graph state, not a parameter (the RFC's default).
- The engine holds each route's base: a `SetParam` on a modulated target
  moves the base, and the daemon's value stays the one set by hand
  (`param.get`, saving, the journal).
- Synced LFOs follow a beat clock that restarts with play (at the start
  point, minus any count-in) and keeps running while stopped, so renders
  are deterministic.
- Project files carry `mod_routes`; format stays 7 and protocol 9 within
  this unreleased stack (both were bumped in phase 2).
- CLI: `4s device add lfo`, `4s mod`, `4s mod route|unroute`.
- UI: a modulator rack under the device panel ("+ lfo", a "map" button per
  output: press it, then any continuous knob; Escape cancels), routes with
  a depth slider and a remove button; routed knobs get a ring.
- Not yet: `state.get` showing live modulated values; modulating a
  route's depth.

## Implementation notes (phase 5: the sampler)

- `InstrumentType::Sampler`: 16 pads on notes 36..51 (the note layout's
  named rows `Pad 1`..`Pad 16`, ids `pad1`..). A pad spanning a key range
  for melodic sampling is not built yet; each pad plays its sample at
  `tune`.
- `sampler.get|load|upload|clear`; the snapshot's `samplers` and the
  `sampler` event. `sampler.upload` stores the file under
  `<data dir>/samples/` and is journaled as the `sampler.load` of it, so
  the journal holds no audio and a replay needs no upload. Uploads are at
  most 11 MB (their base64 fits a 16 MiB WebSocket frame), samples 60 s,
  and an upload cannot go in a `batch` (it is rewritten as a load before
  it is journaled).
- Engine: `Instrument::set_sample` takes and hands back `Arc<[f32]>`
  buffers (`Command::SetSample`, `Returned::Sample`), so the audio thread
  never frees a sample; `no_alloc` plays one.
- Projects: `samples` (instrument -> pad -> path); saving copies them
  into the bundle, loading resolves them against it. Undo key
  `sample:<id>.<pad>` -> the file's path (undo reloads it).
- CLI: `4s sampler [load <pad> <file>|clear <pad>] [--instrument]`; a file
  on the CLI's machine is uploaded, else the path is read on the engine
  host. `4s step sampler 1 pad1` names pads.
- UI: `SamplerEditor`: the step view with each pad's sample, audition,
  load (a file picker, uploaded), and clear beside its row; the selected
  pad's knobs.


## Implementation notes (phase 6: automation lanes)

- `Clip.lanes` (`{path, points: [{tick, value}]}`), at most 8 lanes of 64
  points; `clip.lane.set` replaces one lane (adds it if new), sorting its
  points and clamping their values to the parameter's range;
  `clip.lane.remove` drops it. Continuous and whole-number parameters
  only; not toggles, not the transport's (`transport.*` and the song's
  loop, `song.*`: the daemon counts time by them). Point ticks are checked against
  the longest clip, not the clip's length, so undo can restore a lane
  before the clip's length; points past the loop never play.
- Engine: each clip-table entry holds its lanes (`Command::SetLane`,
  `SetLanePoint`; a retargeted lane keeps its points). Every block, each
  playing track's clip writes its lanes' values at its playhead, after
  modulation, so **automation overrides modulation** on the same target;
  the first lane for a target wins when two playing clips automate it.
  What no playing lane covers goes back to its value set by hand, which
  `param.set` keeps up to date. At play start a lane's first value jumps
  straight there (its target's smoother is snapped, so the first hit
  already sounds automated); later changes, and a clip launched while
  playing, move through the smoothers as any change does.
- The daemon keeps each lane's engine target (`ClipState::sent`) and
  re-sends targets after a graph change; a lane whose parameter goes (an
  instrument or effect removed) goes with it, as modulation routes do, in
  the same undo step. Duplicating a clip copies its lanes; renders and
  `4s render --record` include them; projects save them in the clip.
- CLI: `4s clip lane <instrument> <path> "tick:value ..."` (`--rm`), and
  `4s clip show` lists lanes.
- Whole-number parameters' lanes are rounded (points in the daemon, the
  line between them in the engine). The UI's limits come from the
  generated `limits.ts`.
- The song's loop (`song.*`) counts as the transport's for modulation
  routes too (they took only continuous parameters, so no route could
  reach it before either; now the error says why).
- UI: `AutomationLanes.tsx`: a lane strip per lane under the piano roll's
  velocity lane (click: add a point on the snap grid, drag: move,
  alt-click: remove; a dashed line is the value set by hand), and a
  parameter field to add one.

## Implementation notes (live values)

- Section 6's live values are built as `moving` rather than `automated`,
  and cover modulation as well as automation: the engine notes where each
  block's routes and lanes put their targets (automation's write wins) and
  reports them at meter rate (`Feedback::Moved` per parameter, then
  `MovedDone`; once more when nothing moves, then quiet). The daemon maps
  targets back to paths and keeps `Snapshot.moving` (path -> value), emits
  `moving` events when the set or a value changes, and adds `moving` to
  `param.get` / `param.set` results. `params` and saved values stay the
  ones set by hand.
- CLI: `4s get` prints `(now <value>: modulated or automated)`.
- UI: knobs show a sky dot and faders a sky bar at the moving value, with
  it in the tooltip; the handle stays at the value set by hand.

## Implementation notes (recording knob moves)

- While a take records (`transport.record`), a parameter set by hand
  (`param.set`, a CC, a controller knob; not undo or a load) that a lane
  can take is recorded into the clip the recorded track plays under the
  playhead, on the 1/16 grid (the last move in a 16th wins, so the
  longest clip fits a lane's 64 points). Each pass (and the take's end)
  writes them as `clip.lane.set` requests in the same journal entry as
  the pass's notes: one undo step, and a replay writes the same lanes.
  Recorded moves run forward until the clip loops (a pass written one
  step before the loop's end can cross it); each run replaces the lane's
  points from its first move to its last (gaps between moves included),
  and other points stay (a full lane gives
  up those farthest from the runs).
- The value set by hand follows the knob as usual; after the take the
  lane plays the recorded moves. Only the recorder's moves are recorded
  (the user of the request, as the journal has it); other players' moves
  only change the value set by hand.
