//! Audio analysis (`sample.analyze`): what a span of a decoded file sounds
//! like, for finding a part of a song by description. Classic signal
//! processing, written here (aubio and Essentia, which do the same, are
//! GPL/AGPL):
//!
//! - A short-time spectrum (2048-point FFT, 512 hop, on a mono mix at
//!   about 22-24 kHz).
//! - Onsets: spectral flux (the rise of log magnitudes, bin by bin) less
//!   its local mean, peak-picked against an adaptive threshold.
//! - Tempo: the autocorrelation of that onset envelope over 60..200 BPM,
//!   weighted toward 120 BPM (a log-normal prior, one octave wide), then
//!   beats by dynamic programming (Ellis 2007: each beat is an onset peak
//!   about a period after the last), and the BPM from a line fit through
//!   the beats. Bar lines: the beat phase whose beats have the most
//!   low-band (kick) flux.
//! - Bands: energy below 200 Hz, 200 Hz-2 kHz, and above, as shares.
//! - Tonal: 1 less the normalized entropy of the chroma (12 pitch
//!   classes); key: the chroma's best correlation with the
//!   Krumhansl-Kessler major and minor profiles.
//! - Sections: Foote novelty (a checkerboard kernel along the diagonal of
//!   the rows' self-similarity) peak-picked, at least four rows apart;
//!   alike sections share a label.
//!
//! It runs off the core lock (the server spawns it) on the cached
//! source's `Arc<[f32]>`, cloned under the lock: keep it an owned `Arc`,
//! never a borrow taken under the lock.

use fours_protocol::{AnalysisRow, AnalysisSection, KeyEstimate, SampleAnalysis, Tempo};
use realfft::RealFftPlanner;

const FFT: usize = 2048;
const HOP: usize = 512;
const BINS: usize = FFT / 2 + 1;
const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;
/// Rows a section has at least (and half the novelty kernel's width).
const MIN_SECTION_ROWS: usize = 4;

/// What to analyze and how.
pub struct Request<'a> {
    pub path: String,
    /// Interleaved stereo.
    pub data: &'a [f32],
    pub rate: u32,
    /// The span, in seconds of the file (already inside it).
    pub start: f64,
    pub end: f64,
    pub beats_per_bar: u32,
    pub list_onsets: bool,
    pub list_beats: bool,
}

/// Per spectrum frame.
struct Frames {
    /// Frames a second.
    fps: f64,
    /// Onset strength (rectified flux less its local mean), >= 0.
    env: Vec<f32>,
    /// Low-band flux (where a kick shows), for bar lines.
    low_flux: Vec<f32>,
    /// Band energies.
    bands: Vec<[f32; 3]>,
    chroma: Vec<[f32; 12]>,
}

