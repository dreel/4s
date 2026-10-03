# Engine

Status: v1 -- eight synthesized TR-808-style voices, a 64-step sequencer with
swing, and an 8-channel mixer. Code: `crates/engine`.

## Structure

- `Engine` (`engine.rs`) owns voices, sequencer state, mixer, and a flat
  parameter array. `render()` is real-time safe: no allocation, locks, or I/O.
- `RtEngine` (`rt.rs`) wraps it for the audio thread. The control side talks to
  it only through lock-free SPSC ring buffers (`rtrb`): `Command`s in
  (set param, set step, set track, play, stop, trigger), `Feedback` out
  (step started, voice triggered, meters, stopped).
- `offline.rs` renders patterns faster than real time to WAV and analyzes the
  result (peak, RMS, onset times). Used by `render.offline` for exports and for
  validating what the engine actually produces.
- The daemon keeps the authoritative state and forwards every change to the
  engine; the engine never decides state on its own except the playhead.

## Voices

All synthesized from oscillators, noise, filters, and envelopes -- no samples.
Each voice has three macro params, interpreted per voice:

| Voice | Synthesis | tune | decay | tone |
|-------|-----------|------|-------|------|
| kick | sine with fast pitch sweep (49 Hz base), click, drive | pitch | body length 0.15-2 s | click + drive |
| snare | two sines (180/330 Hz) + high-passed noise | pitch | body + snares | snappy (noise vs body) |
| clap | band-passed noise, 4 rapid bursts + tail | filter pitch | tail length | filter brightness |
| closed_hat | 6 square "metal" oscillators -> band-pass -> high-pass | pitch | 30-250 ms | band-pass center |
| open_hat | same source, longer; choked by closed_hat | pitch | 0.25-1.6 s | band-pass center |
| low_tom / high_tom | sine (95/155 Hz) with pitch drop + noise | pitch | 0.15-1.2 s | noise amount |
| cowbell | two squares (540/800 Hz) -> band-pass, two-stage env | pitch | tail length | band-pass center |

Each voice is monophonic: retriggering restarts it (as on the 808).

## Sequencer

- Steps are 16th notes. Pattern memory is 64 steps per voice; the active length
  is `sequencer.length` (1-64).
- Step levels: 0 off, 1 on (velocity 0.7), 2 accent (velocity 1.0).
- Swing (`transport.swing` 0..1) delays every second 16th: each pair keeps its
  length and the first note takes 50%..75% of it.
- Steps fire sample-accurately inside audio blocks.

## Parameters

| Path | Range | Default |
|------|-------|---------|
| `transport.tempo` | 20-300 bpm | 120 |
| `transport.swing` | 0-1 | 0 |
| `sequencer.length` | 1-64 steps (integer) | 16 |
| `drums.<voice>.tune` | -12..12 semitones | 0 |
| `drums.<voice>.decay` | 0-1 | per voice |
| `drums.<voice>.tone` | 0-1 | 0.5 |
| `mixer.<1-8>.volume` | 0-1 | 0.8 |
| `mixer.<1-8>.pan` | -1..1 | 0 |
| `mixer.<1-8>.mute` / `.solo` | toggle | off |
| `mixer.master.volume` | 0-1 | 0.8 |

Mixer channel N is voice N in track order (1 kick ... 8 cowbell). Volume uses a
squared taper (0.8 -> -3.9 dB, 0.5 -> -12 dB). Gains are smoothed (10 ms) to
avoid clicks; the master has a soft clipper above 0.8.

## Audio output

`4sd` opens the default output device via `cpal` (f32, i16, or i32 formats)
and builds the engine at the device's sample rate. With `--no-audio`, or if
the device cannot be opened, a "null" backend runs the engine paced to real
time so sequencing, events, and LEDs still work headless.

## Extending

New instruments should follow the same pattern: a real-time render path fed by
commands, parameters registered in `params.rs` with stable paths, and offline
render coverage in tests. The fixed 8-track layout is the first instrument, not
a limit of the protocol: paths and the registry are already generic.
