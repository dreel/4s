//! Audio effects (RFC 0010): inserts on mixer channels and the master. An
//! effect is built on the control side (`make`, which allocates any
//! buffers), handed to the audio thread, and dropped back on the control
//! side after removal, as instruments are. `process` must not allocate.
//!
//! An effect renders its wet signal in place; the engine keeps the dry copy
//! and applies the two parameters every effect has first, `bypass` and
//! `mix` (dry/wet), so each effect only implements its own sound.

use crate::dsp::{Smoother, Svf, lerp_exp};
use crate::params::{cont, toggle};
use fours_protocol::{DeviceType, KnobPage, ParamInfo, ParamKind};

/// What an effect may need to know about the transport.
#[derive(Clone, Copy, Debug)]
pub struct ProcessContext {
    pub tempo: f32,
}

pub trait AudioEffect: Send {
    /// Set parameter `index` of its own (after `bypass` and `mix`).
    fn set_param(&mut self, index: usize, value: f32);
    /// Render the wet signal in place: `buf` is interleaved stereo,
    /// `frames` long.
    fn process(&mut self, buf: &mut [f32], frames: usize, ctx: &ProcessContext);
    /// Jump smoothers to their targets (before an offline render).
    fn snap(&mut self, _ctx: &ProcessContext) {}
}

/// Parameters every effect has, before its own: bypass and dry/wet mix.
pub const COMMON_PARAMS: usize = 2;

/// Parameters of an effect with id `id`, in index order: `bypass`, `mix`,
/// then its own.
pub fn params(kind: DeviceType, id: &str) -> Vec<ParamInfo> {
    let mix = match kind {
        DeviceType::Filter => 1.0,
        DeviceType::Delay => 0.35,
        DeviceType::Reverb => 0.3,
    };
    let mut out = vec![toggle(format!("{id}.bypass"), "Bypass".into()), cont(format!("{id}.mix"), "Mix".into(), 0.0, 1.0, mix, None)];
    let int = |name: &str, label: &str, max: i32, default: f64| ParamInfo {
        path: format!("{id}.{name}"),
        label: label.into(),
        kind: ParamKind::Integer { min: 0, max },
        default,
        unit: None,
    };
    match kind {
        DeviceType::Filter => out.extend([
            int("mode", "Mode", 2, 0.0),
            cont(format!("{id}.cutoff"), "Cutoff".into(), 0.0, 1.0, 0.6, None),
            cont(format!("{id}.resonance"), "Resonance".into(), 0.0, 1.0, 0.2, None),
        ]),
        DeviceType::Delay => out.extend([
            toggle(format!("{id}.sync"), "Sync".into()),
            int("division", "Division", DIVISIONS.len() as i32 - 1, 4.0),
            cont(format!("{id}.time"), "Time".into(), 0.0, 1.0, 0.5, None),
            cont(format!("{id}.feedback"), "Feedback".into(), 0.0, 0.95, 0.4, None),
            toggle(format!("{id}.pingpong"), "Ping-Pong".into()),
            cont(format!("{id}.damping"), "Damping".into(), 0.0, 1.0, 0.3, None),
        ]),
        DeviceType::Reverb => out.extend([
            cont(format!("{id}.size"), "Size".into(), 0.0, 1.0, 0.5, None),
            cont(format!("{id}.damping"), "Damping".into(), 0.0, 1.0, 0.5, None),
            cont(format!("{id}.width"), "Width".into(), 0.0, 1.0, 1.0, None),
        ]),
    }
    // A delay is synced to the transport by default.
    if kind == DeviceType::Delay {
        out[COMMON_PARAMS].default = 1.0;
    }
    out
}

/// Knob pages: everything but bypass, in order.
pub fn knob_pages(kind: DeviceType, id: &str) -> Vec<KnobPage> {
    vec![KnobPage { id: "main".into(), label: "Main".into(), params: params(kind, id).into_iter().skip(1).map(|p| p.path).collect() }]
}