pub fn analyze(r: &Request) -> SampleAnalysis {
    let rate = r.rate.max(1) as f64;
    let total = r.data.len() / 2;
    let a = ((r.start * rate).round() as usize).min(total);
    let b = ((r.end * rate).round() as usize).clamp(a, total);
    let span = &r.data[a * 2..b * 2];

    // The span's level, at full rate.
    let (mut peak, mut sum) = (0f32, 0f64);
    for x in span {
        peak = peak.max(x.abs());
        sum += (*x as f64) * (*x as f64);
    }
    let rms = if span.is_empty() { 0.0 } else { (sum / span.len() as f64).sqrt() };

    // A mono mix, decimated to 22-24 kHz (an average: drums and bands do
    // not need more).
    let d = ((r.rate / 22050).max(1)) as usize;
    let sr = rate / d as f64;
    let frames_in = span.len() / 2;
    let mono: Vec<f32> = (0..frames_in / d)
        .map(|i| {
            let mut s = 0.0;
            for k in 0..d {
                let j = (i * d + k) * 2;
                s += span[j] + span[j + 1];
            }
            s / (2 * d) as f32
        })
        .collect();

    let f = frames(&mono, sr);
    let t0 = a as f64 / rate;
    let time = |frame: f64| round3(t0 + frame / f.fps);
    let onset_frames = pick_onsets(&f.env, f.fps);

    // Tempo, beats, and bars.
    let beats_per_bar = r.beats_per_bar.clamp(1, 16) as usize;
    let grid = tempo(&f.env, f.fps).map(|(period, confidence)| {
        let beats = track_beats(&f.env, period);
        (period, confidence, beats)
    });
    let grid = grid.filter(|(_, _, beats)| beats.len() >= 2 * beats_per_bar);
    let span_end = b as f64 / rate;
    let span_start = t0;
    let (tempo_out, row_bounds, beat_list, unit) = match &grid {
        Some((_, confidence, beats)) => {
            let bpm = fit_bpm(beats, f.fps);
            let phase = bar_phase(&f, beats, beats_per_bar);
            let mut bar_starts: Vec<f64> = beats.iter().skip(phase).step_by(beats_per_bar).map(|&i| t0 + i as f64 / f.fps).collect();
            let bar_len = 60.0 / bpm * beats_per_bar as f64;
            // The grid carried back to the span's start (beats the tracker
            // missed there, or a quiet intro), a whole bar at a time.
            while bar_starts[0] - bar_len >= span_start - 0.1 * bar_len / beats_per_bar as f64 {
                let s = (bar_starts[0] - bar_len).max(span_start);
                bar_starts.insert(0, s);
            }
            let mut bounds = Vec::new();
            // A pickup before the first bar line.
            if bar_starts[0] - span_start > 0.25 {
                bounds.push((span_start, bar_starts[0]));
            }
            for w in bar_starts.windows(2) {
                bounds.push((w[0], w[1]));
            }
            let last = *bar_starts.last().unwrap();
            let last_end = (last + bar_len).min(span_end);
            if last_end - last > 0.05 {
                bounds.push((last, last_end));
            }
            if span_end - last_end > 0.25 {
                bounds.push((last_end, span_end));
            }
            let tempo = Tempo {
                bpm: (bpm * 100.0).round() / 100.0,
                confidence: round2(*confidence),
                beats_per_bar: beats_per_bar as u32,
                first_downbeat: round3(bar_starts[0]),
                beat_count: beats.len() as u32,
            };
            let list: Vec<f64> = beats.iter().map(|&i| time(i as f64)).collect();
            (Some(tempo), bounds, list, "bar")
        }
        None => {
            let mut bounds = Vec::new();
            let mut s = span_start;
            while s < span_end - 1e-9 {
                let e = (s + 1.0).min(span_end);
                bounds.push((s, e));
                s = e;
            }
            (None, bounds, Vec::new(), "second")
        }
    };

    // Rows.
    let onset_times: Vec<f64> = onset_frames.iter().map(|&i| t0 + i as f64 / f.fps).collect();
    let env_scale = percentile(&f.env, 0.98).max(1e-9);
    let frame_of = |t: f64| (((t - t0) * f.fps).round().max(0.0) as usize).min(f.env.len());
    let sample_of = |t: f64| (((t - t0) * sr).round().max(0.0) as usize).min(mono.len());
    struct Acc {
        bands: [f64; 3],
        chroma: [f64; 12],
        punch: f64,
        frames: usize,
        sq: f64,
        samples: usize,
        onsets: u32,
    }
    let accumulate = |s: f64, e: f64| -> Acc {
        let (fa, fb) = (frame_of(s), frame_of(e).max(frame_of(s) + 1).min(f.env.len()));
        let mut acc = Acc { bands: [0.0; 3], chroma: [0.0; 12], punch: 0.0, frames: 0, sq: 0.0, samples: 0, onsets: 0 };
        for i in fa..fb {
            for k in 0..3 {
                acc.bands[k] += f.bands[i][k] as f64;
            }
            for k in 0..12 {
                acc.chroma[k] += f.chroma[i][k] as f64;
            }
            acc.punch += (f.env[i] / env_scale).min(1.0) as f64;
            acc.frames += 1;
        }
        for x in &mono[sample_of(s)..sample_of(e)] {
            acc.sq += (*x as f64) * (*x as f64);
            acc.samples += 1;
        }
        acc.onsets = onset_times.iter().filter(|&&t| t >= s && t < e).count() as u32;
        acc
    };
    let shares = |b: &[f64; 3]| {
        let t = b.iter().sum::<f64>();
        if t <= 1e-12 { [0.0; 3] } else { [round2((b[0] / t) as f32), round2((b[1] / t) as f32), round2((b[2] / t) as f32)] }
    };
    let accs: Vec<Acc> = row_bounds.iter().map(|&(s, e)| accumulate(s, e)).collect();
    let rows: Vec<AnalysisRow> = row_bounds
        .iter()
        .zip(&accs)
        .enumerate()
        .map(|(n, (&(s, e), acc))| {
            let [low, mid, high] = shares(&acc.bands);
            AnalysisRow {
                n: n as u32 + 1,
                start: round3(s),
                end: round3(e),
                rms_db: db(if acc.samples > 0 { (acc.sq / acc.samples as f64).sqrt() } else { 0.0 }),
                low,
                mid,
                high,
                onsets: acc.onsets,
                punch: round2((acc.punch / acc.frames.max(1) as f64) as f32),
                tonal: round2(tonality(&acc.chroma)),
            }
        })
        .collect();

    // Sections, from the rows' features.
    let bounds = section_bounds(&rows, &accs.iter().map(|a| a.chroma).collect::<Vec<_>>());
    let mut sections: Vec<AnalysisSection> = Vec::new();
    let mut protos: Vec<(String, Vec<f32>)> = Vec::new();
    let feats = features(&rows, &accs.iter().map(|a| a.chroma).collect::<Vec<_>>());
    let typical = typical_distance(&feats);
    for w in bounds.windows(2) {
        let (i, j) = (w[0], w[1]);
        let mut bands = [0.0; 3];
        let mut chroma = [0.0; 12];
        let (mut sq, mut samples, mut punch, mut frames, mut onsets) = (0.0, 0usize, 0.0, 0usize, 0u32);
        for acc in &accs[i..j] {
            for k in 0..3 {
                bands[k] += acc.bands[k];
            }
            for k in 0..12 {
                chroma[k] += acc.chroma[k];
            }
            sq += acc.sq;
            samples += acc.samples;
            punch += acc.punch;
            frames += acc.frames;
            onsets += acc.onsets;
        }
        let [low, mid, high] = shares(&bands);
        let (s, e) = (rows[i].start, rows[j - 1].end);
        // Its mean feature vector, for labels.
        let dim = feats.first().map_or(0, Vec::len);
        let mean: Vec<f32> = (0..dim).map(|k| feats[i..j].iter().map(|v| v[k]).sum::<f32>() / (j - i) as f32).collect();
        let label = match protos.iter().map(|(l, p)| (l, dist(p, &mean))).min_by(|x, y| x.1.total_cmp(&y.1)) {
            Some((l, d)) if d < 0.5 * typical => l.clone(),
            _ => {
                let l = label_name(protos.len());
                protos.push((l.clone(), mean));
                l
            }
        };
        sections.push(AnalysisSection {
            label,
            first: i as u32 + 1,
            last: j as u32,
            start: s,
            end: e,
            rms_db: db(if samples > 0 { (sq / samples as f64).sqrt() } else { 0.0 }),
            low,
            mid,
            high,
            onset_rate: round2((onsets as f64 / (e - s).max(1e-6)) as f32),
            punch: round2((punch / frames.max(1) as f64) as f32),
            tonal: round2(tonality(&chroma)),
        });
    }

    // The key, from all the span's chroma.
    let mut chroma = [0.0; 12];
    for c in &f.chroma {
        for k in 0..12 {
            chroma[k] += c[k] as f64;
        }
    }

    SampleAnalysis {
        path: r.path.clone(),
        start: round3(span_start),
        end: round3(span_end),
        peak_db: db(peak as f64),
        rms_db: db(rms),
        tempo: tempo_out,
        key: key(&chroma),
        row_unit: unit.into(),
        rows,
        sections,
        onset_count: onset_frames.len() as u32,
        onsets: r.list_onsets.then(|| onset_times.iter().map(|&t| round3(t)).collect()),
        beats: r.list_beats.then_some(beat_list),
    }
}

