# Vision

## What 4S is

4S (Sam's Sequencer and Synthesizer Set) is a personal, extensible suite for
making electronic music. Like other modular music tools, it lets you add
instruments, route them through a mixer, and connect MIDI controllers and
devices. Unlike most, it is built from the ground up to be fully scriptable and
agent-drivable: anything you can do with a knob or a mouse, you can do from the
command line.

## Near-term goal: drum sequencer

- A step sequencer for drums, driven by a **Livid Block** (8x8 momentary
  buttons with LED feedback, plus 8 knobs).
- Synthesized **TR-808-style** voices (kick, snare, clap, hats, toms, cowbell,
  etc.), generated in real time -- no samples.
- Basic transport (play/stop, tempo, swing), per-voice parameters, and a mixer.
- A minimal UI that mirrors the hardware and daemon state.

## Longer-term direction

- More instruments: synths, samplers, other drum machine models.
- Effects and buses (send/return, master processing).
- MIDI in/out to external gear and other controllers.
- Pattern/song arrangement, project save/load.
- Recording and export.

## Non-goals (for now)

- Being a full DAW (audio track recording/editing, plugin hosting).
- Cross-platform polish: macOS is the primary target to start.
- Collaboration, cloud features, or distribution.
