# Engine

Status: v4 (RFC 0004, RFC 0007, RFC 0008 phases A and B, RFC 0009 phase 1). Instruments added and removed at runtime (a
TR-808-style drum machine and a TB-303-style bass synth), each played by a
clip of timed note events on a shared tick clock with swing, and a
multi-channel stereo mixer. Code: `crates/engine`.

## Structure

- `Engine` (`engine.rs`) owns a fixed table of instrument slots
  (`MAX_INSTRUMENTS` = 16), a fixed pool of mixer channels (`MAX_CHANNELS` =
  32), the routes between them, one clip per slot, the tick clock, and the
  global parameters.
  `render()` and `apply()` are real-time safe: no allocation, frees, locks,
  or I/O.
- `Instrument` (`instrument.rs`) is the trait every instrument type
  implements: outputs (each mono or stereo), parameters,
  `note_on`/`note_off` (its only input; a drum machine maps GM notes to
  voices), and block `render` into
  preallocated output buffers (at most `MAX_BLOCK` = 256 frames per call).
  `instrument::make`, `params`, and `outputs` describe each type.
- `RtEngine` (`rt.rs`) wraps the engine for the audio thread. The control side
  talks to it only through lock-free SPSC ring buffers (`rtrb`): `Command`s
  in, `Feedback` out, and a return ring for removed instruments.
- **Adding and removing instruments.** The daemon builds the
  `Box<dyn Instrument>` on the control side and sends it in
  `Command::AddInstrument`. `RemoveInstrument` takes it out of its slot and
  the audio thread pushes it onto the return ring (`RETURN_CAPACITY` = 64);
  the daemon's feedback thread drops it. The daemon never has more removals
  in flight than the ring holds, so the push always has room.
  `crates/engine/tests/no_alloc.rs` checks that the audio thread makes no
  heap operations while instruments are added, routed, played, and removed.
- `offline.rs` renders a `RenderSpec` (a copy of the daemon's graph,
  parameters, and patterns) faster than real time to WAV and analyzes it
  (peak, RMS, per-side levels, onset times). Used by `render.offline`.
- The daemon keeps the authoritative state and forwards every change to the
  engine; the engine never decides state on its own except the playhead.

## Signal flow

```
 808 "drums"                                  Mixer
  kick  level pan mute [main]---+
  snare level pan mute [main]---+--> ch 1 "Drums" (stereo source) --+
  ...                           |                                    |
  hat   level      mute [ch 2]-------> ch 2 "Hat"  (mono source)  ---+--> master
                                                                     |    (soft clip)
 303 "bass"  out ------------------> ch 3 "Bass"  (mono source)  ---+
```

Per block of up to 256 frames (blocks also split at step boundaries):

1. Each instrument renders its outputs.
2. Each routed output is summed into its channel with its own pan law:
   **constant-power pan** for a mono source (`L = cos a`, `R = sin a`, -3 dB
   per side at center), **balance** for a stereo source (unity at center;
   moving to one side attenuates only the other). Both use the channel's
   `pan`.
3. Each channel runs its **insert chain** of audio effects (RFC 0010; up to
   `MAX_INSERTS` = 8, in order), then applies its fader (squared taper) and
   mute/solo, and is metered (post fader, left and right peaks).
4. Track channels send post-fader to **return** channels
   (`mixer.<n>.send.<m>`, ramped per block, through the return's balance);
   returns then run their chains and faders like any channel, after every
   track (so a send never feeds itself). Track solos gate the tracks and
   return solos the returns: soloing a track keeps the returns (its sends
   reach them); soloing only returns plays just them, the tracks still
   sending but their dry signal leaving the master.
5. Channels sum into the master, which runs its own insert chain, then
   applies its fader and a soft clipper above 0.8.

Gains and pans are smoothed (10 ms) to avoid clicks. Outputs that are not
routed are silent, except an 808 voice's direct out: unrouted, the voice
plays through the 808's main mix ("normalled", like a hardware individual
out). Routing a direct out takes the voice out of the main mix. Its `level`
and `mute` still apply; its `pan` is bypassed (the channel pans it).

With default settings a voice plays at the same level as before RFC 0004:
level 0.8 -> constant-power pan -> balance at unity -> fader at unity.

## Audio effects