/// The spectrum's frames: onset envelope, low flux, bands, chroma.
fn frames(mono: &[f32], sr: f64) -> Frames {
    let n = if mono.is_empty() { 0 } else { mono.len() / HOP + 1 };
    let fps = sr / HOP as f64;
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let window: Vec<f32> = (0..FFT).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FFT as f32).cos()).collect();
    let hz = |bin: usize| bin as f64 * sr / FFT as f64;
    // Each bin's band.
    let band: Vec<usize> = (0..BINS).map(|k| if hz(k) < 200.0 { 0 } else if hz(k) < 2000.0 { 1 } else { 2 }).collect();
    // Chroma from spectral peaks between these bins (a peak's frequency
    // interpolated, so a low note does not smear over its neighbors).
    let (chroma_lo, chroma_hi) = ((80.0 * FFT as f64 / sr).ceil() as usize, ((5000.0 * FFT as f64 / sr) as usize).min(BINS - 2));
    let low_top = (0..BINS).take_while(|&k| hz(k) < 150.0).count();
    let mut prev = vec![0f32; BINS];
    let mut log = vec![0f32; BINS];
    let mut mags = vec![0f32; BINS];
    let mut flux = Vec::with_capacity(n);
    let mut low_flux = Vec::with_capacity(n);
    let mut bands = Vec::with_capacity(n);
    let mut chroma = Vec::with_capacity(n);
    let norm = 2.0 / FFT as f32;
    for i in 0..n {
        // Centered frames: frame i is about time i * HOP.
        let c = (i * HOP) as isize - (FFT / 2) as isize;
        for (j, x) in input.iter_mut().enumerate() {
            let s = c + j as isize;
            *x = if s >= 0 && (s as usize) < mono.len() { mono[s as usize] * window[j] } else { 0.0 };
        }
        fft.process(&mut input, &mut spectrum).expect("fft sizes");
        let (mut fl, mut lf) = (0f32, 0f32);
        let mut b = [0f32; 3];
        let mut ch = [0f32; 12];
        for k in 0..BINS {
            mags[k] = spectrum[k].norm() * norm;
        }
        for k in chroma_lo..=chroma_hi {
            let (l, m, r) = (mags[k - 1], mags[k], mags[k + 1]);
            if m > l && m >= r && m > 1e-5 {
                let (la, lb, lc) = ((l + 1e-12).ln(), (m + 1e-12).ln(), (r + 1e-12).ln());
                let den = la - 2.0 * lb + lc;
                let shift = if den.abs() > 1e-9 { (0.5 * (la - lc) / den).clamp(-0.5, 0.5) } else { 0.0 };
                let f = (k as f64 + shift as f64) * sr / FFT as f64;
                let pc = ((12.0 * (f / 440.0).log2()).round() as i64 + 9).rem_euclid(12) as usize;
                ch[pc] += m * m;
            }
        }
        for k in 0..BINS {
            let m = mags[k];
            b[band[k]] += m * m;
            log[k] = (1.0 + 100.0 * m).ln();
            let rise = (log[k] - prev[k]).max(0.0);
            fl += rise;
            if k < low_top {
                lf += rise;
            }
        }
        std::mem::swap(&mut prev, &mut log);
        flux.push(fl);
        low_flux.push(lf);
        bands.push(b);
        chroma.push(ch);
    }
    // The onset envelope: flux less its mean over about +-0.15 s.
    let w = ((0.15 * fps).round() as usize).max(1);
    let env = less_local_mean(&flux, w);
    let low_flux = less_local_mean(&low_flux, w);
    Frames { fps, env, low_flux, bands, chroma }
}

