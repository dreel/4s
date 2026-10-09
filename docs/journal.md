# The journal: history, undo, and reproducing bugs

The daemon records every request that could change state in an append-only
**journal**: who sent it, from where, when, the request itself, and exactly
what it changed. Undo/redo are built on it, and so is a **recording**: a
session you can replay into another daemon to reproduce what happened and
check that it ends up the same. Design: [RFC 0006](rfcs/0006-journal-and-undo.md).

Use the journal whenever you need to know what actually happened, as
opposed to what a client thinks it did. For example: "the pattern changed by
itself", "undo did something odd", "it broke after I did X on the Block", a
flaky e2e.

## Quick reference

```
4s journal                         # last 50 entries: seq time(UTC) user origin method params -> changed keys
4s journal --limit 200 --for alice # one user's entries
4s journal --since 120             # entries after seq 120
4s journal --follow                # stream new entries live
4s --json journal                  # full entries (params, before/after values, context)

4s undo | 4s redo | 4s history     # your own undo history (--user / FOURS_USER to act as someone)

4s journal export -o rec.json      # this session as a recording (read-only; safe on any daemon)
4s journal export --format sh      # the same as `4s call` lines (approximate, editable)
4s journal replay rec.json         # replay into the daemon and compare entry by entry
4s journal replay x.jsonl          # replay a raw journal file (the last segment; --segment N)
4s journal replay rec.json --realtime   # keep the recorded timing between entries
4s journal replay rec.json --accept     # on divergence, rewrite rec.json with the new behavior
4s project import bundle.4s        # load a project from this machine, sent inline
```

**Replay replaces the target daemon's project.** Always replay into an
isolated daemon, never into one a person is using:

```
export FOURS_DATA_DIR=$(mktemp -d)
target/debug/4s daemon start --no-audio --no-midi --listen 127.0.0.1:0
target/debug/4s journal replay rec.json
target/debug/4s daemon stop
```

It refuses a daemon with unsaved changes unless you pass `--force`.

## What an entry contains

```json
{
  "seq": 57, "time": 1791335794.19, "user": "alice", "origin": "midi:Livid Block",
  "method": "controller.press", "params": {"row": 0, "col": 2, "pressed": true},
  "context": {"playing": true, "step": 9, "page": 0},
  "changes": [{"key": "step:drums.kick.2", "before": null, "after": 1}],
  "reverts": null, "error": null
}
```

- **`user`** owns the change in the undo history. It is set by `--user`,
  `FOURS_USER`, or `$USER` for the CLI. A client that names no user, and
  every MIDI device, belongs to the daemon host's user.
- **`origin`** is the client name from `session.hello` (`ui`, `cli`,
  `replay`), or `midi:<port>` for a device.
- **`method` / `params`** is the RPC as received. MIDI input is recorded as
  the RPC it is equivalent to:

  | Input | Recorded as |
  |-------|-------------|
  | Block pad press | `controller.press` |
  | Block knob | `controller.knob` |
  | Keyboard key | `voice.note_on` / `voice.note_off` |
  | GM drum pad | `voice.trigger` |

  Pad releases do nothing and are not recorded.
- **`context`** is the transport and controller page at that moment. Pads
  map columns to steps through the page, so replay restores it.
- **`changes`** are the keys of the undoable state that changed, with their
  `before` and `after` values. `null` means absent; for params and steps it
  means the default value.

  | Key | Value |
  |-----|-------|
  | `param:<path>` | number (e.g. `param:mixer.2.volume`) |
  | `step:<inst>.<voice>.<i>` | level (1 on, 2 accent) |
  | `note:<inst>.<i>` | `{note, accent, slide}` |
  | `instrument:<id>` | `{type, name}` |
  | `channel:<n>` | name |
  | `channels:order` | `[n, ...]` |
  | `route:<source>` | channel number |

- **`reverts`** is set on `history.undo` / `history.redo` entries. It names
  the entry being reverted.
- **`error`** is set if the request failed. Failed requests are journaled
  too.

Some things are **not** in the changes, because they aren't undoable state:
transport play/stop, the playhead, audio, MIDI connections, and the
controller's target, page, and knob mode. Requests that touch them are
still journaled (with empty `changes`).

## Where it lives

- **In memory:** the last 10,000 entries, for `4s journal` and
  `journal.get`. There is also a `journal` event per entry, which is what
  `4s journal --follow` streams.
- **On disk, on the engine host:** `<data-dir>/journal/<start>-<pid>-<part>.jsonl`.
  - Each line is one JSON entry. Before the entries of each **segment**
    there is a `{"segment": {"seq", "time", "base"}}` line holding the
    whole project at that point.
  - A segment starts at daemon start, `project.new`, `project.load`, or
    `project.import`. These also clear undo history.
  - Each line is flushed as it is written, so the file survives a crash.
  - Files rotate at 50 MB, and the newest 20 are kept.
  - `4sd --no-journal-file` disables the files.
- The default data dir is `~/.4s`. An isolated agent daemon uses its own
  `FOURS_DATA_DIR`.

Times are Unix seconds in the JSON, and UTC in `4s journal` output.

## Recipes

### What happened? Who changed this?

```
4s journal --limit 500 | grep 'param:mixer.2.volume'   # every change to one key
4s journal --limit 500 | grep error                    # every failed request
4s journal --limit 500 | grep 'midi:'                  # everything a device did
```

On a journal file, for example after a crash or for a daemon you can't
reach:

