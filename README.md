# 4S

Sam's Sequencer and Synthesizer Set -- a modular electronic music suite with a
Rust audio daemon, a CLI, and a thin Electron UI.

Current: a TR-808-style drum machine (8 synthesized voices, 64-step sequencer
with swing, 8-channel mixer) played from a Livid Block, the UI, or the CLI.

See [AGENTS.md](AGENTS.md) for principles and [docs/](docs/) for design notes.

## Quickstart

Prerequisites: Rust (via [rustup](https://rustup.rs)) and Node.js 22+.

```sh
cargo build
target/debug/4s daemon start --project examples/demo.4s   # runs in the background
target/debug/4s play
target/debug/4s pattern set snare "----X-------X---"
target/debug/4s set mixer.3.volume 35%
target/debug/4s state
target/debug/4s stop
target/debug/4s daemon stop

# UI (starts the daemon itself if it is not running)
cd ui && npm install && npm start
```

See [docs/lifecycle.md](docs/lifecycle.md) for how the daemon is started and
stopped in development, in the packaged app, and for remote use.

A Livid Block is auto-connected when plugged in. Its MIDI map lives in
`~/.4s/livid-block.json`; see [docs/hardware/livid-block.md](docs/hardware/livid-block.md).

## Layout

- `crates/protocol` -- shared types; the API is declared once here
- `crates/engine` -- drum voices, sequencer, mixer, offline render
- `crates/daemon` -- `4sd`: state, RPC server, audio, MIDI
- `crates/cli` -- `4s`
- `ui` -- Electron + React + Tailwind

## Tests

`scripts/check.sh` runs everything: Rust tests, generated-code freshness, the
CLI end-to-end script, and Electron end-to-end tests.
