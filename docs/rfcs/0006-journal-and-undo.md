# RFC 0006: Journal and per-user undo

- Status: implemented (part A: journal, undo/redo); part B (recordings and
  replay) planned
- Author: Sam (@dreel), drafted with Claude
- Created: 2026-10-06
- Discussion: the PR that introduces this RFC (build phase, RFC 0005: a
  design note, not a gate)

## Summary

The daemon keeps a **journal**: an append-only record of every request that
could change state. Each entry records who sent it, when, the request
itself, and the changes it made as `(key, before, after)` over the
project's addressable state. **Undo and redo are per user** and are built on
the journal. Undoing reverts only your own changes. Anything another user
changed since is left alone and reported as skipped. An undo is itself a
journal entry, so the record is never rewritten. That makes the journal a
complete history of a session. Part B uses it to reproduce bugs and to turn
them into tests.

## Motivation

There was no undo. Several clients edit one engine: the UI, the CLI, MIDI
controllers, and later remote collaborators. One global undo stack would let
your Cmd+Z revert someone else's work. A timestamped record of everything
done, by whom, is also what you need to reproduce a bug or write a test from
a real session.

## Design

### Doc keys

The undoable state is a flat map built by `Core::doc`. Values are JSON.
Params and steps at their defaults are left out, so `null` means "default"
for them and "absent" for everything else.

| Key | Value |
|-----|-------|
| `param:<path>` | number |
| `step:<inst>.<voice>.<i>` | level (1, 2) |
| `note:<inst>.<i>` | `{note, accent, slide}` |
| `instrument:<id>` | `{type, name}` |
| `channel:<n>` | name |
| `channels:order` | `[n, ...]` (display order) |
| `route:<source>` | channel number |

Steps are keyed per step, so two people editing different steps of one
track never conflict. Transport, the playhead, MIDI connections, and the
controller view (target, page, knob mode) are not in the doc and are not
undoable. Tempo, swing, and length are params, so they are undoable.

### Recording

`Core::handle` classifies each request with an exhaustive `match` (no
wildcard), so every new method must be marked read-only or not. For each
non-read request, `Core` snapshots the doc, runs the request, and diffs the
doc again. That gives one entry:

```
{seq, time, user, origin, method, params, context: {playing, step, page},
 changes: [{key, before, after}], reverts?, error?}
```

There is no per-RPC inverse code: a new RPC is journaled and undoable as soon
as it changes state. Failed requests are journaled too, with `error`.
Requests that can never change the doc (transport, notes and triggers,
controller mode, MIDI connections, project save) are journaled without
building the doc, which matters for a stream of notes from a keyboard.

MIDI input is recorded as the **equivalent RPC** that the input already
decodes to:
- Block pads and knobs become `controller.press` and `controller.knob`;
- keyboards become `voice.note_on` and `voice.note_off`;
- GM drums become `voice.trigger`.

The origin is `midi:<port>`, and the user is the host user.

`project.new` and `project.load` start a fresh history.

The journal keeps the last 10k entries in memory (`journal.get`, and a
`journal` event per entry). A writer thread appends entries to JSONL files
in `<data_dir>/journal/`, flushed per entry; files rotate at 50 MB, and the
newest 20 are kept. File I/O never happens under the core lock or on the
audio thread. `4sd --no-journal-file` keeps the journal in memory only.

### Identity

`session.hello` takes an optional `user`. The CLI sends `--user`,
`FOURS_USER`, or `$USER`. A client that sends no user, and all MIDI input,
belongs to the **daemon host's user**. So in solo use the UI, the CLI, and
the Livid Block share one history. Connections can't be the identity,
because the CLI opens a new connection per command.

`user` is self-asserted: any client that may connect (has the token, if
one is set) can claim any user, and so undo as them. That matches today's
trust model, where every connected client is a trusted collaborator. Real
accounts are a later change.

### Undo and redo

- Each user has an undo stack and a redo stack, capped at 200 steps.
- An edit pushes a step and clears that user's redo stack.
- **Coalescing:** consecutive edits by the same user that contain only
  params, with the same keys, merge into one step. A knob drag, or turning a
  MIDI knob, is one step. Undoing breaks the run.
  - The rule is key-based, not time-based, so replay is deterministic. The
    cost: two separate tweaks of the same knob, with nothing in between by
    that user, are one step.
  - Another user's edit of the same key in between breaks the run, so your
    undo never jumps back past their value.
