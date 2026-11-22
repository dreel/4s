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
check "device input is journaled as midi.input" 'midi:'"$VDEV"'  midi.input {"data":[144,16,127],"device":"pad","profile":"livid_block","seat":"e2e"} -> event:drums.1.48.36' s journal
check "undo takes back a device pad press (the host user's)" "kick        ---- ---- ---- ----" bash -c "$BIN/4s undo >/dev/null && $BIN/4s pattern show kick"
check "redo" "kick        --x- ---- ---- ----" bash -c "$BIN/4s redo >/dev/null && $BIN/4s pattern show kick"
# Input that can do nothing is not journaled, from a device or over RPC:
# clock, active sensing, pitch bend, a pad release, an unmapped CC.
last_seq() { echo "seq=$("$BIN/4s" --json journal --limit 1 | python3 -c "import json,sys;print(json.load(sys.stdin)['entries'][-1]['seq'])")"; }
BEFORE=$(last_seq)
for m in "F8" "FE" "E0 00 40" "80 10 00"; do echo "raw $m" >&7; done
s midi send knobs F8 >/dev/null; s midi send knobs E0 00 40 >/dev/null; s midi send knobs B0 63 40 >/dev/null
sleep 0.5
check "input that can do nothing is not journaled" "$BEFORE" last_seq
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
# A new track follows its (empty) arrangement; launching its clip loops it
# (RFC 0009), as the default project's drums do.
check "launch the bass's clip" "bass: clips *1; plays clip 1 (launched)" s clip launch bass
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
BEFORE=$(s --json journal --limit 10000 | python3 -c "import json,sys;print(len(json.load(sys.stdin)['entries']))")
for _ in 1 2 3 4 5; do echo "raw F8" >&8; done; echo "raw FE" >&8; echo "raw E0 00 40" >&8; sleep 0.5
check "clock, active sensing, and pitch bend are not journaled" "0 new entries" bash -c "echo \$(( \$($BIN/4s --json journal --limit 10000 | python3 -c \"import json,sys;print(len(json.load(sys.stdin)['entries']))\") - $BEFORE )) new entries"
"$BIN/4s" watch --type trigger --count 1 --json > "$TMP/key.json" &
WATCH=$!; sleep 0.5
echo "raw 90 24 64" >&8   # note on, C2 (36)
wait $WATCH
check "key down plays the note" '"instrument":"bass","voice":null,"note":36' cat "$TMP/key.json"
s cc map keys 21 bass.cutoff >/dev/null
"$BIN/4s" watch --type trigger --json > "$TMP/key-cc.json" &
WATCH=$!; sleep 0.5
echo "raw 90 26 64" >&8; echo "raw 80 26 00" >&8   # D2 (38)
sleep 0.5; kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
check "a keyboard with only a CC map still plays the focus" '"instrument":"bass","voice":null,"note":38' cat "$TMP/key-cc.json"
s cc unmap keys 21 >/dev/null
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
# Bindings (RFC 0007): channel 10 plays the drums' GM voices, and the upper
# keys play the bass an octave down. Once a device has bindings, notes no
# binding matches do nothing.
check "reconnecting keeps the saved name" "$KDEV as keys (Generic)" s midi connect "$KDEV"
# Last-note priority on the 303: releasing the sounding key falls back to
# the key still held (the note keeps sounding); releasing that one ends it.
"$BIN/4s" watch --type meters --json > "$TMP/legato1.json" &
WATCH=$!; sleep 0.2
echo "raw 90 24 64" >&8; sleep 0.2; echo "raw 90 2B 64" >&8; sleep 0.2
echo "raw 80 2B 00" >&8; sleep 0.6
kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
"$BIN/4s" watch --type meters --json > "$TMP/legato2.json" &
WATCH=$!; sleep 0.2
echo "raw 80 24 00" >&8; sleep 0.8
kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
check "releasing the top key keeps the lower one sounding" "still sounding" python3 -c "
import json
levels = [c['left'] for l in open('$TMP/legato1.json') if l.strip()
          for c in json.loads(l)['event']['channels'] if c['channel'] == 2]
print('still sounding' if levels and levels[-1] > 0.01 else f'levels {levels}')"
check "releasing the last key ends the note" "released" python3 -c "
import json
levels = [c['left'] for l in open('$TMP/legato2.json') if l.strip()
          for c in json.loads(l)['event']['channels'] if c['channel'] == 2]
print('released' if levels and levels[-1] < 1e-4 else f'levels {levels}')"
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
# Bindings name the logical device: renamed, the port has no bindings in
# this seat, so it plays the focus (the bass).
check "rename a device" "$KDEV as keyz (Generic)" s midi rename keys keyz
"$BIN/4s" watch --type trigger --count 1 --json > "$TMP/renamed.json" &
WATCH=$!; sleep 0.5
echo "raw 99 26 64" >&8; echo "raw 89 26 00" >&8
wait $WATCH
check "a renamed device no longer uses the old name's bindings" '"instrument":"bass","voice":null,"note":38' cat "$TMP/renamed.json"
s midi rename keyz keys >/dev/null
check "unbind removes one binding" "bind 1: keys ch 10 all notes -> drums" s unbind 2
s unbind 1 >/dev/null
s midi disconnect keys >/dev/null
exec 8>&-
# Device models (RFC 0007): an Akai MPK mini IV, as two virtual ports named
# like the real one's. Its keys/pads come on the MIDI Port, its endless
# knobs on the DAW Port (relative CCs 24-31).
MPK="MPK mini IV MIDI Port e2e$$"; MPKD="MPK mini IV DAW Port e2e$$"
mkfifo "$TMP/mpk.in" "$TMP/mpkd.in"
target/debug/examples/virtual_block "$MPK" < "$TMP/mpk.in" > "$TMP/mpk.out" 2>&1 &
exec 5> "$TMP/mpk.in"
target/debug/examples/virtual_block "$MPKD" < "$TMP/mpkd.in" > "$TMP/mpkd.out" 2>&1 &
exec 6> "$TMP/mpkd.in"
for _ in $(seq 50); do grep -q ready "$TMP/mpk.out" && grep -q ready "$TMP/mpkd.out" && break; sleep 0.1; done
for _ in $(seq 30); do s midi ports | grep -q "$MPKD" && break; sleep 0.1; done
check "known models" "akai_mpk_mini_iv   Akai MPK mini IV" s midi models
check "a model's port gets its name" "$MPK as mpk (Generic) model akai_mpk_mini_iv" s midi connect "$MPK"
check "and its other port its own" "$MPKD as mpk_daw (Generic) model akai_mpk_mini_iv" s midi connect "$MPKD"
check "with no bindings, the seat uses the model's default layout" "default layout: mpk (Akai MPK mini IV)" s seat
s focus drums >/dev/null   # the keys play the bass anyway (@tb303|focus)
"$BIN/4s" watch --type trigger --count 2 --json > "$TMP/mpk.json" &
WATCH=$!; sleep 0.5
echo "raw 90 30 64" >&5; echo "raw 80 30 00" >&5   # a key, channel 1
echo "raw 99 25 64" >&5; echo "raw 89 25 00" >&5   # pad 2, channel 10
echo "raw 99 25 64" >&6; echo "raw 89 25 00" >&6   # the DAW Port's copy of it
wait $WATCH; sleep 0.3
check "keys play the first 303 whatever the focus" '"instrument":"bass","voice":null,"note":48' cat "$TMP/mpk.json"
check "pads play the 808's voices in order" '"instrument":"drums","voice":"snare"' cat "$TMP/mpk.json"
s focus bass >/dev/null
"$BIN/4s" watch --type trigger --json > "$TMP/mpk-daw.json" &
WATCH=$!; sleep 0.5
echo "raw 99 25 64" >&6; echo "raw 89 25 00" >&6   # a pad's copy on the DAW Port only
sleep 0.5; kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
check "the DAW Port's pad copies play nothing, whatever the focus" "0 triggers" bash -c "echo \$(grep -c trigger '$TMP/mpk-daw.json') triggers"
# A held key (channel 1, note 48) survives a pad with the same note number
# on channel 10 (bank B pad 5) being tapped.
"$BIN/4s" watch --type meters --json > "$TMP/mpk-hold.json" &
WATCH=$!; sleep 0.2
echo "raw 90 30 64" >&5; sleep 0.2
echo "raw 99 30 64" >&5; echo "raw 89 30 00" >&5; sleep 0.6
kill $WATCH 2>/dev/null; wait $WATCH 2>/dev/null || true
echo "raw 80 30 00" >&5
check "a pad's note-off on channel 10 does not end a held key on channel 1" "still sounding" python3 -c "
import json
levels = [c['left'] for l in open('$TMP/mpk-hold.json') if l.strip()
          for c in json.loads(l)['event']['channels'] if c['channel'] == 2]
print('still sounding' if levels and levels[-1] > 0.01 else f'levels {levels}')"
s set bass.cutoff 0.5 >/dev/null
echo "raw B0 18 0A" >&6; sleep 0.3   # knob 1 turned up 10 steps
check "an endless knob moves the focus's page parameter by steps" "bass.cutoff = 0.55" s get bass.cutoff
echo "raw B0 18 7B" >&6; sleep 0.3   # 5 steps down
check "and back down" "bass.cutoff = 0.525" s get bass.cutoff
echo "raw B0 01 00" >&5; echo "raw B0 01 7F" >&5; sleep 0.3
check "the mod control moves focus.cutoff" "bass.cutoff = 1" s get bass.cutoff
s focus drums >/dev/null
echo "raw B0 01 00" >&5; sleep 0.3
check "focus.cutoff does nothing when the focus has none" "bass.cutoff = 1" s get bass.cutoff
s focus bass >/dev/null
BEFORE=$(s --json journal --limit 10000 | python3 -c "import json,sys;print(len(json.load(sys.stdin)['entries']))")
for v in 50 60 70 40; do echo "raw E0 00 $v" >&5; done; sleep 0.3
check "pitch bend plays without being journaled" "0 new entries" bash -c "echo \$(( \$($BIN/4s --json journal --limit 10000 | python3 -c \"import json,sys;print(len(json.load(sys.stdin)['entries']))\") - $BEFORE )) new entries"
check "apply the layout to edit it" "bind 2: mpk ch 10 C2..G2 as C2,D2,D#2,F#2,A#2,A2,D3,G#3 -> @tr808" s midi layout mpk --apply
check "the seat now has its own bindings" "knobs: mpk_daw cc 24 25 26 27 28 29 30 31 follow focus (relative)" s seat
check "undo takes the applied layout back" "default layout: mpk_daw" bash -c "$BIN/4s undo >/dev/null && $BIN/4s seat"
# Remap and @type work for any device.
s bind pads --notes 60..61 --remap 36,38 --to @tr808 >/dev/null
"$BIN/4s" watch --type trigger --count 1 --json > "$TMP/remap.json" &
WATCH=$!; sleep 0.5
s midi send pads 90 3D 64 >/dev/null
wait $WATCH
check "remap: the second note plays the second voice of the first tr808" '"instrument":"drums","voice":"snare"' cat "$TMP/remap.json"
s unbind 1 >/dev/null
s midi disconnect mpk >/dev/null; s midi disconnect mpk_daw >/dev/null
exec 5>&- 6>&-
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
s channel add --name Spare >/dev/null
check "a new instrument takes the first empty channel" "ch 3  Spare        vol 100%  pan C          <- fill" bash -c "$BIN/4s instrument add tb303 --id fill >/dev/null && $BIN/4s mixer"
check "route --swap trades channels" "ch 2  Bass         <- fill
ch 3  Spare        <- bass" s route fill 2 --swap
s route bass 2 --swap >/dev/null
s instrument add tb303 --id loose --no-channel >/dev/null
check "swapping in an unrouted source keeps the channel's input" "ch 2  Bass         <- bass, loose" s route loose 2 --swap
s instrument rm loose >/dev/null
check "channel move reorders the mixer" "ch 3  Spare        vol 100%  pan C          <- fill
ch 1  Drums" bash -c "$BIN/4s channel move 3 1 >/dev/null && $BIN/4s mixer"
check "move position is checked" "position must be 1..=3" s channel move 3 4
s channel move 3 3 >/dev/null
s instrument rm fill >/dev/null
check "unknown source" "no output 'nope'" s route nope 1
check "instrument list" "bass       tb303  Bass" s instrument list
check "add onto an existing channel" "ch 2  Bass         vol 100%  pan C          <- bass, bass2" bash -c "$BIN/4s instrument add tb303 --channel 2 >/dev/null && $BIN/4s mixer"
check "removing it keeps the shared channel" "ch 2  Bass         vol 100%  pan C          <- bass" bash -c "$BIN/4s instrument rm bass2 >/dev/null && $BIN/4s mixer"
RMS_BEFORE=$(s --json render --bars 1 --out renders/nc1.wav | python3 -c "import json,sys;print(json.load(sys.stdin)['rms'])")
s instrument add tb303 --id lead --no-channel >/dev/null; s clip launch lead >/dev/null
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

