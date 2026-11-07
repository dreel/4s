# RFC 0013: In-app assistant

- Status: accepted
- Author: Sam (dreel), drafted with Claude
- Created: 2026-11-07
- Discussion: the `assistant-rfc` PR

## Summary

Add a chat panel to the UI where you talk to a Claude-powered assistant that
drives the daemon for you: "make the kick punchier", "darker bass", "set up my
Akai to play the synth", "what does `bass.accent` do?". It runs a plain
Messages API tool loop with the user's own API key, uses a small curated set
of tools on top of the existing RPC API (not one tool per method), loads
"skills" (short markdown playbooks) on demand or through slash commands, and
can *listen* to the result with offline renders so it can check its own work.
Each assistant turn is one undo step.

## Motivation

The CLI already makes 4S agent-drivable, but only from a terminal with Claude
Code or similar next to it. Most of the value of an agent here is in the
middle of a session, with your hands on the gear: describing a sound instead
of hunting for the parameter, asking how something works, or setting up MIDI
without the CLI (Sam's request after the MIDI focus fix). Bringing the agent into the app also
lets it see what you're looking at (focused instrument, selected clip), which
makes "this" and "here" work.

## Design

### Where it runs

```
 Electron renderer                Electron main                   4sd
 +----------------+   IPC    +--------------------+   WS/RPC   +--------+
 | AssistantPanel | <------> | assistant loop     | <--------> | Core   |
 |  chat, chips,  |          |  Anthropic SDK     |            |        |
 |  slash menu    |          |  tools -> RPC      |            |        |
 +----------------+          |  key in keychain   |            +--------+
        |  normal UI RPC (unchanged)              |                 ^
        +-----------------------------------------+-----------------+
```

- The loop lives in the **Electron main process**, in a small TS module
  (`ui/assistant/`), using `@anthropic-ai/sdk` with streaming.
- The API key is stored with Electron `safeStorage` (macOS keychain) and never
  reaches the renderer. Setting: a key field plus model picker in a settings
  popover on the panel.
- Main opens its **own** WebSocket connection to the daemon with
  `session.hello {client: "assistant", user: <the UI's user>}`. Same user as
  you, so Cmd-Z undoes the assistant's work like your own, and the journal
  shows `origin: assistant` for everything it did. It goes through RPC like
  any other client, so a remote engine and other players just see events.
- Default model `claude-sonnet-5-5`, selectable (Opus 5.5 for harder
  sessions, Haiku 5.5 for cheap ones). System prompt and tool definitions are
  prompt-cached.

The alternative (loop inside `4sd`, exposed as `assistant.*` RPCs) is
discussed under Alternatives. The tool layer is written against the RPC API
only, so moving it later is a port, not a redesign.

### Tools (small, curated)

The API has ~120 methods; exposing each as a tool wastes context and confuses
the model. Instead:

| Tool | What it does | Built on |
|------|--------------|----------|
| `overview` | Compact text summary: tempo, playing, instruments and types, channels and device chains, focus, selected clips, MIDI devices. | `state.get` |
| `find_params {prefix? , query?}` | Matching params with current value, range, unit, label. | `param.list`, `param.get` |
| `set_params {changes: [{path, value}], why}` | Applies changes as one `batch`; returns old -> new for each. | `batch` |
| `rpc {method, params}` | Escape hatch for structural edits (add device, edit clip, seats, MIDI). Refuses methods on a denylist (below). | any method |
| `describe {method}` | Params/result schema for one method, from `schema/4s.schema.json`. | generated schema |
| `listen {target?, bars?}` | Renders offline (optionally soloing one instrument or channel) and returns metrics. | `render.offline` (+ phase B additions) |
| `load_skill {name}` | Pulls a skill's playbook into context. | skill files |

Denylist for `rpc` (not undoable or outside the session): `daemon.shutdown`,
`project.new/load/import`, `project.save` to a new path, `history.*`,
`journal.export`. The assistant asks you to do these, or the panel shows a
confirm button. Everything else is journaled and undoable, so it is applied
without asking.

### Context it gets each turn

- A short static system prompt: what 4S is, path conventions, "prefer small
  changes and say what you changed", "listen before and after when the ask is
  about sound".
- A dynamic header on each user message: focused instrument, selected clip,
  transport state, which panel is open. Not the whole snapshot; the model
  calls `overview` when it needs more.
- The list of skills (name + one line) so it can load one itself.

### Skills and slash commands

A skill is a markdown file with frontmatter (`name`, `description`,
`command?`), shipped in `ui/assistant/skills/` and overridable or extendable
from `~/.4s/skills/`. Starting set:

- `punch` (`/punch [instrument]`): transient shaping for drums. Kick: shorter
  decay, more pitch-envelope click, compressor with 10 to 30 ms attack and
  fast release, a touch of saturation once we have it. Verify by crest factor
  and attack time before/after.
- `tone` (`/darker`, `/brighter`): filter/EQ moves per instrument type.
- `mix` (`/mix`): gain staging against master peak, panning, sends.
- `midi-setup` (`/midi`): connect a device, bind it to a seat, map knobs. This
  covers "MIDI setup in the app".
- `beat` (`/beat <style>`): write a clip for the drums.
- `explain` (`/explain <param or device>`): read-only.

Typing `/` in the panel opens a menu of commands. A slash command sends the
skill body as context plus your arguments, then runs a normal turn.

### Listening

`render.offline` already returns peak, RMS, onsets and triggers. For "punchier"
and "darker" to be checkable we add (phase B):

- `RenderParams.solo: Option<Vec<String>>`: render only these instruments or
  channels (stems), without touching mute/solo state.
- `RenderResult.analysis`: crest factor, mean attack time at onsets, and
  energy in a few bands (sub/low/mid/high). Computed in the daemon from the
  rendered buffer, off the audio thread.

The assistant reports before/after numbers in its reply, and you can hit play.

### One turn = one undo step

`batch` groups one call, but a turn makes several tool calls with renders in
between. Add a connection-scoped group:

- `history.group_begin {label}` / `history.group_end`: every journal entry
  from this connection in between merges into one undo step labelled e.g.
  "assistant: punchier kick". Ends automatically if the connection drops.
  (CLI: `4s history group begin/end`.)

Each assistant message in the panel gets an **Undo** button (calls
`history.undo` as you when it is still your latest step; otherwise it says
what is in the way), and Cmd-Z works as usual.

### Panel UI

- A collapsible right-side drawer, toggled from the header and with Cmd-K.
- Streams text. Tool calls show as compact chips: `drums.kick.decay 0.62 ->
  0.41`, `+ compressor on drums`, `listened: kick crest 9.1 -> 12.4 dB`.
  Clicking a param chip focuses that control.
- Stop button cancels the turn (the group still closes, so it stays undoable).
- Conversation persists per project in the UI's data dir
  (`~/.4s/assistant/<project-id>.json`), not in the project bundle. "New chat"
  clears it. Old turns are trimmed or summarised past a token budget.
- Shows an estimated cost for the session from the usage the API returns.

## Impact on the principles

- **Agent-drivable / API parity**: the assistant is a client and only uses
  RPC. The new daemon pieces (`history.group_*`, render `solo`/`analysis`) get
  RPCs and CLI commands as usual, which also helps external agents. The
  assistant itself is UI-only in v1; see open questions.
- **Multiplayer / network transparency**: works against a remote engine since
  it talks RPC. Each player's assistant runs with their own key and user, and
  edits show up for everyone with `origin: assistant`.
- **Real-time safety**: nothing new on the audio thread. Analysis runs on the
  offline render task.
- **Validation**: see below; a fake API server makes it testable without a key.
- **Modularity**: works off the param registry and schema, so new instruments
  and devices are usable by the assistant with no assistant changes (skills
  may want a line about them).

## Alternatives

- **Loop in the daemon (`assistant.*` RPCs, `4s ask`)**: best API parity, and
  a shared chat for a band. But the key would live on the engine host (shared
  billing on a remote engine), the daemon gains an HTTP client and LLM code
  next to the audio engine, and there's no official Rust SDK. Better as a
  later move if `4s ask` or shared chats matter.
- **Claude Agent SDK / Claude Code as the engine**: gives skills, slash
  commands and compaction for free, but brings a shell and file access into a
  music app and a much bigger runtime. The plain Messages API with seven tools
  is small and easy to reason about.
- **One tool per RPC method**: generated from the schema, ~120 tools. Too much
  context per call and worse tool choice. `rpc` + `describe` covers the long
  tail.
- **Renderer-side loop**: simplest, but puts the key in the renderer.

## Migration and compatibility

No project format change. `PROTOCOL_VERSION` bumps once for the new RPCs and
fields (additive). New UI dependency: `@anthropic-ai/sdk` in main.

## Validation plan

- Unit tests for the tool layer against a headless daemon (`overview` text,
  `set_params` diffs, denylist).
- E2E (Playwright) with a **fake Anthropic server** (`ANTHROPIC_BASE_URL`)
  that replays scripted tool calls: type "punchier kick", check the params
  changed over RPC, one undo step, Undo button reverts all of it, Stop works.
- Daemon tests for `history.group_*` (merging, auto-close on disconnect,
  interleaving with another user) and render analysis (known test signals).
- An opt-in eval script (needs a key): a dozen prompts ("punchier kick",
  "darker bass", "set up my Akai for the synth", "four on the floor at 128")
  with checks via render metrics and state.

## Plan (phases, as a draft PR stack)

- **A: talk and tweak.** Key + model settings, loop in main, panel with
  streaming and chips, tools `overview`/`find_params`/`set_params`/`rpc`/
  `describe`, denylist, `history.group_*` + per-turn Undo, fake-server e2e.
- **B: listen and skills.** Render `solo` + `analysis`, `listen` tool,
  skill files + `load_skill`, slash menu, the starting skills above.
- **C: polish.** Persistence per project, selection context ("this"),
  cost display, user skills from `~/.4s/skills`, history trimming.

## Open questions

1. ~~Loop in Electron main or in the daemon?~~ Decided 2026-11-07: Electron main.
2. Should the assistant act straight away (recommended, since everything is
   undoable) or propose a diff you approve first? A per-session "ask first"
   toggle is cheap to add.
3. Same user as you for undo (recommended) or its own user, so Cmd-Z doesn't
   touch its edits?
4. Is a CLI `4s ask` wanted soon? If yes, that tips question 1 toward the
   daemon.
