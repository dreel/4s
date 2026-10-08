# Akai MPK mini IV

Status: supported as a device model (RFC 0007): plug it in and it plays.
Pad lights, the display, and the buttons are not used yet.

Model file: `crates/daemon/devices/akai-mpk-mini-iv.json`.

## Ports

The controller shows up as several MIDI ports. 4S uses two:

| Port | 4S device | Used for |
|------|-----------|----------|
| `MPK mini IV MIDI Port` | `mpk` | keys, pads, pitch and mod |
| `MPK mini IV DAW Port` | `mpk_daw` | the 8 knobs (and buttons, later) |
| `Plugin Port`, `Software Control Port` | - | ignored (Akai software) |
| `Din Port` (output only) | - | ignored (5-pin MIDI out; later a MIDI-out instrument) |

Both are connected automatically when the controller is plugged in (unless
`--no-midi`, or after you disconnect them by hand).

## What it sends (factory preset, measured)

Captured with `4s midi monitor` on a unit with the factory settings:

| Control | Port | Messages |
|---------|------|----------|
| Keys | MIDI | channel 1 notes; 48 (C3) to 72 (C5), octave buttons shift by 12 |
| Pads, bank A | MIDI and DAW (both) | channel 10 notes 36-43, velocity |
| Pads, bank B | MIDI and DAW (both) | channel 10 notes 44-51 |
| Knobs 1-8 | DAW | channel 1 CC 24-31, **relative** (1..10 up, 127..119 down) |
| Pitch | MIDI | pitch bend |
| Mod | MIDI | CC 1, 0-127 |
| Buttons | DAW | CC 12, 73-78, 80-81 (127 on press, some send 0 on release) |
| Mode changes | DAW | sysex `F0 47 00 5D ...` (Akai, model `5D`) |

## Default layout

While your seat has no bindings for `mpk` / `mpk_daw`, they play with the
model's layout (`4s midi layout mpk` shows it):

- keys -> the first `tb303` (the bass), or your seat's focus in a project
  without one (`@tb303|focus`);
- pads (both banks) -> the first `tr808`: pad 1-8 = kick, snare, clap,
  closed hat, open hat, low tom, high tom, cowbell (`remap` + `@tr808`);
- knobs -> the focus's knob page (relative, so they never jump);
- mod -> `focus.cutoff` (the 303's cutoff; nothing for a drum machine);
- pitch -> pitch bend of what the keys play (+/- 2 semitones; not journaled).

To change it, copy it into your seat and edit it like any bindings:

```
4s midi layout mpk --apply      # or "edit" in the UI's seat panel
4s seat                         # see the bindings
4s unbind 2; 4s bind mpk --channel 10 --notes 36..43 --remap 36,38,39,42,46,45,50,56 --to drums2
```

## Not yet

- Pad colors, the display, the buttons: these go through the DAW Port and
  Akai's sysex, which is not documented for this model. The original MPK
  mini's preset format (`F0 47 7F 7C ...`, see
  github.com/gljubojevic/akai-mpk-mini-editor) is a different model id, so
  it likely does not apply.
- Arpeggiator / note repeat clock sync.