# --- Undo/redo and the journal ---
s set mixer.1.volume 0.9 >/dev/null; s set mixer.1.volume 0.8 >/dev/null; s set mixer.1.volume 0.7 >/dev/null
check "a run of sets to one param is one undo step" "mixer.1.volume = 1" bash -c "$BIN/4s undo >/dev/null && $BIN/4s get mixer.1.volume"
check "redo it" "redid: param.set mixer.1.volume" s redo
s undo >/dev/null
NOTES_BEFORE=$(s notes bass)
s set bass.cutoff 0.11 >/dev/null
check "undo an instrument removal" "undid: instrument.remove bass" bash -c "$BIN/4s instrument rm bass >/dev/null && $BIN/4s undo"
check "its channel and route are back" "ch 2  Bass         vol 100%  pan C          <- bass" s mixer
check "its notes are back" "$NOTES_BEFORE" s notes bass
check "its params are back" "bass.cutoff = 0.11" s get bass.cutoff
check "it plays again" "(bass 9, drums 9)" s render --bars 1 --out renders/undo.wav
s undo >/dev/null   # the cutoff
check "nothing left to redo after a new edit" "nothing to redo" bash -c "$BIN/4s set bass.cutoff 0.3 >/dev/null; $BIN/4s undo >/dev/null; $BIN/4s set mixer.1.pan 0.1 >/dev/null; $BIN/4s redo"
s undo >/dev/null   # the pan
FOURS_USER=alice s set mixer.1.pan 0.5 >/dev/null
FOURS_USER=bob s set mixer.1.pan -0.5 >/dev/null
check "undo leaves what another user changed since" "could not undo: param.set mixer.1.pan
skipped: param:mixer.1.pan (changed by someone else)" env FOURS_USER=alice "$BIN/4s" undo
check "...so their value stays" "mixer.1.pan = -0.5" s get mixer.1.pan
check "each user has their own history" "mixer.1.pan = 0.5" bash -c "FOURS_USER=bob $BIN/4s undo >/dev/null && $BIN/4s get mixer.1.pan"
s set mixer.1.pan 0 >/dev/null; s undo >/dev/null; s set mixer.1.pan 0 >/dev/null
check "the journal says who did what" 'alice  cli  param.set {"path":"mixer.1.pan","value":0.5} -> param:mixer.1.pan' s journal --for alice
check "undo is journaled with what it reverts" "history.undo -> param:mixer.1.pan  (reverts #" s journal --for bob
check "the journal is written on the engine host" "jsonl" ls "$FOURS_DATA_DIR/journal"
LAST=$(s --json journal --limit 1 | python3 -c "import json,sys;print(json.load(sys.stdin)['entries'][-1]['seq'])")
s tempo 121 >/dev/null; s tempo 120 >/dev/null
check "journal --since shows only newer entries" "2 entries, first param.set" bash -c "$BIN/4s --json journal --since $LAST | python3 -c \"import json,sys;e=json.load(sys.stdin)['entries'];print(len(e),'entries, first',e[0]['method'])\""
check "journal --limit keeps the newest" '"value":120' s journal --limit 1
check "journal --follow streams entries" 'param.set {"path":"transport.tempo","value":122.0}' bash -c "$BIN/4s journal --follow > $TMP/follow.txt & P=\$!; sleep 0.5; $BIN/4s tempo 122 >/dev/null; sleep 0.5; kill \$P; cat $TMP/follow.txt"
s tempo 120 >/dev/null
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
s clip launch drums >/dev/null
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
s clip launch bass >/dev/null
s notes bass "C2 - G1! C3~" >/dev/null
s set bass.cutoff 0.25 >/dev/null
# Seats (RFC 0007): each performer has a focus and bindings; the CLI joins
# the seat matching its user, and the host's devices play in the host seat.
check "the CLI joins the seat matching its user" "you: e2e" s seat
check "it is this engine's devices' seat" "e2e: cli#" s seat
PROTO=$(s call daemon.info | python3 -c "import json,sys;print(json.load(sys.stdin)['protocol_version'])")
check "a user with no matching seat is asked to choose" '"choose_seat": true' s call session.hello '{"client_name": "x", "protocol_version": '"$PROTO"', "user": "bob"}'
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
check "midi.input rejects data bytes of 0x80 and up" "data bytes (0..0x7F)" s midi send knobs B0 C8 40
# CC maps: a device's CC sets a parameter over its range, picking up.
check "map a CC" "cc: knobs any ch cc 21 -> bass.cutoff" s cc map knobs 21 bass.cutoff
s midi send knobs B0 15 7F >/dev/null
check "a CC far from the value does not jump it" "bass.cutoff = 0.25" s get bass.cutoff
s midi send knobs B0 15 00 >/dev/null; s midi send knobs B0 15 7F >/dev/null
check "passing the value takes over" "bass.cutoff = 1" s get bass.cutoff
s cc unmap knobs 21 >/dev/null
s midi send knobs B0 15 00 >/dev/null
check "an unmapped CC does nothing" "bass.cutoff = 1" s get bass.cutoff
check "learn a CC" "learning a CC for bass.resonance" s cc learn bass.resonance
s midi send knobs B2 16 40 >/dev/null   # CC 22 on channel 3
check "the next CC moved is mapped, on its channel" "cc: knobs ch 3 cc 22 -> bass.resonance" s seat
s midi send knobs B0 16 7F >/dev/null
check "the same CC on another channel is not mapped" "bass.resonance = 0.5" s get bass.resonance
s cc unmap knobs 22 >/dev/null
check "a CC map can follow the focus" "cc: knobs any ch cc 23 -> focus.cutoff" s cc map knobs 23 focus.cutoff --no-pickup
s midi send knobs B0 17 40 >/dev/null
check "and moves the focused 303's cutoff" "bass.cutoff = 0.5039" s get bass.cutoff
check "focus.<param> must exist on some instrument type" "no instrument type has a parameter 'nope'" s cc map knobs 23 focus.nope
s cc unmap knobs 23 >/dev/null
# Following knobs control the focused instrument's knob page.
check "knobs follow the focus" "knobs: knobs cc 1 2 follow focus" s knobs follow knobs 1 2
s midi send knobs B0 02 00 >/dev/null; s midi send knobs B0 02 7F >/dev/null
check "knob 2 on the bass's main page is resonance" "bass.resonance = 1" s get bass.resonance
s focus drums >/dev/null; s knobs page decay >/dev/null
s midi send knobs B0 02 7F >/dev/null; s midi send knobs B0 02 00 >/dev/null
check "after a focus change the same knob drives the new focus" "drums.snare.decay = 0" s get drums.snare.decay
s set drums.snare.decay 0.4 >/dev/null; s focus bass >/dev/null
check "stop following" "focus: bass" s knobs follow knobs
s set bass.cutoff 0.25 >/dev/null; s set bass.resonance 0.5 >/dev/null
s bind pads --to drums >/dev/null
check "undo takes back a seat edit" "no pads binding" bash -c "$BIN/4s undo >/dev/null; $BIN/4s seat | grep -q 'bind 1: pads' && echo still bound || echo no pads binding"
check "and redo brings it back" "bind 1: pads any ch all notes -> drums" bash -c "$BIN/4s redo >/dev/null; $BIN/4s seat"
s unbind 1 >/dev/null
s --seat bob bind keys --notes C1..B2 --to bass >/dev/null
check "reveal needs a saved project" "save it first" s project reveal --no-open
check "save" "e2e.4s" s project save e2e
check "reveal prints location" "$FOURS_DATA_DIR/projects/e2e.4s" s project reveal --no-open
# --- Recordings: this whole session, replayed elsewhere (docs/journal.md) ---
check "export the session as a recording" "entries, digest" s journal export -o "$TMP/session.json"
B_DIR=$(mktemp -d)
FOURS_DATA_DIR=$B_DIR s daemon start --no-audio --no-midi --listen 127.0.0.1:0 >/dev/null
check "replay it into a fresh daemon: same changes, same final state" "replay matched" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay "$TMP/session.json"
check "a daemon with unsaved changes is not replaced without --force" "has unsaved changes" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay "$TMP/session.json"
check "replay the raw journal file from the engine host" "replay matched" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay --force "$(ls -t "$FOURS_DATA_DIR"/journal/*.jsonl | head -1)"
python3 -c "
import json; r = json.load(open('$TMP/session.json'))
e = [x for x in r['entries'] if x['changes']][3]; e['changes'][0]['after'] = 'tampered'
json.dump(r, open('$TMP/tampered.json', 'w'))"
check "a divergence is caught and the entry named" "replay diverged at entry" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay --force "$TMP/tampered.json"
check "--accept rewrites a diverging recording with what the replay did" "accepted: rewrote" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay --force --accept "$TMP/tampered.json"
check "...so it replays cleanly after" "replay matched" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay --force "$TMP/tampered.json"
for f in tests/journals/*.json; do
  check "fixture $(basename "$f") replays" "replay matched" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay --force "$f"
done
FOURS_DATA_DIR=$B_DIR s project new >/dev/null
FOURS_DATA_DIR=$B_DIR s pattern set kick "x-x-x-x-" >/dev/null
FOURS_DATA_DIR=$B_DIR s tempo 99 >/dev/null
check "a journal file's last segment (after project new) replays without its starting request" "replay matched: 2 entries" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay --force "$(ls -t "$B_DIR"/journal/*.jsonl | head -1)"
check "--accept refuses a journal file before touching the daemon" "rewrites a recording" env FOURS_DATA_DIR="$B_DIR" "$BIN/4s" journal replay --force --accept "$(ls -t "$B_DIR"/journal/*.jsonl | head -1)"
check "project import sends a local project inline" "ch 2  Bass" bash -c "FOURS_DATA_DIR='$B_DIR' $BIN/4s project import '$FOURS_DATA_DIR/projects/e2e.4s' >/dev/null && FOURS_DATA_DIR='$B_DIR' $BIN/4s mixer"
check "export as a script of 4s calls" "4s call project.import" s journal export --format sh
FOURS_DATA_DIR=$B_DIR s daemon stop >/dev/null

check "new clears" "kick        ---- ---- ---- ----" bash -c "$BIN/4s project new >/dev/null && $BIN/4s pattern show kick"
check "new is the default graph" "ch 1  Drums        vol 100%  pan C          <- drums" s mixer
check "a new project starts a fresh history" "undo: (empty)" s history
check "a one-off command as another user on the host makes no seat" "no dave" bash -c "$BIN/4s --user dave status >/dev/null; $BIN/4s seat | grep -q '^dave' && echo dave || echo no dave"
check "undoing the only 808's removal makes it the focus again" "focus: drums" bash -c "$BIN/4s instrument rm drums >/dev/null && $BIN/4s undo >/dev/null && $BIN/4s controller"
check "load restores" "kick        X--- x--- X--- x---" bash -c "$BIN/4s project load e2e >/dev/null && $BIN/4s pattern show kick"
check "load restores the 303 and its channel" "ch 2  Bass         vol 100%  pan C          <- bass" s mixer
check "load restores notes" "bass: C2 - G1! C3~" s notes bass
check "load restores instrument params" "bass.cutoff = 0.25" s get bass.cutoff
check "load restores seats and their bindings" "bind 1: keys any ch C1..B2 -> bass" bash -c "$BIN/4s seat | grep -A3 '^bob'"
check "and focus" "focus: bass" bash -c "$BIN/4s seat | grep -A1 '^e2e'"
# Clips (RFC 0007 phase 2): sequences are timed note events; step patterns
# are views over them.
s instrument add tb303 --id seq --no-channel >/dev/null
s clip launch seq >/dev/null
s notes seq "C2 - D#2~ G1" >/dev/null
check "a note pattern is a clip: half-step notes, a slide overlaps the next" "0:C2:12:89 48:D#2:25:89 72:G1:12:89" s clip show seq
check "add a note between steps" "36:C3:6:100" s clip add seq 36 C3 --len 6 --vel 100
check "the step view leaves it out" "seq: C2 - D#2~ G1" s notes seq
check "it plays at its tick (step 2 + half a step at 120 bpm)" "C3 at 0.1875" python3 -c "
import json, subprocess
r = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render', '--bars', '1', '--out', 'renders/clip.wav']))
t = [x['time'] for x in r['triggers'] if x['instrument'] == 'seq' and x['note'] == 48]
print(f'C3 at {t[0]:.4f}' if len(t) == 1 else t)"
check "a clip loops at its own length: 3 steps against 16 (3 notes x 6 loops, cut by the bar)" "16 notes" python3 -c "
import json, subprocess
subprocess.check_output(['$BIN/4s', 'clip', 'length', 'seq', '3'])
r = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render', '--bars', '1', '--out', 'renders/poly.wav']))
print(len([x for x in r['triggers'] if x['instrument'] == 'seq']), 'notes')"
check "an off-grid clip saves as events and loads back" "36:C3:6:100" bash -c "$BIN/4s project save clips >/dev/null && grep -q '\"clips\"' '$FOURS_DATA_DIR/projects/clips.4s/project.json' && $BIN/4s project new >/dev/null && $BIN/4s project load clips >/dev/null && $BIN/4s clip show seq"
check "its own length too" "length 3 steps" s clip show seq
check "and it is still launched" "seq: clips *1; plays clip 1 (launched)" s clip list seq
s clip length seq 16 >/dev/null
check "quantize moves it to the nearest 16th" "48:C3:6:100" s clip quantize seq 1/16
check "undo takes back the quantize" "36:C3:6:100" bash -c "$BIN/4s undo >/dev/null && $BIN/4s clip show seq"
check "clip edits are journaled per event" "event:seq.1.36.48" s journal --limit 3
check "quantize wraps a note at the loop's end to its start" "0:D2:6:89" bash -c "$BIN/4s clip set seq 70:D2:6 >/dev/null && $BIN/4s clip length seq 3 >/dev/null && $BIN/4s clip quantize seq 1/16 | tail -1"
s clip length seq 16 >/dev/null; s clip set seq "0:C2:12:89 36:C3:6:100 48:D#2:25:89 72:G1:12:89" >/dev/null
check "remove a note" "0:C2:12:89 48:D#2:25:89 72:G1:12:89" bash -c "$BIN/4s clip rm seq 36 C3 >/dev/null && $BIN/4s clip show seq | tail -1"
s clip set seq "0:C2:12:100 0:G2:12:89 48:D#2:6:89" >/dev/null
check "a step edit leaves chords, velocities, and lengths on other steps alone" "0:C2:12:100 0:G2:12:89 48:D#2:6:89 96:C3:12:89" bash -c "$BIN/4s note seq 5 C3 >/dev/null && $BIN/4s clip show seq | tail -1"
s instrument add tr808 --id seqd --no-channel >/dev/null
s clip set seqd "0:36:24:100 12:42:6:89" >/dev/null
check "so does a drum step edit" "0:C2:24:100 12:F#2:6:89 96:D2:24:89" bash -c "$BIN/4s pattern --instrument seqd step snare 5 on >/dev/null && $BIN/4s clip show seqd | tail -1"
s instrument rm seqd >/dev/null
check "clip set replaces the events" "seq clip 1: 1 notes" s clip set seq "0:60:96:127"
check "clear" "seq clip 1: 0 notes" s clip clear seq
check "quantize keeps a note past the longest clip on the grid" "1512:C2:6:89" bash -c "$BIN/4s clip set seq 1535:C2:6 >/dev/null && $BIN/4s clip quantize seq 1/16 | tail -1"
s clip clear seq >/dev/null
check "clip update removes and adds in one edit" "0:C2:24:89 30:G2:24:89 48:E2:6:100" bash -c "$BIN/4s clip set seq '0:C2 24:D2' >/dev/null && $BIN/4s clip update seq --rm 24:D2 --add '48:E2:6:100 30:G2' | tail -1"
check "...as one undo step" "0:C2:24:89 24:D2:24:89" bash -c "$BIN/4s undo >/dev/null && $BIN/4s clip show seq | tail -1"
check "quantize --notes moves only those notes" "0:C2:24:89 24:G2:24:89 30:A2:24:89" bash -c "$BIN/4s clip set seq '0:C2 30:G2 30:A2' >/dev/null && $BIN/4s clip quantize seq 1/16 --notes 30:G2 | tail -1"
check "quantize at half strength moves notes halfway" "0:C2:24:89 27:G2:24:89" bash -c "$BIN/4s clip set seq '0:C2 30:G2' >/dev/null && $BIN/4s clip quantize seq 1/16 --strength 50% | tail -1"
s clip clear seq >/dev/null
# Recording (RFC 0008). Offline first: the same take code over a render's
# feedback, sample-accurate. At 120 bpm a second is 192 ticks: 0.51 s is
# tick 97.9, 1.13 s is 217.
check "render records played notes at their ticks" "98:C2:19:100 217:D#2:38:127" bash -c "$BIN/4s render --to seq --input '0.51:C2:0.1:100 1.13:D#2:0.2:127' --quantize off | tail -1"
check "record quantize moves them onto the 16ths" "96:C2:19:100 216:D#2:38:127" bash -c "$BIN/4s render --to seq --input '0.51:C2:0.1:100 1.13:D#2:0.2:127' --quantize 1/16 | tail -1"
check "half strength moves them halfway" "97:C2:19:100" bash -c "$BIN/4s render --to seq --input '0.51:C2:0.1:100' --quantize 1/16 --strength 50% | tail -1"
check "a render's take leaves the project alone" "seq clip 1: 0 notes" s clip show seq
s clip set seq "0:G1" >/dev/null
check "overdub keeps the clip; a note just before the loop's end wraps to its start" "0:G1:24:89 0:E2:24:100 48:C2:24:100 96:D2:24:100" bash -c "$BIN/4s render --bars 2 --to seq --input '0.25:C2 1.99:E2 2.5:D2' --quantize 1/16 | tail -1"
check "replace: each pass clears the loop it covers" "0:E2:24:100 96:D2:24:100" bash -c "$BIN/4s render --bars 2 --to seq --replace --input '0.25:C2 1.99:E2 2.5:D2' --quantize 1/16 | tail -1"
# The click is after the master fader: with the mix silenced, only it sounds.
s set mixer.master.volume 0 >/dev/null
check "the metronome clicks on the beats (only when asked)" "onsets (s): 0.000 0.499 1.000 1.499" bash -c "$BIN/4s render --metronome | tail -1"
check "and not otherwise" "detected onsets: 0" bash -c "$BIN/4s metronome on >/dev/null && $BIN/4s render | grep onsets: ; $BIN/4s metronome off >/dev/null"
s set mixer.master.volume 0.8 >/dev/null
s clip clear seq >/dev/null
# Live: a take on the real-time engine, written at the loop's end.
check "record starts the transport" "recording into seq" s record --to seq --count-in 0 --quantize 1/16
sleep 0.2
check "notes not written yet are sent as take_notes" "seq: 1 notes recorded, not written yet" bash -c "(perl -e 'alarm 5; exec @ARGV' $BIN/4s watch --type take_notes --count 1 &) ; sleep 0.2; $BIN/4s key C2 --instrument seq --for 0.2 >/dev/null; sleep 0.1"
# Written one step before the loop's end (1.875 s in); poll rather than
# guess how long a loaded machine takes.
check "the pass is written into the clip" "seq clip 1: 1 notes" bash -c "for i in \$(seq 60); do $BIN/4s clip show seq | grep -q 'seq clip 1: 1 notes' && break; sleep 0.1; done; $BIN/4s clip show seq"
check "as one journal entry: the clip.update that writes it" '"recorded":true' s journal --limit 3
check "record --off ends the take, still playing" "not recording" s record --off
check "status shows the next take's settings" "record: not recording (next take: overdub, quantize 1/16 at 100%" s status
s stop >/dev/null
check "undo takes back the take" "seq clip 1: 0 notes" bash -c "$BIN/4s undo >/dev/null && $BIN/4s clip show seq"
check "play during a take writes it and keeps recording" "seq clip 1: 1 notes" bash -c "$BIN/4s record --to seq --count-in 0 >/dev/null && sleep 0.3 && $BIN/4s key C2 --instrument seq --for 0.1 >/dev/null && $BIN/4s play >/dev/null && $BIN/4s clip show seq"
check "still recording after it" "recording into seq" s record --show
s stop >/dev/null
s clip clear seq >/dev/null
s instrument add tb303 --id rec >/dev/null
s record --to rec --count-in 0 >/dev/null
check "undoing the instrument's add ends a take into it" "not recording" bash -c "$BIN/4s undo >/dev/null && $BIN/4s record --show"
s stop >/dev/null
check "recording into another instrument ends the take and starts one there" "recording into drums" bash -c "$BIN/4s record --to seq --count-in 0 >/dev/null && sleep 0.2 && $BIN/4s key C2 --instrument seq --for 0.1 >/dev/null && $BIN/4s record --to drums"
check "...writing the first one" "seq clip 1: 1 notes" s clip show seq
s stop >/dev/null
# A session with takes replays: the takes are entries of their own, and
# replay does not record the replayed notes again.
check "a session with recorded takes replays" "replay matched" bash -c "$BIN/4s journal export -o '$TMP/takes.json' >/dev/null && B=\$(mktemp -d) && FOURS_DATA_DIR=\$B $BIN/4s daemon start --no-audio --no-midi --listen 127.0.0.1:0 >/dev/null && FOURS_DATA_DIR=\$B $BIN/4s journal replay '$TMP/takes.json'; FOURS_DATA_DIR=\$B $BIN/4s daemon stop >/dev/null"
check "record settings are checked" "strength must be 0..1" s record --show --strength 2
s record --show --quantize off --count-in 1 >/dev/null
s instrument rm seq >/dev/null
# The Block grid's playhead follows the focus's selected clip where it
# plays: a 3-step clip, launched, at 30 bpm (a step is 0.5 s).
cat > "$TMP/grid.py" <<PY
import subprocess
def run(*a): return subprocess.check_output(['$BIN/4s', *a], text=True)
# One stream, so no step is missed between reads; the playhead event shows
# each track's clip step from 1.
watch = subprocess.Popen(['$BIN/4s', 'watch', '--type', 'playhead'], stdout=subprocess.PIPE, text=True)
def step():
    while 'gridd clip 1 step ' not in (line := watch.stdout.readline()): pass
    return int(line.split('gridd clip 1 step ')[1].split()[0].rstrip(',')) - 1
run('tempo', '30'); run('play')
try:
    while (s := step()) == 0: pass
    col = run('controller').splitlines()[8].split()[s]
    print('grid playhead at clip step' if col == '#' else run('controller'))
finally:
    watch.kill(); run('stop'); run('tempo', '120')
PY
s instrument add tr808 --id gridd --no-channel >/dev/null; s focus gridd >/dev/null; s clip length gridd 3 >/dev/null; s clip launch gridd >/dev/null
check "the Block grid's playhead follows the focus clip where it plays" "grid playhead at clip step" python3 "$TMP/grid.py"
s instrument rm gridd >/dev/null
# --- Step views and the Block on clips (RFC 0009 phase 2) ---
s instrument add tb303 --id sv --no-channel >/dev/null
s clip launch sv >/dev/null
s step sv 1 C2 --set accent >/dev/null
check "step set writes notes through the instrument's layout" "C2! - D#2~ -" bash -c "$BIN/4s step sv 3 D#2 --set slide | tail -1"
check "...the same events the note view reads" "sv: C2! - D#2~" s notes sv
check "a mono step takes a new pitch, keeping its expressions" "C2! - G2~ -" bash -c "$BIN/4s step sv 3 G2 | tail -1"
check "--off makes a mono step a rest" "C2! - - -" bash -c "$BIN/4s step sv 3 --off | tail -1"
check "clearing an expression" "C2 - - -" bash -c "$BIN/4s step sv 1 C2 --clear accent | tail -1"
check "steps at another resolution" "8 steps of 48 ticks" s step sv --res 1/8
check "drum rows, an accent as X" "SD   ---- X---" bash -c "$BIN/4s step drums 5 snare --set accent | grep SD; $BIN/4s step drums 5 snare --off >/dev/null"
check "expressions are the instrument's own" "no expression 'slide' (this instrument has: accent)" s step drums 1 kick --set slide
check "notes off the instrument's grid are refused" "not on this instrument's grid" s step drums 1 C5
s focus sv >/dev/null
check "the Block shows a pitched focus as pitch rows" ".  C2" bash -c "$BIN/4s controller mode --base C2 | tail -1"
check "...edits the clip the armed track plays" "grid: steps (clip 1)" s controller
check "a pad puts its row's pitch on its step" "C2 D2 - -" bash -c "$BIN/4s controller press 6 2 >/dev/null && $BIN/4s step sv | tail -1"
s clip new sv --keep >/dev/null
s clip launch sv 2 >/dev/null
check "launching another clip retargets the grid" "grid: steps (clip 2)" s controller
check "launch mode: a row per track, its launched clip and back pad lit" ". # . . . . . #  sv" bash -c "$BIN/4s controller mode --grid launch | grep ' sv'"
SV_ROW=$(s controller | awk '/ sv$/ {print NR - 1}')
check "...a pad launches the row's clip" "sv: clips *1, 2; plays clip 1 (launched)" bash -c "$BIN/4s controller press $SV_ROW 1 >/dev/null && $BIN/4s clip list sv"
check "...the last pad goes back to the arrangement" "sv: clips *1, 2; plays its arrangement" bash -c "$BIN/4s controller press $SV_ROW 8 >/dev/null && $BIN/4s clip list sv"
s controller mode --grid steps >/dev/null
# A take into a launched clip that is deleted mid-take writes nothing into
# the arrangement (the notes went with the clip). At 60 bpm no pass is
# written in the first second.
s clip new sv --keep >/dev/null
s clip launch sv 3 >/dev/null
s tempo 60 >/dev/null
check "a take into a launched clip deleted mid-take writes nothing elsewhere" "clean" bash -c "$BIN/4s record --to sv --count-in 0 >/dev/null; sleep 0.3; $BIN/4s key C2 --instrument sv --for 0.1 >/dev/null; $BIN/4s clip del sv --clip 3 >/dev/null; $BIN/4s record --off >/dev/null; $BIN/4s stop >/dev/null; L=\$($BIN/4s clip list sv); [[ \"\$L\" == 'sv: clips *1, 2; plays its arrangement' ]] && echo clean || echo \"\$L\""
s tempo 120 >/dev/null
check "step expressions only go with a note turned on" "not off" s step sv 1 C2 --off --set accent
check "steps past the clip's end are refused" "past the clip's end" s step sv 17 C2
check "a step of the focus" "C2 D2 G2 -" bash -c "$BIN/4s step 3 G2 | tail -1"
s step 3 --off >/dev/null
check "the Block comes back to its step page after launch mode" "page: 2" bash -c "$BIN/4s clip length sv 32 >/dev/null; $BIN/4s controller mode --page 2 >/dev/null; $BIN/4s controller mode --grid launch >/dev/null; $BIN/4s controller mode --grid steps | head -1"
s focus drums >/dev/null
s instrument rm sv >/dev/null
# --- The song (RFC 0008 phase B, RFC 0009): clip pools, the arrangement,
# launching ---
# Renders list what the `song` 808 fired as voice@seconds (120 bpm: a bar
# is 2 s).
fired() { "$BIN/4s" --json render "$@" | python3 -c "import json,sys;print(' '.join(f\"{t['voice'] or t['note']}@{t['time']:.2f}\" for t in json.load(sys.stdin)['triggers'] if t['instrument'] == 'song'))"; }
export -f fired
export BIN
s instrument add tr808 --id song --no-channel >/dev/null
s focus song >/dev/null
s pattern --instrument song set kick "x---x---x---x---" >/dev/null
check "a new clip is selected, and the step editors edit it" 'song: clips 1, *2 "B"' bash -c "$BIN/4s clip new song --name B >/dev/null && $BIN/4s pattern --instrument song set snare '----x-------x---' >/dev/null && $BIN/4s clip list song"
check "a new track plays its arrangement, which is empty" "song: clips 1, *2 \"B\"; plays its arrangement" s clip list song
check "launching a clip loops it" "snare@0.50 snare@1.50" bash -c "$BIN/4s clip launch song >/dev/null; fired --bars 1"
check "selecting another clip does not change what plays" "snare@0.50 snare@1.50" bash -c "$BIN/4s clip select song 1 >/dev/null; fired --bars 1"
check "launching it does" "kick@0.00 kick@0.50 kick@1.00 kick@1.50" bash -c "$BIN/4s clip launch song >/dev/null; fired --bars 1"
check "launches are journaled with the clip they resolved to" 'clip.launch {"clip":1,"instrument":"song"' s journal --limit 2
check "...but not undone" "nothing to undo" bash -c "$BIN/4s --user launcher clip launch song 2 >/dev/null; $BIN/4s --user launcher undo"
check "back to the arrangement" "song: clips *1, 2 \"B\"; plays its arrangement" bash -c "$BIN/4s song back song | grep '^song:'"
check "duplicate copies a clip into a new one" 'song clip 3 "1 copy": 4 notes' s clip dup song
s clip del song --clip 3 >/dev/null
s song place song --at 1 --bars 2 --clip 1 >/dev/null
check "placing clips on the song" "bar 3       clip 2 (B), 1 bar" s song place song --at 3 --clip 2
check "the transport plays the arrangement" "kick@3.50 snare@4.50 snare@5.50" bash -c "$BIN/4s song loop off >/dev/null && fired --bars 4"
check "looping bars 2 to 3 comes back to bar 2" "snare@5.50 kick@6.00 kick@6.50 kick@7.00 kick@7.50 snare@8.50" bash -c "$BIN/4s song loop 2 3 >/dev/null && fired --bars 5"
check "render --from a bar" "snare@0.50 snare@1.50" fired --bars 1 --from 3
check "locate sets where play starts from" "snare@0.50 snare@1.50" bash -c "$BIN/4s locate 3 >/dev/null && fired --bars 1"
s locate 1 >/dev/null
s song loop off >/dev/null
check "a launched clip plays instead of the track's arrangement" "kick@0.00 kick@0.50 kick@1.00 kick@1.50 kick@2.00" bash -c "$BIN/4s clip launch song 1 >/dev/null; fired --bars 2 --from 3 | cut -d' ' -f1-5"
check "and records into that clip, at its loop" "recorded song clip 1: 5 notes" bash -c "$BIN/4s render --bars 1 --to song --input '0.25:39' | grep 'recorded song'"
s song back song >/dev/null
check "placing over a placement cuts it" "bar 1       clip 1 (1), 1 bar" s song place song --at 2 --clip 2
check "...and undo puts it back" "bar 1       clip 1 (1), 2 bars" bash -c "$BIN/4s undo >/dev/null && $BIN/4s song"
check "moving a placement" "bar 5       clip 2 (B), 1 bar" s song mv song 3 5
s song mv song 5 3 >/dev/null
check "cutting a placement longer than its clip keeps the loop's place" "bar 8       clip 1 (1), 2 bars" bash -c "$BIN/4s song place song --at 5 --bars 5 --clip 1 >/dev/null && $BIN/4s song place song --at 7 --clip 2 | grep 'bar 8'"
s undo >/dev/null
s undo >/dev/null
check "a bar loop is one undo step" "undid: batch: param.set" bash -c "$BIN/4s song loop 2 5 >/dev/null && $BIN/4s undo"
check "deleting a clip removes its placements" "song: clips *1" bash -c "$BIN/4s clip del song --clip 2 | head -1"
check "...and undo brings both back" "bar 3       clip 2 (B), 1 bar" bash -c "$BIN/4s undo >/dev/null && $BIN/4s song"
check "pools and arrangements save with the project" '"arrangement": [{"clip": 1, "start": 0, "length": 768, "offset": 0}, {"clip": 2, "start": 768, "length": 384, "offset": 0}]' bash -c "$BIN/4s project save song >/dev/null && python3 -c \"import json;print(json.dumps(json.load(open('$FOURS_DATA_DIR/projects/song.4s/project.json'))['tracks']['song']))\""
check "...and load back" 'song: clips *1, 2 "B"' bash -c "$BIN/4s project new >/dev/null && $BIN/4s project load song >/dev/null && $BIN/4s clip list song"
check "a batch is one journal entry and one undo step" "undid: batch: param.set, clip.rename" bash -c "$BIN/4s batch '[{\"method\":\"param.set\",\"params\":{\"path\":\"transport.tempo\",\"value\":100}},{\"method\":\"clip.rename\",\"params\":{\"instrument\":\"song\",\"clip\":1,\"name\":\"Verse\"}}]' >/dev/null && $BIN/4s undo"
check "...undoing all of it" "transport.tempo = 120" s get transport.tempo
check "batches cannot nest" "cannot be in a batch" s batch '[{"method":"batch","params":{"requests":[]}}]'
# Recording into the arrangement: notes go into the clip under them; with nothing
# under them, into a new clip made there.
check "takes into the arrangement write into placements and make a clip past the end" "recorded song clip 3: 1 notes, length 16 steps" bash -c "$BIN/4s render --bars 4 --to song --input '0.25:39 4.25:39 6.5:39:0.1' | grep 'recorded song clip 3'"
check "...the bar-3 note in clip 2 at its own tick" "48:D#2:24:100 96:D2:24:89 288:D2:24:89" bash -c "$BIN/4s render --bars 4 --to song --input '0.25:39 4.25:39 6.5:39:0.1' | grep -A4 'clip 2' | tail -1"
check "notes past the end go in clips of whole bars, a new one past the longest" "recorded song clip 4: 1 notes" bash -c "$BIN/4s render --bars 12 --to song --input '8.5:39 14.5:39 20.5:39' | grep 'recorded song clip 4'"
check "...the clip before it grew to hold both of its notes" "96:D#2:24:100 1248:D#2:24:100" bash -c "$BIN/4s render --bars 12 --to song --input '8.5:39 14.5:39 20.5:39' | grep -A3 'recorded song clip 3' | tail -1"
# Live, at 240 bpm (a bar is 1 s): the song's end stops playback, unless
# recording, which goes on past it.
s tempo 240 >/dev/null
check "the drums still loop their launched clip" "drums: clips *1; plays clip 1 (launched)" bash -c "$BIN/4s clip list drums"
check "...so the song does not stop at its end" "transport: playing" bash -c "$BIN/4s play >/dev/null && sleep 3.6 && $BIN/4s status | grep transport"
check "a launch while playing waits for the bar" "plays clip 2 (launched) (waiting for the boundary)" bash -c "$BIN/4s clip launch song 2 | grep '^song:'"
check "...and lands on it (240 bpm: within a second)" "landed: song: clips *1, 2 \"B\"; plays clip 2 (launched)" bash -c "for i in \$(seq 40); do $BIN/4s clip list song | grep -q waiting || break; sleep 0.05; done; $BIN/4s clip list song | grep -q waiting || echo landed: \$($BIN/4s clip list song)"
check "relaunching the clip a track plays waits for the boundary too" "plays clip 2 (launched) (waiting for the boundary)" bash -c "$BIN/4s clip launch song 2 | grep '^song:'"
check "...and stops waiting when it restarts" "landed" bash -c "for i in \$(seq 40); do $BIN/4s clip list song | grep -q waiting || break; sleep 0.05; done; $BIN/4s clip list song | grep -q waiting && echo still waiting || echo landed"
check "back to the arrangement for every track" "plays its arrangement" bash -c "$BIN/4s song back --now >/dev/null && sleep 0.2 && $BIN/4s clip list drums"
check "with a one-bar loop and a two-bar quantum, a launch lands on the loop's wrap" "landed" bash -c "$BIN/4s stop >/dev/null; $BIN/4s set transport.launch_quantize 3 >/dev/null; $BIN/4s song loop 2 2 >/dev/null; $BIN/4s locate 2 >/dev/null; $BIN/4s play >/dev/null; $BIN/4s clip launch song 2 >/dev/null; sleep 1.5; $BIN/4s clip list song | grep -q waiting && echo still waiting || echo landed; $BIN/4s stop >/dev/null; $BIN/4s song back --now >/dev/null; $BIN/4s song loop off >/dev/null; $BIN/4s locate 1 >/dev/null; $BIN/4s set transport.launch_quantize 2 >/dev/null"
check "with nothing launched, the song stops at its end" "transport: stopped" bash -c "$BIN/4s stop >/dev/null && $BIN/4s play >/dev/null && sleep 3.6 && $BIN/4s status | grep transport"
s locate 3 >/dev/null
s record --to song --count-in 0 >/dev/null
sleep 1.2
s key D#2 --instrument song --for 0.1 >/dev/null
sleep 0.4
s record --off >/dev/null
s stop >/dev/null
check "recording past the end makes a clip there" "bar 4       clip 3 (3), 1 bar" s song
check "...journaled as one batch" "batch {\"requests\":[{\"method\":\"clip.new\"" s journal --limit 4
check "...that replays" "replay matched" bash -c "$BIN/4s journal export -o '$TMP/song.json' >/dev/null && B=\$(mktemp -d) && FOURS_DATA_DIR=\$B $BIN/4s daemon start --no-audio --no-midi --listen 127.0.0.1:0 >/dev/null && FOURS_DATA_DIR=\$B $BIN/4s journal replay '$TMP/song.json'; FOURS_DATA_DIR=\$B $BIN/4s daemon stop >/dev/null"
check "...and is one undo step" "undid: batch: clip.new, clip.update, song.place" s undo
s tempo 120 >/dev/null
# A v5 project saved in pattern mode launches each track's selected clip
# (RFC 0009), so it plays as before: the drums' clip, at sequencer.length.
mkdir -p "$FOURS_DATA_DIR/projects/v5.4s"
cat > "$FOURS_DATA_DIR/projects/v5.4s/project.json" <<'JSON'
{"format_version": 5, "instruments": [{"id": "drums", "type": "tr808", "name": "Drums"}],
 "channels": [{"n": 1, "name": "Drums"}], "routes": {"drums": 1}, "controller": {"follow": true},
 "params": {"song.mode": 0, "sequencer.length": 8},
 "tracks": {"drums": {"selected": 1, "clips": {"1": {"pattern": {"kick": "x---x---"}}}}}}
JSON
check "a v5 pattern-mode project launches its selected clips" "drums: clips *1; plays clip 1 (launched)" bash -c "$BIN/4s project load v5 >/dev/null && $BIN/4s clip list drums"
check "...which loop at its sequencer.length" "kick@0.00 kick@0.50 kick@1.00 kick@1.50" bash -c "$BIN/4s --json render --bars 1 | python3 -c \"import json,sys;print(' '.join(f\\\"{t['voice']}@{t['time']:.2f}\\\" for t in json.load(sys.stdin)['triggers']))\""
s project load song >/dev/null
s instrument rm song >/dev/null
# The clip table is full: undoing an instrument's removal (it needs a clip)
# fails cleanly instead of taking the daemon down.
s instrument add tb303 --id full --no-channel >/dev/null
s instrument rm full >/dev/null
FREE=$(s --json state | python3 -c "import json,sys;print(256 - len(json.load(sys.stdin)['clips']))")
python3 -c "import json;print(json.dumps([{'method':'clip.new','params':{'instrument':'drums','select':False}}]*$FREE))" > "$TMP/fill.json"
s --user other batch "$(cat "$TMP/fill.json")" >/dev/null
check "undo that needs a clip when the table is full says so" "at most 256 clips" s undo
check "...and the daemon is still up" "transport:" s status
s --user other undo >/dev/null
# --- Devices (RFC 0010): audio effects in insert chains ---
s project new >/dev/null
s pattern set kick "x---------------" >/dev/null
check "a lone kick is one onset" "onsets (s): 0.000" bash -c "$BIN/4s render | tail -1"
check "device types list instruments and effects" "delay   audio_effect" s device types
check "add a delay to channel 1" "delay      delay   Delay        on ch 1 at 1" s device add delay --on 1
check "its parameters are under its id" "delay.feedback" s params delay
check "a synced delay echoes every dotted eighth (0.375 s at 120 bpm)" "echoes on time" python3 -c "
import json, subprocess
o = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render']))['onsets']
ok = len(o) >= 4 and all(abs(t - 0.375 * k) < 0.01 for k, t in enumerate(o[:4]))
print('echoes on time' if ok else o)"
check "bypass passes the kick through alone" "onsets (s): 0.000" bash -c "$BIN/4s set delay.bypass on >/dev/null && $BIN/4s render | tail -1"
s set delay.bypass off >/dev/null
check "a filter goes first in the chain" "<- drums  [filter, delay]" bash -c "$BIN/4s device add filter --on 1 --at 1 >/dev/null && $BIN/4s mixer"
check "a reverb on the master" "master        vol  80%  [reverb]" bash -c "$BIN/4s device add reverb --on master >/dev/null && $BIN/4s mixer"
check "moving one is one undo step" "undid: device.move filter" bash -c "$BIN/4s device mv filter --on master >/dev/null && $BIN/4s undo"
check "moving within its own chain needs no --on" "<- drums  [delay, filter]" bash -c "$BIN/4s device mv filter --at 2 >/dev/null && $BIN/4s mixer"
s device mv filter --at 1 >/dev/null
check "ids are shared with instruments" "already an instrument or device id" s device add delay --on 1 --id drums
check "unknown types are refused" "unknown device type" s device add flanger --on 1
check "a chain holds at most 8" "at most 8 inserts" bash -c "for i in 1 2 3 4 5 6; do $BIN/4s device add filter --on 1 >/dev/null; done; $BIN/4s device add filter --on 1"
for i in 2 3 4 5 6 7; do s device rm "filter$i" >/dev/null; done
check "effects save with the project and load back" "delay      delay   Delay        on ch 1 at 2" bash -c "$BIN/4s set delay.feedback 0.6 >/dev/null && $BIN/4s project save fx >/dev/null && $BIN/4s project new >/dev/null && $BIN/4s project load fx >/dev/null && $BIN/4s device list"
check "...with their parameters" "delay.feedback = 0.6" s get delay.feedback
check "removing a channel removes its inserts" "reverb     reverb" bash -c "$BIN/4s channel rm 1 >/dev/null; $BIN/4s device list"
check "...and undo brings them back" "delay      delay   Delay        on ch 1 at 2" bash -c "$BIN/4s undo >/dev/null && $BIN/4s device list"
cat > "$TMP/levels.py" <<PY
import array, json, struct, subprocess, sys
def level(lo, hi):
    r = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render', '--out', 'renders/lv.wav']))
    b = open(r['path'], 'rb').read(); i = b.index(b'data'); n = struct.unpack('<I', b[i + 4:i + 8])[0]
    a = array.array('f'); a.frombytes(b[i + 8:i + 8 + n]); sr = r['sample_rate']
    return max(abs(x) for x in a[int(lo * sr) * 2:int(hi * sr) * 2])
print('peak %.3f tail %.4f' % (level(0, 0.05), level(0.5, 1.5)))
PY
s set delay.bypass on >/dev/null
check "a low-pass filter takes the top off the kick" "filtered quieter" bash -c "$BIN/4s set filter.cutoff 0.05 >/dev/null; A=\$(python3 '$TMP/levels.py' | cut -d' ' -f2); $BIN/4s set filter.bypass on >/dev/null; B=\$(python3 '$TMP/levels.py' | cut -d' ' -f2); $BIN/4s set filter.bypass off >/dev/null; $BIN/4s set filter.cutoff 0.6 >/dev/null; python3 -c \"print('filtered quieter' if \$A < 0.8 * \$B else (\$A, \$B))\""
check "the master's reverb adds a tail" "a tail" bash -c "T=\$(python3 '$TMP/levels.py' | cut -d' ' -f4); python3 -c \"print('a tail' if \$T > 0.003 else \$T)\""
check "...which is gone when bypassed" "no tail" bash -c "$BIN/4s set reverb.bypass on >/dev/null; T=\$(python3 '$TMP/levels.py' | cut -d' ' -f4); $BIN/4s set reverb.bypass off >/dev/null; python3 -c \"print('no tail' if \$T < 0.002 else \$T)\""
s set delay.bypass off >/dev/null
# Undo is per user: a redo whose channel someone removed since does not
# leave its effect behind, unplaced.
s channel add --name Two >/dev/null
s --user alice device add filter --on 2 --id orphan >/dev/null
s --user alice undo >/dev/null
s channel rm 2 >/dev/null
s --user alice redo >/dev/null
check "a redone effect whose channel went away is not left behind" "no device 'orphan'" s device mv orphan --on 1
s --user alice instrument add tb303 --id twin --no-channel >/dev/null
s --user alice undo >/dev/null
s device add filter --on 1 --id twin >/dev/null
check "a redone instrument whose id an effect took since stays undone" "skipped: instrument:twin" s --user alice redo
check "...and the effect keeps its parameters" "twin.cutoff = 0.6" s get twin.cutoff
s device rm twin >/dev/null
check "a session with effects replays" "replay matched" bash -c "$BIN/4s journal export -o '$TMP/fx.json' >/dev/null && B=\$(mktemp -d) && FOURS_DATA_DIR=\$B $BIN/4s daemon start --no-audio --no-midi --listen 127.0.0.1:0 >/dev/null && FOURS_DATA_DIR=\$B $BIN/4s journal replay '$TMP/fx.json'; FOURS_DATA_DIR=\$B $BIN/4s daemon stop >/dev/null"
# Return buses and sends (RFC 0010): a reverb on a return, fed by channel
# 1's send, adds a tail after the kick (the peak between 0.5 and 1.5 s).
s project new >/dev/null
s pattern set kick "x---------------" >/dev/null
cat > "$TMP/tail.py" <<PY
import array, json, struct, subprocess, sys
r = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render', '--out', 'renders/tail.wav']))
b = open(r['path'], 'rb').read(); i = b.index(b'data'); n = struct.unpack('<I', b[i + 4:i + 8])[0]
a = array.array('f'); a.frombytes(b[i + 8:i + 8 + n]); sr = r['sample_rate']
print('tail %.4f' % max(abs(x) for x in a[int(0.5 * sr) * 2:int(1.5 * sr) * 2]))
PY
check "add a return bus" "ch 2 Verb" s channel add --return --name Verb
check "every track channel gets a send to it" "mixer.1.send.2" s params mixer.1.send
s device add reverb --on 2 >/dev/null
s set reverb.mix 1 >/dev/null
check "no send, no tail" "no tail" bash -c "T=\$(python3 '$TMP/tail.py' | cut -d' ' -f2); python3 -c \"print('no tail' if \$T < 0.002 else \$T)\""
check "a send feeds the return" "<- return <- sends from 1 50%  [reverb]" bash -c "$BIN/4s send 1 2 50% >/dev/null && $BIN/4s mixer"
check "...and the reverb adds a tail" "a tail" bash -c "T=\$(python3 '$TMP/tail.py' | cut -d' ' -f2); python3 -c \"print('a tail' if \$T > 0.004 else \$T)\""
check "post-fader: a muted track sends nothing" "no tail" bash -c "$BIN/4s set mixer.1.mute on >/dev/null; T=\$(python3 '$TMP/tail.py' | cut -d' ' -f2); python3 -c \"print('no tail' if \$T < 0.002 else \$T)\""
check "soloing the return keeps the track that feeds it" "a tail" bash -c "$BIN/4s set mixer.1.mute off >/dev/null; $BIN/4s set mixer.2.solo on >/dev/null; T=\$(python3 '$TMP/tail.py' | cut -d' ' -f2); $BIN/4s set mixer.2.solo off >/dev/null; python3 -c \"print('a tail' if \$T > 0.004 else \$T)\""
s set mixer.1.mute off >/dev/null
check "instruments cannot feed a return" "is a return bus" s route drums 2
check "returns save and load back" "<- return <- sends from 1 50%" bash -c "$BIN/4s project save buses >/dev/null && $BIN/4s project new >/dev/null && $BIN/4s project load buses >/dev/null && $BIN/4s mixer"
check "an undo does not turn a channel number reused as a return back into a track" "skipped: channel:3" bash -c "$BIN/4s channel add --name Three >/dev/null; $BIN/4s --user alice channel rm 3 >/dev/null; $BIN/4s channel add --return --name R3 >/dev/null; $BIN/4s --user alice undo"
check "...which stays a return" "ch 3  R3           vol 100%  pan C          <- return" s mixer
s channel rm 3 >/dev/null
check "removing a return removes its sends, and undo brings them back" "mixer.1.send.2 = 0.5" bash -c "$BIN/4s channel rm 2 >/dev/null && $BIN/4s undo >/dev/null && $BIN/4s get mixer.1.send.2"
# Modulators (RFC 0010): an LFO moving a filter's cutoff around its value.
s project new >/dev/null
s pattern set closed_hat "xxxxxxxxxxxxxxxx" >/dev/null
s device add filter --on 1 >/dev/null
s set filter.cutoff 0.5 >/dev/null
RMS() { "$BIN/4s" --json render | python3 -c "import json,sys;print(json.load(sys.stdin)['rms'])"; }
export -f RMS
check "add an LFO bank to the rack" "lfo        lfo     LFO          in the modulator rack" s device add lfo
check "its parameters are per LFO" "lfo.1.rate" s params lfo.1
check "route LFO 1 to the cutoff" "lfo.1 -> filter.cutoff depth +40%" s mod route lfo.1 filter.cutoff 40%
check "only continuous parameters take modulation" "not continuous" s mod route lfo.1 filter.mode 50%
check "unknown outputs are refused" "no modulator output 'lfo.9'" s mod route lfo.9 filter.cutoff 50%
check "the LFO opens the filter on the hats (renders differ with and without it)" "modulated" bash -c "A=\$(RMS); $BIN/4s mod unroute lfo.1 filter.cutoff >/dev/null; B=\$(RMS); python3 -c \"print('modulated' if abs(\$A - \$B) > 0.3 * \$B else (\$A, \$B))\""
check "param get shows the value set by hand, not the modulated one" "filter.cutoff = 0.5" bash -c "$BIN/4s mod route lfo.1 filter.cutoff -40% >/dev/null && $BIN/4s get filter.cutoff"
check "removing the LFO drops its routes" "(no modulation routes)" bash -c "$BIN/4s device rm lfo >/dev/null && $BIN/4s mod"
check "...and undo brings them back" "lfo.1 -> filter.cutoff depth -40%" bash -c "$BIN/4s undo >/dev/null && $BIN/4s mod"
check "routes save and load back" "lfo.1 -> filter.cutoff depth -40%" bash -c "$BIN/4s project save mods >/dev/null && $BIN/4s project new >/dev/null && $BIN/4s project load mods >/dev/null && $BIN/4s mod"
check "removing the target drops the route" "(no modulation routes)" bash -c "$BIN/4s device rm filter >/dev/null && $BIN/4s mod"
check "a session with modulation replays" "replay matched" bash -c "$BIN/4s journal export -o '$TMP/mods.json' >/dev/null && B=\$(mktemp -d) && FOURS_DATA_DIR=\$B $BIN/4s daemon start --no-audio --no-midi --listen 127.0.0.1:0 >/dev/null && FOURS_DATA_DIR=\$B $BIN/4s journal replay '$TMP/mods.json'; FOURS_DATA_DIR=\$B $BIN/4s daemon stop >/dev/null"
# The sampler (RFC 0010): a WAV on pad 1, played from a clip, saved inside
# the bundle.
s project new >/dev/null
python3 - "$TMP/tone.wav" <<'PY'
import math, struct, sys
sr = 44100; n = int(sr * 0.3)
data = b''.join(struct.pack('<h', int(20000 * math.sin(2 * math.pi * 220 * i / sr) * (1 - i / n))) for i in range(n))
fmt = struct.pack('<IHHIIHH', 16, 1, 1, sr, sr * 2, 2, 16)
open(sys.argv[1], 'wb').write(b'RIFF' + struct.pack('<I', 36 + len(data)) + b'WAVEfmt ' + fmt + b'data' + struct.pack('<I', len(data)) + data)
PY
check "add a sampler" "sampler    sampler" s instrument add sampler
s clip launch sampler >/dev/null
check "a file here is sent to the engine and loaded into a pad" "sampler pad 1  tone.wav                 0.30s" s sampler load 1 "$TMP/tone.wav"
check "...journaled as the load of the stored file" "sampler.load" s journal --limit 1
check "pads are rows of the step view" "P1   x--- x---" bash -c "$BIN/4s step sampler 1 pad1 >/dev/null; $BIN/4s step sampler 5 pad1 | grep P1"
check "the pad plays from the clip" "triggers: 2 (sampler 2)  detected onsets: 2" bash -c "$BIN/4s render | grep triggers"
check "a pad with no sample stays silent" "triggers: 2 (sampler 2)" bash -c "$BIN/4s step sampler 9 pad2 >/dev/null; $BIN/4s render | grep triggers"
check "tune moves its pitch" "sampler.1.tune = 12" s set sampler.1.tune 12
check "non-WAV files are refused, and nothing is stored" "nothing stored" bash -c "$BIN/4s sampler load 2 Cargo.toml 2>&1 | grep -q 'sample.upload: Cargo.toml: not an audio file' && test ! -e '$FOURS_DATA_DIR/samples/Cargo.toml' && echo nothing stored"
check "a path on the engine host loads directly" "sampler pad 3  host.wav" bash -c "mkdir -p '$FOURS_DATA_DIR/samples' && cp '$TMP/tone.wav' '$FOURS_DATA_DIR/samples/host.wav' && $BIN/4s sampler load 3 samples/host.wav | grep 'pad 3'"
check "a choke group cuts the pad it shares (a silent pad 2 shortens pad 1)" "choked" bash -c "$BIN/4s set sampler.1.tune 0 >/dev/null; $BIN/4s step sampler 5 pad1 --off >/dev/null; $BIN/4s sampler load 2 '$FOURS_DATA_DIR/samples/host.wav' >/dev/null; $BIN/4s set sampler.2.gain 0 >/dev/null; $BIN/4s step sampler 2 pad2 >/dev/null; A=\$($BIN/4s --json render | python3 -c 'import json,sys;print(json.load(sys.stdin)[\"rms\"])'); $BIN/4s set sampler.1.choke 1 >/dev/null; $BIN/4s set sampler.2.choke 1 >/dev/null; B=\$($BIN/4s --json render | python3 -c 'import json,sys;print(json.load(sys.stdin)[\"rms\"])'); $BIN/4s sampler clear 2 >/dev/null; $BIN/4s sampler clear 3 >/dev/null; $BIN/4s step sampler 2 pad2 --off >/dev/null; python3 -c \"print('choked' if \$B < 0.93 * \$A else (\$A, \$B))\""
check "saving copies the sample into the bundle" "samples/sampler.1.tone.wav" bash -c "$BIN/4s project save sampled >/dev/null && ls '$FOURS_DATA_DIR/projects/sampled.4s/samples' && grep -o 'samples/sampler.1.tone.wav' '$FOURS_DATA_DIR/projects/sampled.4s/project.json'"
check "after a save the project needs only the bundle's copy" "sampled.4s" bash -c "rm '$FOURS_DATA_DIR/samples/tone.wav' && $BIN/4s project save"
check "...so it loads without the original, under its own name" "sampler pad 1  tone.wav" bash -c "rm '$TMP/tone.wav' && $BIN/4s project new >/dev/null && $BIN/4s project load sampled >/dev/null && $BIN/4s sampler"
check "saving again keeps one copy under the same name" "1 samples/sampler.1.tone.wav" bash -c "$BIN/4s project save >/dev/null && echo \$(ls '$FOURS_DATA_DIR/projects/sampled.4s/samples' | wc -l) \$(grep -o 'samples/sampler.1.tone.wav' '$FOURS_DATA_DIR/projects/sampled.4s/project.json')"
check "saving under a new name does not stack the name" "1 samples/sampler.1.tone.wav" bash -c "$BIN/4s project save copied >/dev/null && echo \$(ls '$FOURS_DATA_DIR/projects/copied.4s/samples' | wc -l) \$(grep -o 'samples/sampler.1.tone.wav' '$FOURS_DATA_DIR/projects/copied.4s/project.json')"
check "clearing a pad, and undo" "sampler pad 1  tone.wav" bash -c "$BIN/4s sampler clear 1 >/dev/null && $BIN/4s undo >/dev/null && $BIN/4s sampler"
check "removing the sampler and undo bring its samples back" "sampler pad 1" bash -c "$BIN/4s instrument rm sampler >/dev/null && $BIN/4s undo >/dev/null && $BIN/4s sampler"
# Formats and regions (RFC 0011): MP3, FLAC, and AAC files (tests/fixtures/
# audio: 2 s, a 220 Hz tone to 0.5 s, a 1 kHz burst at 1.0 s, a 440 Hz
# tone 1.5..1.8 s), any region of one on a pad, sent in chunks.
FX=tests/fixtures/audio
check "an MP3's length and format" "mp3, 44100 Hz, 2 ch" s sample info $FX/song.mp3
check "an AAC (M4A) file's" "aac, 44100 Hz, 2 ch" s sample info $FX/song.m4a
check "and AIFF, ALAC, and Ogg Vorbis files'" "pcm_s16be alac vorbis" bash -c "echo \$(for f in song.aiff song-alac.m4a song.ogg; do $BIN/4s sample info $FX/\$f | head -1 | awk '{print \$3}' | tr -d ,; done)"
check "a device or a folder is not read as a sample" "not a file" s call sample.info '{"path": "/dev/zero"}'

check "a FLAC file's waveform: the tones and the burst, quiet between" "tones" bash -c "$BIN/4s sample peaks $FX/song.flac --bins 16 | tail -1 | python3 -c 'import sys; l=sys.stdin.read().strip()[1:-1]; print(\"tones\" if l[1]!=\" \" and l[5]==\" \" and l[8] in \"*+=#@\" and l[13]!=\" \" else repr(l))'"
check "...and zoomed in on the burst" "0:00.980 - 0:01.060" s sample peaks $FX/song.flac --from 0.98 --to 1.06 --bins 8
check "a region of an MP3 on a pad" "sampler pad 1  song.mp3                 0.30s  0:01.500 - 0:01.800 of 0:02." s sampler load 1 $FX/song.mp3 --from 1.5 --to 1.8
check "...plays what that region holds (one hit)" "triggers: 1 (sampler 1)" bash -c "$BIN/4s step sampler 1 pad1 >/dev/null; $BIN/4s render | grep triggers"
check "...journaled as a load of the stored file (not the upload), with its region" "path and region" bash -c "$BIN/4s --json journal --limit 3 | python3 -c 'import json,sys; e=[x[\"params\"] for x in json.load(sys.stdin)[\"entries\"] if x[\"method\"]==\"sampler.load\"][-1]; print(\"path and region\" if e[\"path\"].endswith(\"song.mp3\") and e[\"upload\"] is None and e[\"start\"]==1.5 else e)'"
check "a region in m:ss.ms, of a FLAC file" "0:00.000 - 0:00.500 of" s sampler load 2 $FX/song.flac --from 0:00.000 --to 0:00.5
check "undo brings back the pad's last region" "0:01.500 - 0:01.800" bash -c "$BIN/4s sampler load 1 $FX/song.mp3 --from 1.0 --to 1.1 >/dev/null && $BIN/4s undo >/dev/null && $BIN/4s sampler | grep 'pad 1 '"
check "a region past the file's end is refused" "past the end of the file" s sampler load 3 $FX/song.mp3 --from 5 --to 6
python3 - "$TMP/long.wav" <<'PY'
import struct, sys
sr = 8000; n = sr * 61
data = struct.pack('<h', 1000) * n
fmt = struct.pack('<IHHIIHH', 16, 1, 1, sr, sr * 2, 2, 16)
open(sys.argv[1], 'wb').write(b'RIFF' + struct.pack('<I', 36 + len(data)) + b'WAVEfmt ' + fmt + b'data' + struct.pack('<I', len(data)) + data)
PY
check "all of a file longer than a pad plays is refused: load a region" "load a region of it" s sampler load 3 "$TMP/long.wav"
check "...and a region of it loads" "0:30.000 - 0:30.250 of 1:01.000" s sampler load 3 "$TMP/long.wav" --from 30 --to 30.25
# Two chunks by hand (the CLI sends 11 MB ones): the second must go where
# the first ended; the finished file is checked and stored.
cat > "$TMP/chunks.py" <<PY
import base64, json, subprocess, sys
data = open('$FX/song.flac', 'rb').read(); half = len(data) // 2
def up(offset, chunk, done):
    p = {'upload': 'two-chunks', 'name': 'chunked.flac', 'offset': offset, 'data': base64.b64encode(chunk).decode(), 'done': done}
    return subprocess.run(['$BIN/4s', 'call', 'sample.upload', json.dumps(p)], capture_output=True, text=True)
up(0, data[:half], False)
bad = up(half + 1, data[half:], True)
ok = up(half, data[half:], True)
print('gap refused' if 'the next chunk goes there' in bad.stderr + bad.stdout else bad.stdout + bad.stderr)
print(json.loads(ok.stdout)['path'].endswith('samples/chunked.flac'))
PY
check "an upload in two chunks: a gap is refused, then it is stored" "gap refused
True" python3 "$TMP/chunks.py"
check "...and a load names it" "sampler pad 4  chunked.flac             0.50s" bash -c "$BIN/4s call sampler.load '{\"pad\": 4, \"upload\": \"two-chunks\", \"end\": 0.5}' >/dev/null && $BIN/4s sampler | grep 'pad 4 '"
check "4s sample upload sends a file and says where it is stored" "samples/song.flac" bash -c "$BIN/4s sample upload $FX/song.flac"
check "...and sending it again keeps one copy" "1" bash -c "$BIN/4s sample upload $FX/song.flac >/dev/null && ls '$FOURS_DATA_DIR/samples' | grep -c 'song.flac$'"
check "a load of an upload cannot go in a batch (it is journaled as its stored file)" "cannot be in a batch" s call batch '{"requests": [{"method": "sampler.load", "params": {"pad": 4, "upload": "two-chunks"}}]}'
check "saving writes a region as a WAV in the bundle, with where it came from" "sampler.1.song.wav" bash -c "$BIN/4s project save regions >/dev/null && ls '$FOURS_DATA_DIR/projects/regions.4s/samples' && grep -o '\"start\": 1.5' '$FOURS_DATA_DIR/projects/regions.4s/project.json'"
check "...which loads back as that region, still knowing its origin" "(cut from " bash -c "$BIN/4s project new >/dev/null && $BIN/4s project load regions >/dev/null && $BIN/4s sampler | grep 'pad 1 '"
check "cutting the bundle's own copy and saving keeps that copy whole" "0.3 s, and the cut beside it" bash -c "R='$FOURS_DATA_DIR/projects/regions.4s/samples'; $BIN/4s call sampler.load \"{\\\"pad\\\": 1, \\\"path\\\": \\\"\$R/sampler.1.song.wav\\\", \\\"start\\\": 0.05, \\\"end\\\": 0.2}\" >/dev/null && $BIN/4s project save >/dev/null && ls \"\$R\" | grep -q 'sampler.1.song-2.wav' && $BIN/4s call sample.info \"{\\\"path\\\": \\\"\$R/sampler.1.song.wav\\\"}\" | python3 -c 'import json,sys; print(\"%.1f s, and the cut beside it\" % json.load(sys.stdin)[\"seconds\"])'"
check "a session with samples replays" "replay matched" bash -c "$BIN/4s journal export -o '$TMP/smp.json' >/dev/null && B=\$(mktemp -d) && FOURS_DATA_DIR=\$B $BIN/4s daemon start --no-audio --no-midi --listen 127.0.0.1:0 >/dev/null && FOURS_DATA_DIR=\$B $BIN/4s journal replay '$TMP/smp.json'; FOURS_DATA_DIR=\$B $BIN/4s daemon stop >/dev/null"
# Automation lanes (RFC 0010): a clip's lane moves a parameter while the
# clip plays; the value set by hand comes back when nothing plays it.
s project new >/dev/null
s pattern set kick "x-x-x-x-x-x-x-x-" >/dev/null
cat > "$TMP/lr.py" <<PY
import json, subprocess
r = json.loads(subprocess.check_output(['$BIN/4s', '--json', 'render']))
l, rr = r['left']['rms'], r['right']['rms']
# Hard left from the first hit (a lane's first value jumps there at play).
print('left' if r['right']['peak'] < 0.001 else 'even' if abs(rr - l) < 0.01 else (l, rr, r['right']['peak']))
PY
check "a centered kick renders even" "even" python3 "$TMP/lr.py"
check "a lane on channel 1's pan" "lane mixer.1.pan: 0:-1 192:-1" s clip lane drums mixer.1.pan "0:-1 192:-1"
check "...pans the render left" "left" python3 "$TMP/lr.py"
check "...and leaves the value set by hand" "pan C" bash -c "$BIN/4s mixer | grep 'ch 1'"
check "while playing, get shows where the lane has it" "mixer.1.pan = 0  (now -1: modulated or automated)" bash -c "$BIN/4s play >/dev/null; sleep 0.3; $BIN/4s get mixer.1.pan; $BIN/4s stop >/dev/null"
check "...and state.get lists it under moving, until it stops" "{}" bash -c "sleep 0.3; $BIN/4s call state.get | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"moving\"])'"
check "the transport takes no automation" "the transport's" s clip lane drums transport.tempo "0:100"
check "nor does the song's loop (the daemon counts time by it)" "the transport's" s clip lane drums song.loop_end "0:2"
check "nor does modulation" "the transport's" bash -c "$BIN/4s device add lfo >/dev/null && $BIN/4s mod route lfo.1 song.loop_end 40%; $BIN/4s device rm lfo >/dev/null"
check "nor do toggles" "is a toggle" s clip lane drums mixer.1.mute "0:1"
check "values are clamped to the parameter's range" "lane mixer.1.volume: 0:1" s clip lane drums mixer.1.volume "0:9"
check "a duplicate copies the lanes" "lane mixer.1.pan: 0:-1 192:-1" bash -c "$BIN/4s clip dup drums | grep pan"
check "undoing a duplicate takes the copy and its lanes, and leaves the original's" "drums: clips *1; kept" bash -c "$BIN/4s undo >/dev/null && echo \$($BIN/4s clip list drums | grep -o 'drums: clips [^ ]*') \$($BIN/4s clip show drums | grep -q 'lane mixer.1.pan' && echo kept)"
check "removing a lane is one undo step" "undid: clip.lane.remove" bash -c "$BIN/4s clip lane drums mixer.1.volume --rm >/dev/null && $BIN/4s undo"
check "lanes save with the project and load back" "lane mixer.1.pan: 0:-1 192:-1" bash -c "$BIN/4s project save lanes >/dev/null && $BIN/4s project new >/dev/null && $BIN/4s project load lanes >/dev/null && $BIN/4s clip show drums"
check "a lane on an unknown parameter is dropped on load (a warning in the log), and the rest loads" "1
ignored the lane for 'mixer.9.pan'" bash -c "sed -i.bak 's/mixer.1.pan/mixer.9.pan/' '$FOURS_DATA_DIR/projects/lanes.4s/project.json' && $BIN/4s project load lanes >/dev/null && $BIN/4s clip show drums | grep -c 'lane ' && $BIN/4s daemon logs | grep -o \"ignored the lane for 'mixer.9.pan'\" | tail -1; mv '$FOURS_DATA_DIR/projects/lanes.4s/project.json.bak' '$FOURS_DATA_DIR/projects/lanes.4s/project.json'"
s project load lanes >/dev/null
check "a whole-number parameter's lane points are rounded" "lane filter.mode: 0:2" bash -c "$BIN/4s device add filter --on 1 >/dev/null && $BIN/4s clip lane drums filter.mode '0:1.6' | grep filter.mode"
s clip lane drums filter.mode --rm >/dev/null
check "a lane on an effect goes with the effect" "no lane" bash -c "$BIN/4s clip lane drums filter.cutoff '0:0.1' >/dev/null && $BIN/4s device rm filter >/dev/null && ($BIN/4s clip show drums | grep -q filter.cutoff || echo no lane)"
check "...and undo brings both back" "lane filter.cutoff: 0:0.1" bash -c "$BIN/4s undo >/dev/null && $BIN/4s clip show drums"
check "without the lane, the render is even again" "even" bash -c "$BIN/4s clip lane drums mixer.1.pan --rm >/dev/null && python3 '$TMP/lr.py'"
# Wait for the playhead (instead of sleeping, so slow machines land the
# same): `at.py song <tick>` for a song tick, `at.py clip <tick> [--wrap]`
# for the drums clip's tick (after it next loops, with --wrap). A song tick
# grows within a pass of the song, so it returns once the playhead is at or
# past it (a song that loops brings a missed one round again); a clip loops,
# so it waits for the beat from the tick (a miss comes round again).
cat > "$TMP/at.py" <<PY
import json, subprocess, sys, time
kind, want, wrap = sys.argv[1], int(sys.argv[2]), "--wrap" in sys.argv
last, end = None, time.time() + 10
while time.time() < end:
    t = json.loads(subprocess.check_output(["$BIN/4s", "call", "state.get"]))["transport"]
    now = t["tick"] if kind == "song" else (t["clips"].get("drums") or {}).get("tick")
    if now is not None:
        if wrap and last is not None and now < last:
            wrap = False
        if not wrap and want <= now and (kind == "song" or now < want + 96):
            sys.exit(0)
        last = now
    time.sleep(0.005)
sys.exit("the playhead never reached %s %d" % (kind, want))
PY
# Knob moves while recording become a lane in the clip under the playhead
# (on the 1/16 grid), written with the pass: one undo step.
cat > "$TMP/autorec.sh" <<SH
$BIN/4s tempo 240 >/dev/null
$BIN/4s clip length drums 64 >/dev/null
$BIN/4s record --count-in 0 >/dev/null
python3 '$TMP/at.py' song 24 >/dev/null
for v in -1 -0.5 0 0.5 1; do $BIN/4s set mixer.1.pan \$v >/dev/null; sleep 0.15; done
$BIN/4s stop >/dev/null
SH
check "knob moves while recording become a lane, rising with the knob" "rising" bash -c "$BIN/4s clip lane drums mixer.1.pan --rm >/dev/null 2>&1; bash '$TMP/autorec.sh'; $BIN/4s --json clip show drums | python3 -c 'import json,sys; l=[x for x in json.load(sys.stdin)[\"lanes\"] if x[\"path\"]==\"mixer.1.pan\"][0][\"points\"]; v=[p[\"value\"] for p in l]; t=[p[\"tick\"] for p in l]; print(\"rising\" if len(v)>=3 and v==sorted(v) and all(x%24==0 for x in t) else l)'"
check "...written with the pass, one undo step" "undid: clip.lane.set" s undo
check "...which removes the lane" "no lane" bash -c "$BIN/4s clip show drums | grep -q 'lane mixer.1.pan' || echo no lane"
check "a pass across the loop point keeps the earlier pass's points" "kept" bash -c "$BIN/4s clip length drums 16 >/dev/null; $BIN/4s record --count-in 0 >/dev/null; for v in 48:0.2 144:0.4 240:0.6 288:0.8; do python3 '$TMP/at.py' clip \${v%%:*} >/dev/null && $BIN/4s set mixer.1.pan \${v#*:} >/dev/null; done; python3 '$TMP/at.py' clip 24 --wrap >/dev/null && $BIN/4s set mixer.1.pan -0.5 >/dev/null; python3 '$TMP/at.py' clip 72 >/dev/null && $BIN/4s set mixer.1.pan -0.5 >/dev/null; $BIN/4s stop >/dev/null; $BIN/4s clip length drums 32 >/dev/null; $BIN/4s --json clip show drums | python3 -c 'import json,sys; l=[x for x in json.load(sys.stdin)[\"lanes\"] if x[\"path\"]==\"mixer.1.pan\"][0][\"points\"]; print(\"kept\" if any(p[\"tick\"]>=192 and p[\"value\"]>0 for p in l) and any(p[\"value\"]==-0.5 for p in l) else l)'"
s clip lane drums mixer.1.pan --rm >/dev/null
s set mixer.1.pan 0 >/dev/null
check "another player's moves meanwhile are not recorded, only the recorder's" "lane mixer.1.volume" bash -c "$BIN/4s record --count-in 0 >/dev/null; python3 '$TMP/at.py' song 24 >/dev/null; $BIN/4s --user bob set mixer.1.pan 0.7 >/dev/null; $BIN/4s set mixer.1.volume 0.5 >/dev/null; sleep 0.2; $BIN/4s stop >/dev/null; $BIN/4s clip show drums | grep -q 'lane mixer.1.pan' || $BIN/4s clip show drums | grep lane"
s clip lane drums mixer.1.volume --rm >/dev/null
s set mixer.1.volume 1 >/dev/null
check "a mapped CC's moves are recorded too" "lane mixer.1.pan: " bash -c "$BIN/4s cc map keys 21 mixer.1.pan >/dev/null; $BIN/4s record --count-in 0 >/dev/null; python3 '$TMP/at.py' song 24 >/dev/null; $BIN/4s midi send keys B0 15 40 >/dev/null; sleep 0.15; $BIN/4s midi send keys B0 15 7F >/dev/null; sleep 0.2; $BIN/4s stop >/dev/null; $BIN/4s cc unmap keys 21 >/dev/null; $BIN/4s clip show drums | grep 'lane mixer.1.pan'"
s clip lane drums mixer.1.pan --rm >/dev/null
s set mixer.1.pan 0 >/dev/null
check "a clip with room for one more lane takes one, and the pass keeps its notes" "8 lanes, 1 notes" bash -c "$BIN/4s clip lane drums filter.cutoff --rm >/dev/null 2>&1; for k in kick snare clap closed_hat open_hat low_tom high_tom; do $BIN/4s clip lane drums drums.\$k.level 0:0.5 >/dev/null; done; $BIN/4s clip clear drums >/dev/null; $BIN/4s record --count-in 0 >/dev/null; python3 '$TMP/at.py' song 24 >/dev/null; $BIN/4s set mixer.1.volume 0.5 >/dev/null; $BIN/4s set mixer.1.pan 0.5 >/dev/null; $BIN/4s key C2 --for 0.05 >/dev/null; sleep 0.3; $BIN/4s stop >/dev/null; echo \$($BIN/4s clip show drums | grep -c lane) lanes, \$($BIN/4s clip show drums | head -1 | grep -o '[0-9]* notes')"
for k in kick snare clap closed_hat open_hat low_tom high_tom; do s clip lane drums drums.$k.level --rm >/dev/null; done
for p in mixer.1.volume mixer.1.pan; do $BIN/4s clip lane drums $p --rm >/dev/null 2>&1 || true; done
s set mixer.1.volume 1 >/dev/null
s set mixer.1.pan 0 >/dev/null
s clip length drums 16 >/dev/null
check "moves in two placements of a clip are separate runs: the points between them stay" "separate runs" bash -c "$BIN/4s song back >/dev/null; $BIN/4s song place drums --at 1 --bars 1 >/dev/null; $BIN/4s song place drums --at 2 --bars 1 >/dev/null; $BIN/4s clip lane drums mixer.1.pan '0:0 144:0.9 336:0' >/dev/null; $BIN/4s record --count-in 0 >/dev/null; python3 '$TMP/at.py' song 48 >/dev/null && $BIN/4s set mixer.1.pan -0.5 >/dev/null; python3 '$TMP/at.py' song 600 >/dev/null && $BIN/4s set mixer.1.pan -0.7 >/dev/null; python3 '$TMP/at.py' song 672 >/dev/null; $BIN/4s stop >/dev/null; $BIN/4s --json clip show drums | python3 -c 'import json,sys; l={p[\"tick\"]: p[\"value\"] for p in [x for x in json.load(sys.stdin)[\"lanes\"] if x[\"path\"]==\"mixer.1.pan\"][0][\"points\"]}; v=list(l.values()); a=[t for t,x in l.items() if x==-0.5]; b=[t for t,x in l.items() if x==-0.7]; print(\"separate runs\" if l.get(144)==0.9 and a and b and a[0]<144<b[0] else l)'"
s song rm drums 2 >/dev/null
s song rm drums 1 >/dev/null
s clip launch drums 1 --now >/dev/null
s clip lane drums mixer.1.pan --rm >/dev/null
s set mixer.1.pan 0 >/dev/null
check "a removed device's parameters leave moving at once" "moving then gone" bash -c "$BIN/4s device add filter --on 1 >/dev/null; $BIN/4s device add lfo --id lfo9 >/dev/null; $BIN/4s mod route lfo9.1 filter.cutoff 50% >/dev/null; sleep 0.3; M=\$($BIN/4s call state.get | python3 -c 'import json,sys; print(int(\"filter.cutoff\" in json.load(sys.stdin)[\"moving\"]))'); $BIN/4s device rm filter >/dev/null; G=\$($BIN/4s call state.get | python3 -c 'import json,sys; print(\"gone\" if \"filter.cutoff\" not in json.load(sys.stdin)[\"moving\"] else \"there\")'); $BIN/4s device rm lfo9 >/dev/null; [ \$M -gt 0 ] && echo moving then \$G || echo never moving"
s tempo 120 >/dev/null
check "a session with lanes replays" "replay matched" bash -c "$BIN/4s journal export -o '$TMP/lanes.json' >/dev/null && B=\$(mktemp -d) && FOURS_DATA_DIR=\$B $BIN/4s daemon start --no-audio --no-midi --listen 127.0.0.1:0 >/dev/null && FOURS_DATA_DIR=\$B $BIN/4s journal replay '$TMP/lanes.json'; FOURS_DATA_DIR=\$B $BIN/4s daemon stop >/dev/null"
s project load e2e >/dev/null
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
