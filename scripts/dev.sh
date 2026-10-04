#!/usr/bin/env bash
# One-command development mode. From the repo root:
#
#   scripts/dev.sh              build, start (or refresh) the daemon, open the UI
#                               with hot reload
#   scripts/dev.sh --no-ui      build + daemon only (CLI / agent work)
#   scripts/dev.sh --headless   daemon without audio device or MIDI
#   scripts/dev.sh --fresh      stop any running daemon first (discards its state)
#   scripts/dev.sh -- ARGS      extra args for `4s daemon start`,
#                               e.g. -- --project examples/demo.4s
#
# The daemon keeps running after the UI closes (dev mode = detached lifecycle,
# see docs/lifecycle.md). If Rust code changed since it started, it is
# restarted and the session carried over. Stop it with: target/debug/4s daemon stop
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || export PATH="$HOME/.cargo/bin:$PATH"

usage() { sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; }

UI=1
FRESH=0
DAEMON_ARGS=()
while [[ $# -gt 0 ]]; do
  case $1 in
    --no-ui) UI=0 ;;
    --headless) DAEMON_ARGS+=(--no-audio --no-midi) ;;
    --fresh) FRESH=1 ;;
    -h | --help) usage; exit 0 ;;
    --) shift; DAEMON_ARGS+=("$@"); break ;;
    *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
  esac
  shift
done

CLI=target/debug/4s

echo "==> cargo build"
cargo build -q

if [[ $UI == 1 && ( ! -d ui/node_modules || ui/package-lock.json -nt ui/node_modules ) ]]; then
  echo "==> npm install (ui)"
  (cd ui && npm install --no-audit --no-fund)
fi

if [[ $FRESH == 1 ]]; then
  "$CLI" daemon stop || true
fi
echo "==> daemon"
"$CLI" daemon start --restart-if-stale ${DAEMON_ARGS[@]+"${DAEMON_ARGS[@]}"}

if [[ $UI == 0 ]]; then
  echo "daemon ready. Stop it with: $CLI daemon stop"
  exit 0
fi

PORT=${FOURS_UI_PORT:-5174}
echo "==> ui (vite dev server on :$PORT, hot reload)"
(cd ui && exec ./node_modules/.bin/vite --port "$PORT" --strictPort --logLevel warn) &
VITE=$!
cleanup() {
  kill "$VITE" 2>/dev/null || true
  wait "$VITE" 2>/dev/null || true
  echo
  echo "UI closed; daemon still running. Stop it with: $CLI daemon stop"
}
trap cleanup EXIT

for _ in $(seq 100); do
  curl -sf -o /dev/null "http://localhost:$PORT" && break
  sleep 0.1
done

cd ui
FOURS_UI_DEV_URL="http://localhost:$PORT" FOURS_DAEMON=detached ./node_modules/.bin/electron .