- **Applying a step** sets each key back to its `before`:
  - It is checked as one batch first: queue room, free slots, instruments
    in flight, and channel limits.
  - Then it applies in an order that keeps the graph valid. Channels and
    instruments are created, then the order, routes, params, and steps are
    restored (grouped per track), then instruments and channels are removed.
- **Conflicts:** `last_touch` records the last user to write each key.
  - Undo skips a key another user wrote last.
  - Creating or removing an instrument is skipped if another user wrote its
    key, or (when removing) anything it owns. The keys it owns are skipped
    with it.
  - A channel that still has other sources is not removed.
  - Skipped keys are reported (`skipped`). A step whose keys were all
    skipped is dropped from the stack.
- **The entry it writes:** an undo is journaled as `history.undo` with
  `reverts: <seq>`. Its changes are what was actually set, and they become
  the redo step. So a redo restores exactly what the undo replaced, and redo
  runs the same conflict check.

### API

| Method | CLI | Notes |
|--------|-----|-------|
| `history.undo` / `history.redo` | `4s undo` / `4s redo` | returns `{label, changed, skipped, history}` |
| `history.get` | `4s history` | the caller's stacks (labels, newest first) |
| `journal.get {since?, limit?, user?}` | `4s journal [--since N] [--for U] [--limit N]` | `4s journal --follow` streams `journal` events |

There are two new events. `history {user, undo_label, redo_label,
undo_count, redo_count}` is sent when a user's summary changes. `journal
{entry}` is sent for every entry.

The UI has undo and redo buttons in the header. In Electron, the app's
Edit > Undo / Redo menu items (Cmd/Ctrl+Z, Shift+Cmd+Z / Ctrl+Y) are sent to
the page. A focused text field keeps its own undo; otherwise the item calls
`history.undo` / `history.redo`. In a plain browser, the same keys are
handled in the page.

## Impact on the principles

- **Agent-drivable / API parity:** every capability is an RPC with a CLI
  command. The journal makes every change observable after the fact.
- **Multiplayer / network transparency:** history is per user and lives on
  the engine. Clients read it over RPC and events, and nothing assumes a
  shared file system. Conflicts are explicit (skipped and reported) rather
  than silent last-write-wins.
- **Real-time safety:** everything runs in `Core` under its lock, off the
  audio thread. Undo sends the same engine commands as the edits it
  reverts, after checking queue room.
- **Validation:** CLI e2e and Electron e2e tests, plus unit tests for the
  selective-undo logic.
- **Modularity:** keys are the existing param paths and graph names. A new
  instrument type is undoable with no extra code.

## Alternatives

- **One global stack:** simplest, but with collaborators your undo reverts
  their work.
- **Command pattern (hand-written inverses):** precise, but every RPC needs
  an inverse, and forgetting one breaks undo silently.
- **Whole-project snapshots:** restoring one reverts everyone, and
  re-applying a project rebuilds every instrument (an audible glitch).
- **OT/CRDT transforms for selective undo:** the most correct option, but
  heavy machinery for state that is mostly last-write-wins values.

## Migration and compatibility

Everything is additive: an optional `user` in hello, new methods, and new
events. There is no project-format change and no protocol-version bump. An
older client simply ignores the new events.

## Part B: recordings (planned)

- `journal.export` returns `{base: ProjectFile, entries, digest}` for the
  current history segment.
- `project.import {file}` loads a project sent inline.
- `4s journal replay <file>` re-sends each entry as its recorded user (one
  connection per user), restores the recorded controller page before pad
  presses, and compares changes entry by entry plus the final digest.
- A mismatch exits non-zero, so a saved recording works as a regression test
  (`tests/journals/`).

## Open questions

- Grouping several RPCs into one undo step, e.g. the strip's `(none)`, which
  unroutes each source with its own call.
- Exporting from the middle of a segment, by reverse-applying diffs to get
  the base.
- An RPC to inject raw MIDI, for replaying input that has no RPC
  equivalent.
- Whether history should survive `daemon start --restart-if-stale`. Today it
  does not; the session's state carries over, but its history doesn't.
