# Extending 4S

Step-by-step recipes for the changes that do **not** need an RFC (the
"Extension" class in [CONTRIBUTING.md](../CONTRIBUTING.md#change-classes)).
Each recipe lists the files to touch in order, the pitfalls, and the
end-to-end check to add. When you are done, run `scripts/gates.sh`.

If what you want is not here, check the last section: it may need an RFC.

Conventions used throughout:

- **API first.** A user-facing capability is an RPC, then a CLI command, then
  UI. Never UI-only. See [api-parity.md](api-parity.md).
- **State changes go through the daemon core** (`crates/daemon/src/core.rs`)
  and emit an event, so every client stays in sync.
- **Nothing on the audio thread allocates, locks, or does I/O.**
- **Test end to end through the real interfaces.** Unit tests only for
  non-obvious logic. See [validation.md](validation.md).

---

## 1. Improve or replace a drum voice

The `tr808` instrument (`crates/engine/src/tr808.rs`) has eight voices
(`Voice` in `crates/protocol/src/types.rs`), each synthesized by a type
implementing `DrumVoice` in `crates/engine/src/voices.rs`.

1. Edit the voice's struct, or write a new one implementing `DrumVoice`
   (`trigger`, `process`, `active`, optionally `choke`). Keep `process`
   allocation-free. Use the building blocks in `crates/engine/src/dsp.rs`
   (`Svf` filters, `Decay` envelopes, `Noise`, `lerp_exp`).
2. Hook it up in `make_voice`. Adjust `default_decay` if the default should
   change.
3. Interpret the three macro params (`tune`, `decay`, `tone` in
   `VoiceParams`) in the voice's own musical terms, and document that in the
   voice table in [engine.md](engine.md).

Pitfalls:
- Parameters are read at trigger time (`Tr808::play`). A parameter
  that must change a sounding note needs to be read per block instead.
- Keep peaks at or below 1.0 at velocity 1.0. The master soft clipper is a
  safety net, not a mixer.

Validate:
- `every_voice_sounds_and_decays` must still pass (audible, no clipping,
  decays to silence).
- Render a pattern with `4s render --bars 2` and check the peak, RMS, and
  that the onsets land on the steps you programmed. Put the commands and
  output in the PR.
- For a new sound character, a short WAV in the PR is welcome.

## 2. Add a parameter

Parameters are addressed by path (`drums.kick.decay`, `mixer.2.pan`). Inside
the engine each one is a `ParamTarget`: a global, a channel parameter, or an
instrument parameter by index. The daemon rebuilds the registry from the
graph (`Core::rebuild_params`), so a parameter only needs to be declared in
one place:

1. Declare it:
   - an **instrument** parameter: add it to the type's `params(id)` list
     (e.g. `Tr808::params`, `Tb303::params`) and handle its index in that
     type's `set_param`. For the 808, per-voice parameters are
     `track * VOICE_PARAMS + p`; add a constant and bump `VOICE_PARAMS`;
   - a **channel** parameter: add it to `params::channel()`, add an index
     constant, bump `CHANNEL_PARAMS`, and use it in `Channel::targets`;
   - a **global** parameter: add it to `params::globals()`, add an index
     constant, and bump `NUM_GLOBALS`.
2. Use the value in the engine.
3. Clients: `param.list`, `4s params`, `4s get/set`, and project files pick
   it up automatically. **The UI does not**: controls are placed by path.
   Add a `ParamKnob` for it (e.g. in `DrumEditor.tsx`, `BassEditor.tsx`, a
   console strip in `Console.tsx`, or `Transport.tsx`). Range, label, and
   default come from the registry. Without that, the parameter has no UI
   control, which breaks UI/CLI parity.

Pitfalls:
- Paths are a public contract (projects, scripts, agents). Renaming or
  removing a path needs a project-format migration
  ([project-format.md](project-format.md)). Adding one does not, since old
  projects just get the default.
- Use a squared or exponential taper for perceptual ranges (see
  `volume_to_gain`, `lerp_exp`).

Validate:
- A CLI e2e check in `scripts/e2e-cli.sh` that sets the parameter and shows
  its effect, usually through `4s render` (e.g. a longer decay raises RMS).
- Update the parameter table in [engine.md](engine.md).

## 3. Support a new MIDI controller

Controllers are MIDI devices whose input the daemon decodes into the same
actions the UI and CLI use. The Livid Block is the reference for a grid
controller with feedback. `DeviceKind::GenericDrums` (note-on triggers
the controller target's voices) and `DeviceKind::Keyboard` (note on/off
plays a note instrument, the connection's `instrument` or the first
`tb303`, holding the note until its note-off), both in `Core::handle_midi`,
are the simpler references for note-only devices. A keyboard is connected
with `4s midi connect <port> --kind keyboard [--instrument bass]` or the
UI's MIDI panel.

1. Add a `DeviceKind` variant in `crates/protocol/src/types.rs` and
   regenerate bindings (`cargo run -p fours-protocol --bin gen-bindings`).
2. Decode its messages in `Core::handle_midi` (`crates/daemon/src/core.rs`).
   For a grid or knob controller, follow `BlockMap` and `decode_block`
   (`crates/daemon/src/controller.rs`): keep the note/CC map in a JSON file
   in the data dir so users can correct it without a rebuild.
3. Turn input into existing core actions (`set_step`, `set_param`,
   `controller_pad`, `controller_knob`) so events fire and all clients sync.
   Don't add a parallel state path.
4. Feedback (LEDs, displays): today's output path is Block-specific
   (`Midi::send_block`, `BlockMap::led_message`, `Midi::block_name`, called
   from `refresh_controller`). A second device kind with feedback needs a
   per-kind send path in `crates/daemon/src/midi.rs`, still diffing so only
   changes go out.
5. Auto-connect, if appropriate: `midi_autoconnect` currently matches only
   "block" and connects as `LividBlock`; extend it with your device's name
   match.
6. Expose it in `4s midi connect --kind ...` and the UI's MIDI panel (the
   kind list).
7. Document the mapping in `docs/hardware/<device>.md`, following
   [livid-block.md](hardware/livid-block.md).

Validate:
- With `virtual_block` (`crates/daemon/examples/virtual_block.rs`), or a
  similar virtual device for your controller's protocol, add CLI e2e checks
  that cover connect, input changing state, feedback arriving at the
  device, and unplug. See the hotplug block in `scripts/e2e-cli.sh`.

## 4. Add a capability end to end (RPC -> CLI -> UI)

1. **Protocol.** Declare the method once in the `api!` macro in
   `crates/protocol/src/api.rs`, with param and result structs. Add any new
   event variants to `Event` in `types.rs`, and new state to `Snapshot` so
   reconnecting clients resync. Regenerate bindings.
2. **Daemon.** Handle it in `Core::handle` (`crates/daemon/src/core.rs`).
   Mutate state, push a `Command` to the engine if audio is involved, and
   `emit` an event. Validate inputs and return `RpcError::invalid` with a
   helpful message.
3. **CLI.** Add a subcommand and map it to the request in `plan()`
   (`crates/cli/src/main.rs`). Add the command to `cli_covers_every_method`.
   That test fails until every method has a CLI command. Format the result
   readably, and support `--json`.
4. **UI.** Call it with `client.call("method", params)` (fully typed from
   the generated `Methods`), and handle the new event in `apply`
   (`ui/src/store.ts`). No business logic in the UI.

Pitfalls:
- Don't assume the client and engine share a machine. File paths are
  engine-side. See [topology.md](topology.md).
- Long work (renders, file I/O) must not hold the core lock for long. See
  `render.offline` in `crates/daemon/src/server.rs`.

Validate:
- CLI e2e checks in `scripts/e2e-cli.sh`.
- A Playwright test in `ui/e2e/app.spec.ts` that drives the UI and checks
  the daemon over RPC, and the other way round.
- Update [rpc.md](rpc.md) if you add a new category of method.

## 5. Add a UI panel or control

1. Add a component in `ui/src/components/`, reading state with `useApp` /
   `useLive` selectors (return primitives or stable references) and acting
   through `client.call`, `setParam`, or `act`.
2. Use `ParamKnob` (`ParamKnob.tsx`) for parameters, so range, label, and
   default come from the registry instead of being duplicated.
3. Give interactive elements a `data-testid`.
4. Everything the panel does must already be an RPC with a CLI command
   (recipe 4). Local desktop actions are the only exception
   ([api-parity.md](api-parity.md)).

Validate:
- A Playwright test in `ui/e2e/`. Check the screenshots the suite writes
  (`ui/test-results/groove.png`, `bass.png`) for layout regressions.

## 6. Add an instrument type

Instruments are nodes in the graph (RFC 0004): the daemon creates instances
with `instrument.add`, each instance's id prefixes its parameter paths, and
its outputs are routed to mixer channels. A new type (a sampler, an FM synth,
a second drum machine) is an Extension.

1. **Protocol.** Add a variant to `InstrumentType` in
   `crates/protocol/src/types.rs` (`id`, `default_id`, `label`, `parse`).
   If it needs a new kind of pattern, add a `PatternData` variant and a
   `ProjectPattern` form; otherwise reuse drum steps or note steps.
   Regenerate bindings.
2. **Engine.** Write a type implementing `Instrument`
   (`crates/engine/src/instrument.rs`) in its own file, following
   `tb303.rs` (one mono voice, note pattern) or `tr808.rs` (several voices,
   a main mix plus direct outs):
   - allocate everything in `new` (output buffers of `MAX_BLOCK * 2`); never
     allocate in `render`, `on_step`, or the setters;
   - declare `params(id)` and outputs (each mono or stereo; main first);
   - keep peaks at or below 1.0.
   Add it to `instrument::make`, `params`, and `outputs`.
3. **Daemon.** Patterns are stored per instance in `Core` (`PatternState`).
   A type that reuses drum or note patterns works with the existing
   `pattern.*` methods; `Core::resolve` picks the default instance by type.
4. **CLI.** Usually nothing: `4s instrument add <type>` and the pattern
   commands cover it. Extend `InstrumentType::parse` aliases if helpful.
5. **UI.** Add an editor component (like `BassEditor.tsx`) and show it from
   `Editor.tsx` for the new type. Use `ParamKnob` for every parameter.

Validate:
- An engine test that the instrument sounds, stays at or below 1.0, and
  decays; `crates/engine/tests/no_alloc.rs` must still pass with it added
  to the command list.
- CLI e2e: add it, program a pattern, and check `4s render` triggers and
  onsets; remove it and check its params are gone.
- A Playwright test for its editor, and a look at the screenshots.
- Document it in [engine.md](engine.md) (synthesis, parameters, outputs).

---

## Not an extension: big changes

These change the system's shape. Outside the build phase they need an RFC
first ([docs/rfcs/](rfcs/README.md)); during the build phase
([RFC 0005](rfcs/0005-build-phase.md)) an RFC is optional, but a short one
is a good place to record the design:

- Audio routing beyond instrument outputs to channels: effects, inserts,
  sends and returns, buses, sidechains.
- Multiple patterns per instrument, per-instrument lengths (polymeter), or
  song arrangement.
- Protocol or sync model changes, the daemon lifecycle, the multiplayer
  topology.
- The UI's overall structure or interaction model.
