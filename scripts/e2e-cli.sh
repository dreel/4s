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
# The daemon's host seat and the CLI's seat are matched by this user name.
export FOURS_USER=e2e
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
check "default graph: one 808 on channel 1" "ch 1  Drums        vol 100%  pan C          <- drums" s mixer
# Default levels match the old fixed kit (before RFC 0004): this groove
# rendered on main at 17d668c gave peak 0.40297, rms 0.08550.
s pattern set kick "X---x---X---x---" >/dev/null
s pattern set snare "----x-------x---" >/dev/null
s pattern set closed_hat "x-x-x-x-x-x-x-xX" >/dev/null
s pattern set cowbell "---------------X" >/dev/null
check "default levels match the old kit within 0.1 dB" "same level" python3 -c "
import json, math, subprocess
r = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render', '--bars', '1', '--out', 'renders/ref.wav']))
db = lambda a, b: abs(20 * math.log10(a / b))
print('same level' if db(r['peak'], 0.40297) < 0.1 and db(r['rms'], 0.08550) < 0.1 else f'peak {r[\"peak\"]} rms {r[\"rms\"]}')"
s pattern clear >/dev/null
check "set percent" "drums.snare.level = 0.35" s set drums.snare.level 35%
check "get" "drums.snare.level = 0.35" s get drums.snare.level
check "clamp" "transport.tempo = 300" s tempo 9999
check "tempo" "transport.tempo = 120" s tempo 120
check "negative value" "mixer.1.pan = -0.5" s set mixer.1.pan -0.5
s set mixer.1.pan 0 >/dev/null
check "unknown param" "unknown parameter" s set nope 1
check "invalid value" "invalid value 'loud'" s set mixer.1.volume loud
check "pattern set" "kick        X--- x--- X--- x---" s pattern set kick "X---x---X---x---"
check "leading hyphen" "snare       ---- x--- ---- x---" s pattern set snare "----x-------x---"
check "toggle" "clap step 13 = on" s pattern toggle clap 13
check "accent" "cowbell step 16 = accent" s pattern step cowbell 16 accent
check "step range" "step must be 1..64" s pattern step kick 65 on
check "steps are 1-based" "step must be 1..64" s pattern step kick 0 on
check "voice by alias" "closed_hat  " s pattern show ch
check "voice by 1-based track number" "cowbell     " s pattern show 8
check "virtual pad" "#" s controller press 2 1
check "pad edited pattern" "snare       x--- x---" s pattern show snare
check "controller follows the seat's focus: the 808" "focus: drums  knobs: volume" s controller
check "knob page" "knobs: decay" s knobs page decay
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
check "connect virtual block" "$VDEV as pad (LividBlock)" s midi connect "$VDEV" --name pad --profile block
s pattern clear kick >/dev/null
echo "pad 0 2" >&7
echo "raw 91 03 7F" >&7   # note-on on MIDI channel 2: must be ignored
sleep 0.5
check "pad press from device edits pattern (other channels ignored)" "kick        --x- ---- ---- ----" s pattern show kick
# Knob 2 (CC 2) on the decay page -> snare decay. Knobs pick up: one far
# from the current value does nothing until it passes it.
echo "knob 1 0" >&7; sleep 0.5
check "a knob far from the value does not jump it (pickup)" "drums.snare.decay = 0.4" s get drums.snare.decay
echo "knob 1 127" >&7; sleep 0.5
check "passing the value picks it up" "drums.snare.decay = 1" s get drums.snare.decay
s knobs page volume >/dev/null
echo "knob 2 127" >&7; echo "knob 2 0" >&7; sleep 0.5
check "volume page moves the voice level in the 808" "drums.clap.level = 0" s get drums.clap.level
s set drums.clap.level 0.8 >/dev/null
s knobs page decay >/dev/null
check "device receives LED updates (row 1, step 3 = note 16: notes run down columns)" "recv 90 10 7F" cat "$TMP/vdev.out"
exec 7>&-; sleep 3
check "unplugged device pruned" "(none)" s midi ports
s pattern set kick "X---x---X---x---" >/dev/null   # restore for later checks

