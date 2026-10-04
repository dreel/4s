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

The kit has eight voices (`Voice` in `crates/protocol/src/types.rs`), each
synthesized by a type implementing `DrumVoice` in
`crates/engine/src/voices.rs`.

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
- Parameters are read at trigger time (`Engine::voice_params`). A parameter
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

Parameters are addressed by path (`drums.kick.decay`, `mixer.3.pan`) and
stored in a flat array indexed by `ParamId`
(`crates/engine/src/params.rs`).

1. Extend the index layout. The offsets are derived from each other
   (`VOICE_BASE`, `MIXER_BASE`, `MASTER_VOLUME`, `NUM_PARAMS =
   MASTER_VOLUME + 1`), so:
   - a new **per-voice** parameter: add a `VoiceParam` variant, bump
     `VOICE_PARAMS`, add the field to `VoiceParams`
     (`crates/engine/src/voices.rs`), and fill it in `Engine::voice_params`;
   - a new **per-channel** parameter: add a `MixerParam` variant and bump
     `MIXER_PARAMS`;
   - a new **global** parameter: add a constant after `MASTER_VOLUME` and
     update `NUM_PARAMS`.
   Then add the matching `ParamInfo` entry **in the same position** in
   `registry()`. `layout_matches_registry` catches a mismatch; add your path
   to it.
2. Use the value in the engine (`Engine` reads `self.params[...]`).
3. Clients: `param.list`, `4s params`, `4s get/set`, and project files pick
   it up automatically. **The UI does not**: controls are placed by path.
   Add a `ParamKnob` for it (e.g. in a mixer strip in
   `ui/src/components/Mixer.tsx`, or in `Transport.tsx`). Range, label, and
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
controller with feedback; `DeviceKind::GenericDrums` (note-on triggers
voices, in `Core::handle_midi`) is the simpler reference for note-only
devices.

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
2. Use `ParamKnob` (`Mixer.tsx`) for parameters, so range, label, and
   default come from the registry instead of being duplicated.
3. Give interactive elements a `data-testid`.
4. Everything the panel does must already be an RPC with a CLI command
   (recipe 4). Local desktop actions are the only exception
   ([api-parity.md](api-parity.md)).

Validate:
- A Playwright test in `ui/e2e/`. Check the screenshot the suite writes
  (`ui/test-results/groove.png`) for layout regressions.

---

## Not an extension: needs an RFC

These change the system's shape. Write an RFC first
([docs/rfcs/](rfcs/README.md)):

- **New instrument types** (a synth, a sampler, a second drum machine). The
  engine is currently one fixed 8-voice kit: `Voice`, `NUM_TRACKS`, the
  8x8 Block layout, and `mixer.1..8` all assume it.
- Audio routing: effects, sends and returns, buses, sidechains.
- More or fewer than 8 tracks, or multiple patterns and song arrangement.
- Protocol or sync model changes, the daemon lifecycle, the multiplayer
  topology.
- The UI's overall structure or interaction model.

### Next RFC to write: an instrument framework

To let contributors add instruments as Extensions, 4S needs an instrument
abstraction. Questions that RFC should answer:

- How an instrument is instantiated and addressed. Parameter paths like
  `instruments.<id>.<param>`, and how the current `drums.*` and `mixer.N.*`
  paths migrate.
- How instruments receive notes or steps (a sequencer per instrument?) and
  route into mixer channels.
- How the real-time engine adds and removes instruments without allocating
  on the audio thread.
- How controllers and the UI discover an instrument's controls (the
  parameter registry generalizes naturally).
- Project-format and protocol changes, with migration for existing projects.
