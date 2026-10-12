//! `sampler` (RFC 0010): 16 pads, each a sample on its own note (GM drum
//! notes from 36 up, as the note layout says), polyphonic with linear
//! interpolation. Per pad: gain, pan, tune, start and end points, attack and
//! release, one-shot or gated, and a choke group.
//!
//! Samples are decoded and resampled to the engine rate on the control side
//! and handed in as shared buffers (`set_sample`); the audio thread never
//! decodes, allocates, or frees one (a replaced sample is handed back).

use crate::instrument::{Hit, Instrument, MAX_BLOCK};
use crate::params::{cont, toggle};
use fours_protocol::{KnobPage, OutputWidth, ParamInfo, ParamKind};
use std::sync::Arc;

/// Pads in a sampler.
pub const PADS: usize = 16;
/// The note pad 1 plays; pad k plays `FIRST_NOTE + k - 1`.
pub const FIRST_NOTE: u8 = 36;
/// Voices sounding at once.
const VOICES: usize = 16;
/// Parameters per pad: gain, pan, tune, start, end, attack, release,
/// one-shot, choke.
pub const PAD_PARAMS: usize = 9;

/// A sample: interleaved stereo frames at the engine's rate.
pub type Sample = Arc<[f32]>;

pub fn params(id: &str) -> Vec<ParamInfo> {
    (1..=PADS)
        .flat_map(|k| {
            let p = |name: &str| format!("{id}.{k}.{name}");
            let mut oneshot = toggle(p("oneshot"), format!("Pad {k} One-shot"));
            oneshot.default = 1.0;
            [
                cont(p("gain"), format!("Pad {k} Gain"), 0.0, 1.0, 0.8, None),
                cont(p("pan"), format!("Pad {k} Pan"), -1.0, 1.0, 0.0, None),
                cont(p("tune"), format!("Pad {k} Tune"), -24.0, 24.0, 0.0, Some("st")),
                cont(p("start"), format!("Pad {k} Start"), 0.0, 1.0, 0.0, None),
                cont(p("end"), format!("Pad {k} End"), 0.0, 1.0, 1.0, None),
                cont(p("attack"), format!("Pad {k} Attack"), 0.0, 1.0, 0.0, None),
                cont(p("release"), format!("Pad {k} Release"), 0.0, 1.0, 0.1, None),
                oneshot,
                ParamInfo { path: p("choke"), label: format!("Pad {k} Choke"), kind: ParamKind::Integer { min: 0, max: 8 }, default: 0.0, unit: None },
            ]
        })
        .collect()
}

/// A knob page per pad.
pub fn knob_pages(id: &str) -> Vec<KnobPage> {
    let names = ["gain", "pan", "tune", "start", "end", "attack", "release", "choke"];
    (1..=PADS)
        .map(|k| KnobPage {
            id: format!("pad{k}"),
            label: format!("Pad {k}"),
            params: names.iter().map(|n| format!("{id}.{k}.{n}")).collect(),
        })
        .collect()
}

#[derive(Clone, Copy, Default)]
struct Pad {
    gain: f32,
    pan: f32,
    tune: f32,
    start: f32,
    end: f32,
    attack: f32,
    release: f32,
    oneshot: bool,
    choke: u8,
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    pad: usize,
    /// Frame position in the sample (fractional).
    pos: f64,
    rate: f64,
    end: f64,
    gain: f32,
    env: f32,
    /// Per-sample envelope step while attacking (> 0) or releasing (< 0).
    step: f32,
    held: bool,
    /// Samples until a gated (`gate_samples`) note releases.
    gate_left: Option<f64>,
}

pub struct Sampler {
    sr: f32,
    pads: [Pad; PADS],
    samples: [Option<Sample>; PADS],
    voices: [Voice; VOICES],
    out: Vec<f32>,
}