fn less_local_mean(x: &[f32], w: usize) -> Vec<f32> {
    let mut prefix = vec![0f64; x.len() + 1];
    for (i, v) in x.iter().enumerate() {
        prefix[i + 1] = prefix[i] + *v as f64;
    }
    (0..x.len())
        .map(|i| {
            let (a, b) = (i.saturating_sub(w), (i + w + 1).min(x.len()));
            let mean = (prefix[b] - prefix[a]) / (b - a) as f64;
            (x[i] as f64 - mean).max(0.0) as f32
        })
        .collect()
}

/// Onset frames: local maxima of the envelope (+-3 frames) above an
/// adaptive threshold, at least 50 ms apart.
fn pick_onsets(env: &[f32], fps: f64) -> Vec<usize> {
    let top = env.iter().cloned().fold(0f32, f32::max);
    if top <= 1e-9 {
        return Vec::new();
    }
    let w = (fps * 0.5) as usize;
    let gap = ((0.05 * fps).ceil() as usize).max(1);
    let mut prefix = vec![0f64; env.len() + 1];
    for (i, v) in env.iter().enumerate() {
        prefix[i + 1] = prefix[i] + *v as f64;
    }
    let mut out: Vec<usize> = Vec::new();
    for i in 0..env.len() {
        let v = env[i];
        let (a, b) = (i.saturating_sub(3), (i + 4).min(env.len()));
        if env[a..b].iter().any(|&x| x > v) || v < 0.05 * top {
            continue;
        }
        let (la, lb) = (i.saturating_sub(w), (i + w + 1).min(env.len()));
        let mean = ((prefix[lb] - prefix[la]) / (lb - la) as f64) as f32;
        if v <= 1.5 * mean + 0.02 * top {
            continue;
        }
        if out.last().is_some_and(|&p| i - p < gap) {
            continue;
        }
        out.push(i);
    }
    out
}

