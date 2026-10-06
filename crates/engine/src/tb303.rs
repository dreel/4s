//! `tb303`: a monophonic bass synth in the style of the TB-303.
//!
//! Band-limited (PolyBLEP) saw or square -> 4-pole resonant ladder lowpass
//! (24 dB/oct, tanh stages) -> VCA. A filter envelope decays from the
//! `env_mod` depth. Accented notes play louder and add an accent sweep to the
//! filter that builds up over consecutive accents. A step with slide glides
//! into the next note (~60 ms) without retriggering, holding the gate.
//! Gates last half a step otherwise.

use crate::dsp::{lerp_exp, semitones_to_ratio, soft_clip};
use crate::instrument::{Hit, Instrument, MAX_BLOCK};
use crate::params::{cont, toggle};
use fours_protocol::{MAX_STEPS, NoteStep, OutputWidth, ParamInfo};

pub const TUNE: usize = 0;
pub const WAVEFORM: usize = 1;
pub const CUTOFF: usize = 2;
pub const RESONANCE: usize = 3;
pub const ENV_MOD: usize = 4;
pub const DECAY: usize = 5;
pub const ACCENT: usize = 6;
pub const NUM_PARAMS: usize = 7;

const VELOCITY_ON: f32 = 0.7;
const VELOCITY_ACCENT: f32 = 1.0;
const GLIDE_SECS: f32 = 0.06;

fn midi_hz(note: f32) -> f32 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

/// PolyBLEP residual for a discontinuity at phase 0 (phase and dt in cycles).
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

pub struct Tb303 {
    sr: f32,
    params: [f32; NUM_PARAMS],
    notes: [NoteStep; MAX_STEPS],
    phase: f32,
    /// Current pitch as a (fractional) MIDI note, gliding toward `target`.
    pitch: f32,
    target: f32,
    glide_coef: f32,
    /// Gate state and samples until it closes (`None` = held).
    gate: bool,
    gate_left: Option<f64>,
    /// VCA level and target peak for the current note.
    amp: f32,
    velocity: f32,
    attack_coef: f32,
    release_coef: f32,
    /// Filter envelope (1 at note start, decaying).
    fenv: f32,
    accent: bool,
    /// Accent sweep "capacitor": charges on accented notes, so consecutive
    /// accents push the filter further.
    accent_cap: f32,
    accent_coef: f32,
    ladder: [f32; 4],
    /// The previous step asked to slide into this one.
    slide_pending: bool,
    /// The current note is held by a keyboard (until its note-off), so the
    /// sequencer's rests and stop do not cut it; a sequenced note takes over.
    held_by_key: bool,
    buf: Vec<f32>,
}

impl Tb303 {
    pub fn new(sr: f32) -> Self {
        let mut params = [0.0; NUM_PARAMS];
        for (i, p) in Self::params("bass").iter().enumerate() {
            params[i] = p.default as f32;
        }
        let coef = |secs: f32| (-1.0 / (secs * sr)).exp();
        Self {
            sr,
            params,
            notes: [NoteStep::default(); MAX_STEPS],
            phase: 0.0,
            pitch: 36.0,
            target: 36.0,
            glide_coef: coef(GLIDE_SECS / 3.0),
            gate: false,
            gate_left: None,
            amp: 0.0,
            velocity: 0.0,
            attack_coef: coef(0.002),
            release_coef: coef(0.008),
            fenv: 0.0,
            accent: false,
            accent_cap: 0.0,
            accent_coef: coef(0.08),
            ladder: [0.0; 4],
            slide_pending: false,
            held_by_key: false,
            buf: vec![0.0; MAX_BLOCK * 2],
        }
    }

    pub fn params(id: &str) -> Vec<ParamInfo> {
        vec![
            cont(format!("{id}.tune"), "Tune".into(), -12.0, 12.0, 0.0, Some("st")),
            toggle(format!("{id}.waveform"), "Square".into()),
            cont(format!("{id}.cutoff"), "Cutoff".into(), 0.0, 1.0, 0.4, None),
            cont(format!("{id}.resonance"), "Resonance".into(), 0.0, 1.0, 0.5, None),
            cont(format!("{id}.env_mod"), "Env Mod".into(), 0.0, 1.0, 0.5, None),
            cont(format!("{id}.decay"), "Decay".into(), 0.0, 1.0, 0.4, None),
            cont(format!("{id}.accent"), "Accent".into(), 0.0, 1.0, 0.5, None),
        ]
    }

    fn start(&mut self, note: u8, accent: bool, glide: bool, gate: Option<f64>) {
        self.target = note as f32;
        if !glide {
            self.pitch = self.target;
            self.fenv = 1.0;
            self.accent = accent;
            self.velocity = if accent { VELOCITY_ACCENT } else { VELOCITY_ON };
        }
        self.gate = true;
        self.gate_left = gate;
    }

    fn release(&mut self) {
        self.gate = false;
        self.gate_left = None;
    }
}

impl Instrument for Tb303 {
    fn num_outputs(&self) -> usize {
        1
    }