impl Sampler {
    pub fn new(sr: f32) -> Self {
        let mut s = Self { sr, pads: [Pad::default(); PADS], samples: Default::default(), voices: [Voice::default(); VOICES], out: vec![0.0; MAX_BLOCK * 2] };
        for (i, p) in params("x").iter().enumerate() {
            s.set_param(i, p.default as f32);
        }
        s
    }

    fn release(&mut self, v: usize) {
        let secs = 0.002 + 2.0 * self.pads[self.voices[v].pad].release;
        let step = 1.0 / (secs * self.sr);
        let vo = &mut self.voices[v];
        vo.held = false;
        vo.step = -step;
    }
}

impl Instrument for Sampler {
    fn num_outputs(&self) -> usize {
        1
    }

    fn output_width(&self, _output: usize) -> OutputWidth {
        OutputWidth::Stereo
    }

    fn set_param(&mut self, index: usize, value: f32) {
        let Some(p) = self.pads.get_mut(index / PAD_PARAMS) else { return };
        match index % PAD_PARAMS {
            0 => p.gain = value.clamp(0.0, 1.0),
            1 => p.pan = value.clamp(-1.0, 1.0),
            2 => p.tune = value.clamp(-24.0, 24.0),
            3 => p.start = value.clamp(0.0, 1.0),
            4 => p.end = value.clamp(0.0, 1.0),
            5 => p.attack = value.clamp(0.0, 1.0),
            6 => p.release = value.clamp(0.0, 1.0),
            7 => p.oneshot = value >= 0.5,
            _ => p.choke = value.round().clamp(0.0, 8.0) as u8,
        }
    }

    fn set_sample(&mut self, pad: usize, sample: Option<Sample>) -> Option<Sample> {
        let Some(slot) = self.samples.get_mut(pad) else { return sample };
        // Voices playing the old sample stop: it is about to leave.
        for v in self.voices.iter_mut().filter(|v| v.active && v.pad == pad) {
            v.active = false;
        }
        std::mem::replace(slot, sample)
    }

    fn note_on(&mut self, note: u8, velocity: f32, gate_samples: Option<f64>) -> Option<Hit> {
        let pad = note.checked_sub(FIRST_NOTE).map(|p| p as usize).filter(|p| *p < PADS)?;
        let sample = self.samples[pad].as_ref()?;
        let frames = (sample.len() / 2) as f64;
        let p = self.pads[pad];
        let (a, b) = (p.start.min(p.end) as f64 * frames, p.start.max(p.end) as f64 * frames);
        if b - a < 1.0 {
            return None;
        }
        // A choke group stops the other pads in it.
        if p.choke > 0 {
            for i in 0..VOICES {
                let v = self.voices[i];
                if v.active && v.pad != pad && self.pads[v.pad].choke == p.choke {
                    self.voices[i].step = -1.0 / (0.005 * self.sr);
                    self.voices[i].held = false;
                }
            }
        }
        // A free voice, else the quietest.
        let v = (0..VOICES).find(|i| !self.voices[*i].active).unwrap_or_else(|| {
            (0..VOICES).min_by(|x, y| self.voices[*x].env.total_cmp(&self.voices[*y].env)).unwrap_or(0)
        });
        let attack = 0.0005 + 0.5 * p.attack;
        self.voices[v] = Voice {
            active: true,
            pad,
            pos: a,
            rate: (p.tune as f64 / 12.0).exp2(),
            end: b,
            gain: velocity.clamp(0.0, 1.0) * p.gain * p.gain,
            env: 0.0,
            step: 1.0 / (attack * self.sr),
            held: true,
            gate_left: if p.oneshot { None } else { gate_samples },
        };
        Some(Hit { voice: None, note: Some(note), velocity })
    }

    fn note_off(&mut self, note: u8) {
        let Some(pad) = note.checked_sub(FIRST_NOTE).map(|p| p as usize).filter(|p| *p < PADS) else { return };
        if self.pads[pad].oneshot {
            return;
        }
        for i in 0..VOICES {
            if self.voices[i].active && self.voices[i].pad == pad && self.voices[i].held {
                self.release(i);
            }
        }
    }