/// Build an effect at `sample_rate`, its own parameters at their defaults.
pub fn make(kind: DeviceType, sample_rate: f32) -> Box<dyn AudioEffect> {
    let mut e: Box<dyn AudioEffect> = match kind {
        DeviceType::Filter => Box::new(Filter::new(sample_rate)),
        DeviceType::Delay => Box::new(Delay::new(sample_rate)),
        DeviceType::Reverb => Box::new(Reverb::new(sample_rate)),
    };
    for (i, p) in params(kind, "x").iter().enumerate().skip(COMMON_PARAMS) {
        e.set_param(i - COMMON_PARAMS, p.default as f32);
    }
    e.snap(&ProcessContext { tempo: 120.0 });
    e
}

// ---- filter --------------------------------------------------------------------

/// A stereo state-variable filter: low, band, or high pass.
pub struct Filter {
    sr: f32,
    svf: [Svf; 2],
    mode: u8,
    cutoff: Smoother,
    cutoff_target: f32,
    resonance: f32,
}

impl Filter {
    fn new(sr: f32) -> Self {
        Self { sr, svf: [Svf::new(sr, 1000.0, 0.7), Svf::new(sr, 1000.0, 0.7)], mode: 0, cutoff: Smoother::new(sr, 0.01, 0.6), cutoff_target: 0.6, resonance: 0.2 }
    }
}

impl AudioEffect for Filter {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.mode = value.round().clamp(0.0, 2.0) as u8,
            1 => self.cutoff_target = value.clamp(0.0, 1.0),
            2 => self.resonance = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32], frames: usize, _ctx: &ProcessContext) {
        // Q from 0.5 (no resonance) to 12.
        let q = lerp_exp(0.5, 12.0, self.resonance);
        for f in 0..frames {
            // Coefficients follow the smoothed cutoff every 16 frames.
            if f % 16 == 0 {
                let c = self.cutoff.next(self.cutoff_target);
                let hz = lerp_exp(20.0, 20000.0, c);
                for s in &mut self.svf {
                    s.set(self.sr, hz, q);
                }
            }
            for (ch, s) in self.svf.iter_mut().enumerate() {
                let o = s.process(buf[f * 2 + ch]);
                buf[f * 2 + ch] = match self.mode {
                    1 => o.bp,
                    2 => o.hp,
                    _ => o.lp,
                };
            }
        }
    }

    fn snap(&mut self, _ctx: &ProcessContext) {
        self.cutoff.value = self.cutoff_target;
    }
}

// ---- delay ---------------------------------------------------------------------

/// Tempo-synced delay divisions, in beats: 1/32, 1/16, 1/8 triplet, 1/8,
/// dotted 1/8, 1/4, dotted 1/4, 1/2, one bar.
pub const DIVISIONS: [f32; 9] = [0.125, 0.25, 1.0 / 3.0, 0.5, 0.75, 1.0, 1.5, 2.0, 4.0];

/// Longest delay: 2 seconds.
const MAX_DELAY_SECS: f32 = 2.0;

/// A stereo delay with feedback, ping-pong, and a damping low-pass in the
/// feedback path. Its buffers are allocated when it is built.
pub struct Delay {
    sr: f32,
    buf: [Vec<f32>; 2],
    write: usize,
    sync: bool,
    division: usize,
    time: f32,
    feedback: f32,
    pingpong: bool,
    damping: f32,
    /// Delay in samples, smoothed so time changes glide.
    delay: Smoother,
    lp: [f32; 2],
}

impl Delay {
    fn new(sr: f32) -> Self {
        let len = (MAX_DELAY_SECS * sr) as usize + 2;
        Self {
            sr,
            buf: [vec![0.0; len], vec![0.0; len]],
            write: 0,
            sync: true,
            division: 4,
            time: 0.5,
            feedback: 0.4,
            pingpong: false,
            damping: 0.3,
            delay: Smoother::new(sr, 0.05, sr * 0.25),
            lp: [0.0; 2],
        }
    }