check "play" "playing" s play
check "playhead events" "playhead" s watch --type playhead --count 2
check "stop" "stopped" s stop
check "render: onsets match distinct hit times" "triggers: 9 (drums 9)  detected onsets: 5" s render --bars 1 --out renders/e2e.wav
# Mute and solo are audible, not just stored: the kick still triggers but its
# step-9 hit (where nothing else plays) disappears from the audio.
check "mute kick in the 808" "drums.kick.mute = 1" s set drums.kick.mute on
check "voice mute removes kick from the audio" "triggers: 9 (drums 9)  detected onsets: 4" s render --bars 1 --out renders/mute.wav
check "unmute kick" "drums.kick.mute = 0" s set drums.kick.mute off

# --- Instruments, channels, routing (RFC 0004) ---
check "instrument types" "tb303  Bass   default id \`bass\`" s instrument types
check "add a 303 on a new channel" "bass       tb303  Bass" s instrument add tb303
check "its channel is in the mixer" "ch 2  Bass         vol 100%  pan C          <- bass" s mixer
check "its params are registered" "bass.cutoff" s params bass
check "notes round trip" "bass: C2 C2! D#2~ - G1 - C3 - C2 - - - G1 G1 A#1 -" s notes bass "C2 C2! D#2~ - G1 - C3 - C2 - - - G1 G1 A#1 -"
check "bad note" "out of range" s notes bass "C9"
check "set one note step" "bass: C2 C2! D#2~ - G1 - C3 - C2 - - - G1 G1 A#1 G2!~" s note bass 16 "G2!~"
s note bass 16 - >/dev/null
check "render reports bass triggers" "triggers: 18 (bass 9, drums 9)" s render --bars 1 --out renders/bass.wav
# Bass alone (drums muted): every note in the pattern is retriggered (the
# one slide leads into a rest), so each bass trigger shows up as an onset
# at its time.
s set mixer.1.mute on >/dev/null
check "bass onsets land on its notes" "9 onsets on 9 notes" python3 -c "
import json, subprocess
r = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render', '--bars', '1', '--out', 'renders/bass-only.wav']))
notes = [t['time'] for t in r['triggers'] if t['instrument'] == 'bass']
ok = len(notes) == len(r['onsets']) and all(abs(a - b) < 0.01 for a, b in zip(notes, r['onsets']))
print(f'{len(r[\"onsets\"])} onsets on {len(notes)} notes' if ok else f'onsets {r[\"onsets\"]} vs notes {notes}')"
s set mixer.1.mute off >/dev/null
"$BIN/4s" watch --type notes_changed --count 1 --json > "$TMP/notes.json" &
WATCH=$!; sleep 0.5
s note bass 2 C3 >/dev/null
wait $WATCH
s note bass 2 "C2!" >/dev/null
check "note events carry the instrument" '"type":"notes_changed","instrument":"bass"' cat "$TMP/notes.json"
# A MIDI keyboard without bindings plays the seat's focus: key down starts a
# held note, key up releases it.
KDEV="4S E2E Keys $$"
mkfifo "$TMP/kdev.in"
target/debug/examples/virtual_block "$KDEV" < "$TMP/kdev.in" > "$TMP/kdev.out" 2>&1 &
exec 8> "$TMP/kdev.in"
for _ in $(seq 50); do grep -q ready "$TMP/kdev.out" && break; sleep 0.1; done
for _ in $(seq 30); do s midi ports | grep -q "$KDEV" && break; sleep 0.1; done
s instrument add tb303 --id lead2 --no-channel >/dev/null
s focus lead2 >/dev/null
check "a seat focused on a removed instrument falls back to the first" "focus: drums " bash -c "$BIN/4s instrument rm lead2 >/dev/null && $BIN/4s controller"
check "focus the bass" "focus: bass" s focus bass
check "connect a keyboard as a named device" "$KDEV as keys (Generic) out: -" s midi connect "$KDEV" --name keys
check "the name is saved for this machine" '"name": "keys"' cat "$FOURS_DATA_DIR/midi-devices.json"
"$BIN/4s" watch --type trigger --count 1 --json > "$TMP/key.json" &
WATCH=$!; sleep 0.5
echo "raw 90 24 64" >&8   # note on, C2 (36)
wait $WATCH
check "key down plays the note" '"instrument":"bass","voice":null,"note":36' cat "$TMP/key.json"
"$BIN/4s" watch --type meters --json > "$TMP/key-meters.json" &
WATCH=$!; sleep 0.6
echo "raw 80 24 00" >&8   # note off
sleep 0.8; kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
check "held key sustains, key up releases" "sustained then released" python3 -c "
import json
levels = [c['left'] for l in open('$TMP/key-meters.json') if l.strip()
          for c in json.loads(l)['event']['channels'] if c['channel'] == 2]