/// The beat period in frames and a 0..1 confidence: the onset envelope's
/// autocorrelation peak over 60..200 BPM, weighted toward 120 BPM.
fn tempo(env: &[f32], fps: f64) -> Option<(f64, f32)> {
    let lo = (60.0 * fps / MAX_BPM).floor().max(1.0) as usize;
    let hi = (60.0 * fps / MIN_BPM).ceil() as usize;
    if env.len() < 2 * hi + 2 {
        return None;
    }
    let n = env.len();
    let acf = |lag: usize| -> f64 { (0..n - lag).map(|i| env[i] as f64 * env[i + lag] as f64).sum::<f64>() / (n - lag) as f64 };
    let zero = acf(0);
    if zero <= 1e-12 {
        return None;
    }
    let ac: Vec<f64> = (0..=hi + 1).map(|l| if l < lo.saturating_sub(1) { 0.0 } else { acf(l) }).collect();
    let weight = |lag: f64| {
        let bpm = 60.0 * fps / lag;
        (-0.5 * ((bpm / 120.0).log2() / 1.0).powi(2)).exp()
    };
    let best = (lo..=hi).max_by(|&a, &b| (ac[a] * weight(a as f64)).total_cmp(&(ac[b] * weight(b as f64))))?;
    // A fractional lag from the parabola through the peak.
    let (y0, y1, y2) = (ac[best - 1], ac[best], ac[best + 1]);
    let den = y0 - 2.0 * y1 + y2;
    let shift = if den.abs() > 1e-12 { (0.5 * (y0 - y2) / den).clamp(-0.5, 0.5) } else { 0.0 };
    let mean = (lo..=hi).map(|l| ac[l]).sum::<f64>() / (hi - lo + 1) as f64;
    let confidence = ((y1 - mean) / (zero - mean).max(1e-12)).clamp(0.0, 1.0) as f32;
    (confidence > 0.02).then_some((best as f64 + shift, confidence))
}

/// Beat frames by dynamic programming: each beat an envelope peak about a
/// period after the one before.
fn track_beats(env: &[f32], period: f64) -> Vec<usize> {
    let n = env.len();
    if n == 0 || period < 1.0 {
        return Vec::new();
    }
    let scale = (env.iter().map(|&x| x as f64 * x as f64).sum::<f64>() / n as f64).sqrt().max(1e-9);
    let local: Vec<f64> = env.iter().map(|&x| x as f64 / scale).collect();
    let tight = 100.0;
    let (min_back, max_back) = ((period / 2.0).round().max(1.0) as usize, (2.0 * period).round() as usize);
    let mut score = vec![0f64; n];
    let mut back = vec![usize::MAX; n];
    for i in 0..n {
        let mut best = f64::NEG_INFINITY;
        let mut from = usize::MAX;
        for j in i.saturating_sub(max_back)..i.saturating_sub(min_back - 1).min(i) {
            let gap = (i - j) as f64;
            let s = score[j] - tight * (gap / period).ln().powi(2);
            if s > best {
                best = s;
                from = j;
            }
        }
        score[i] = local[i] + if from == usize::MAX { 0.0 } else { best.max(0.0) };
        back[i] = if best > 0.0 { from } else { usize::MAX };
    }
    // The last beat: the best score in the last period.
    let tail = n.saturating_sub(period.round() as usize + 1);
    let mut i = (tail..n).max_by(|&a, &b| score[a].total_cmp(&score[b])).unwrap_or(n - 1);
    let mut beats = vec![i];
    while back[i] != usize::MAX {
        i = back[i];
        beats.push(i);
    }
    beats.reverse();
    // Drop beats in silence at either end.
    let strong = |b: usize| near_max(env, b, 2) > 0.0;
    while beats.len() > 1 && !strong(beats[0]) {
        beats.remove(0);
    }
    while beats.len() > 1 && !strong(*beats.last().unwrap()) {
        beats.pop();
    }
    beats
}