    fn target(&self, ctx: &ProcessContext) -> f32 {
        let secs = if self.sync {
            DIVISIONS[self.division.min(DIVISIONS.len() - 1)] * 60.0 / ctx.tempo.clamp(20.0, 300.0)
        } else {
            lerp_exp(0.001, MAX_DELAY_SECS, self.time)
        };
        (secs * self.sr).clamp(1.0, (self.buf[0].len() - 2) as f32)
    }
}

impl AudioEffect for Delay {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.sync = value >= 0.5,
            1 => self.division = value.round().max(0.0) as usize,
            2 => self.time = value.clamp(0.0, 1.0),
            3 => self.feedback = value.clamp(0.0, 0.95),
            4 => self.pingpong = value >= 0.5,
            5 => self.damping = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32], frames: usize, ctx: &ProcessContext) {
        let len = self.buf[0].len();
        let target = self.target(ctx);
        // One-pole low-pass in the feedback path: 1 (none) down to 0.1.
        let a = 1.0 - 0.9 * self.damping;
        for f in 0..frames {
            let d = self.delay.next(target);
            let pos = self.write as f32 + len as f32 - d;
            let i = pos as usize;
            let frac = pos - i as f32;
            let read = |b: &Vec<f32>| b[i % len] * (1.0 - frac) + b[(i + 1) % len] * frac;
            let wet = [read(&self.buf[0]), read(&self.buf[1])];
            for ch in 0..2 {
                self.lp[ch] += a * (wet[ch] - self.lp[ch]);
            }
            let (l, r) = (buf[f * 2], buf[f * 2 + 1]);
            if self.pingpong {
                // The input enters on the left; echoes cross sides.
                self.buf[0][self.write] = 0.5 * (l + r) + self.lp[1] * self.feedback;
                self.buf[1][self.write] = self.lp[0] * self.feedback;
            } else {
                self.buf[0][self.write] = l + self.lp[0] * self.feedback;
                self.buf[1][self.write] = r + self.lp[1] * self.feedback;
            }
            buf[f * 2] = wet[0];
            buf[f * 2 + 1] = wet[1];
            self.write = (self.write + 1) % len;
        }
    }

    fn snap(&mut self, ctx: &ProcessContext) {
        self.delay.value = self.target(ctx);
    }
}

// ---- reverb --------------------------------------------------------------------

/// Freeverb's tunings, in samples at 44.1 kHz.
const COMBS: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
const ALLPASSES: [usize; 4] = [556, 441, 341, 225];
const SPREAD: usize = 23;

struct Comb {
    buf: Vec<f32>,
    i: usize,
    store: f32,
}

impl Comb {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], i: 0, store: 0.0 }
    }

    #[inline]
    fn process(&mut self, x: f32, feedback: f32, damp: f32) -> f32 {
        let y = self.buf[self.i];
        self.store = y * (1.0 - damp) + self.store * damp;
        self.buf[self.i] = x + self.store * feedback;
        self.i = (self.i + 1) % self.buf.len();
        y
    }
}

struct Allpass {
    buf: Vec<f32>,
    i: usize,
}

impl Allpass {
    fn new(len: usize) -> Self {
        Self { buf: vec![0.0; len.max(1)], i: 0 }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let b = self.buf[self.i];
        self.buf[self.i] = x + b * 0.5;
        self.i = (self.i + 1) % self.buf.len();
        b - x
    }
}

/// A Freeverb-style stereo reverb: eight parallel combs and four series
/// all-passes per side.
pub struct Reverb {
    combs: [Vec<Comb>; 2],
    allpasses: [Vec<Allpass>; 2],
    size: f32,
    damping: f32,
    width: f32,
}

impl Reverb {
    fn new(sr: f32) -> Self {
        let scale = |n: usize| ((n as f32) * sr / 44100.0) as usize;
        let side = |spread: usize| {
            (
                COMBS.iter().map(|n| Comb::new(scale(n + spread))).collect::<Vec<_>>(),
                ALLPASSES.iter().map(|n| Allpass::new(scale(n + spread))).collect::<Vec<_>>(),
            )
        };
        let (cl, al) = side(0);
        let (cr, ar) = side(SPREAD);
        Self { combs: [cl, cr], allpasses: [al, ar], size: 0.5, damping: 0.5, width: 1.0 }
    }
}

