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
target/debug/4sd --project examples/demo.4s      # start the engine (plays through your default output)

# in another terminal
target/debug/4s play
target/debug/4s pattern set snare "----X-------X---"
target/debug/4s set mixer.3.volume 35%
target/debug/4s state
target/debug/4s stop

# UI
cd ui && npm install && npm start
```

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