    fn output_width(&self, _output: usize) -> OutputWidth {
        OutputWidth::Mono
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index < NUM_PARAMS {
            self.params[index] = value;
        }
    }

    fn set_notes(&mut self, steps: &[NoteStep; MAX_STEPS]) {
        self.notes = *steps;
    }

    fn on_step(&mut self, step: usize, step_samples: f64, emit: &mut dyn FnMut(Hit)) {
        let s = self.notes[step];
        let sliding_in = self.slide_pending && self.gate;
        self.slide_pending = false;
        let Some(note) = s.note else {
            if !self.held_by_key {
                self.release();
            }
            return;
        };
        self.held_by_key = false;
        let gate = if s.slide { None } else { Some(step_samples * 0.5) };
        self.start(note, s.accent, sliding_in, gate);
        self.slide_pending = s.slide;
        let velocity = if s.accent { VELOCITY_ACCENT } else { VELOCITY_ON };
        emit(Hit { voice: None, note: Some(note), velocity });
    }

    fn on_stop(&mut self) {
        if !self.held_by_key {
            self.release();
        }
        self.slide_pending = false;
    }

    fn note_on(&mut self, note: u8, velocity: f32, gate_samples: Option<f64>) -> bool {
        // Overlapping keyboard notes glide, like playing legato on a 303.
        let glide = self.gate && gate_samples.is_none();
        self.held_by_key = gate_samples.is_none();
        self.start(note, velocity >= 0.95, glide, gate_samples);
        true
    }

    fn note_off(&mut self) {
        // A sequenced note that took over a held key is not the key's to end.
        if !self.held_by_key {
            return;
        }
        self.held_by_key = false;
        self.release();
    }

    fn render(&mut self, frames: usize) {
        let frames = frames.min(MAX_BLOCK);
        let sr = self.sr;
        let p = self.params;
        let tune = p[TUNE];
        let square = p[WAVEFORM] >= 0.5;
        let base_cutoff = lerp_exp(40.0, 6000.0, p[CUTOFF]);
        let k = 4.2 * p[RESONANCE].clamp(0.0, 1.0);
        let env_octaves = 5.0 * p[ENV_MOD];
        let accent_amt = p[ACCENT];
        // Accented notes always use the shortest filter decay, as on the 303.
        let decay_secs = if self.accent { 0.2 } else { lerp_exp(0.2, 2.0, p[DECAY]) };
        let fenv_coef = (-6.9 / (decay_secs * sr)).exp();
        let max_fc = 0.42 * sr;

        for f in 0..frames {
            if let Some(left) = self.gate_left.as_mut() {
                *left -= 1.0;
                if *left <= 0.0 {
                    self.gate = false;
                    self.gate_left = None;
                }
            }
            self.pitch = self.target + (self.pitch - self.target) * self.glide_coef;
            let hz = midi_hz(self.pitch) * semitones_to_ratio(tune);
            let dt = (hz / sr).min(0.45);
            self.phase += dt;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            let osc = if square {
                let mut x = if self.phase < 0.5 { 1.0 } else { -1.0 };
                x += poly_blep(self.phase, dt);
                x -= poly_blep((self.phase + 0.5).fract(), dt);
                x
            } else {
                2.0 * self.phase - 1.0 - poly_blep(self.phase, dt)
            };

            self.fenv *= fenv_coef;
            let accent_in = if self.accent && self.gate { self.fenv } else { 0.0 };
            self.accent_cap = accent_in + (self.accent_cap - accent_in) * self.accent_coef;
            let octaves = env_octaves * self.fenv + 3.0 * accent_amt * self.accent_cap;
            let fc = (base_cutoff * octaves.exp2()).min(max_fc);
            let g = 1.0 - (-std::f32::consts::TAU * fc / sr).exp();

            // Ladder: four one-pole stages with resonant feedback, each
            // saturating both its input and its state, so every stage moves
            // toward its (bounded) input and none can run away. (Comparing
            // a raw stage output against tanh of the state let a stage's
            // state grow without limit once the previous stage exceeded 1,
            // which ended in a stuck, inaudible DC output.)
            let mut x = (osc - k * self.ladder[3]).tanh();
            for y in self.ladder.iter_mut() {
                *y += g * (x - y.tanh());
                x = y.tanh();
            }
            // Belt and braces: never let a bad state stick.
            if !self.ladder[3].is_finite() {
                self.ladder = [0.0; 4];
            }
            let filtered = self.ladder[3] * (1.0 + 0.5 * k);

            let (target, coef) = if self.gate {
                let boost = if self.accent { 1.0 + 0.4 * accent_amt } else { 1.0 };
                (self.velocity * boost, self.attack_coef)
            } else {
                (0.0, self.release_coef)
            };
            self.amp = target + (self.amp - target) * coef;
            self.buf[f * 2] = soft_clip(0.75 * filtered * self.amp);
            self.buf[f * 2 + 1] = 0.0;
        }
    }

    fn output(&self, _output: usize) -> &[f32] {
        &self.buf
    }
}