print('sustained then released' if levels and max(levels[:5]) > 0.01 and levels[-1] < 1e-4 else f'levels {levels}')"
"$BIN/4s" watch --type meters --json > "$TMP/key-meters2.json" &
WATCH=$!
echo "raw 90 24 64" >&8; sleep 0.6   # hold a key...
s midi disconnect "$KDEV" >/dev/null   # ...and disconnect the keyboard
sleep 0.8; kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
check "disconnecting a keyboard releases its held note" "released" python3 -c "
import json
levels = [c['left'] for l in open('$TMP/key-meters2.json') if l.strip()
          for c in json.loads(l)['event']['channels'] if c['channel'] == 2]
print('released' if levels and max(levels) > 0.01 and levels[-1] < 1e-4 else f'levels {levels}')"
# Bindings (RFC 0006): channel 10 plays the drums' GM voices, and the upper
# keys play the bass an octave down. Once a device has bindings, notes no
# binding matches do nothing.
check "reconnecting keeps the saved name" "$KDEV as keys (Generic)" s midi connect "$KDEV"
check "bind channel 10 to the drums" "bind 1: keys ch 10 all notes -> drums" s bind keys --channel 10 --to drums
check "bind a split with transpose" "bind 2: keys any ch C3..C8 -12 st -> bass" s bind keys --notes C3..C8 --transpose -12 --to bass
"$BIN/4s" watch --type trigger --count 2 --json > "$TMP/gm.json" &
WATCH=$!; sleep 0.5
echo "raw 99 26 64" >&8   # GM 38 = snare, on channel 10 (below the split)
echo "raw 80 26 00" >&8
echo "raw 90 18 64" >&8   # C1: no binding matches
echo "raw 80 18 00" >&8
echo "raw 90 3C 64" >&8   # C4 -> C3 on the bass
echo "raw 80 3C 00" >&8
wait $WATCH
check "channel 10 plays the drum voice" '"instrument":"drums","voice":"snare"' cat "$TMP/gm.json"
check "the split plays the bass transposed" '"instrument":"bass","voice":null,"note":48' cat "$TMP/gm.json"
check "nothing else played" "2 triggers" bash -c "echo \$(wc -l < '$TMP/gm.json' | tr -d ' ') triggers"
check "unbind removes one binding" "bind 1: keys ch 10 all notes -> drums" s unbind 2
s unbind 1 >/dev/null
s midi disconnect keys >/dev/null
exec 8>&-
BASS_RMS=$(s --json render --bars 1 --out renders/b1.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['rms'])")
s set mixer.2.mute on >/dev/null
MUTED_RMS=$(s --json render --bars 1 --out renders/b2.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['rms'])")
check "muting the bass channel lowers RMS" "lower" python3 -c "print('lower' if $MUTED_RMS < $BASS_RMS * 0.95 else 'same: $MUTED_RMS vs $BASS_RMS')"
s set mixer.2.mute off >/dev/null
s set mixer.2.volume 0 >/dev/null
check "bass fader at 0 matches mute" "same" python3 -c "r=$(s --json render --bars 1 --out renders/b3.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['rms'])"); print('same' if abs(r - $MUTED_RMS) < 1e-4 else f'differs {r} vs $MUTED_RMS')"
s set mixer.2.volume 1 >/dev/null
check "solo the bass channel" "mixer.2.solo = 1" s set mixer.2.solo on
SOLO_RMS=$(s --json render --bars 1 --out renders/b4.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['rms'])")
s set mixer.2.solo off >/dev/null
s set mixer.1.mute on >/dev/null
ALONE_RMS=$(s --json render --bars 1 --out renders/b5.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['rms'])")
s set mixer.1.mute off >/dev/null
check "solo on the bass = drums muted" "same" python3 -c "print('same' if abs($SOLO_RMS - $ALONE_RMS) < 1e-4 and $SOLO_RMS > 0.01 else 'differs: $SOLO_RMS vs $ALONE_RMS')"
check "mono pan hard left: right side silent" "right: peak 0.000 rms 0.0000" bash -c "$BIN/4s set mixer.2.pan -1 >/dev/null && $BIN/4s set mixer.1.mute on >/dev/null && $BIN/4s render --bars 1 --out renders/p1.wav"
s set mixer.2.pan 0 >/dev/null; s set mixer.1.mute off >/dev/null; s set mixer.2.mute on >/dev/null
check "stereo balance hard left: right side silent" "right: peak 0.000 rms 0.0000" bash -c "$BIN/4s set mixer.1.pan -1 >/dev/null && $BIN/4s render --bars 1 --out renders/p2.wav"
CENTER=$(s set mixer.1.pan 0 >/dev/null; s --json render --bars 1 --out renders/p3.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['left']['rms'])")
LEFT=$(s set mixer.1.pan -1 >/dev/null; s --json render --bars 1 --out renders/p4.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['left']['rms'])")
check "balance keeps the near side at unity" "unity" python3 -c "print('unity' if abs($LEFT - $CENTER) < 1e-4 else 'changed: $LEFT vs $CENTER')"
s set mixer.1.pan 0 >/dev/null; s set mixer.2.mute off >/dev/null
check "add a channel" "ch 3 Kick" s channel add --name Kick
check "break the kick out to it" "ch 3  Kick         <- drums.kick" s route drums.kick 3
s set mixer.3.mute on >/dev/null
check "muting the kick's channel removes the kick" "triggers: 18 (bass 9, drums 9)  detected onsets: 0" bash -c "$BIN/4s set mixer.2.mute on >/dev/null; $BIN/4s pattern --instrument drums show kick >/dev/null; $BIN/4s set drums.snare.mute on >/dev/null; $BIN/4s set drums.clap.mute on >/dev/null; $BIN/4s set drums.closed_hat.mute on >/dev/null; $BIN/4s set drums.cowbell.mute on >/dev/null; $BIN/4s render --bars 1 --out renders/k1.wav | grep -v '^onsets'"
check "unrouting returns the kick to the Drums mix" "detected onsets: 4" bash -c "$BIN/4s route drums.kick none >/dev/null && $BIN/4s render --bars 1 --out renders/k2.wav"
for v in snare clap closed_hat cowbell; do s set drums.$v.mute off >/dev/null; done
s set mixer.2.mute off >/dev/null
check "rename a channel" "ch 3 Hits" s channel rename 3 Hits
check "remove a channel" "ch 2  Bass         <- bass" s channel rm 3
check "a reused channel number starts at defaults" "mixer.3.mute = 0" bash -c "$BIN/4s channel add >/dev/null && $BIN/4s get mixer.3.mute"
s channel rm 3 >/dev/null
check "unknown source" "no output 'nope'" s route nope 1
check "instrument list" "bass       tb303  Bass" s instrument list
check "add onto an existing channel" "ch 2  Bass         vol 100%  pan C          <- bass, bass2" bash -c "$BIN/4s instrument add tb303 --channel 2 >/dev/null && $BIN/4s mixer"
check "removing it keeps the shared channel" "ch 2  Bass         vol 100%  pan C          <- bass" bash -c "$BIN/4s instrument rm bass2 >/dev/null && $BIN/4s mixer"
RMS_BEFORE=$(s --json render --bars 1 --out renders/nc1.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['rms'])")
s instrument add tb303 --id lead --no-channel >/dev/null
s notes lead "C3 C3 C3 C3" >/dev/null
check "--no-channel leaves the main out unrouted" "unrouted, 2 channels" python3 -c "
import json, subprocess
g = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'state']))['graph']
print('unrouted, %d channels' % len(g['channels']) if 'lead' not in g['routes'] else g)"
check "an unrouted instrument plays but is not heard" "silent lead" python3 -c "
import json, subprocess
r = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render', '--bars', '1', '--out', 'renders/nc2.wav']))
lead = sum(1 for t in r['triggers'] if t['instrument'] == 'lead')
print('silent lead' if lead == 4 and abs(r['rms'] - $RMS_BEFORE) < 1e-6 else f'lead {lead} rms {r[\"rms\"]} vs $RMS_BEFORE')"
s channel add --name Lead >/dev/null
s route lead 3 >/dev/null
check "rm --keep-channels keeps the emptied channel" "ch 3  Lead         vol 100%  pan C          <- (nothing)" bash -c "$BIN/4s instrument rm lead --keep-channels >/dev/null && $BIN/4s mixer"
s channel rm 3 >/dev/null
for _ in $(seq 30); do s channel add >/dev/null; done
check "channel pool limit" "at most 32 channels" s channel add
for n in $(seq 3 32); do s channel rm "$n" >/dev/null; done
for _ in $(seq 14); do s instrument add tb303 --no-channel >/dev/null; done
check "instrument pool limit" "at most 16 instruments" s instrument add tb303 --no-channel
for n in $(seq 2 15); do s instrument rm "bass$n" >/dev/null; done
check "pools back to the start" "ch 2  Bass         vol 100%  pan C          <- bass" s mixer
mkdir -p "$FOURS_DATA_DIR/projects/bad.4s"
python3 -c "
import json
p = {'format_version': 2, 'instruments': [{'id': 'x', 'type': 'tb303', 'name': 'X'}, {'id': 'x', 'type': 'tr808', 'name': 'X'}],
     'channels': [], 'routes': {}, 'params': {}, 'patterns': {}, 'controller': {'knob_mode': 'volume', 'follow': True}}