Effects (`crates/engine/src/effect.rs`, RFC 0010) are built on the control
side (`effect::make`, which allocates any buffers), sent in with
`AddEffect` into one of `MAX_EFFECTS` = 64 slots, placed in chains with
`SetChain`, and handed back to be dropped with `RemoveEffect`. Each renders
its wet signal in place on the stereo bus; the engine keeps the dry copy
and applies the two parameters every effect has first: `<id>.bypass` (a
bypassed effect fades out and then does not run) and `<id>.mix` (dry/wet,
smoothed).

| Type | Parameters (after `bypass`, `mix`) | Default mix |
|------|-------------------------------------|-------------|
| `filter` | `mode` (0 low, 1 band, 2 high pass), `cutoff` (0..1, 20 Hz..20 kHz), `resonance` | 1.0 |
| `delay` | `sync` (on), `division` (1/32, 1/16, 1/8t, 1/8, 1/8d (default), 1/4, 1/4d, 1/2, 1 bar), `time` (free: 1 ms..2 s), `feedback` (0..0.95), `pingpong`, `damping` (a low-pass in the feedback path) | 0.35 |
| `reverb` | `size`, `damping`, `width` (Freeverb's eight combs and four all-passes per side) | 0.3 |

## Modulators

Modulators (`crates/engine/src/modulator.rs`, RFC 0010) sit in
`MAX_MODULATORS` = 8 slots, built and dropped on the control side like
effects. At the start of every rendered block each one advances, and every
**modulation route** (`MAX_MOD_ROUTES` = 64, set by `SetModRoute`) moves its
target to its **base** (the value last set on it with `SetParam`) plus the
sum of its routes' `depth x range x output`, clamped to its range, through
the same `set_param` path as any change. Clearing a route (or removing its
modulator) puts the target back at its base.

- `lfo`: four LFOs (`<id>.<k>.shape`: sine, triangle, saw up, saw down,
  square, sample and hold; `sync` (on) with `division` (1/16 .. 4 bars,
  default a bar) on a beat clock that runs from play (it does not follow
  a loop's wrap or a locate while playing, so a period that does not divide
  the loop drifts across passes), or a free `rate`
  (0.01..20 Hz); `phase`). Outputs `<id>.1` .. `<id>.4`, -1..1.

## Automation lanes

Each clip-table entry holds up to `MAX_LANES` = 8 **automation lanes**
(`SetLane`, `SetLanePoint`; RFC 0010): a target, its range, its value set
by hand, and up to 64 (tick, value) points. Every block, after
modulation, each track playing a clip writes that clip's lanes' values at
its playhead (linear between points), so automation overrides modulation
on the same target. A target no playing lane covers goes back to its
value set by hand (`SetParam` keeps it current, as it does a route's
base).

## Instruments

### `tr808` (default id `drums`)

Eight voices synthesized from oscillators, noise, filters, and envelopes --
no samples. Outputs: a stereo main, plus one mono direct out per voice
(`drums.kick`, ...). Each voice has three sound macros, interpreted per
voice, and an internal mix strip:

| Voice | Synthesis | tune | decay | tone |
|-------|-----------|------|-------|------|
| kick | sine with fast pitch sweep (49 Hz base), click, drive | pitch | body length 0.15-2 s | click + drive |
| snare | two sines (180/330 Hz) + high-passed noise | pitch | body + snares | snappy (noise vs body) |
| clap | band-passed noise, 4 rapid bursts + tail | filter pitch | tail length | filter brightness |
| closed_hat | 6 square "metal" oscillators -> band-pass -> high-pass | pitch | 30-250 ms | band-pass center |
| open_hat | same source, longer; choked by closed_hat | pitch | 0.25-1.6 s | band-pass center |
| low_tom / high_tom | sine (95/155 Hz) with pitch drop + noise | pitch | 0.15-1.2 s | noise amount |
| cowbell | two squares (540/800 Hz) -> band-pass, two-stage env | pitch | tail length | band-pass center |

Each voice is monophonic: retriggering restarts it (as on the 808). Pattern:
step levels per voice, 0 off, 1 on (velocity 0.7), 2 accent (velocity 1.0).

### `tb303` (default id `bass`)

A monophonic bass synth: band-limited (PolyBLEP) saw or square into a 4-pole
resonant ladder lowpass (24 dB/oct, tanh stages), and a VCA.

- The filter envelope decays from the `env_mod` depth over `decay` (accented
  notes always use the shortest decay, as on the 303).
- Accented notes play louder and add an accent sweep to the filter. The
  sweep comes from a smoothed "capacitor", so consecutive accents build up.
- A note sounds until its note-off: from its clip (a step's note is half a
  step long), a MIDI key, `voice.note_on`, or `4s key C2 --for 1`. Velocity
  only selects accent, at 0.95 (121) and up, as on the 303.
- A note that starts while another is held glides in (~60 ms) without
  retriggering the envelopes: a **slide** step is a note held one tick past
  the next step's start. Releasing the sounding note glides back to the last
  one still held (last-note priority). The clip's notes and keys share this
  stack; stopping the transport releases only the clip's notes. Notes are
  keyed by pitch: a clip note ending on the same pitch as a held key ends
  that key's note too (and the other way round).
- Output: one mono main.

Pattern: per step either a rest or `{note, accent, slide}` (MIDI note,
12-108). String form (CLI, project files): space-separated tokens, `C2` note
(C4 = MIDI 60), `!` accent, `~` slide, `-` rest, e.g.
`"C2! - C2 D#2~ G2 - C2 -"`.

### `sampler` (default id `sampler`, RFC 0010)

- 16 pads; pad k plays on note `35 + k` (GM drum notes from 36, as its note
  layout's rows), so the step view and the Block treat it as a drum
  machine. Polyphonic (16 voices, the quietest stolen), linear
  interpolation, one stereo output.
- Per pad (`<id>.<k>.*`): `gain`, `pan`, `tune` (-24..24 st), `start` and
  `end` (0..1 of the sample), `attack` (to 0.5 s), `release` (to 2 s),
  `oneshot` (on: plays to the end; off: a held note releases), `choke`
  (0 none, 1..8: a pad stops the others in its group). `gain`, `tune`,
  `start`, `end`, `attack`, and `oneshot` are read when a note starts (a
  knob or an LFO on them changes the next notes); `pan` and `release`
  follow while a note plays.
- Samples are decoded (8/16/24/32-bit integer or 32-bit float WAV, mono
  doubled) and resampled to the engine rate on the control side, sent in
  with `SetSample` as shared buffers, and handed back to be dropped when
  replaced; at most 60 s each.

## Sequencer

- One shared clock: 96 ticks per quarter note (`PPQ`), 24 per 16th step
  (`TICKS_PER_STEP`), on the song timeline: the transport always plays the
  arrangement from the locate point (RFC 0009), and `Feedback::Step`
  reports each step of the song position.
- Swing (`transport.swing` 0..1) delays every second 16th of the song
  position: each pair keeps its length and the first note takes 50%..75%
  of it. Ticks inside a step are spaced evenly.
- **Clips** (RFC 0007): up to `MAX_EVENTS` = 1024 events `{tick, len,
  note, velocity}`, each clip looping at its own length (1 tick to 64
  steps; one bar by default). On every tick the engine releases the notes
  that end there, then starts the events that begin there,
  sample-accurately inside audio blocks.
- Clip storage is preallocated per slot; the daemon edits it with
  `ClearClip`, `AddEvent`, `RemoveEvent`, and `SetClipLength` commands, so
  nothing is allocated or freed on the audio thread.
- **Recording** (RFC 0008): live notes (keys, pads, `voice.*`) emit
  `Feedback::Live` with the song tick the player heard (fractional, output
  latency removed, negative during a count-in). The daemon's take
  (`crates/daemon/src/core/record.rs`) writes them into the clip once per
  loop pass, quantized toward the record grid by its strength.
- **Metronome**: a click (1.5 kHz on the downbeat, 1 kHz on other beats)
  mixed after the master fader and its meter, while `metronome.on`, and
  always during a count-in. `Play { count_in }` runs whole bars of clicks
  before tick 0.
- **Clip table and the song** (RFC 0008 phase B): clips live in one
  preallocated table (`MAX_CLIPS` = 256, each `MAX_EVENTS`) shared by all
  slots; each slot has up to `MAX_PLACEMENTS` = 256 placements `{start,
  length, offset, clip}`. Each slot plays the placement under the song
  position, from `offset` into its clip and looping at the clip's length;
  its notes end with the placement. Play starts from the locate point
  (`Locate`), and at the song's end (the last placement's end) loops the
  whole song (`song.loop` 1), loops bars `song.loop_start..song.loop_end`
  when the playhead reaches the range's end (2), or reports `SongEnd` (0)
  and the daemon stops (unless recording, which goes on past it, or a slot
  plays a launched clip).
- **Clip launching** (RFC 0009): `Launch { slot, clip, now }` makes a slot
  loop a clip instead of its placements (or, with no clip, go back to
  them). While playing it waits in the slot's preallocated state for the
  first tick whose song position is a multiple of
  `transport.launch_quantize` (none, a beat, a bar, two, four), or the
  next tick with `now`; stopped or counting in, it lands at once, and
  every launched clip starts from its beginning when play starts. A
  launched clip loops from the tick it started, whatever the song does (a
  loop's wrap or a locate does not move it); a switch ends the slot's clip
  notes and reports `Feedback::Launched { slot, clip, tick }`, from which
  the daemon knows where the clip is (playheads, recording).
- Step patterns are views over clips (`crates/protocol/src/clip.rs`): a
  drum step is an event on the step's first tick at the voice's GM note,
  velocity 89 (on, 0.7) or 127 (accent), one step long; a 303 step is an
  event half a step long, and a slide holds one tick past the next step, so
  the next note starts while it is held and glides.

## Parameters

| Path | Range | Default |
|------|-------|---------|
| `transport.tempo` | 20-300 bpm | 120 |
| `transport.swing` | 0-1 | 0 |
| `mixer.master.volume` | 0-1 | 0.8 |
| `metronome.on` | toggle (not undone) | off |
| `metronome.level` | 0-1 (squared taper) | 0.6 |
| `song.loop` | 0 off, 1 the song, 2 bars (integer) | 1 |
| `song.loop_start` / `song.loop_end` | bars from 0, end exclusive (integer) | 0 / 4 |
| `transport.launch_quantize` | 0 none, 1 a beat, 2 a bar, 3 two bars, 4 four bars (integer) | 2 |
| `mixer.<n>.volume` | 0-1 (squared taper) | 1.0 (unity) |
| `mixer.<n>.pan` | -1..1 (pan for mono sources, balance for stereo) | 0 |
| `mixer.<n>.mute` / `.solo` | toggle | off |
| `mixer.<n>.send.<m>` | 0-1, track channel `n` to return `m` (post-fader) | 0 |
| `<drums>.<voice>.tune` | -12..12 semitones | 0 |
| `<drums>.<voice>.decay` | 0-1 | per voice |
| `<drums>.<voice>.tone` | 0-1 | 0.5 |
| `<drums>.<voice>.level` | 0-1 (squared taper) | 0.8 |
| `<drums>.<voice>.pan` | -1..1 | 0 |
| `<drums>.<voice>.mute` | toggle | off |
| `<bass>.tune` | -12..12 semitones | 0 |
| `<bass>.waveform` | toggle (off saw, on square) | off |
| `<bass>.cutoff` | 0-1 (exponential, ~40 Hz-6 kHz) | 0.4 |
| `<bass>.resonance` | 0-1 | 0.5 |
| `<bass>.env_mod` | 0-1 (up to 5 octaves) | 0.5 |
| `<bass>.decay` | 0-1 (0.2-2 s) | 0.4 |
| `<bass>.accent` | 0-1 | 0.5 |

`<drums>` and `<bass>` stand for an instrument's id. `n` is a channel number
(1-32), stable while the channel exists. The registry (`param.list`) is
rebuilt whenever instruments or channels change.

## Audio output

`4sd` opens the default output device via `cpal` (f32, i16, or i32 formats)
and builds the engine at the device's sample rate. With `--no-audio`, or if
the device cannot be opened, a "null" backend runs the engine paced to real
time so sequencing, events, and LEDs still work headless.

After the master (and its meter) come two things that are not part of the
mix: the metronome's click, and an audition (`Command::Preview`, RFC 0011),
a sample played once at a fixed level with short fades, handed back to the
control side when replaced.

## Extending

New instrument types implement `Instrument` and are listed in `instrument.rs`
and `InstrumentType`. See the "Add an instrument type" recipe in
[extending.md](extending.md).