impl AudioEffect for Reverb {
    fn set_param(&mut self, index: usize, value: f32) {
        match index {
            0 => self.size = value.clamp(0.0, 1.0),
            1 => self.damping = value.clamp(0.0, 1.0),
            2 => self.width = value.clamp(0.0, 1.0),
            _ => {}
        }
    }

    fn process(&mut self, buf: &mut [f32], frames: usize, _ctx: &ProcessContext) {
        let feedback = 0.7 + 0.28 * self.size;
        let damp = 0.4 * self.damping;
        let (w1, w2) = (0.5 * (1.0 + self.width), 0.5 * (1.0 - self.width));
        for f in 0..frames {
            let input = (buf[f * 2] + buf[f * 2 + 1]) * 0.015;
            let mut out = [0.0f32; 2];
            for (ch, o) in out.iter_mut().enumerate() {
                let mut acc = 0.0;
                for c in &mut self.combs[ch] {
                    acc += c.process(input, feedback, damp);
                }
                for a in &mut self.allpasses[ch] {
                    acc = a.process(acc);
                }
                *o = acc;
            }
            buf[f * 2] = out[0] * w1 + out[1] * w2;
            buf[f * 2 + 1] = out[1] * w1 + out[0] * w2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;
    const CTX: ProcessContext = ProcessContext { tempo: 120.0 };

    /// An impulse through an effect, `secs` long, in blocks.
    fn impulse(e: &mut dyn AudioEffect, secs: f32) -> Vec<f32> {
        let frames = (secs * SR) as usize;
        let mut out = vec![0.0f32; frames * 2];
        out[0] = 1.0;
        out[1] = 1.0;
        for chunk in out.chunks_mut(512) {
            let n = chunk.len() / 2;
            e.process(chunk, n, &CTX);
        }
        out
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }

    /// Every effect stays at or below full scale on an impulse and decays
    /// to silence.
    #[test]
    fn effects_stay_in_range_and_decay() {
        for kind in DeviceType::ALL {
            let mut e = make(kind, SR);
            let out = impulse(e.as_mut(), 8.0);
            assert!(peak(&out) <= 1.0, "{kind:?} peak {}", peak(&out));
            let tail = &out[out.len() - 4800..];
            assert!(peak(tail) < 0.001, "{kind:?} did not decay: {}", peak(tail));
        }
    }

    /// A synced delay's first echo comes a dotted eighth (0.375 s at 120
    /// bpm) after the input.
    #[test]
    fn delay_echoes_on_time() {
        let mut e = make(DeviceType::Delay, SR);
        let out = impulse(e.as_mut(), 0.5);
        let first = out.iter().step_by(2).position(|x| x.abs() > 0.3).map(|i| i as f32 / SR);
        assert!(first.is_some_and(|t| (t - 0.375).abs() < 0.002), "{first:?}");
    }

    /// The low-pass mode cuts a high tone and passes a low one.
    #[test]
    fn filter_low_pass_cuts_highs() {
        let level = |hz: f32| {
            let mut e = make(DeviceType::Filter, SR);
            e.set_param(1, 0.3); // about 160 Hz
            let frames = SR as usize / 2;
            let mut buf: Vec<f32> = (0..frames).flat_map(|i| {
                let v = (std::f32::consts::TAU * hz * i as f32 / SR).sin() * 0.5;
                [v, v]
            }).collect();
            for chunk in buf.chunks_mut(512) {
                let n = chunk.len() / 2;
                e.process(chunk, n, &CTX);
            }
            peak(&buf[buf.len() / 2..])
        };
        assert!(level(5000.0) < 0.05 && level(50.0) > 0.3, "{} {}", level(5000.0), level(50.0));
    }
}
