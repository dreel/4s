# The in-app assistant

A chat panel in the app where you ask a Claude-powered assistant to change
the project for you ("make the kick punchier", "what does `bass.accent`
do?"). Design: [RFC 0013](rfcs/0013-assistant.md). This page describes what
is built (phase A: the loop and tools; the panel is the next step).

## Where it runs

The loop lives in the **Electron main process**, in `ui/assistant/`:

| File | What it does |
|------|--------------|
| `index.cjs` | Wires it up: settings, the daemon connection, IPC to the renderer (`assistant:*` channels, exposed as `window.fours.assistant` by the preload). |
| `loop.cjs` | The turn loop: streamed Messages API calls with tools, one undo group per turn, stop, usage and cost. |
| `tools.cjs` | The tools and their text results. |
| `schema.cjs` | Reads `schema/4s.schema.json` (method params, method classes) and decides what `rpc` may call. |
| `daemon.cjs` | Its own JSON-RPC connection to 4sd. |
| `settings.cjs` | API key and model. |

It opens its **own** WebSocket connection to the daemon, with
`session.hello {client_name: "assistant", user: <the app's user>}`. So its
edits are yours (Cmd-Z undoes them), the journal shows `origin: assistant`
for everything it did, and it works against a remote engine like any other
client. It only uses RPC.

The renderer never sees the API key. It gets streamed events:
`{type: "text", delta}`, `{type: "tool", name, input, ok, chips, error?}`,
and `{type: "done", stop, label, changed, usage, error?}` at the end of each
turn (`stop` is `end_turn`, `stopped`, `refusal`, `tool_limit`, `error`, ...).

## A turn

1. `history.group_begin {label: "assistant: <your message>"}` on its
   connection.
2. Your message, prefixed with a one-line context header: the focused
   instrument, the selected clip, transport and tempo, and the panel you have
   open (sent by the renderer).
3. Streamed requests until the model stops calling tools. At most 20 tool
   calls per turn; past that, the model is told to wrap up (`tool_choice:
   none`), and a turn that still calls tools ends as `tool_limit`.
4. `history.group_end`, always, also when the turn was stopped or failed. So
   a turn is one undo step, and `done.changed` lists its keys. See
   [journal.md](journal.md#undo-groups).

Stop aborts the stream (or the remaining tool calls) and keeps the
conversation valid for the next message. "New chat" during a turn stops it
and waits for it to end before clearing the conversation.

## Tools

| Tool | What it does |
|------|--------------|
| `overview` | The project as text: tempo, transport, instruments, channels with volume, pan and inserts, seats and focus, MIDI devices. (`state.get`) |
| `find_params {prefix?, query?}` | Matching params with current value, range, unit and label. (`param.list`, `state.get`) |
| `set_params {changes, why}` | Sets params as one `batch`; returns `path old -> new`. |
| `rpc {method, params}` | Any allowed method (below). |
| `describe {method?}` | A method's params schema and whether `rpc` may call it, or the list of methods. |

`listen` (offline renders with metrics) and skills come in phase B.

### What `rpc` may call

A rule over the daemon's own method classes, so a new method is refused
until the daemon classes it:

- allowed: methods the daemon classes **read-only** (`Method::read_only`, not
  journaled) or that **`batch` accepts** (`Method::batchable`: journaled,
  undoable edits, plus performance and controller input), and
  `transport.play/stop/locate`;
- except: `session.*`, `events.*`, `daemon.*`, `project.*` (new, load,
  import, save, list), `history.*` (the turn's undo group is the loop's),
  `journal.export`, `sample.upload`, and `render.offline` (it writes a file
  on the engine host).

- and never with a **file path on the engine host**: `sampler.load`,
  `sample.info`, `sample.peaks` and `sample.play` are refused when given a
  `path` (a name read from the project, or one it was talked into using,
  must not load or read arbitrary files there). Loading a pad from an
  upload id still works, and you load files yourself in the app. A test
  checks that every method whose params describe a file path is refused
  or has its path param listed (`PATH_PARAMS` in `schema.cjs`).

`gen-bindings` writes both classes into `schema/4s.schema.json`
(`read_only_methods`, `batchable_methods`), which is where the assistant
reads them. For a refused method the assistant tells you what to do
instead.

## Model, key, and what is sent

- Models: `claude-sonnet-5-5` (default), `claude-opus-5-5`,
  `claude-haiku-5-5`. Sonnet and Opus requests opt into the API's
  server-side refusal fallback (`fallbacks: "default"`).
- The system prompt and tools are prompt-cached; the per-message context
  header is in the user message, after the cached prefix.
- **The key** is stored in `<data dir>/assistant/settings.json` encrypted
  with Electron `safeStorage` (the macOS keychain, DPAPI on Windows, the
  Linux secret store). Where encryption is not available, the key is
  **not written anywhere**: it is kept for this session only, and the
  settings say so (`keySource: "session"`). With no key set,
  `ANTHROPIC_API_KEY` from the environment is used.
- **What goes to Anthropic:** your messages, the context header, and every
  tool result, which includes parts of your project (parameter values,
  instrument, clip, device, channel and project names, MIDI port names).
  Audio does not leave the machine.
- Tool results and names in the project are data, not instructions: the
  system prompt says so, and the allowlist limits any damage to undoable
  edits in the open project.

## Testing

- `node --test ui/assistant/assistant.test.cjs` (part of
  `scripts/check.sh`): the allowlist, each tool against a headless daemon,
  and whole turns against **a fake Anthropic API** (`ui/e2e/fake-anthropic.cjs`,
  scripted SSE replies): a turn is one undo step that undo reverts, a
  refused method changes nothing, Stop closes the group, the tool-call cap,
  no key, and key storage with and without encryption.
- The loop honours `ANTHROPIC_BASE_URL`, so the Electron e2e can run the app
  against the same fake server. No test needs a real key.
