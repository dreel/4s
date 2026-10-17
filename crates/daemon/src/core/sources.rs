//! Sample sources (RFC 0011): audio files decoded once and kept, so a pad
//! can take any region of a long file (one drum hit from a whole song)
//! without decoding it again, and the editor can draw its waveform.
//!
//! A file is decoded with symphonia (WAV, AIFF, FLAC, MP3, AAC/M4A, ALAC,
//! Ogg Vorbis), mixed or doubled to stereo, and resampled to the engine's
//! rate with rubato. Decoded sources are kept in a cache keyed by the
//! file's contents (so one file under two paths decodes once), bounded by
//! a memory budget: the least recently used go first, and come back by
//! decoding again. Pads never hold a source: they get a copy of their
//! region (`Source::region`), so a whole song is not pinned for one hit.
//! A source also keeps a coarse min/max overview (`PEAK_BIN` frames a
//! bin), so the editor's waveform costs little at any zoom.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Longest source: 20 minutes.
pub const MAX_SOURCE_SECONDS: f64 = 20.0 * 60.0;
/// Longest region a pad plays: 60 seconds.
pub const MAX_REGION_SECONDS: f64 = 60.0;
/// The largest file read as a source (a 20-minute FLAC fits).
pub const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
/// Frames per bin of a source's waveform overview.
const PEAK_BIN: usize = 256;

/// A decoded file: interleaved stereo at the engine's rate.
pub struct Source {
    pub data: Arc<[f32]>,
    pub frames: usize,
    /// The engine's rate (what `data` is at).
    pub rate: u32,
    /// The file's codec, own rate, and channels (`mp3, 44100 Hz, 2 ch`).
    pub format: String,
    /// Min and max of each `PEAK_BIN` frames (both channels).
    peaks: Vec<[f32; 2]>,
}

impl Source {
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / self.rate.max(1) as f64
    }

    fn bytes(&self) -> usize {
        self.data.len() * 4 + self.peaks.len() * 8
    }

    /// The frames `start..end` seconds cover, checked: inside the source,
    /// not empty, not longer than `MAX_REGION_SECONDS`.
    pub fn frame_range(&self, start: f64, end: f64) -> Result<(usize, usize), String> {
        if !(start.is_finite() && end.is_finite()) || start < 0.0 || end <= start {
            return Err("a region runs from a start to a later end, in seconds".into());
        }
        if end - start > MAX_REGION_SECONDS + 1e-9 {
            return Err(format!("a region is at most {MAX_REGION_SECONDS} s"));
        }
        let a = (start * self.rate as f64).round() as usize;
        let b = ((end * self.rate as f64).round() as usize).min(self.frames);
        if a + 2 > b {
            return Err(format!("the region is past the end of the file ({:.3} s)", self.seconds()));
        }
        Ok((a, b))
    }

    /// A copy of the frames `a..b` (interleaved stereo).
    pub fn region(&self, a: usize, b: usize) -> Arc<[f32]> {
        self.data[a * 2..b * 2].into()
    }

    /// `bins` (min, max) pairs over `start..end` seconds: from the overview
    /// when a bin spans at least `PEAK_BIN` frames, else from the audio.
    pub fn peaks(&self, start: f64, end: f64, bins: usize) -> Vec<[f32; 2]> {
        let a = ((start.max(0.0) * self.rate as f64) as usize).min(self.frames);
        let b = ((end.max(0.0) * self.rate as f64) as usize).min(self.frames);
        if bins == 0 || b <= a {
            return Vec::new();
        }
        let span = (b - a) as f64 / bins as f64;
        (0..bins)
            .map(|i| {
                let (x, y) = (a + (i as f64 * span) as usize, (a + ((i + 1) as f64 * span) as usize).max(a + (i as f64 * span) as usize + 1).min(b));
                let mut m = [f32::MAX, f32::MIN];
                if span >= PEAK_BIN as f64 {
                    for p in &self.peaks[x / PEAK_BIN..y.div_ceil(PEAK_BIN).min(self.peaks.len())] {
                        m = [m[0].min(p[0]), m[1].max(p[1])];
                    }
                } else {
                    for v in &self.data[x * 2..y * 2] {
                        m = [m[0].min(*v), m[1].max(*v)];
                    }
                }
                if m[0] > m[1] { [0.0, 0.0] } else { m }
            })
            .collect()
    }
}