json.dump(p, open('$FOURS_DATA_DIR/projects/bad.4s/project.json', 'w'))"
check "an invalid project is rejected" "duplicate instrument id 'x'" s project load bad
check "and leaves the current project as it was" "ch 2  Bass         vol 100%  pan C          <- bass" s mixer
rm -rf "$FOURS_DATA_DIR/projects/bad.4s"
# Held notes over RPC, as a bridge or script would send them. A note belongs
# to the connection that started it: only it can release the note, and it
# is released when that connection closes.
"$BIN/4s" watch --type meters --json > "$TMP/rpc-key.json" &
WATCH=$!
s key C2 --instrument bass --for 0.6 >/dev/null; sleep 0.8
kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
check "4s key holds, then releases" "sustained then released" python3 -c "
import json
levels = [c['left'] for l in open('$TMP/rpc-key.json') if l.strip()
          for c in json.loads(l)['event']['channels'] if c['channel'] == 2]
print('sustained then released' if levels and max(levels[:5]) > 0.01 and levels[-1] < 1e-4 else f'levels {levels}')"
"$BIN/4s" watch --type meters --json > "$TMP/rpc-key2.json" &
WATCH=$!
"$BIN/4s" key C2 --for 1.2 >/dev/null &
KEY=$!; sleep 0.3
s call voice.note_off '{"note": 36}' >/dev/null   # another connection
sleep 0.5
wait $KEY; sleep 0.6
kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
check "another client's note-off does not release the note" "held until its owner let go" python3 -c "
import json
levels = [c['left'] for l in open('$TMP/rpc-key2.json') if l.strip()
          for c in json.loads(l)['event']['channels'] if c['channel'] == 2]