    fn render(&mut self, frames: usize) {
        let out = &mut self.out[..frames * 2];
        out.fill(0.0);
        for i in 0..VOICES {
            if !self.voices[i].active {
                continue;
            }
            let pad = self.voices[i].pad;
            let Some(sample) = self.samples[pad].as_ref() else {
                self.voices[i].active = false;
                continue;
            };
            let p = self.pads[pad];
            let (pl, pr) = (((1.0 - p.pan) * 0.5).sqrt() * std::f32::consts::SQRT_2, ((1.0 + p.pan) * 0.5).sqrt() * std::f32::consts::SQRT_2);
            let (pl, pr) = (pl.min(1.0), pr.min(1.0));
            let release_step = -1.0 / ((0.002 + 2.0 * p.release) * self.sr);
            let v = &mut self.voices[i];
            for f in 0..frames {
                if let Some(left) = v.gate_left.as_mut() {
                    *left -= 1.0;
                    if *left <= 0.0 && v.held {
                        v.gate_left = None;
                        v.held = false;
                        v.step = release_step;
                    }
                }
                if v.pos >= v.end - 1.0 {
                    v.active = false;
                    break;
                }
                let k = v.pos as usize;
                let t = (v.pos - k as f64) as f32;
                let l = sample[k * 2] * (1.0 - t) + sample[k * 2 + 2] * t;
                let r = sample[k * 2 + 1] * (1.0 - t) + sample[k * 2 + 3] * t;
                v.env += v.step;
                if v.env >= 1.0 && v.step > 0.0 {
                    v.env = 1.0;
                    v.step = 0.0;
                }
                if v.env <= 0.0 && v.step < 0.0 {
                    v.active = false;
                    break;
                }
                let g = v.gain * v.env;
                out[f * 2] += l * g * pl;
                out[f * 2 + 1] += r * g * pr;
                v.pos += v.rate;
            }
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

    /// A one-second 100 Hz sine, stereo.
    fn sine() -> Sample {
        (0..SR as usize).flat_map(|i| {
            let v = (std::f32::consts::TAU * 100.0 * i as f32 / SR).sin() * 0.5;
            [v, v]
        }).collect::<Vec<f32>>().into()
    }

    fn crossings(s: &mut Sampler, note: u8, secs: f32) -> usize {
        s.note_on(note, 1.0, None);
        let mut n = 0;
        let mut last = 0.0f32;
        for _ in 0..(secs * SR / 256.0) as usize {
            s.render(256);
            for f in 0..256 {
                let x = s.out[f * 2];
                if last < 0.0 && x >= 0.0 {
                    n += 1;
                }
                last = x;
            }
        }
        n
    }

    /// A pad plays its sample at its own pitch, an octave up at +12 st,
    /// and only on its own note.
    #[test]
    fn pads_play_their_samples_at_their_pitch() {
        let mut s = Sampler::new(SR);
        assert!(s.set_sample(0, Some(sine())).is_none());
        assert!(s.note_on(FIRST_NOTE + 1, 1.0, None).is_none(), "pad 2 has no sample");
        let base = crossings(&mut s, FIRST_NOTE, 0.5);
        assert!((48..=52).contains(&base), "100 Hz over half a second: {base}");
        let mut up = Sampler::new(SR);
        up.set_sample(0, Some(sine()));
        up.set_param(2, 12.0);
        let oct = crossings(&mut up, FIRST_NOTE, 0.25);
        assert!((48..=52).contains(&oct), "200 Hz over a quarter: {oct}");
    }

    /// Replacing a sample hands the old one back to be dropped elsewhere.
    #[test]
    fn a_replaced_sample_comes_back() {
        let mut s = Sampler::new(SR);
        let a = sine();
        s.set_sample(3, Some(a.clone()));
        let old = s.set_sample(3, None).expect("the old one");
        assert!(Arc::ptr_eq(&old, &a));
    }
}