/// Decode audio bytes (`name` hints the format) to a source at `rate`.
pub fn decode(bytes: Vec<u8>, name: &str, rate: u32) -> Result<Source, String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let mut hint = Hint::new();
    if let Some(ext) = Path::new(name).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("not an audio file this build can read ({e})"))?;
    let mut reader = probed.format;
    let track = reader
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no audio track")?;
    let (track_id, params) = (track.id, track.codec_params.clone());
    let file_rate = params.sample_rate.ok_or("unknown sample rate")?;
    // What the container says, until a packet says otherwise.
    let mut channels = params.channels.map_or(1, |c| c.count()).max(1);
    let codec = symphonia::default::get_codecs().get_codec(params.codec).map_or("audio", |d| d.short_name).to_string();
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|e| format!("cannot decode {codec} ({e})"))?;
    let max_frames = (MAX_SOURCE_SECONDS * file_rate as f64) as usize;
    if params.n_frames.is_some_and(|n| n as usize > max_frames) {
        return Err(format!("longer than {} minutes", MAX_SOURCE_SECONDS / 60.0));
    }

    // Stereo frames at the file's rate (mono doubled, more channels: the
    // first two).
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match reader.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(format!("reading the file: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A damaged packet is skipped, as players do.
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decoding: {e}")),
        };
        let b = buf.get_or_insert_with(|| SampleBuffer::new(decoded.capacity() as u64, *decoded.spec()));
        if b.capacity() < decoded.capacity() * decoded.spec().channels.count() {
            *b = SampleBuffer::new(decoded.capacity() as u64, *decoded.spec());
        }
        // The packet says how many channels it has (a container may not).
        let ch = decoded.spec().channels.count().max(1);
        channels = ch;
        b.copy_interleaved_ref(decoded);
        for f in b.samples().chunks_exact(ch) {
            left.push(f[0]);
            right.push(f[1.min(ch - 1)]);
        }
        if left.len() > max_frames {
            return Err(format!("longer than {} minutes", MAX_SOURCE_SECONDS / 60.0));
        }
    }
    if left.len() < 2 {
        return Err("no audio".into());
    }
    let (left, right) = if file_rate == rate { (left, right) } else { resample(left, right, file_rate, rate)? };
    let frames = left.len();
    let mut data = Vec::with_capacity(frames * 2);
    for (l, r) in left.iter().zip(&right) {
        data.extend([*l, *r]);
    }
    let peaks = data
        .chunks(PEAK_BIN * 2)
        .map(|c| c.iter().fold([f32::MAX, f32::MIN], |m, v| [m[0].min(*v), m[1].max(*v)]))
        .collect();
    let format = format!("{}, {} Hz, {}", codec, file_rate, if channels == 1 { "mono".into() } else { format!("{channels} ch") });
    Ok(Source { data: data.into(), frames, rate, format, peaks })
}

/// Band-limited resampling (windowed sinc) of two channels from `from` to
/// `to` Hz.
fn resample(left: Vec<f32>, right: Vec<f32>, from: u32, to: u32) -> Result<(Vec<f32>, Vec<f32>), String> {
    use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
    let params = SincInterpolationParameters {
        sinc_len: 128,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 128,
        window: WindowFunction::BlackmanHarris2,
    };
    const CHUNK: usize = 4096;
    let ratio = to as f64 / from as f64;
    let mut r = SincFixedIn::<f32>::new(ratio, 1.0, params, CHUNK, 2).map_err(|e| e.to_string())?;
    let want = (left.len() as f64 * ratio).round() as usize;
    let delay = r.output_delay();
    let (mut ol, mut or) = (Vec::with_capacity(want + delay + CHUNK), Vec::with_capacity(want + delay + CHUNK));
    let push = |ol: &mut Vec<f32>, or: &mut Vec<f32>, out: Vec<Vec<f32>>| {
        ol.extend_from_slice(&out[0]);
        or.extend_from_slice(&out[1]);
    };
    let mut at = 0;
    while at + CHUNK <= left.len() {
        push(&mut ol, &mut or, r.process(&[&left[at..at + CHUNK], &right[at..at + CHUNK]], None).map_err(|e| e.to_string())?);
        at += CHUNK;
    }
    if at < left.len() {
        push(&mut ol, &mut or, r.process_partial(Some(&[&left[at..], &right[at..]]), None).map_err(|e| e.to_string())?);
    }
    // Flush what the filter still holds.
    let mut guard = 0;
    while ol.len() < want + delay && guard < 64 {
        push(&mut ol, &mut or, r.process_partial::<&[f32]>(None, None).map_err(|e| e.to_string())?);
        guard += 1;
    }
    let end = (want + delay).min(ol.len());
    Ok((ol[delay.min(end)..end].to_vec(), or[delay.min(end)..end].to_vec()))
}

