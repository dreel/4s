# RFC 0012: A compressor and sidechains

- Status: implemented
- Author: Sam (with Claude)
- Created: 2026-10-17
- Discussion: the `compressor` PR

## Summary

A `compressor` audio effect, and the first sidechain: an effect may be
keyed by another channel's signal instead of its own input. A kick on
channel 1 can then duck a bass on channel 2 ("pumping"), even with the
kick's channel muted.

## Motivation

The insert chains (RFC 0010) have a filter, a delay, and a reverb, but no
dynamics: nothing controls loudness, and the classic dance-music duck of a
bass or pad under the kick is impossible. Sidechains were listed in
[extending](../extending.md) as routing beyond RFC 0010, so this records
the one shape we add.

## Design

- **The effect** (`crates/engine/src/effect.rs`): a feed-forward stereo
  compressor. Its detector follows the louder side of its key, sample by
  sample; a soft-knee curve gives the gain reduction in dB, smoothed with
  separate attack and release one-poles, then makeup gain. Parameters after
  `bypass` and `mix` (default 1, so `mix` gives parallel compression):
  `threshold` (-60..0 dB), `ratio` (1..20), `attack` (0.1..100 ms),
  `release` (10..1000 ms), `knee` (0..24 dB), `makeup` (0..24 dB), and
  `sidechain` (an integer, 0..32).
- **The key.** `sidechain` 0 keys the compressor by its own input (the
  signal where it sits in its chain). `n` keys it by channel `n`'s signal
  *before that channel's inserts and fader*: what its instruments put on
  it, panned. So a muted channel still keys, and a compressor keyed by its
  own channel hears that channel before any earlier insert. The engine
  copies every active track channel's bus into a preallocated key buffer
  after the instruments are summed and before any chain runs, so order in
  the mixer does not matter and nothing feeds back. A return bus is fed by
  sends while the chains run, so it keys silence; a channel that does not
  exist keys silence too.
- **Engine interface.** `AudioEffect` gains three default methods:
  `process_keyed` (render with a key, defaulting to `process`),
  `sidechain` (the channel that keys it, if any), and `reduction` (the
  most gain reduction since the last read, for meters). `run_chain` passes
  the effect's dry copy as the key when it has no sidechain.
- **Meters.** `Feedback::Meters` carries each effect slot's reduction; the
  daemon adds `reduction` (compressor id -> dB) to the `meters` event, and
  `4s watch` prints it.
- **Channel removal.** Removing a channel sets every `sidechain` that
  named it back to 0 in the same request (journaled as
  `param:<id>.sidechain`), so a channel later taking its number does not
  key them; undo restores both.
- **UI.** The device panel shows `sidechain` as a channel picker ("own
  input" or a track channel) and a gain reduction meter for a compressor.

## Impact on the principles

- **Agent-drivable / API parity**: the compressor is a device type and the
  sidechain a parameter, so `device.add`, `param.set`, `4s device add
  compressor`, and `4s set comp.sidechain 1` reach all of it; the meter is
  in the `meters` event.
- **Multiplayer / network transparency**: no impact: parameters and events
  as before.
- **Real-time safety**: the key buffers (32 channels x a block) are
  allocated when the engine is built; the copy and the compressor do not
  allocate (`no_alloc` runs a keyed compressor).
- **Validation**: unit tests of the curve and keying, a CLI e2e render
  measuring the duck, an Electron test of the picker and meter.
- **Modularity**: fits the device and parameter-path model; the sidechain
  is a parameter, not a new kind of connection.

## Alternatives

- **Sidechain as a port connection** (the patching graph RFC 0010 defers):
  more general, but needs the graph first. A parameter can move to a port
  later without changing what a project means.
- **Key after the source channel's fader or inserts**: would make the
  result depend on channel order (a chain keyed by a later channel would
  hear the last block), and a muted kick could no longer duck.
- **An instrument output as the key**: a channel is already the place an
  output goes; routing the kick's direct out to its own channel gives the
  same thing.

## Migration and compatibility

No project format change: device types and parameters are already saved
generically. `PROTOCOL_VERSION` is unchanged: a new device type and a new
event field with a default are additions, as `lfo` was.

## Validation plan

As above: `cargo test`, `scripts/e2e-cli.sh` (the "compressor keyed by
another channel" section), and the Electron "compressor" test.

## Open questions

See [backlog](../backlog.md): a limiter, a filter on the key, and returns
as keys.
