//! `poly`: an eight-voice analog-style polyphonic synth.
//!
//! Each voice: two oscillators (saw, square, triangle, or sine; band-limited
//! with PolyBLEP where they jump) plus noise, a resonant state-variable
//! low-pass per side, and two ADSR envelopes, one for the filter and one for
//! the amp. Unison: each oscillator is a pair, detuned apart by `detune` and
//! spread left and right by `width`, so one note is already wide. Velocity
//! scales loudness and the filter envelope by `velocity`. One LFO, shared by
//! all voices, adds vibrato and moves the cutoff.
//!
//! Voices: a note already sounding is retriggered in its own voice; a new one
//! takes a free voice, else the oldest released one, else the oldest. A
//! retriggered or stolen voice's envelopes start from where they are, so
//! nothing clicks.

use crate::dsp::{Noise, Svf, TAU, lerp_exp, midi_hz, poly_blep, semitones_to_ratio, soft_clip};
use crate::instrument::{Hit, Instrument, MAX_BLOCK};
use crate::params::cont;
use fours_protocol::{KnobPage, OutputWidth, ParamInfo, ParamKind};

/// Voices sounding at once.
pub const VOICES: usize = 8;

pub const OSC1_WAVE: usize = 0;
pub const OSC2_WAVE: usize = 1;
pub const OSC2_TUNE: usize = 2;
pub const OSC2_FINE: usize = 3;
pub const OSC2_LEVEL: usize = 4;
pub const NOISE: usize = 5;
pub const CUTOFF: usize = 6;
pub const RESONANCE: usize = 7;
pub const ENV_AMOUNT: usize = 8;
pub const F_ATTACK: usize = 9;
pub const F_DECAY: usize = 10;
pub const F_SUSTAIN: usize = 11;
pub const F_RELEASE: usize = 12;
pub const ATTACK: usize = 13;
pub const DECAY: usize = 14;
pub const SUSTAIN: usize = 15;
pub const RELEASE: usize = 16;
pub const DETUNE: usize = 17;
pub const WIDTH: usize = 18;
pub const VELOCITY: usize = 19;
pub const LFO_RATE: usize = 20;
pub const VIBRATO: usize = 21;
pub const LFO_CUTOFF: usize = 22;
pub const LEVEL: usize = 23;
pub const NUM_PARAMS: usize = 24;

/// Frames between updates of pitches and filter coefficients.
const CONTROL: usize = 16;
/// Widest unison detune, cents apart from the note on each side.
const MAX_DETUNE_CENTS: f32 = 25.0;
/// Most a full filter envelope opens the cutoff, in octaves.
const ENV_OCTAVES: f32 = 6.0;

/// Envelope stage times: 0..1 maps to 1 ms..10 s, exponentially.
pub fn env_secs(x: f32) -> f32 {
    lerp_exp(0.001, 10.0, x)
}

/// LFO rate: 0..1 maps to 0.1..20 Hz, exponentially.
pub fn lfo_hz(x: f32) -> f32 {
    lerp_exp(0.1, 20.0, x)
}

