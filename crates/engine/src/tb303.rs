//! `tb303`: a monophonic bass synth in the style of the TB-303.
//!
//! Band-limited (PolyBLEP) saw or square -> 4-pole resonant ladder lowpass
//! (24 dB/oct, tanh stages) -> VCA. A filter envelope decays from the
//! `env_mod` depth. Accented notes play louder and add an accent sweep to the
//! filter that builds up over consecutive accents. A note that starts while
//! another is held glides into it (~60 ms) without retriggering (legato), as
//! a 303 slide does; the sequencer plays a slide as overlapping notes.

use crate::dsp::{lerp_exp, semitones_to_ratio, soft_clip};
use crate::instrument::{Hit, Instrument, MAX_BLOCK};
use crate::params::{cont, toggle};
use fours_protocol::{KnobPage, OutputWidth, ParamInfo};

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
/// Most keys held at once; pressing more forgets the oldest.
const MAX_KEYS: usize = 16;

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
    /// The current note is held (until its note-off) rather than timed.
    held_by_key: bool,
    /// Pitch bend in semitones: target and smoothed value.
    bend_target: f32,
    bend: f32,
    /// Held keys, oldest first; the last one sounds (last-note priority).
    keys: [u8; MAX_KEYS],
    num_keys: usize,
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
            held_by_key: false,
            bend_target: 0.0,
            bend: 0.0,
            keys: [0; MAX_KEYS],
            num_keys: 0,
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

    /// Forget a held key. Returns true if it was the sounding (last) one.
    fn remove_key(&mut self, note: u8) -> bool {
        let Some(i) = self.keys[..self.num_keys].iter().position(|k| *k == note) else { return false };
        let was_top = i + 1 == self.num_keys;
        self.keys.copy_within(i + 1..self.num_keys, i);
        self.num_keys -= 1;
        was_top
    }

    pub fn knob_pages(id: &str) -> Vec<KnobPage> {
        let params = ["cutoff", "resonance", "env_mod", "decay", "accent", "tune", "waveform"];
        vec![KnobPage {
            id: "main".into(),
            label: "Main".into(),
            params: params.iter().map(|p| format!("{id}.{p}")).collect(),
        }]
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

    fn note_on(&mut self, note: u8, velocity: f32, gate_samples: Option<f64>) -> Option<Hit> {
        // Overlapping notes (keys, or a sequenced slide) glide: legato.
        let held = gate_samples.is_none();
        let glide = self.gate && held;
        if held {
            self.remove_key(note);
            if self.num_keys == MAX_KEYS {
                self.remove_key(self.keys[0]);
            }
            self.keys[self.num_keys] = note;
            self.num_keys += 1;
        }
        self.held_by_key = held;
        self.start(note, velocity >= 0.95, glide, gate_samples);
        Some(Hit { voice: None, note: Some(note), velocity })
    }

    fn pitch_bend(&mut self, semitones: f32) {
        self.bend_target = semitones.clamp(-12.0, 12.0);
    }

    fn note_off(&mut self, note: u8) {
        let was_top = self.remove_key(note);
        // A timed note (an audition) that took over is not the key's to end,
        // and releasing a key under the sounding one changes nothing.
        if !self.held_by_key || !was_top {
            return;
        }
        match self.num_keys {
            // Back to the key still held below, gliding (last-note priority).
            n if n > 0 => self.start(self.keys[n - 1], self.accent, true, None),
            _ => {
                self.held_by_key = false;
                self.release();
            }
        }
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
            self.bend += (self.bend_target - self.bend) * 0.002;
            let hz = midi_hz(self.pitch) * semitones_to_ratio(tune + self.bend);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Last-note priority: releasing the sounding key glides back to the key
    /// still held; releasing a key under it changes nothing.
    #[test]
    fn held_keys_fall_back_to_the_last_one_still_held() {
        let mut b = Tb303::new(48000.0);
        b.note_on(36, 0.7, None);
        b.note_on(43, 0.7, None);
        b.note_on(48, 0.7, None);
        assert_eq!(b.target, 48.0);
        b.note_off(43);
        assert_eq!((b.target, b.gate), (48.0, true), "a key under the sounding one");
        b.note_off(48);
        assert_eq!((b.target, b.gate), (36.0, true), "back to the key still held");
        b.note_off(36);
        assert!(!b.gate, "last key up releases");
    }
}