```
python3 - <<'EOF'
import json, sys
for line in open(sys.argv[1] if len(sys.argv) > 1 else "/dev/stdin"):
    e = json.loads(line)
    if "segment" in e:
        print("--- segment at seq", e["segment"]["seq"]); continue
    keys = [c["key"] for c in e["changes"]]
    if any(k.startswith("step:drums.kick") for k in keys):     # your filter here
        print(e["seq"], e["user"], e["origin"], e["method"], keys, e["error"] or "")
EOF
```

### Reproduce a bug someone hit in the app

1. Get the session:
   - If their daemon is still running, run `4s journal export -o bug.json`
     against it. This is read-only and changes nothing for them.
   - Otherwise, take the newest file in their `<data-dir>/journal/`.
2. In an isolated daemon, replay it: `4s journal replay bug.json` (or the
   `.jsonl`). A clean replay ends in exactly their state.
3. Inspect with the normal tools: `4s state`, `4s mixer`, `4s pattern show`,
   `4s render` (audio is not part of the comparison, so render to hear or
   measure it), and the UI pointed at the isolated daemon.
4. If the bug depends on timing (playing, note lengths), use `--realtime`.
   Replay restores the controller page before each pad or knob entry, but
   not the playhead. If the recording was made while playing with the page
   following the playhead, only `--realtime` comes close to the live page
   movement.

**Narrowing it down.** Truncate the recording and replay the prefix:

```
python3 -c "import json,sys; r=json.load(open('bug.json')); r['entries']=r['entries'][:40]; r['digest']=''; json.dump(r, open('bug40.json','w'))"
```

An empty `digest` skips the final-state check; entries are still compared.

**If the daemon crashed:** an entry is written when its request *finishes*,
so the request that crashed is not in the journal. The last entry is the
last one that completed. Check `4s daemon logs` for the panic, and the
client's side for what was sent next.

### "Replay diverged"

```
replay diverged at entry 12 of 40 (recorded seq 57: controller.press by alice from midi:Block):
  recorded: changes [...] error None
  replayed: controller.press changes [...] error None
```

The same requests, applied from the same starting project, produced
different changes. Possible reasons:
- the code changed behavior since the recording was made, which is what a
  regression test is for;
- something isn't deterministic;
- the recording depends on something outside the doc that replay doesn't
  restore, such as engine-host files or real MIDI ports.

Look at the named entry and the ones just before it.

### Turn a bug into a regression test

Recordings in `tests/journals/*.json` are replayed by `scripts/e2e-cli.sh`
on every check.

- **The bug is a wrong result** (the recording captured the buggy changes):
  1. Fix the bug.
  2. Run `4s journal replay tests/journals/<bug>.json --accept` on an
     isolated daemon.
  3. Review the rewritten file with `git diff`. Only the entries the fix
     should change may differ.
  4. Commit it.
- **The bug is an error or crash** (the request fails or panics): write the
  steps as CLI commands against an isolated daemon, fix, then
  `4s journal export -o tests/journals/<bug>.json` and commit. The replay
  now pins the fixed behavior.

Keep fixtures small and named for what they cover. `basic-session.json`
covers drums, a 303, notes, controller pads and knobs, routing,
`channel.move`, two users with a skipped undo, undoing an instrument
removal, and failed requests.

**When a deliberate behavior change breaks a fixture:** replay it with
`--accept`, check the `git diff` shows only the intended change, and commit
it with the change.

### Record a scenario on purpose

Drive an isolated daemon (CLI, `4s call`, the UI, or `virtual_block` for
MIDI), then run `4s journal export -o scenario.json`. To act as several
people, set `FOURS_USER=alice`, `FOURS_USER=bob`, and so on, per command.

## Undo and redo

Each user has their own history. In solo use, the UI, the CLI, and the
Block share one history (the host user).
- Undo reverts your last step. Consecutive edits of the same params, like a
  knob drag, are one step.
- Any key someone else changed since is **kept** and reported:
  `skipped: param:mixer.1.pan (changed by someone else)`. A step whose keys
  were all skipped reports `could not undo`.
- Undoing an instrument removal brings back its params, steps or notes,
  route, and channel.

In the app, use the header buttons or Edit > Undo / Redo (Cmd/Ctrl+Z,
Shift+Cmd+Z / Ctrl+Y). `4s history` shows your stacks.

## Limits

- **Not replayed:** `project.save`, `project.load` (a successful one starts
  a new segment, so one inside a segment is a failed load), `midi.connect`,
  `midi.disconnect`, `midi.rename`, and `midi.set_seat`. They depend on the
  engine host's disk or ports and never change the doc. MIDI input from a
  device is recorded as `midi.input` with the device's profile, so it
  replays the same way without the device.
- **Replay into a daemon with the recording's host user.** Each entry
  records its caller's seat, and replay joins it, but `controller.*` and a
  Block act on the host seat, the daemon's own user (`FOURS_USER`, else the
  OS user). Fixtures in `tests/journals/` are recorded as `e2e`, the user
  `scripts/e2e-cli.sh` runs as.
- **Export covers the current segment only.** If it is longer than the
  in-memory journal (10k entries), export fails; replay the `.jsonl` file
  instead.
- **Several RPCs are several steps.** A UI action that sends several RPCs
  (e.g. a strip's `(none)`) undoes one RPC at a time.
- **An undo or redo with nothing to do is not journaled.** It changes
  nothing.
- **`user` is self-asserted.** Any client that may connect can claim any
  user.