/// Decoded sources by content, least recently used first out once over a
/// memory budget.
pub struct SourceCache {
    budget: usize,
    rate: u32,
    /// By a cryptographic hash of the file's bytes (so no file, even one
    /// made to, is taken for another).
    by_hash: HashMap<[u8; 32], (Arc<Source>, u64)>,
    /// A path's size and modification time when its contents were hashed,
    /// so an unchanged file is not read again to find its source.
    by_path: HashMap<PathBuf, (u64, Option<SystemTime>, [u8; 32])>,
    clock: u64,
}

impl SourceCache {
    pub fn new(budget_bytes: usize, rate: u32) -> Self {
        Self { budget: budget_bytes, rate, by_hash: HashMap::new(), by_path: HashMap::new(), clock: 0 }
    }

    /// The decoded source of a file (decoding it if it is not cached).
    pub fn get(&mut self, path: &Path) -> Result<Arc<Source>, String> {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // A device, a pipe, or a folder would never end (or block the
        // daemon), and a huge file would fill memory: neither is read.
        if !meta.is_file() {
            return Err(format!("{}: not a file", path.display()));
        }
        if meta.len() > MAX_SOURCE_BYTES {
            return Err(format!("{}: larger than {} MB", path.display(), MAX_SOURCE_BYTES / 1024 / 1024));
        }
        let stamp = (meta.len(), meta.modified().ok());
        self.clock += 1;
        if let Some((len, time, hash)) = self.by_path.get(path)
            && (*len, *time) == stamp
            && let Some(entry) = self.by_hash.get_mut(hash)
        {
            entry.1 = self.clock;
            return Ok(entry.0.clone());
        }
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let hash = *blake3::hash(&bytes).as_bytes();
        self.by_path.insert(path.to_path_buf(), (stamp.0, stamp.1, hash));
        if let Some(entry) = self.by_hash.get_mut(&hash) {
            entry.1 = self.clock;
            return Ok(entry.0.clone());
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let source = Arc::new(decode(bytes, &name, self.rate).map_err(|e| format!("{}: {e}", path.display()))?);
        self.by_hash.insert(hash, (source.clone(), self.clock));
        self.evict(hash);
        Ok(source)
    }

    /// Drop the least recently used sources until under the budget (never
    /// `keep`, the one just used).
    fn evict(&mut self, keep: [u8; 32]) {
        while self.used() > self.budget {
            let Some(oldest) = self.by_hash.iter().filter(|(h, _)| **h != keep).min_by_key(|(_, e)| e.1).map(|(h, _)| *h) else {
                break;
            };
            self.by_hash.remove(&oldest);
        }
        // Paths of sources no longer cached are found again by hashing.
        let by_hash = &self.by_hash;
        self.by_path.retain(|_, (_, _, hash)| by_hash.contains_key(hash));
    }

    /// Forget a path (a file renamed or removed); its source stays cached
    /// by content.
    pub fn forget(&mut self, path: &Path) {
        self.by_path.remove(path);
    }

    /// Bytes the cached sources take.
    pub fn used(&self) -> usize {
        self.by_hash.values().map(|e| e.0.bytes()).sum()
    }

    /// How many sources are cached.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.by_hash.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/audio").join(name)
    }

