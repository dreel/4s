# Livid Block

Status: v1 implemented. The grid note layout (column-major) was corrected
after testing on the device: the first guess (row-major) showed the grid
transposed. The knob CCs are still unverified -- see "Calibrating" below.

## Hardware

- 8x8 grid of momentary buttons with (blue) LED feedback.
- 8 knobs, 2 faders, 7 function buttons (faders and function buttons are not
  used yet).
- USB MIDI. On the dev machine it enumerates as `block Controls` (input and
  output).

## Connection

- `4sd` scans MIDI ports every 2 s and auto-connects any input whose name
  contains "block" as a Livid Block, using the matching output for LEDs.
  Disable with `--no-midi`.
- Manual: `4s midi connect "block Controls"` (or the MIDI panel in the UI).
- Disconnected devices are dropped automatically; replugging reconnects.
- macOS note: CoreMIDI only tells a process about newly plugged-in devices
  via a run loop on the thread that created its first MIDI client. 4sd runs a
  dedicated `4s-coremidi` thread for this (`midi::start_device_watcher`);
  without it, devices plugged in after startup never appear.

## Testing without the device

`cargo run -p fours-daemon --example virtual_block [NAME]` creates a virtual
MIDI device (default name "Virtual Block") from a separate process. It prints
the LED messages 4sd sends it and accepts stdin commands (`pad ROW COL`,
`knob INDEX VALUE`, `raw HEX..`) to press pads and turn knobs. A name
containing "block" is auto-connected like the real device.
`scripts/e2e-cli.sh` uses it to test hotplug, pad input, LED output, and
unplug.

## Layout

- The Block plays in the host seat (RFC 0007: the seat of this machine's
  devices, see `4s midi ports`) and drives that seat's **focus** (default:
  the first instrument). Choose it with `4s focus drums2` or the focus
  button in an instrument's editor. If the focus is removed, the first
  instrument takes over. When the focus is not a drum machine the grid goes
  dark and pads do nothing; the knobs still work.
- Rows 1-8 = the focus's tracks (kick, snare, clap, closed hat, open hat,
  low tom, high tom, cowbell). Columns = 8 steps of the current page.
- Pressing a pad toggles that step. Steps past `sequencer.length` are dark and
  ignored.
- LEDs: lit = step on. The playhead column is inverted (lit steps go dark,
  dark steps light) so it is visible either way.
- Pages: page 1 = steps 1-8, page 2 = steps 9-16, and so on. With `follow` on
  (default), the page follows the playhead while playing. Choose a page with
  `4s controller mode --page N` or the UI (choosing a page while playing turns
  follow off).
- Knobs: knob N controls parameter N of the focus's **knob page**
  (`4s knobs page decay`). A `tr808` has volume (`<id>.<voice>.level`, the
  voice's level in the 808's own mix), tune, decay, and tone, knob N =
  voice N; a `tb303` has `main` (cutoff, resonance, env mod, decay, accent,
  tune, waveform). Knobs **pick up**: after the value changed elsewhere, a
  knob does nothing until it passes the current value, so it never jumps.
  `controller.knob` (the virtual knobs) sets the value directly.

The same logic (`crates/daemon/src/controller.rs`) serves the real device and
the virtual one (`controller.press`, `controller.knob`, the UI's Block mirror),
so everything is testable without hardware.

## MIDI map

Stored in `<data-dir>/livid-block.json` (default `~/.4s/livid-block.json`),
written with defaults on first run:

- `channel`: 0 (MIDI channel 1)
- `grid_notes[row][col]`: note `col * 8 + row` (0-63, down each column from
  the top left: the first column is notes 0-7)
- A map file still holding the first, transposed default (`row * 8 + col`) is
  rewritten with this one when `4sd` starts; an edited map is left alone.
- `knob_ccs`: CC 1-8, left to right
- LEDs: note-on to the pad's note, velocity 127 = on, 0 = off

## Calibrating

The grid layout is verified on the device. The knob CCs and the channel are
still the defaults:

1. Run `4s midi monitor`, then turn knob 1 and knob 8 (and, to double-check
   the grid, press the top-left pad and the pad below it: notes 0 and 1).
2. Compare the printed messages (`90 nn vv` = note-on, `B0 cc vv` = CC) with
   the defaults above.
3. Edit `livid-block.json` to match and restart `4sd`.

If LEDs need something other than note-on velocity (e.g. CC or sysex), that
needs a code change in `BlockMap::led_message`.

## Remote engine

The Block will usually be plugged into a laptop. When the engine runs on a
remote server, the laptop's daemon runs as a bridge: it forwards button and
knob input upstream and drives the LEDs from engine events. The bridge lights
LEDs immediately on press and reconciles with engine state, so the grid feels
instant despite network latency. See [../topology.md](../topology.md). (The
bridge role is not implemented yet.)