pub fn params(id: &str) -> Vec<ParamInfo> {
    let p = |name: &str, label: &str, min: f64, max: f64, default: f64, unit: Option<&str>| cont(format!("{id}.{name}"), label.into(), min, max, default, unit);
    let wave = |name: &str, label: &str| ParamInfo { path: format!("{id}.{name}"), label: label.into(), kind: ParamKind::Integer { min: 0, max: 3 }, default: 0.0, unit: None };
    vec![
        wave("osc1_wave", "Osc 1 Wave"),
        wave("osc2_wave", "Osc 2 Wave"),
        p("osc2_tune", "Osc 2 Tune", -24.0, 24.0, 0.0, Some("st")),
        p("osc2_fine", "Osc 2 Fine", -50.0, 50.0, 7.0, Some("ct")),
        p("osc2_level", "Osc 2 Level", 0.0, 1.0, 0.5, None),
        p("noise", "Noise", 0.0, 1.0, 0.0, None),
        p("cutoff", "Cutoff", 0.0, 1.0, 0.5, None),
        p("resonance", "Resonance", 0.0, 1.0, 0.2, None),
        p("env_amount", "Filter Env", 0.0, 1.0, 0.4, None),
        p("f_attack", "Filter Attack", 0.0, 1.0, 0.0, None),
        p("f_decay", "Filter Decay", 0.0, 1.0, 0.6, None),
        p("f_sustain", "Filter Sustain", 0.0, 1.0, 0.3, None),
        p("f_release", "Filter Release", 0.0, 1.0, 0.55, None),
        p("attack", "Attack", 0.0, 1.0, 0.1, None),
        p("decay", "Decay", 0.0, 1.0, 0.55, None),
        p("sustain", "Sustain", 0.0, 1.0, 0.7, None),
        p("release", "Release", 0.0, 1.0, 0.55, None),
        p("detune", "Detune", 0.0, 1.0, 0.2, None),
        p("width", "Width", 0.0, 1.0, 0.5, None),
        p("velocity", "Velocity", 0.0, 1.0, 0.6, None),
        p("lfo_rate", "LFO Rate", 0.0, 1.0, 0.7, None),
        p("vibrato", "Vibrato", 0.0, 1.0, 0.0, None),
        p("lfo_cutoff", "LFO > Cutoff", 0.0, 1.0, 0.0, None),
        p("level", "Level", 0.0, 1.0, 0.7, None),
    ]
}

