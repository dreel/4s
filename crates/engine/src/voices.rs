//! TR-808-style drum voices, synthesized from oscillators, noise, filters, and
//! envelopes. No samples. Each voice is monophonic (retriggering restarts it).
//!
//! All voices share three macro parameters, interpreted per voice:
//! - `tune`: semitones of pitch offset.
//! - `decay`: 0..1, length of the main envelope.
//! - `tone`: 0..1, voice-specific color (kick click/drive, snare snappy, ...).

use crate::dsp::*;
use fours_protocol::Voice;

#[derive(Clone, Copy, Debug)]
pub struct VoiceParams {
    pub tune: f32,
    pub decay: f32,
    pub tone: f32,
}

pub trait DrumVoice: Send {
    fn trigger(&mut self, velocity: f32, p: VoiceParams);
    fn process(&mut self) -> f32;
    fn active(&self) -> bool;
    /// Fast fade-out (used for hat choke).
    fn choke(&mut self) {}
}

pub fn make_voice(voice: Voice, sr: f32) -> Box<dyn DrumVoice> {
    let seed = voice.index() as u32 * 7919 + 17;
    match voice {
        Voice::Kick => Box::new(Kick::new(sr, seed)),
        Voice::Snare => Box::new(Snare::new(sr, seed)),
        Voice::Clap => Box::new(Clap::new(sr, seed)),
        Voice::ClosedHat => Box::new(Hat::new(sr, false)),
        Voice::OpenHat => Box::new(Hat::new(sr, true)),
        Voice::LowTom => Box::new(Tom::new(sr, 95.0, seed)),
        Voice::HighTom => Box::new(Tom::new(sr, 155.0, seed)),
        Voice::Cowbell => Box::new(Cowbell::new(sr)),
    }
}

/// Default `decay` per voice (other macro params default to 0 tune, 0.5 tone).
pub fn default_decay(voice: Voice) -> f64 {
    match voice {
        Voice::Kick => 0.5,
        Voice::Snare => 0.4,
        Voice::Clap => 0.4,
        Voice::ClosedHat => 0.3,
        Voice::OpenHat => 0.5,
        Voice::LowTom => 0.5,
        Voice::HighTom => 0.5,
        Voice::Cowbell => 0.4,
    }
}

// ---------------------------------------------------------------------------

/// Sine with a fast downward pitch sweep, plus a click transient and drive.
struct Kick {
    sr: f32,
    phase: f32,
    base: f32,
    amp: Decay,
    pitch: Decay,
    click: Decay,
    click_amt: f32,
    drive: f32,
    vel: f32,
    noise: Noise,
    click_lp: Svf,
}

impl Kick {
    fn new(sr: f32, seed: u32) -> Self {
        Self {
            sr,
            phase: 0.0,
            base: 49.0,
            amp: Decay::default(),
            pitch: Decay::default(),
            click: Decay::default(),
            click_amt: 0.0,
            drive: 1.0,
            vel: 0.0,
            noise: Noise::new(seed),
            click_lp: Svf::new(sr, 3000.0, 0.7),
        }
    }
}

impl DrumVoice for Kick {
    fn trigger(&mut self, velocity: f32, p: VoiceParams) {
        self.vel = velocity;
        self.base = 49.0 * semitones_to_ratio(p.tune);
        self.phase = 0.0;
        self.amp.set_time(self.sr, lerp_exp(0.15, 2.0, p.decay));
        self.amp.trigger(1.0);
        self.pitch.set_time(self.sr, 0.07);
        self.pitch.trigger(1.0);
        self.click.set_time(self.sr, 0.006);
        self.click.trigger(1.0);
        self.click_amt = 0.1 + 0.5 * p.tone;
        self.drive = 1.0 + 3.0 * p.tone;
    }

    fn process(&mut self) -> f32 {
        if !self.amp.active() {
            return 0.0;
        }
        let f = self.base * (1.0 + 2.5 * self.pitch.next());
        self.phase = (self.phase + f / self.sr).fract();
        let body = (TAU * self.phase).sin() * self.amp.next();
        let click = self.click_lp.process(self.noise.next()).lp * self.click.next() * self.click_amt;
        let x = body + click;
        (self.drive * x).tanh() / self.drive.tanh() * self.vel
    }

    fn active(&self) -> bool {
        self.amp.active()
    }
}

// ---------------------------------------------------------------------------

/// Two detuned sines (the drum body) plus filtered noise (the snares).
struct Snare {
    sr: f32,
    p1: f32,
    p2: f32,
    f1: f32,
    f2: f32,
    body: Decay,
    snap: Decay,
    pitch: Decay,
    noise: Noise,
    hp: Svf,
    lp: Svf,
    body_amt: f32,
    snap_amt: f32,
    vel: f32,
}

impl Snare {
    fn new(sr: f32, seed: u32) -> Self {
        Self {
            sr,
            p1: 0.0,
            p2: 0.0,
            f1: 180.0,
            f2: 330.0,
            body: Decay::default(),
            snap: Decay::default(),
            pitch: Decay::default(),
            noise: Noise::new(seed),
            hp: Svf::new(sr, 1200.0, 0.7),
            lp: Svf::new(sr, 9000.0, 0.7),
            body_amt: 0.0,
            snap_amt: 0.0,
            vel: 0.0,
        }
    }
}