/// Which beat (0..beats_per_bar) bars start on: the phase whose beats
/// have the most low-band flux (where a kick lands; against its mean over
/// all beats) plus the most change in band shares between the bar's worth
/// of beats before and after (parts change on bar lines: a break, a
/// verse). Ties go to the earlier phase.
fn bar_phase(f: &Frames, beats: &[usize], beats_per_bar: usize) -> usize {
    let n = f.bands.len();
    // Band shares over frames a..b.
    let shares = |a: usize, b: usize| {
        let mut v = [0f32; 3];
        for i in a.min(n)..b.min(n) {
            for j in 0..3 {
                v[j] += f.bands[i][j];
            }
        }
        let t = v.iter().sum::<f32>().max(1e-12);
        v.map(|x| x / t)
    };
    let change: Vec<f32> = (0..beats.len())
        .map(|k| {
            if k < beats_per_bar || k + beats_per_bar >= beats.len() {
                return 0.0;
            }
            let (a, m, b) = (beats[k - beats_per_bar], beats[k], beats[k + beats_per_bar]);
            // Squared: one bar line where a part changes outweighs the two
            // beats either side whose windows half overlap it.
            dist(&shares(a, m), &shares(m, b)).powi(2)
        })
        .collect();
    let low: Vec<f32> = beats.iter().map(|&i| near_max(&f.low_flux, i, 2)).collect();
    let mean = |x: &[f32]| x.iter().sum::<f32>() / x.len().max(1) as f32;
    let low_mean = mean(&low).max(1e-9);
    let score = |k: usize| {
        let pick = |x: &[f32]| mean(&x.iter().skip(k).step_by(beats_per_bar).cloned().collect::<Vec<_>>());
        pick(&low) / low_mean + 8.0 * pick(&change)
    };
    let mut best = 0;
    for k in 1..beats_per_bar {
        if score(k) > score(best) + 1e-6 {
            best = k;
        }
    }
    best
}

/// BPM from a least-squares line through the beats.
fn fit_bpm(beats: &[usize], fps: f64) -> f64 {
    let n = beats.len() as f64;
    let mx = (n - 1.0) / 2.0;
    let my = beats.iter().map(|&b| b as f64).sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (k, &b) in beats.iter().enumerate() {
        num += (k as f64 - mx) * (b as f64 - my);
        den += (k as f64 - mx).powi(2);
    }
    let frames_per_beat = num / den.max(1e-12);
    60.0 * fps / frames_per_beat.max(1e-9)
}

fn near_max(x: &[f32], i: usize, w: usize) -> f32 {
    let (a, b) = (i.saturating_sub(w), (i + w + 1).min(x.len()));
    x[a..b].iter().cloned().fold(0.0, f32::max)
}

fn percentile(x: &[f32], q: f64) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    let mut v = x.to_vec();
    v.sort_by(f32::total_cmp);
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// 1 less the chroma's normalized entropy: 1 for one pitch class, 0 for
/// all twelve alike.
fn tonality(chroma: &[f64; 12]) -> f32 {
    let t: f64 = chroma.iter().sum();
    if t <= 1e-12 {
        return 0.0;
    }
    let h: f64 = chroma.iter().map(|&c| c / t).filter(|&p| p > 0.0).map(|p| -p * p.ln()).sum();
    (1.0 - h / 12f64.ln()).clamp(0.0, 1.0) as f32
}

/// The key whose Krumhansl-Kessler profile the chroma fits best.
fn key(chroma: &[f64; 12]) -> Option<KeyEstimate> {
    const MAJOR: [f64; 12] = [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
    const MINOR: [f64; 12] = [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    if chroma.iter().sum::<f64>() <= 1e-9 {
        return None;
    }
    // Magnitudes, not energies, so one loud bass note does not decide it.
    let c: Vec<f64> = chroma.iter().map(|x| x.sqrt()).collect();
    let corr = |profile: &[f64; 12], root: usize| {
        let p: Vec<f64> = (0..12).map(|k| profile[(k + 12 - root) % 12]).collect();
        pearson(&c, &p)
    };
    let mut best = (f64::NEG_INFINITY, 0, false);
    for root in 0..12 {
        for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
            let r = corr(profile, root);
            if r > best.0 {
                best = (r, root, minor);
            }
        }
    }
    let (r, root, minor) = best;
    Some(KeyEstimate { name: format!("{} {}", NAMES[root], if minor { "minor" } else { "major" }), confidence: round2(r.max(0.0) as f32) })
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        num += (x - ma) * (y - mb);
        da += (x - ma).powi(2);
        db += (y - mb).powi(2);
    }
    num / (da * db).sqrt().max(1e-12)
}

