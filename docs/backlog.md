# Backlog

Things noticed but not done: possible bugs, follow-ups, docs that disagree
with the code, review suggestions left for later. A plain list until a better
tracker exists (AGENTS.md, "Log what you notice but don't do").

- Add an item in the branch you are working on when you notice it; don't
  stop to fix something outside your change's scope.
- One bullet per item: `(date, source) area: what, and where in the code`.
  The source is a PR, a review, or what you were doing when you noticed.
- Delete the item in the PR that fixes it (git history keeps the record). If
  it turns out not to matter, delete it and say why in the commit message.
- Unverified is fine: say "possible" and what would confirm it.

## Bugs

- (2026-10-09, #24 review) seats: a client whose `session.hello` has no user
  counts as the host user, so a remote UI opened without `?user=` joins the
  host's seat and can change its focus and bindings
  (`crates/daemon/src/core/seats.rs`, hello handling). Limit the default to
  local clients and add an e2e for a remote client with no user.
- (2026-10-09, #25 review) pitch bend: a bend held through a focus change,
  a target that resolves elsewhere, or an unplug leaves the old instrument
  bent; the wheel's return to center goes to the new target
  (`input_pitch_bend`). Remember the slot each device bent and reset it.
- (2026-10-09, #25 review) relative CCs: raw 64 moves by -64 steps; many
  encoders send 64 for "no change" (`cc_to`, `CcMode::Relative`).
- (2026-10-09, #25 review) relative CCs on a toggle parameter move it in
  1/200 steps; flip it instead, or reject relative mode for toggles.
- (2026-10-09, #25 review) auto-connect connects every port whose name
  contains "block" with the `livid_block` profile (`block`, `block_2`, ...);
  before, at most one Block. Confirm or restore the one-Block guard
  (`midi_autoconnect`); `main.rs`'s comment still mentions only the Block.
- (2026-10-09, #24 follow-up) `set_step` rewrites an unchanged step's event
  (velocity 100 becomes 89 when setting an "on" step to on); return early
  when the level is the same.
- (2026-10-09, #24 review) project load: one unreadable voice string fails
  the whole drum pattern for that instrument; before, only that voice was
  reported (`crates/daemon/src/core/clips.rs`, `from_project`).
- (2026-10-09, #24 follow-up) a held key and a clip note on the same pitch
  end each other's notes (documented); revisit with recording.
- (2026-10-09, #25 review) `midi.rename` rewrites every `input:*:<old>`
  holder, including notes held by a remote bridge that uses the same device
  name; its later note-off under the old name then misses them.
- (2026-10-09, #24 follow-up) a remote `midi.input` without `profile` takes
  the profile (and now the model) of a local device with the same name; use
  the local device's only for input from this machine.
- (2026-10-09, #25 review) `seat.apply_layout` with `model` and an empty
  `ports` list applies nothing without an error.
- (2026-10-09, #25 review) an invalid `midi-devices.json` is moved to
  `.json.bak`, overwriting any earlier `.bak` (`crates/daemon/src/hardware.rs`).
- (2026-10-09, #25 follow-up) remote clients can call `midi.set_seat` and
  `midi.rename`; `seat create`/`claim` on the host by another `--user` moves
  the host's devices; pickup keys don't follow a device rename;
  `midi.input` doesn't cap its length.
- (2026-10-09, #25 follow-up) a seat-level `pitch_bend` is ignored for a
  model device until its layout is applied (the layout's own target wins).
- (2026-10-10, recording branch review) recording: a take maps song ticks onto
  the loop by the current length, but the global loop keeps its own
  position when `sequencer.length` changes while playing; a length change
  during a take places later notes off. Report the loop position with
  `Feedback::Live` (`crates/daemon/src/core/record.rs`, `Take::flush`).
- (2026-10-10, recording branch review) recording: `Feedback::Live` is sent even
  when the instrument ignores the note (a non-GM note on a `tr808`), so
  recording into a drum machine stores silent events
  (`crates/engine/src/engine.rs`, `Command::NoteOn`).
- (2026-10-10, recording branch review) recording: dropped feedback (a full
  feedback ring) can leave `starting` set (playhead frozen until the next
  play) or a note open until the take ends (recorded take-long).
- (2026-10-10, recording branch review) recording: a write that fails when a take
  ends or restarts is lost (only pass writes are retried); `write_take`
  tells a busy engine from an invalid clip by comparing error codes; name
  the codes.
- (2026-10-10, recording branch review) recording: in `replace` mode the
  write when a take ends or restarts clears up to the end of the current
  step (`play_tick + TICKS_PER_STEP`), including the part the playhead had
  not reached (during a count-in, the first step); bound the cleared span
  by the heard tick (`crates/daemon/src/core/record.rs`,
  `end_take`/`restart_take`).
- (2026-10-10, recording branch review) recording: `transport_record` saves
  the new settings before `self.slot(&id)?`, so a request that fails there
  still changes them (`crates/daemon/src/core/record.rs`).
- (2026-10-10, recording branch review) recording, replace mode: a clip
  event at or past the loop length (left after shortening a clip) is only
  cleared by a pass that covers the whole loop (`Take::flush`); decide and
  document.
- (2026-10-10, recording branch review) quantize: `snap_tick`'s wrap at the
  loop's end can still leave a tick past the loop when the loop is shorter
  than the grid (a 10-tick clip, 1/16 grid); wrap with `%`
  (`crates/protocol/src/clip.rs`).
- (2026-10-10, recording branch review) recording: `settle_take` runs after
  only the settings are checked, so `transport.record {arm: false,
  instrument: "nope"}` ends the take and then fails on the instrument.
- (2026-10-10, recording branch review) recording: while the command queue
  stays full, a pass write is retried every step and each failed try is a
  journal entry; retry at the next pass instead, or journal only the first
  failure.

## Follow-ups

- (2026-10-09, #24 review) UI constants `TICKS_PER_STEP` and the GM drum
  notes are copied by hand into `ui/src/components/Editor.tsx` and
  `voices.ts` (and the record quantize grids into `Transport.tsx`, from
  `clip.rs`'s `GRIDS`); export them through the generated bindings.
- (2026-10-09, #24 review) editor: the hidden-note count misses extra 303
  chord notes on a step's first tick, and a clip length that isn't a whole
  number of steps shows as "loops every 2.5 steps".
- (2026-10-09, #24 review) `notes_result.length` reports `sequencer.length`
  even when the clip has its own length.
- (2026-10-09, #25 review) `Seat.defaults` is a display string; the UI gets
  the device name with `d.split(" ")[0]` (`ui/src/components/Seats.tsx`).
  Make it structured (`{device, model, label}`).
- (2026-10-09, #25 review) the Livid Block (empty layout) shows as a
  "default layout" in every seat with an "edit" that applies nothing; list
  only devices whose layout has entries for their role, in the seat they
  play in. A device with only CC maps or knobs is left out of `defaults`
  though its notes still play the model's bindings.
- (2026-10-09, #25 review) no RPC or CLI sets or clears a seat's
  `pitch_bend` (only `seat.apply_layout` and project files).
- (2026-10-09, #25 review) no e2e shows pitch bend bending what the keys
  play (only that it isn't journaled).
- (2026-10-09, #24/#25 follow-up) CC and Block input build the full journal
  doc (one key per clip event) twice per message; diff only the touched keys.
- (2026-10-09, #25 follow-up) `cc learn` from an endless encoder stores an
  absolute map; add a way to learn relative mode, or say so in the help.
- (2026-10-09, #24/#25 follow-up) auto-connect of model ports and
  auto-reconnect of hand-connected ports have no e2e (the e2e daemons run
  with `--no-midi`).
- (2026-10-09, #25 follow-up) the 303 note-stack unit test duplicates the
  e2e legato checks.
- (2026-10-10, recording branch review) record UI: while someone else's take
  runs, the Rec button ends it (any client can); show the owner
  (`RecordState.user`) in its title, and say so in `docs/rpc.md`.
- (2026-10-10, recording branch review) the `parse_grid` asserts in
  `clip.rs`'s unit test restate `GRIDS`; drop them and add a CLI e2e check
  that `--quantize 1/8t` is a 32-tick grid.
- (2026-10-10, recording branch review) render input: a note with
  `duration` 0 sends its note-off on the same frame as its note-on (a
  silent, 1-tick recorded note); require a positive duration.
- (2026-10-10, recording branch review) recording: when the connection that
  armed a take closes, the take keeps running and later passes are
  journaled with that gone client's context (no seat); end the take, or
  keep the seat on the take.
- (2026-10-10, recording branch review) `4s render --replace/--quantize/...`
  without `--record` or `--to` ignores those flags silently; require one.
- (2026-10-10, recording branch review) `4s render --to` alone turns on
  recording; there is no way to play `--input` into a non-focus instrument
  without recording.
- (2026-10-10, recording branch) SMF import/export (RFC 0007 phase 3) is not
  built.

- (2026-10-10, song branch review) a song-mode take is written as a
  `batch` that is not atomic: if `clip.new` succeeds and a later request
  fails with a busy engine, the retry next pass makes another clip, leaving
  an empty, unplaced one (`crates/daemon/src/core/record.rs`,
  `write_take`). Make `batch` all or nothing, or check room for the whole
  write first.
- (2026-10-10, song branch review) a song-mode take's `Write::New` relies
  on `clip.new` giving the id `make_clip` computed (`next_id`); a partial
  write that failed shifts it (same fix as the item above).
- (2026-10-10, song branch review) recording: a note heard in the output
  latency just after a `transport.locate` jump gets a song position before
  the new point (the engine's heard position does not know the jump); the
  restarted take then places it before where it starts.
- (2026-10-10, song branch review) `song.loop_start` and `song.loop_end`
  are set separately, so `start >= end` is accepted and the loop silently
  does nothing; the CLI checks it, the UI inputs and `param.set` do not.
- (2026-10-10, song branch review) turning `song.mode` on while playing
  goes on from the locate point plus the time played (the engine's song
  position counts in pattern mode too), not from the locate point.
- (2026-10-10, song branch review) `apply_sets`' clip-table check counts a
  restored instrument's clip 1 twice (conservative: with exactly enough
  free entries the undo is refused); `validate_project` covers the load's
  `Locate` and per-instrument select commands only by slack.
- (2026-10-10, song branch review) a project load resets the song start to
  bar 1 without a `located` event (clients catch up through `reset`).
- (2026-10-10, song branch review) `batch` allows `project.save`,
  `midi.input`, `seat.claim/leave`, and `voice.*`; `BatchParams` docs don't
  say so.
- (2026-10-10, song branch review) `apply_clip_sets` renames a clip before
  `edit_clip`, which can still fail (full queue), leaving the name changed;
  `new_clip` with `select` sends `track` twice; `ADD_COMMANDS` (96) should
  be re-checked against the largest instrument plus the track's commands.
- (2026-10-10, song branch review) recording, smaller cases: the offline
  recorder skips the live `offset_ms` loop-wrap correction
  (`record_feedback`); `settle_take` writes a take before a
  `transport.locate` is validated; a song loop of 2 steps or less is due at
  its end tick, after it wrapped; if the clip a take made is deleted or
  moved during the take, later grow writes fail as invalid and drop that
  pass.
- (2026-10-10, song branch review) `delete_clip` selects another clip
  before `set_arrangement`, which can still fail (full queue).
- (2026-10-10, song branch) `song.loop` is an integer parameter (0 off, 1
  song, 2 bars); the registry has no enum kind, so `4s params` and generic
  controls show a number. Add a choice kind with labels.
- (2026-10-10, song branch) placing clips has no UI until the arrangement
  view (RFC 0008 phase C); the CLI and RPC place them.
- (2026-10-10, song branch) the transport row is crowded (record, song, and
  tempo controls in one line); give it a layout pass with the arrangement
  view.

- (2026-10-10, piano-roll branch review) take notes are only events, not
  in the snapshot: a client that reconnects during a take shows none until
  the next note or step; the CLI e2e's `take_notes` check waits a fixed
  0.2 s for `watch` to subscribe.
- (2026-10-10, piano-roll branch review) smaller piano roll and take
  cases: held take notes are drawn `offset_ms` too long (the step tick goes
  to `emit_take_notes` without the offset, `record.rs`); `clip.quantize
  --notes` ignores keys that are not in the clip instead of rejecting them;
  dragging a selection against the grid's edge can land two notes on one
  `tick:note`; take notes outside the rows (non-drum clips) are not drawn
  until written; resizing is not clamped to the loop.
- (2026-10-10, piano-roll branch) the piano roll has no copy/paste,
  keyboard nudging, or scroll-to-notes; its 808 view still shows the step
  editor below it (with the voices' knobs).

## Mismatches

- (2026-10-10, recording branch review) RFC 0008 says live-note ticks are
  "counted in the unswung tick grid"; `heard_tick` follows the swung clock
  (each swung step's ticks evenly spaced). Fix the RFC wording.
- (2026-10-09, #24/#25 follow-up) RFC 0007's main "Migration" section
  predates the v3/v4 split (still says v3/protocol 3 and clip swapping); the
  implementation notes override it.
- (2026-10-09, #25 review) the daemon's fallback host user is now `local`
  (was `me`); check `docs/journal.md` and the seats docs, which talk about
  the host user.

## Process