impl DrumVoice for Snare {
    fn trigger(&mut self, velocity: f32, p: VoiceParams) {
        let r = semitones_to_ratio(p.tune);
        self.vel = velocity;
        self.f1 = 180.0 * r;
        self.f2 = 330.0 * r;
        self.p1 = 0.0;
        self.p2 = 0.0;
        self.body.set_time(self.sr, lerp_exp(0.08, 0.35, p.decay));
        self.body.trigger(1.0);
        self.snap.set_time(self.sr, lerp_exp(0.1, 0.7, p.decay));
        self.snap.trigger(1.0);
        self.pitch.set_time(self.sr, 0.03);
        self.pitch.trigger(1.0);
        self.snap_amt = 0.25 + 0.75 * p.tone;
        self.body_amt = 1.0 - 0.5 * p.tone;
    }

    fn process(&mut self) -> f32 {
        if !self.active() {
            return 0.0;
        }
        let bend = 1.0 + 0.15 * self.pitch.next();
        self.p1 = (self.p1 + self.f1 * bend / self.sr).fract();
        self.p2 = (self.p2 + self.f2 * bend / self.sr).fract();
        let body = ((TAU * self.p1).sin() * 0.6 + (TAU * self.p2).sin() * 0.4) * self.body.next();
        let n = self.lp.process(self.hp.process(self.noise.next()).hp).lp;
        let snap = n * self.snap.next() * 1.4;
        (body * self.body_amt + snap * self.snap_amt) * 0.8 * self.vel
    }

    fn active(&self) -> bool {
        self.body.active() || self.snap.active()
    }
}

// ---------------------------------------------------------------------------

/// Band-passed noise with several rapid bursts followed by a reverb-like tail.
struct Clap {
    sr: f32,
    t: f32,
    noise: Noise,
    bp: Svf,
    tail: Decay,
    tail_started: bool,
    vel: f32,
    running: bool,
}

const CLAP_BURSTS: [f32; 4] = [0.0, 0.011, 0.022, 0.031];

impl Clap {
    fn new(sr: f32, seed: u32) -> Self {
        Self {
            sr,
            t: 0.0,
            noise: Noise::new(seed),
            bp: Svf::new(sr, 1100.0, 1.4),
            tail: Decay::default(),
            tail_started: false,
            vel: 0.0,
            running: false,
        }
    }
}

impl DrumVoice for Clap {
    fn trigger(&mut self, velocity: f32, p: VoiceParams) {
        self.vel = velocity;
        self.t = 0.0;
        self.running = true;
        self.tail_started = false;
        self.tail.value = 0.0;
        let center = lerp_exp(700.0, 2000.0, p.tone) * semitones_to_ratio(p.tune);
        self.bp.set(self.sr, center, 1.4);
        self.tail.set_time(self.sr, lerp_exp(0.12, 0.9, p.decay));
    }

    fn process(&mut self) -> f32 {
        if !self.running {
            return 0.0;
        }
        let t = self.t;
        self.t += 1.0 / self.sr;
        let last = CLAP_BURSTS.iter().rev().find(|b| **b <= t).copied().unwrap_or(0.0);
        let burst = (-(t - last) / 0.0035).exp();
        if !self.tail_started && t >= CLAP_BURSTS[3] {
            self.tail_started = true;
            self.tail.trigger(0.7);
        }
        let tail = if self.tail_started { self.tail.next() } else { 0.0 };
        if self.tail_started && !self.tail.active() {
            self.running = false;
        }
        let env = burst.max(tail);
        self.bp.process(self.noise.next()).bp * env * 2.8 * self.vel
    }

    fn active(&self) -> bool {
        self.running
    }
}

// ---------------------------------------------------------------------------

/// Six detuned square oscillators (the 808 "metal" source) through a
/// band-pass and high-pass. Open and closed hats differ in decay range.
struct Hat {
    sr: f32,
    open: bool,
    phases: [f32; 6],
    freqs: [f32; 6],
    bp: Svf,
    hp: Svf,
    amp: Decay,
    vel: f32,
}

const METAL_FREQS: [f32; 6] = [205.3, 304.4, 369.6, 522.7, 540.0, 800.0];

impl Hat {
    fn new(sr: f32, open: bool) -> Self {
        Self {
            sr,
            open,
            phases: [0.0, 0.13, 0.29, 0.41, 0.57, 0.73],
            freqs: METAL_FREQS,
            bp: Svf::new(sr, 8000.0, 1.0),
            hp: Svf::new(sr, 7000.0, 0.7),
            amp: Decay::default(),
            vel: 0.0,
        }
    }
}