/// Each row's features, standardized across the rows: level, bands,
/// onsets, punch, tonal, and the chroma (at half weight).
fn features(rows: &[AnalysisRow], chroma: &[[f64; 12]]) -> Vec<Vec<f32>> {
    let raw: Vec<Vec<f32>> = rows
        .iter()
        .zip(chroma)
        .map(|(r, c)| {
            let len = (r.end - r.start).max(1e-6) as f32;
            let t: f64 = c.iter().sum::<f64>().max(1e-12);
            let mut v = [r.rms_db.max(-60.0), r.low, r.mid, r.high, r.onsets as f64 / len as f64, r.punch, r.tonal]
                .map(|x| x as f32)
                .to_vec();
            v.extend(c.iter().map(|x| (x / t) as f32));
            v
        })
        .collect();
    let dim = raw.first().map_or(0, Vec::len);
    let n = raw.len().max(1) as f32;
    let mut out = raw.clone();
    for k in 0..dim {
        let mean = raw.iter().map(|v| v[k]).sum::<f32>() / n;
        let sd = (raw.iter().map(|v| (v[k] - mean).powi(2)).sum::<f32>() / n).sqrt();
        let w = if k >= 7 { 0.5 } else { 1.0 };
        for (o, v) in out.iter_mut().zip(&raw) {
            o[k] = if sd > 1e-6 { w * (v[k] - mean) / sd } else { 0.0 };
        }
    }
    out
}

fn dist(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f32>().sqrt()
}

/// The mean distance between rows (what "alike" is measured against).
fn typical_distance(f: &[Vec<f32>]) -> f32 {
    let n = f.len();
    if n < 2 {
        return 1.0;
    }
    let step = (n / 64).max(1);
    let (mut sum, mut count) = (0.0, 0);
    for i in (0..n).step_by(step) {
        for j in (i + 1..n).step_by(step) {
            sum += dist(&f[i], &f[j]);
            count += 1;
        }
    }
    if count == 0 { 1.0 } else { (sum / count as f32).max(1e-6) }
}

/// Section boundaries as row indices (0 and the row count included).
fn section_bounds(rows: &[AnalysisRow], chroma: &[[f64; 12]]) -> Vec<usize> {
    let n = rows.len();
    if n == 0 {
        return vec![0];
    }
    let f = features(rows, chroma);
    let w = MIN_SECTION_ROWS;
    if n < 2 * w {
        return vec![0, n];
    }
    // Similarity: e^(-d / typical).
    let typical = typical_distance(&f);
    let sim = |i: usize, j: usize| (-dist(&f[i], &f[j]) / typical).exp();
    // Foote novelty at each boundary b (between rows b-1 and b): the
    // checkerboard kernel, Gaussian-tapered, over w rows each side.
    let mut nov = vec![0f32; n + 1];
    for b in 1..n {
        let mut s = 0.0;
        for di in 0..w {
            for dj in 0..w {
                let taper = (-((di * di + dj * dj) as f32) / (w * w) as f32).exp();
                let (bi, aj) = (b as isize - 1 - di as isize, b + dj);
                let (bj, ai) = (b as isize - 1 - dj as isize, b + di);
                if bi < 0 || bj < 0 || aj >= n || ai >= n {
                    continue;
                }
                let (bi, bj) = (bi as usize, bj as usize);
                // Same side: + ; across: -.
                s += taper * (sim(bi, bj) + sim(ai, aj) - sim(bi, aj) - sim(ai, bj));
            }
        }
        nov[b] = s.max(0.0);
    }
    let vals: Vec<f32> = nov[1..n].to_vec();
    let mean = vals.iter().sum::<f32>() / vals.len() as f32;
    let sd = (vals.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / vals.len() as f32).sqrt();
    // Peaks (the most within w/2 rows), strongest first, kept if at
    // least w rows from each kept one
    // and from both ends.
    let mut cands: Vec<usize> = (w..=n - w)
        .filter(|&b| {
            let (a, c) = (b.saturating_sub(w / 2).max(1), (b + w / 2).min(n - 1));
            nov[b] > mean + 0.5 * sd && (a..=c).all(|k| nov[k] <= nov[b])
        })
        .collect();
    cands.sort_by(|&a, &b| nov[b].total_cmp(&nov[a]));
    let mut kept: Vec<usize> = Vec::new();
    for c in cands {
        if kept.iter().all(|&k| c.abs_diff(k) >= w) {
            kept.push(c);
        }
    }
    kept.push(0);
    kept.push(n);
    kept.sort();
    kept
}