n = len(levels); print('held until its owner let go' if n > 30 and min(levels[3:30]) > 0.01 and levels[-1] < 1e-4 else f'levels {levels}')"
"$BIN/4s" watch --type meters --json > "$TMP/rpc-key3.json" &
WATCH=$!; sleep 0.2
# A client that dies while holding a note: kill `4s key` mid-note. The note
# sounds while the client is connected and stops when its connection drops.
python3 - "$BIN/4s" <<'PY' &
import subprocess, sys, time
p = subprocess.Popen([sys.argv[1], "key", "C2", "--for", "10"])
time.sleep(0.6)
p.kill()
PY
KILLER=$!
sleep 1.6; wait $KILLER
kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
check "a client that disconnects releases its notes" "sounded, then released" python3 -c "
import json
levels = [c['left'] for l in open('$TMP/rpc-key3.json') if l.strip()
          for c in json.loads(l)['event']['channels'] if c['channel'] == 2]
print('sounded, then released' if levels and max(levels) > 0.01 and levels[-1] < 1e-4 else f'levels {levels}')"
check "trigger takes a voice or a note, not both" "exactly one" s call voice.trigger '{"voice": "kick", "note": 36}'
check "trigger needs a voice or a note" "exactly one" s call voice.trigger '{}'
check "empty names are rejected" "name must not be empty" s channel add --name " "
check "channel or no_channel, not both" "not both" s instrument add tb303 --channel 1 --no-channel
check "second 808 gets a numbered id" "drums2     tr808  Drums 2" s instrument add tr808
check "pattern on a chosen instrument" "drums2:" s pattern --instrument drums2 show kick
"$BIN/4s" watch --type trigger --count 1 --json > "$TMP/audition.json" &
WATCH=$!; sleep 0.5
check "audition a note" "ok" s trigger --note C2
wait $WATCH
check "the audition plays the bass" '"instrument":"bass","voice":null,"note":36' cat "$TMP/audition.json"
check "focus the second 808" "focus: drums2" bash -c "$BIN/4s focus drums2 >/dev/null && $BIN/4s controller"
check "removing the focus refocuses the first instrument" "focus: drums " bash -c "$BIN/4s instrument rm drums2 >/dev/null && $BIN/4s controller"
s focus bass >/dev/null
check "removing an instrument removes its empty channel" "ch 1  Drums        <- drums" s instrument rm bass
check "drums render unaffected after removing the bass" "triggers: 9 (drums 9)" s render --bars 1 --out renders/after-rm.wav
check "its params are gone" "bass params: 0" bash -c "echo bass params: \$($BIN/4s params bass | wc -l | tr -d ' ')"
# Events say which instrument they belong to.
"$BIN/4s" watch --type step_changed --count 1 --json > "$TMP/events.json" &
WATCH=$!; sleep 0.5
s pattern step clap 2 on >/dev/null
wait $WATCH
check "step events carry the instrument" '"instrument":"drums"' cat "$TMP/events.json"
check "no 808 left: calls without instrument explain" "no tr808 instrument" bash -c "$BIN/4s instrument rm drums >/dev/null && $BIN/4s pattern show kick"
check "no instruments left: note calls explain" "no instruments" s trigger --note C2
check "controller has no focus" "focus: (none)" s controller
check "no focus: grid is dark" "dark" bash -c "$BIN/4s controller | grep -q '#' && echo lit || echo dark"
check "an added 808 is the focus" "focus: drums " bash -c "$BIN/4s instrument add tr808 >/dev/null && $BIN/4s controller"
s pattern set kick "X---x---X---x---" >/dev/null
s pattern set snare "----x-------x---" >/dev/null
s pattern step clap 13 on >/dev/null
s pattern step cowbell 16 accent >/dev/null
s set drums.snare.level 35% >/dev/null
s play >/dev/null
check "meters are per channel" '"channel":1' s watch --type meters --count 1 --json
s stop >/dev/null
mkdir -p "$FOURS_DATA_DIR/projects/old.4s"
echo '{"format_version": 1, "params": {}, "patterns": {}, "controller": {"knob_mode": "volume", "follow": true}}' > "$FOURS_DATA_DIR/projects/old.4s/project.json"
check "v1 projects are rejected" "no longer supported" s project load old
rm -rf "$FOURS_DATA_DIR/projects/old.4s"
s instrument add tb303 >/dev/null
s notes bass "C2 - G1! C3~" >/dev/null
s set bass.cutoff 0.25 >/dev/null
# Seats (RFC 0006): each performer has a focus and bindings; the CLI joins
# the seat matching its user, and the host's devices play in the host seat.
check "the CLI joins the seat matching its user" "you: e2e" s seat
check "it is this engine's devices' seat" "e2e: cli#" s seat
check "a user with no matching seat is asked to choose" '"choose_seat": true' s call session.hello '{"client_name": "x", "protocol_version": 3, "user": "bob"}'
check "and stays unseated" "you: (no seat)" s --user bob seat
check "seat edits need a seat" "you have no seat" s --user bob focus drums
check "create a seat for bob" "you: bob" s --user bob --new-seat seat
check "bob focuses the drums" "focus: drums" s --seat bob focus drums
check "e2e focuses the bass" "focus: bass" s focus bass
"$BIN/4s" watch --type trigger --count 2 --json > "$TMP/seats.json" &
WATCH=$!; sleep 0.5
s --seat bob midi send pads 90 24 64 >/dev/null   # C2 in bob's seat: the 808's kick
s midi send pads 90 24 64 >/dev/null              # C2 in e2e's seat: the bass
wait $WATCH
check "bob's device plays bob's focus" '"instrument":"drums","voice":"kick"' cat "$TMP/seats.json"
check "the same device name in another seat plays that seat's focus" '"instrument":"bass","voice":null,"note":36' cat "$TMP/seats.json"
check "ignore: a seat for this session only" "carol: cli#" bash -c "$BIN/4s --user carol seat create --ignore | grep carol"
check "an empty session-only seat goes away" "no carol" bash -c "$BIN/4s seat | grep -q carol && echo carol || echo no carol"
check "pin this machine's devices to bob's seat" "devices play in seat: bob (pinned)" s midi seat bob
check "the Block follows bob's focus" "focus: drums" s controller
check "unpin" "devices play in seat: e2e" s midi seat
s --seat bob bind keys --notes C1..B2 --to bass >/dev/null
check "reveal needs a saved project" "save it first" s project reveal --no-open
check "save" "e2e.4s" s project save e2e
check "reveal prints location" "$FOURS_DATA_DIR/projects/e2e.4s" s project reveal --no-open
check "new clears" "kick        ---- ---- ---- ----" bash -c "$BIN/4s project new >/dev/null && $BIN/4s pattern show kick"
check "new is the default graph" "ch 1  Drums        vol 100%  pan C          <- drums" s mixer
check "load restores" "kick        X--- x--- X--- x---" bash -c "$BIN/4s project load e2e >/dev/null && $BIN/4s pattern show kick"
check "load restores the 303 and its channel" "ch 2  Bass         vol 100%  pan C          <- bass" s mixer
check "load restores notes" "bass: C2 - G1! C3~" s notes bass
check "load restores instrument params" "bass.cutoff = 0.25" s get bass.cutoff
check "load restores seats and their bindings" "bind 1: keys any ch C1..B2 -> bass" bash -c "$BIN/4s seat | grep -A3 '^bob'"
check "and focus" "focus: bass" bash -c "$BIN/4s seat | grep -A1 '^e2e'"
check "json output" '"value": 0.35' s --json get drums.snare.level
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
# A session the new build cannot load: without a terminal to ask on, nothing
# is discarded and the command explains how to start fresh.
s project save stale-old >/dev/null
echo '{"format_version": 1}' > "$FOURS_DATA_DIR/projects/stale-old.4s/project.json"
sleep 1; touch "$BIN/4sd"
check "stale: unloadable session is not discarded without asking" "dev.sh --fresh" s daemon start --restart-if-stale --no-audio --no-midi --listen 127.0.0.1:0 </dev/null
check "start fresh after that" "4sd started" s daemon start --no-audio --no-midi --listen 127.0.0.1:0
check "final stop" "4sd stopped" s daemon stop

echo "all $pass checks passed"