    /// RMS of the left channel over `a..b` seconds.
    fn rms(s: &Source, a: f64, b: f64) -> f32 {
        let (x, y) = ((a * s.rate as f64) as usize, (b * s.rate as f64) as usize);
        let sum: f32 = (x..y).map(|f| s.data[f * 2] * s.data[f * 2]).sum();
        (sum / (y - x) as f32).sqrt()
    }

    /// MP3, FLAC, AAC, AIFF, ALAC, and Vorbis decode to the fixture's 2 s, resampled from
    /// 44.1 kHz to 48 kHz, with its tones where they are and silence
    /// between them.
    #[test]
    fn compressed_formats_decode_and_resample() {
        for name in ["song.flac", "song.mp3", "song.m4a", "song.aiff", "song-alac.m4a", "song.ogg"] {
            let mut cache = SourceCache::new(64 << 20, 48000);
            let s = cache.get(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(s.rate == 48000 && s.format.contains("44100 Hz, 2 ch"), "{name}: {}", s.format);
            assert!((s.seconds() - 2.0).abs() < 0.1, "{name}: {} s", s.seconds());
            // A 0.5-amplitude sine has an RMS near 0.35 (lossy codecs and
            // their delay blur the edges, so measure inside the tones).
            let (tone, gap, high) = (rms(&s, 0.15, 0.4), rms(&s, 0.65, 0.9), rms(&s, 1.6, 1.75));
            assert!(tone > 0.3 && high > 0.3 && gap < 0.02, "{name}: tone {tone} gap {gap} high {high}");
        }
    }

    /// One file decodes once, however many times (or under however many
    /// paths) it is asked for; over the budget the least recently used goes.
    #[test]
    fn cache_by_content_with_a_budget() {
        let dir = std::env::temp_dir().join(format!("4s-sources-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("same.flac");
        std::fs::copy(fixture("song.flac"), &copy).unwrap();
        let mut cache = SourceCache::new(64 << 20, 48000);
        let a = cache.get(&fixture("song.flac")).unwrap();
        let b = cache.get(&copy).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the same contents are one source");
        assert_eq!(cache.len(), 1);
        // A budget smaller than two sources keeps only the last one used.
        let mut small = SourceCache::new(a.bytes() + a.bytes() / 2, 48000);
        small.get(&fixture("song.flac")).unwrap();
        small.get(&fixture("song.mp3")).unwrap();
        assert_eq!(small.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Only regular files of a sane size are read.
    #[test]
    fn not_files_are_refused() {
        let mut cache = SourceCache::new(64 << 20, 48000);
        assert!(cache.get(Path::new("/dev/zero")).err().is_some_and(|e| e.contains("not a file")));
        assert!(cache.get(&std::env::temp_dir()).err().is_some_and(|e| e.contains("not a file")));
    }

    /// Regions are checked and copied; peaks follow the audio at any zoom.
    #[test]
    fn regions_and_peaks() {
        let mut cache = SourceCache::new(64 << 20, 48000);
        let s = cache.get(&fixture("song.flac")).unwrap();
        let (a, b) = s.frame_range(1.5, 1.8).unwrap();
        assert_eq!((a, b), (72000, 86400));
        assert_eq!(s.region(a, b).len(), (b - a) * 2);
        assert!(s.frame_range(1.8, 1.5).is_err());
        assert!(s.frame_range(1.0, 70.0).is_err());
        assert!(s.frame_range(5.0, 6.0).is_err());
        // The whole file in 8 bins (from the overview): the tones show.
        let p = s.peaks(0.0, 2.0, 8);
        assert!(p[0][1] > 0.45 && p[6][1] > 0.45, "{p:?}");
        // Spans away from the edges: the gap is quiet, the burst loud.
        assert!(s.peaks(0.6, 0.9, 1)[0][1] < 0.02);
        assert!(s.peaks(0.98, 1.06, 1)[0][1] > 0.7);
        // Zoomed in finer than an overview bin: from the audio itself.
        let z = s.peaks(1.0, 1.001, 4);
        assert!(z.len() == 4 && z.iter().any(|b| b[1] - b[0] > 0.1), "{z:?}");
    }
}