fn label_name(i: usize) -> String {
    let letters = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if i < 26 { (letters[i] as char).to_string() } else { format!("Z{}", i - 25) }
}

fn db(x: f64) -> f64 {
    if x <= 1e-6 { -120.0 } else { ((20.0 * x.log10()) * 10.0).round() / 10.0 }
}

/// Two decimals, as an f64 (so it prints as such in JSON).
fn round2(x: f32) -> f64 {
    (x as f64 * 100.0).round() / 100.0
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 48000;

    /// Interleaved stereo of a mono signal.
    fn stereo(mono: &[f32]) -> Vec<f32> {
        mono.iter().flat_map(|&x| [x, x]).collect()
    }

    fn kick(out: &mut [f32], at: usize, gain: f32) {
        for i in 0..(SR as usize / 8) {
            if at + i >= out.len() {
                break;
            }
            let t = i as f32 / SR as f32;
            let f = 50.0 + 100.0 * (-t * 30.0).exp();
            out[at + i] += gain * (2.0 * std::f32::consts::PI * f * t).sin() * (-t * 20.0).exp();
        }
    }

    fn run(mono: &[f32]) -> SampleAnalysis {
        let data = stereo(mono);
        analyze(&Request {
            path: "t".into(),
            data: &data,
            rate: SR,
            start: 0.0,
            end: mono.len() as f64 / SR as f64,
            beats_per_bar: 4,
            list_onsets: true,
            list_beats: true,
        })
    }

    #[test]
    fn finds_tempo_beats_and_bar_lines() {
        for bpm in [84.0, 100.0, 126.0, 140.0, 174.0] {
            let beat = 60.0 / bpm;
            let n = (SR as f64 * beat * 32.0) as usize;
            let mut x = vec![0f32; n];
            // A loud kick on each bar's first beat, softer ones between.
            for k in 0..32 {
                kick(&mut x, (k as f64 * beat * SR as f64) as usize, if k % 4 == 0 { 0.9 } else { 0.4 });
            }
            let a = run(&x);
            let t = a.tempo.expect("a tempo");
            assert!((t.bpm - bpm).abs() < 1.0, "{bpm}: {}", t.bpm);
            assert!(t.first_downbeat < 0.05, "{bpm}: downbeat {}", t.first_downbeat);
            let beats = a.beats.unwrap();
            assert!(beats.len() >= 30, "{bpm}: {} beats", beats.len());
            for (k, b) in beats.iter().enumerate() {
                assert!((b - k as f64 * beat).abs() < 0.03, "{bpm}: beat {k} at {b}");
            }
            assert_eq!(a.row_unit, "bar");
            assert!(a.onset_count >= 30 && a.onset_count <= 34, "{bpm}: {} onsets", a.onset_count);
        }
    }

    #[test]
    fn finds_key_and_tonal_vs_noise_sections() {
        // 8 s of an A minor chord, then 8 s of noise bursts.
        let n = SR as usize * 16;
        let mut x = vec![0f32; n];
        let mut seed = 12345u32;
        for i in 0..n {
            let t = i as f32 / SR as f32;
            if t < 8.0 {
                for f in [220.0, 261.63, 329.63, 110.0] {
                    x[i] += 0.15 * (2.0 * std::f32::consts::PI * f * t).sin();
                }
            } else if (t * 4.0).fract() < 0.1 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                x[i] = 0.4 * ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5);
            }
        }
        let a = run(&x);
        assert_eq!(a.key.as_ref().map(|k| k.name.as_str()), Some("A minor"), "{:?}", a.key);
        assert!(a.sections.len() >= 2, "{:?}", a.sections);
        let first = &a.sections[0];
        let last = a.sections.last().unwrap();
        assert!((first.end - 8.0).abs() < 1.5, "boundary at {}", first.end);
        assert!(first.tonal > last.tonal + 0.2, "{} vs {}", first.tonal, last.tonal);
        assert!(last.punch > first.punch, "{} vs {}", last.punch, first.punch);
        assert!(last.high > first.high, "{} vs {}", last.high, first.high);
    }

    #[test]
    fn silence_has_no_tempo_and_rows_by_the_second() {
        let a = run(&vec![0f32; SR as usize * 3]);
        assert!(a.tempo.is_none());
        assert_eq!(a.row_unit, "second");
        assert_eq!(a.rows.len(), 3);
        assert_eq!(a.onset_count, 0);
        assert!(a.key.is_none());
    }
}
