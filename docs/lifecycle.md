# Daemon Lifecycle

Status: implemented.

`4sd` is a background service. You start and stop it; you do not keep a
terminal open for it. Who starts it and who stops it depends on the mode.

## Modes

| Situation | Who starts 4sd | Who stops 4sd | Electron `FOURS_DAEMON` |
|-----------|----------------|---------------|-------------------------|
| Packaged app (end users) | the app, if none is running | the app on quit, **only if it started it** | `owned` (default when packaged) |
| Development | `scripts/dev.sh`, `4s daemon start`, or the app if none is running | you (`4s daemon stop`) | `detached` (default in dev) |
| Remote / multiplayer | whoever runs the server | whoever runs the server | `external` (default for non-loopback URLs) |

Rules that hold in every mode:

- The app first tries to connect. If a daemon is already running, the app uses
  it and never stops it, even in `owned` mode.
- In `owned` mode the daemon is started with `--parent-pid <app pid>` and
  exits on its own if the app dies, so a crash never leaves an orphaned daemon
  holding the audio device.
- Stopping is an RPC (`daemon.shutdown`), so the CLI, the app, and remote
  clients all stop a daemon the same way. SIGTERM and Ctrl-C also shut down
  cleanly.

## CLI

```
4s daemon start [--listen ADDR] [--no-audio] [--no-midi] [--project P]
4s daemon status        # pid, URL, version, uptime, audio; exit code 3 if not running
4s daemon stop [--force]
4s daemon restart [...start flags]
4s daemon logs [-n N]
```

`start` launches `4sd` in its own process group (closing the terminal or
Ctrl-C does not affect it), waits until it is ready, and returns. It is a
no-op if a daemon is already running for the data dir.

## Development

`scripts/dev.sh` is the one-command dev loop: `cargo build`, then
`4s daemon start --restart-if-stale`, then the Vite dev server plus Electron
(hot reload for renderer code). Closing the window or Ctrl-C stops the UI and
leaves the daemon running.

`--restart-if-stale` compares the `4sd` binary's modification time with the
daemon's start time. If the binary is newer (you changed Rust code and
rebuilt), it restarts the daemon and carries the session over: a saved,
unmodified project is reloaded from its path; otherwise the session is saved
to `<data-dir>/autosave/dev-session.4s` and loaded from there.

If the session cannot be carried over, it asks before discarding it:

- The running daemon is from an incompatible build (a different protocol
  version, so the new CLI cannot talk to it):
  "Stop it and start fresh, discarding its unsaved session? [y/N]". Yes stops
  it with a signal and starts the new daemon with a new project.
- The new daemon cannot load the saved session (e.g. an old project format):
  "Start with a new project instead? [y/N]".

The default, and the answer without a terminal (CI, scripts), is no: nothing
is discarded, and the command fails with a hint to use
`scripts/dev.sh --fresh` (or `4s daemon stop`).

## Runtime file and discovery

While running, the daemon writes `<data-dir>/4sd.json` (pid, URL, version,
data dir, log file, start time) and removes it on exit.

- **One daemon per data dir.** `4sd` refuses to start if the runtime file
  names a live process. Stale files are cleaned up automatically.
- **Discovery.** CLI commands without `--url` use the runtime file's URL, so a
  daemon on a random port (`--listen 127.0.0.1:0`) needs no configuration.
  The Electron app does the same when `FOURS_URL` is not set.
- If you pass a data dir explicitly (`--data-dir` / `FOURS_DATA_DIR`) and no
  daemon is running for it, commands fail with a hint instead of falling back
  to the default port (which might be a different daemon).

## Logs

Background daemons log to `<data-dir>/logs/4sd.log` (`4sd --log-file`).
`4s daemon logs` prints the tail. Running `4sd` directly logs to stderr.

## Configuration

| Variable | Used by | Meaning |
|----------|---------|---------|
| `FOURS_DATA_DIR` | 4sd, 4s, app | data dir (default `~/.4s`) |
| `FOURS_URL` | 4s, app | daemon URL (default: runtime file, else `ws://127.0.0.1:4440`) |
| `FOURS_TOKEN` | 4sd, 4s, app | auth token |
| `FOURS_DAEMON` | app | `owned`, `detached`, or `external` |
| `FOURSD_BIN` | 4s, app | path to the `4sd` binary |
| `FOURSD_ARGS` | app | extra daemon flags (e.g. `--no-audio --no-midi` in tests) |

Binary lookup: `FOURSD_BIN`; in the packaged app `resources/bin/4sd`; in
development `target/debug/4sd`; then `PATH`. The `4s daemon start` command
looks next to the `4s` executable before `PATH`.

## Packaging (future)

The packaged app will ship `4sd` in its resources and run in `owned` mode, so
from the user's perspective it just works: open the app, play, quit.
