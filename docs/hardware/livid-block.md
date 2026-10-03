# Livid Block

Status: stub. To be filled in once we probe the device's MIDI messages.

## Hardware

- 8x8 grid of momentary buttons with LED feedback.
- 8 knobs.
- USB MIDI device.

## Initial mapping ideas

- Grid rows = drum voices (8 voices), columns = steps (8 at a time).
- 16-step patterns via paging (e.g. a modifier or dedicated row/button switches
  steps 1-8 / 9-16), or a mode where each voice spans 2 rows x 8 = 16 steps.
  TBD.
- LEDs show: active steps, current playhead position, selected voice.
- Knobs control parameters of the selected voice (tune, decay, level, ...), or
  mixer levels in a mixer mode.

## To discover

- Note/CC numbers for each button and knob.
- How LED color/state is set (note velocity, CC, sysex?).
- Any device init/handshake messages.

The mapping logic lives in the daemon; a virtual Block implementation should be
built alongside the real one (see [../validation.md](../validation.md)).
