#!/usr/bin/env bash
# End-to-end check of daemon + CLI: starts a headless 4sd on a random port,
# drives it only through the `4s` CLI, and asserts on the results.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || export PATH="$HOME/.cargo/bin:$PATH"
cargo build -q -p fours-daemon --example virtual_block

BIN=target/debug
TMP=$(mktemp -d)
# The daemon is found through the runtime file in this data dir; no URL needed.
export FOURS_DATA_DIR="$TMP/data"
unset FOURS_URL
trap '"$BIN/4s" daemon stop --force >/dev/null 2>&1 || true; rm -rf "$TMP"' EXIT
s() { "$BIN/4s" "$@"; }
pass=0
check() { # check <description> <expected substring> <command...>
  local desc=$1 want=$2; shift 2
  local got; got=$("$@" 2>&1) || true
  if [[ "$got" == *"$want"* ]]; then pass=$((pass + 1)); echo "ok   $desc";
  else echo "FAIL $desc"; echo "  wanted: $want"; echo "  got: $got"; exit 1; fi
}

check "not running yet" "4sd not running" s daemon status
check "daemon start returns" "4sd started" s daemon start --no-audio --no-midi --listen 127.0.0.1:0
check "second start is a no-op" "already running" s daemon start --no-audio --no-midi
check "daemon status" "4sd running (pid $(python3 -c "import json;print(json.load(open('$FOURS_DATA_DIR/4sd.json'))['pid'])"))" s daemon status
check "direct 4sd refused" "already running" "$BIN/4sd" --no-audio --no-midi --listen 127.0.0.1:0
check "hello/status" "transport: stopped" s status
check "set percent" "mixer.3.volume = 0.35" s set mixer.3.volume 35%
check "get" "mixer.3.volume = 0.35" s get mixer.3.volume
check "clamp" "transport.tempo = 300" s tempo 9999
check "tempo" "transport.tempo = 120" s tempo 120
check "negative value" "mixer.1.pan = -0.5" s set mixer.1.pan -0.5
check "unknown param" "unknown parameter" s set nope 1
check "pattern set" "kick        X--- x--- X--- x---" s pattern set kick "X---x---X---x---"
check "leading hyphen" "snare       ---- x--- ---- x---" s pattern set snare "----x-------x---"
check "toggle" "clap step 13 = on" s pattern toggle clap 13
check "accent" "cowbell step 16 = accent" s pattern step cowbell 16 accent
check "step range" "step must be 1..64" s pattern step kick 65 on
check "steps are 1-based" "step must be 1..64" s pattern step kick 0 on
check "virtual pad" "#" s controller press 2 1
check "pad edited pattern" "snare       x--- x---" s pattern show snare
check "knob mode" "knobs: Decay" s controller mode --knobs decay
check "knob" "page: 1" s controller knob 1 100%
check "knob set param" "drums.kick.decay = 1" s get drums.kick.decay
# MIDI hotplug: a virtual device that appears after the daemon started must be
# seen, connectable, and pruned when it goes away (macOS CoreMIDI regression).
VDEV="4S E2E Pad $$"
mkfifo "$TMP/vdev.in"
target/debug/examples/virtual_block "$VDEV" < "$TMP/vdev.in" > "$TMP/vdev.out" 2>&1 &
exec 7> "$TMP/vdev.in"
for _ in $(seq 50); do grep -q ready "$TMP/vdev.out" && break; sleep 0.1; done
# CoreMIDI announces new devices asynchronously; give it a moment.
for _ in $(seq 30); do s midi ports | grep -q "$VDEV" && break; sleep 0.1; done
check "hotplugged device listed" "$VDEV" s midi ports
check "connect virtual block" "$VDEV (LividBlock)" s midi connect "$VDEV" --kind block
s pattern clear kick >/dev/null
echo "pad 0 2" >&7
echo "raw 91 03 7F" >&7   # note-on on MIDI channel 2: must be ignored
sleep 0.5
check "pad press from device edits pattern (other channels ignored)" "kick        --x- ---- ---- ----" s pattern show kick
# Knob 2 (CC 2) in decay mode -> snare decay. 127 -> 1.0, which the default
# (0.4) cannot match.
echo "knob 1 127" >&7; sleep 0.5
check "knob turn from device sets its parameter" "drums.snare.decay = 1" s get drums.snare.decay
check "device receives LED updates" "recv 90 02 7F" cat "$TMP/vdev.out"
exec 7>&-; sleep 3
check "unplugged device pruned" "(none)" s midi ports
s pattern set kick "X---x---X---x---" >/dev/null   # restore for later checks

check "play" "playing" s play
check "playhead events" "playhead" s watch --type playhead --count 2
check "stop" "stopped" s stop
check "render: onsets match distinct hit times" "triggers: 9  detected onsets: 5" s render --bars 1 --out renders/e2e.wav
# Mute and solo are audible, not just stored: the kick still triggers but its
# step-9 hit (where nothing else plays) disappears from the audio.
s set mixer.1.mute on >/dev/null
check "mute removes kick from the audio" "triggers: 9  detected onsets: 4" s render --bars 1 --out renders/mute.wav
s set mixer.1.mute off >/dev/null
s set mixer.8.solo on >/dev/null
check "solo leaves only the cowbell" "triggers: 9  detected onsets: 1" s render --bars 1 --out renders/solo.wav
s set mixer.8.solo off >/dev/null
check "reveal needs a saved project" "save it first" s project reveal --no-open
check "save" "e2e.4s" s project save e2e
check "reveal prints location" "$FOURS_DATA_DIR/projects/e2e.4s" s project reveal --no-open
check "new clears" "kick        ---- ---- ---- ----" bash -c "$BIN/4s project new >/dev/null && $BIN/4s pattern show kick"
check "load restores" "kick        X--- x--- X--- x---" bash -c "$BIN/4s project load e2e >/dev/null && $BIN/4s pattern show kick"
check "json output" '"value": 0.35' s --json get mixer.3.volume
check "raw call" '"backend": "null"' s call engine.status
check "daemon logs" "listening on ws://" s daemon logs
check "daemon stop" "4sd stopped" s daemon stop
check "runtime file removed" "absent" bash -c "test -e '$FOURS_DATA_DIR/4sd.json' && echo present || echo absent"
check "stopped status" "4sd not running" s daemon status
check "commands hint at start" "4s daemon start" s state
check "restart from stopped" "4sd started" s daemon restart --no-audio --no-midi --listen 127.0.0.1:0
check "project survives restart" "kind:" bash -c "$BIN/4s project load e2e >/dev/null && echo kind: ok"
check "not stale: start is a no-op" "already running" s daemon start --restart-if-stale --no-audio --no-midi
s tempo 97 >/dev/null
sleep 1; touch "$BIN/4sd"   # simulate a rebuild after the daemon started
check "stale: restarted" "restarted 4sd (code changed" s daemon start --restart-if-stale --no-audio --no-midi --listen 127.0.0.1:0
check "stale: unsaved session carried over" "transport.tempo = 97" s get transport.tempo
check "final stop" "4sd stopped" s daemon stop

echo "all $pass checks passed"