impl DrumVoice for Hat {
    fn trigger(&mut self, velocity: f32, p: VoiceParams) {
        let r = semitones_to_ratio(p.tune);
        for (f, base) in self.freqs.iter_mut().zip(METAL_FREQS) {
            *f = base * r;
        }
        self.vel = velocity;
        self.bp.set(self.sr, lerp_exp(6000.0, 11000.0, p.tone), 1.0);
        let t60 = if self.open {
            lerp_exp(0.25, 1.6, p.decay)
        } else {
            lerp_exp(0.03, 0.25, p.decay)
        };
        self.amp.set_time(self.sr, t60);
        self.amp.trigger(1.0);
    }

    fn process(&mut self) -> f32 {
        if !self.amp.active() {
            return 0.0;
        }
        let mut sum = 0.0;
        for (ph, f) in self.phases.iter_mut().zip(self.freqs) {
            *ph = (*ph + f / self.sr).fract();
            sum += if *ph < 0.5 { 1.0 } else { -1.0 };
        }
        let x = self.hp.process(self.bp.process(sum / 6.0).bp).hp;
        x * self.amp.next() * 2.2 * self.vel
    }

    fn active(&self) -> bool {
        self.amp.active()
    }

    fn choke(&mut self) {
        self.amp.set_time(self.sr, 0.012);
    }
}

// ---------------------------------------------------------------------------

/// Sine with a gentle pitch drop and a touch of noise.
struct Tom {
    sr: f32,
    base_default: f32,
    base: f32,
    phase: f32,
    amp: Decay,
    pitch: Decay,
    noise: Noise,
    noise_env: Decay,
    noise_amt: f32,
    lp: Svf,
    vel: f32,
}

impl Tom {
    fn new(sr: f32, base: f32, seed: u32) -> Self {
        Self {
            sr,
            base_default: base,
            base,
            phase: 0.0,
            amp: Decay::default(),
            pitch: Decay::default(),
            noise: Noise::new(seed),
            noise_env: Decay::default(),
            noise_amt: 0.0,
            lp: Svf::new(sr, 4000.0, 0.7),
            vel: 0.0,
        }
    }
}

impl DrumVoice for Tom {
    fn trigger(&mut self, velocity: f32, p: VoiceParams) {
        self.vel = velocity;
        self.base = self.base_default * semitones_to_ratio(p.tune);
        self.phase = 0.0;
        self.amp.set_time(self.sr, lerp_exp(0.15, 1.2, p.decay));
        self.amp.trigger(1.0);
        self.pitch.set_time(self.sr, 0.15);
        self.pitch.trigger(1.0);
        self.noise_env.set_time(self.sr, 0.05);
        self.noise_env.trigger(1.0);
        self.noise_amt = 0.3 * p.tone;
    }

    fn process(&mut self) -> f32 {
        if !self.amp.active() {
            return 0.0;
        }
        let f = self.base * (1.0 + 0.6 * self.pitch.next());
        self.phase = (self.phase + f / self.sr).fract();
        let body = (TAU * self.phase).sin() * self.amp.next();
        let n = self.lp.process(self.noise.next()).lp * self.noise_env.next() * self.noise_amt;
        (body + n) * 0.9 * self.vel
    }

    fn active(&self) -> bool {
        self.amp.active()
    }
}

// ---------------------------------------------------------------------------

/// Two square oscillators (540/800 Hz) through a band-pass, with a sharp
/// attack stage and a longer tail.
struct Cowbell {
    sr: f32,
    p1: f32,
    p2: f32,
    f1: f32,
    f2: f32,
    bp: Svf,
    fast: Decay,
    slow: Decay,
    vel: f32,
}

impl Cowbell {
    fn new(sr: f32) -> Self {
        Self {
            sr,
            p1: 0.0,
            p2: 0.0,
            f1: 540.0,
            f2: 800.0,
            bp: Svf::new(sr, 1600.0, 1.5),
            fast: Decay::default(),
            slow: Decay::default(),
            vel: 0.0,
        }
    }
}

impl DrumVoice for Cowbell {
    fn trigger(&mut self, velocity: f32, p: VoiceParams) {
        let r = semitones_to_ratio(p.tune);
        self.f1 = 540.0 * r;
        self.f2 = 800.0 * r;
        self.vel = velocity;
        self.bp.set(self.sr, lerp_exp(900.0, 3000.0, p.tone), 1.5);
        self.fast.set_time(self.sr, 0.06);
        self.fast.trigger(0.7);
        self.slow.set_time(self.sr, lerp_exp(0.2, 1.2, p.decay));
        self.slow.trigger(0.3);
    }

    fn process(&mut self) -> f32 {
        if !self.active() {
            return 0.0;
        }
        self.p1 = (self.p1 + self.f1 / self.sr).fract();
        self.p2 = (self.p2 + self.f2 / self.sr).fract();
        let sq = |p: f32| if p < 0.5 { 1.0 } else { -1.0 };
        let x = (sq(self.p1) + sq(self.p2)) * 0.5;
        let env = self.fast.next() + self.slow.next();
        self.bp.process(x).bp * env * 1.6 * self.vel
    }

    fn active(&self) -> bool {
        self.fast.active() || self.slow.active()
    }
}