/// Knob pages of eight (RFC 0007): oscillators, filter, amp, and the rest.
pub fn knob_pages(id: &str) -> Vec<KnobPage> {
    let page = |pid: &str, label: &str, names: [&str; 8]| KnobPage {
        id: pid.into(),
        label: label.into(),
        params: names.iter().map(|n| format!("{id}.{n}")).collect(),
    };
    vec![
        page("osc", "Oscillators", ["osc1_wave", "osc2_wave", "osc2_tune", "osc2_fine", "osc2_level", "noise", "detune", "width"]),
        page("filter", "Filter", ["cutoff", "resonance", "env_amount", "f_attack", "f_decay", "f_sustain", "f_release", "lfo_cutoff"]),
        page("amp", "Amp", ["attack", "decay", "sustain", "release", "velocity", "level", "lfo_rate", "vibrato"]),
    ]
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
enum Stage {
    #[default]
    Idle,
    Attack,
    Decay,
    Release,
}

/// A linear-attack, exponential decay and release envelope that restarts
/// from where it is.
#[derive(Clone, Copy, Default)]
struct Adsr {
    stage: Stage,
    value: f32,
}

/// An envelope's settings at the engine's rate.
#[derive(Clone, Copy, Default)]
struct AdsrRates {
    attack_step: f32,
    decay_coef: f32,
    sustain: f32,
    release_coef: f32,
}

impl AdsrRates {
    fn new(sr: f32, a: f32, d: f32, s: f32, r: f32) -> Self {
        // Decay and release fall by 60 dB over their times.
        let coef = |x: f32| (0.001f32.ln() / (env_secs(x) * sr)).exp();
        Self { attack_step: 1.0 / (env_secs(a) * sr), decay_coef: coef(d), sustain: s.clamp(0.0, 1.0), release_coef: coef(r) }
    }
}

impl Adsr {
    #[inline]
    fn next(&mut self, r: &AdsrRates) -> f32 {
        match self.stage {
            Stage::Idle => self.value = 0.0,
            Stage::Attack => {
                self.value += r.attack_step;
                if self.value >= 1.0 {
                    self.value = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => self.value = r.sustain + (self.value - r.sustain) * r.decay_coef,
            Stage::Release => {
                self.value *= r.release_coef;
                if self.value < 1e-4 {
                    self.value = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.value
    }
}

#[derive(Clone, Default)]
struct Voice {
    note: u8,
    velocity: f32,
    /// When it started (a counter), for stealing the oldest.
    age: u64,
    /// Held: not yet released by a note-off or its gate.
    held: bool,
    /// Samples until a timed note releases.
    gate_left: Option<f64>,
    amp: Adsr,
    fenv: Adsr,
    /// Phases (cycles) of oscillator 1 and 2, each a unison pair.
    phase: [[f32; 2]; 2],
    /// Phase increments for this control period.
    dt: [[f32; 2]; 2],
    filter: [Svf; 2],
}

impl Voice {
    fn active(&self) -> bool {
        self.amp.stage != Stage::Idle
    }
}

/// One oscillator sample at `phase` (cycles), stepping `dt` a sample.
#[inline]
fn osc(wave: u8, phase: f32, dt: f32) -> f32 {
    match wave {
        1 => {
            let mut x = if phase < 0.5 { 1.0 } else { -1.0 };
            x += poly_blep(phase, dt);
            x -= poly_blep((phase + 0.5).fract(), dt);
            x
        }
        2 => 4.0 * (phase - 0.5).abs() - 1.0,
        3 => (TAU * phase).sin(),
        _ => 2.0 * phase - 1.0 - poly_blep(phase, dt),
    }
}

pub struct Poly {
    sr: f32,
    params: [f32; NUM_PARAMS],
    voices: [Voice; VOICES],
    started: u64,
    noise: Noise,
    lfo_phase: f32,
    bend_target: f32,
    bend: f32,
    out: Vec<f32>,
}

impl Poly {
    pub fn new(sr: f32) -> Self {
        let mut s = Self {
            sr,
            params: [0.0; NUM_PARAMS],
            voices: Default::default(),
            started: 0,
            noise: Noise::new(0x5eed),
            lfo_phase: 0.0,
            bend_target: 0.0,
            bend: 0.0,
            out: vec![0.0; MAX_BLOCK * 2],
        };
        for (i, p) in params("x").iter().enumerate() {
            s.set_param(i, p.default as f32);
        }
        s
    }

    /// The voice a new `note` takes.
    fn pick(&self, note: u8) -> usize {
        let v = &self.voices;
        let oldest = |f: &dyn Fn(&Voice) -> bool| (0..VOICES).filter(|i| f(&v[*i])).min_by_key(|i| v[*i].age);
        (0..VOICES)
            .find(|i| v[*i].active() && v[*i].note == note)
            .or_else(|| (0..VOICES).find(|i| !v[*i].active()))
            .or_else(|| oldest(&|x| !x.held))
            .or_else(|| oldest(&|_| true))
            .unwrap_or(0)
    }
}

impl Instrument for Poly {
    fn num_outputs(&self) -> usize {
        1
    }

    fn output_width(&self, _output: usize) -> OutputWidth {
        OutputWidth::Stereo
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index < NUM_PARAMS {
            self.params[index] = value;
        }
    }

    fn note_on(&mut self, note: u8, velocity: f32, gate_samples: Option<f64>) -> Option<Hit> {
        let i = self.pick(note);
        self.started += 1;
        let v = &mut self.voices[i];
        if !v.active() {
            // A fresh voice: oscillators from the start, filters empty.
            v.phase = [[0.0; 2]; 2];
            v.filter.iter_mut().for_each(Svf::reset);
        }
        v.note = note;
        v.velocity = velocity.clamp(0.0, 1.0);
        v.age = self.started;
        v.held = true;
        v.gate_left = gate_samples;
        v.amp.stage = Stage::Attack;
        v.fenv.stage = Stage::Attack;
        Some(Hit { voice: None, note: Some(note), velocity })
    }

    fn note_off(&mut self, note: u8) {
        for v in self.voices.iter_mut().filter(|v| v.held && v.note == note && v.gate_left.is_none()) {
            v.held = false;
            v.amp.stage = Stage::Release;
            v.fenv.stage = Stage::Release;
        }
    }

    fn pitch_bend(&mut self, semitones: f32) {
        self.bend_target = semitones.clamp(-12.0, 12.0);
    }

    fn render(&mut self, frames: usize) {
        let frames = frames.min(MAX_BLOCK);
        let sr = self.sr;
        let p = self.params;
        let wave = |x: f32| x.round().clamp(0.0, 3.0) as u8;
        let (w1, w2) = (wave(p[OSC1_WAVE]), wave(p[OSC2_WAVE]));
        let osc2_ratio = semitones_to_ratio(p[OSC2_TUNE] + p[OSC2_FINE] / 100.0);
        let level2 = p[OSC2_LEVEL].clamp(0.0, 1.0);
        let noise = p[NOISE].clamp(0.0, 1.0);
        let norm = 1.0 / (1.0 + level2 + noise);
        let detune = semitones_to_ratio(p[DETUNE].clamp(0.0, 1.0) * MAX_DETUNE_CENTS / 100.0);
        // Each unison pair's sides: the "A" oscillators lean left.
        let width = p[WIDTH].clamp(0.0, 1.0);
        let (near, far) = (0.5 + 0.5 * width, 0.5 - 0.5 * width);
        let base_cutoff = lerp_exp(20.0, 18000.0, p[CUTOFF]);
        let q = lerp_exp(0.5, 15.0, p[RESONANCE]);
        let env_oct = ENV_OCTAVES * p[ENV_AMOUNT].clamp(0.0, 1.0);
        let vel_amt = p[VELOCITY].clamp(0.0, 1.0);
        let fr = AdsrRates::new(sr, p[F_ATTACK], p[F_DECAY], p[F_SUSTAIN], p[F_RELEASE]);
        let ar = AdsrRates::new(sr, p[ATTACK], p[DECAY], p[SUSTAIN], p[RELEASE]);
        let lfo_dt = lfo_hz(p[LFO_RATE]) / sr;
        let gain = 0.5 * p[LEVEL].clamp(0.0, 1.0);

        let out = &mut self.out[..frames * 2];
        out.fill(0.0);
        let mut start = 0;
        while start < frames {
            let n = (frames - start).min(CONTROL);
            // Control rate: the LFO, the bend, then each voice's pitches
            // and filter.
            let lfo = (TAU * self.lfo_phase).sin();
            self.lfo_phase = (self.lfo_phase + lfo_dt * n as f32).fract();
            self.bend += (self.bend_target - self.bend) * 0.03;
            let vibrato = lfo * p[VIBRATO].clamp(0.0, 1.0);
            let lfo_oct = 2.0 * lfo * p[LFO_CUTOFF].clamp(0.0, 1.0);
            for v in self.voices.iter_mut().filter(|v| v.active()) {
                let hz = midi_hz(v.note as f32 + self.bend + vibrato);
                for (o, ratio) in [1.0, osc2_ratio].into_iter().enumerate() {
                    v.dt[o] = [(hz * ratio / detune / sr).min(0.45), (hz * ratio * detune / sr).min(0.45)];
                }
                let vel = 1.0 - vel_amt + vel_amt * v.velocity;
                let oct = env_oct * v.fenv.value * vel + lfo_oct;
                let fc = base_cutoff * oct.exp2();
                for f in &mut v.filter {
                    f.set(sr, fc, q);
                }
            }
            for v in self.voices.iter_mut().filter(|v| v.active()) {
                let vel = 1.0 - vel_amt + vel_amt * v.velocity;
                for f in start..start + n {
                    if let Some(left) = v.gate_left.as_mut() {
                        *left -= 1.0;
                        if *left <= 0.0 {
                            v.gate_left = None;
                            v.held = false;
                            v.amp.stage = Stage::Release;
                            v.fenv.stage = Stage::Release;
                        }
                    }
                    let mut side = [0.0f32; 2];
                    for (u, s) in side.iter_mut().enumerate() {
                        *s = osc(w1, v.phase[0][u], v.dt[0][u]) + level2 * osc(w2, v.phase[1][u], v.dt[1][u]);
                        for o in 0..2 {
                            v.phase[o][u] += v.dt[o][u];
                            if v.phase[o][u] >= 1.0 {
                                v.phase[o][u] -= 1.0;
                            }
                        }
                    }
                    let nz = noise * self.noise.next();
                    let l = (side[0] * near + side[1] * far + nz) * norm;
                    let r = (side[0] * far + side[1] * near + nz) * norm;
                    v.fenv.next(&fr);
                    let a = v.amp.next(&ar) * vel * gain;
                    out[f * 2] += v.filter[0].process(l).lp * a;
                    out[f * 2 + 1] += v.filter[1].process(r).lp * a;
                }
            }
            start += n;
        }
        for x in out.iter_mut() {
            *x = soft_clip(*x);
        }
    }

    fn output(&self, _output: usize) -> &[f32] {
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    /// Render `secs` in blocks; the left channel.
    fn run(s: &mut Poly, secs: f32) -> Vec<f32> {
        let mut left = Vec::new();
        for _ in 0..(secs * SR / 256.0) as usize {
            s.render(256);
            left.extend(s.out[..512].iter().step_by(2));
        }
        left
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Eight notes sound at once; a ninth steals the oldest, and every
    /// note-off releases its own voice to silence.
    #[test]
    fn eight_voices_then_the_oldest_is_stolen() {
        let mut s = Poly::new(SR);
        for k in 0..8 {
            s.note_on(48 + k, 0.8, None);
        }
        assert_eq!(s.voices.iter().filter(|v| v.active()).count(), 8);
        s.note_on(70, 0.8, None);
        let notes: Vec<u8> = s.voices.iter().map(|v| v.note).collect();
        assert!(!notes.contains(&48) && notes.contains(&70), "{notes:?}");
        let full = run(&mut s, 0.2);
        assert!(rms(&full) > 0.01);
        assert!(full.iter().all(|x| x.abs() <= 1.0), "eight loud voices stay in range");
        for n in (49..56).chain([70]) {
            s.note_off(n);
        }
        run(&mut s, 3.0);
        assert!(s.voices.iter().all(|v| !v.active()), "all released");
        assert!(rms(&run(&mut s, 0.1)) < 1e-6);
    }

    /// The same note again restarts its own voice instead of stacking.
    #[test]
    fn a_repeated_note_reuses_its_voice() {
        let mut s = Poly::new(SR);
        s.note_on(60, 0.8, None);
        s.note_on(60, 0.8, None);
        assert_eq!(s.voices.iter().filter(|v| v.active()).count(), 1);
    }

    /// A timed note (from a clip) releases by itself after its gate.
    #[test]
    fn a_timed_note_releases_itself() {
        let mut s = Poly::new(SR);
        s.note_on(60, 0.8, Some(0.1 * SR as f64));
        run(&mut s, 0.05);
        assert!(s.voices[0].held);
        run(&mut s, 0.1);
        assert!(!s.voices[0].held);
    }

    /// Velocity scales the level by `velocity`: a soft note is quieter
    /// than a hard one, and the same with velocity off.
    #[test]
    fn velocity_scales_loudness() {
        let level = |vel: f32, amt: f32| {
            let mut s = Poly::new(SR);
            s.set_param(VELOCITY, amt);
            s.note_on(57, vel, None);
            rms(&run(&mut s, 0.3)[4800..])
        };
        assert!(level(0.3, 1.0) < 0.6 * level(1.0, 1.0));
        assert!((level(0.3, 0.0) - level(1.0, 0.0)).abs() < 1e-4);
    }

    /// Width spreads a note's unison pair: with width at 0 both sides are
    /// the same; at 1 they differ.
    #[test]
    fn width_spreads_the_unison_pair() {
        let diff = |width: f32| {
            let mut s = Poly::new(SR);
            s.set_param(WIDTH, width);
            s.set_param(DETUNE, 1.0);
            s.note_on(57, 1.0, None);
            let mut d = 0.0f32;
            for _ in 0..40 {
                s.render(256);
                d += s.out[..512].chunks(2).map(|c| (c[0] - c[1]).abs()).sum::<f32>();
            }
            d
        };
        assert!(diff(0.0) < 1e-3, "{}", diff(0.0));
        assert!(diff(1.0) > 1.0, "{}", diff(1.0));
    }
}
