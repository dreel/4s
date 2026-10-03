//! Small DSP building blocks. All of these are allocation-free and safe to use
//! on the audio thread.

use std::f32::consts::PI;

pub const TAU: f32 = 2.0 * PI;

/// Exponential interpolation between `min` and `max` for `x` in 0..1.
pub fn lerp_exp(min: f32, max: f32, x: f32) -> f32 {
    min * (max / min).powf(x.clamp(0.0, 1.0))
}

pub fn semitones_to_ratio(st: f32) -> f32 {
    (st / 12.0).exp2()
}

/// Fast deterministic white noise (xorshift32), output in -1..1.
#[derive(Clone)]
pub struct Noise {
    state: u32,
}

impl Noise {
    pub fn new(seed: u32) -> Self {
        Self { state: seed.max(1) }
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// Exponential decay envelope. `set_time` takes the time to fall by 60 dB.
#[derive(Clone, Default)]
pub struct Decay {
    pub value: f32,
    coef: f32,
}

impl Decay {
    pub fn set_time(&mut self, sr: f32, t60: f32) {
        self.coef = (0.001f32.ln() / (t60.max(0.0005) * sr)).exp();
    }

    pub fn trigger(&mut self, level: f32) {
        self.value = level;
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        let v = self.value;
        self.value *= self.coef;
        v
    }

    pub fn active(&self) -> bool {
        self.value > 1e-4
    }
}

/// Topology-preserving state variable filter (Simper/Cytomic form).
#[derive(Clone, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
    a1: f32,
    a2: f32,
    a3: f32,
    k: f32,
}

pub struct SvfOut {
    pub lp: f32,
    pub bp: f32,
    pub hp: f32,
}

impl Svf {
    pub fn new(sr: f32, cutoff: f32, q: f32) -> Self {
        let mut f = Self::default();
        f.set(sr, cutoff, q);
        f
    }

    pub fn set(&mut self, sr: f32, cutoff: f32, q: f32) {
        let fc = cutoff.clamp(10.0, sr * 0.45);
        let g = (PI * fc / sr).tan();
        self.k = 1.0 / q.max(0.05);
        self.a1 = 1.0 / (1.0 + g * (g + self.k));
        self.a2 = g * self.a1;
        self.a3 = g * self.a2;
    }

    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }

    #[inline]
    pub fn process(&mut self, v0: f32) -> SvfOut {
        let v3 = v0 - self.ic2;
        let v1 = self.a1 * self.ic1 + self.a2 * v3;
        let v2 = self.ic2 + self.a2 * self.ic1 + self.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        SvfOut { lp: v2, bp: v1, hp: v0 - self.k * v1 - v2 }
    }
}

/// One-pole smoother for parameter changes (avoids zipper noise).
#[derive(Clone, Default)]
pub struct Smoother {
    pub value: f32,
    coef: f32,
}

impl Smoother {
    pub fn new(sr: f32, time: f32, value: f32) -> Self {
        Self { value, coef: (-1.0 / (time * sr)).exp() }
    }

    #[inline]
    pub fn next(&mut self, target: f32) -> f32 {
        self.value = target + (self.value - target) * self.coef;
        self.value
    }
}

/// Soft clip that is transparent below 0.8 and saturates smoothly above.
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.8 {
        x
    } else {
        x.signum() * (0.8 + 0.2 * ((a - 0.8) / 0.2).tanh())
    }
}
