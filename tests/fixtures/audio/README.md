# Audio fixtures

One 2 s stereo signal at 44.1 kHz in three formats, for decoding tests
(`crates/daemon/src/core/sources.rs`, `scripts/e2e-cli.sh`): a 220 Hz tone
(amplitude 0.5) to 0.5 s, silence, a 1 kHz burst (0.8, fading) at
1.0-1.05 s, silence, a 440 Hz tone (0.5) at 1.5-1.8 s, silence.

- `song.mp3`: LAME, 128 kbit/s (about 24 ms of encoder delay).
- `song.flac`: macOS `afconvert -f flac -d flac`.
- `song.m4a`: AAC in MP4, `afconvert -f m4af -d aac`.
