//! The engine: sequencer + voices + mixer. `Engine::render` is called from the
//! audio thread (or offline) and must not allocate, lock, or block.

use crate::dsp::{Smoother, soft_clip};
use crate::params::*;
use crate::voices::{DrumVoice, VoiceParams, make_voice};
use fours_protocol::{MAX_STEPS, NUM_TRACKS, STEP_ACCENT, STEP_OFF, Voice};

pub const VELOCITY_ON: f32 = 0.7;
pub const VELOCITY_ACCENT: f32 = 1.0;
/// Meter feedback rate.
const METER_HZ: f32 = 30.0;

/// Control messages into the engine.
#[derive(Clone, Copy, Debug)]
pub enum Command {
    SetParam { id: ParamId, value: f32 },
    SetStep { track: u8, step: u8, level: u8 },
    SetTrack { track: u8, steps: [u8; MAX_STEPS] },
    Play,
    Stop,
    Trigger { track: u8, velocity: f32 },
}

/// Messages out of the engine. Times are engine time in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Feedback {
    Step { step: u32, time: f64 },
    Trigger { track: u8, velocity: f32, time: f64, step: Option<u32> },
    Stopped { time: f64 },
    Meters { tracks: [f32; NUM_TRACKS], master: [f32; 2] },
}

struct Channel {
    gain: Smoother,
    pan_l: Smoother,
    pan_r: Smoother,
    peak: f32,
}

pub struct Engine {
    sr: f32,
    params: Vec<f32>,
    pattern: [[u8; MAX_STEPS]; NUM_TRACKS],
    voices: Vec<Box<dyn DrumVoice>>,
    channels: Vec<Channel>,
    master: Smoother,
    master_peak: [f32; 2],
    playing: bool,
    /// Index of the next step to fire.
    next_step: u32,
    /// Absolute sample position at which the next step fires.
    next_step_at: f64,
    /// Total samples rendered.
    pos: u64,
    meter_countdown: u32,
}

impl Engine {
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let params = default_values();
        let voices = Voice::ALL.iter().map(|v| make_voice(*v, sr)).collect();
        let channels = (0..NUM_TRACKS)
            .map(|t| {
                let vol = params[mixer_param(t, MixerParam::Volume)];
                Channel {
                    gain: Smoother::new(sr, 0.01, volume_to_gain(vol)),
                    pan_l: Smoother::new(sr, 0.01, std::f32::consts::FRAC_1_SQRT_2),
                    pan_r: Smoother::new(sr, 0.01, std::f32::consts::FRAC_1_SQRT_2),
                    peak: 0.0,
                }
            })
            .collect();
        let master = Smoother::new(sr, 0.01, volume_to_gain(params[MASTER_VOLUME]));
        Self {
            sr,
            params,
            pattern: [[STEP_OFF; MAX_STEPS]; NUM_TRACKS],
            voices,
            channels,
            master,
            master_peak: [0.0; 2],
            playing: false,
            next_step: 0,
            next_step_at: 0.0,
            pos: 0,
            meter_countdown: 0,
        }
    }

    pub fn sample_rate(&self) -> u32 {
        self.sr as u32
    }

    pub fn time(&self) -> f64 {
        self.pos as f64 / self.sr as f64
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Load full state (non-real-time; used before the engine starts).
    pub fn load(&mut self, params: &[f32], pattern: &[[u8; MAX_STEPS]; NUM_TRACKS]) {
        self.params.copy_from_slice(params);
        self.pattern = *pattern;
        // Start the gain smoothers at the loaded mix, so muted or non-soloed
        // channels don't leak for the first few milliseconds.
        let (targets, master) = self.mix_targets();
        for (ch, (gain, l, r)) in self.channels.iter_mut().zip(targets) {
            ch.gain.value = gain;
            ch.pan_l.value = l;
            ch.pan_r.value = r;
        }
        self.master.value = master;
    }

    pub fn apply(&mut self, cmd: Command, emit: &mut impl FnMut(Feedback)) {
        match cmd {
            Command::SetParam { id, value } => {
                if id < self.params.len() {
                    self.params[id] = value;
                }
            }
            Command::SetStep { track, step, level } => {
                if (track as usize) < NUM_TRACKS && (step as usize) < MAX_STEPS {
                    self.pattern[track as usize][step as usize] = level;
                }
            }
            Command::SetTrack { track, steps } => {
                if (track as usize) < NUM_TRACKS {
                    self.pattern[track as usize] = steps;
                }
            }
            Command::Play => {
                self.playing = true;
                self.next_step = 0;
                self.next_step_at = self.pos as f64;
            }
            Command::Stop => {
                if self.playing {
                    self.playing = false;
                    emit(Feedback::Stopped { time: self.time() });
                }
            }
            Command::Trigger { track, velocity } => {
                self.trigger(track as usize, velocity, None, emit);
            }
        }
    }

    fn voice_params(&self, track: usize) -> VoiceParams {
        VoiceParams {
            tune: self.params[voice_param(track, VoiceParam::Tune)],
            decay: self.params[voice_param(track, VoiceParam::Decay)],
            tone: self.params[voice_param(track, VoiceParam::Tone)],
        }
    }

    fn trigger(&mut self, track: usize, velocity: f32, step: Option<u32>, emit: &mut impl FnMut(Feedback)) {
        if track >= NUM_TRACKS {
            return;
        }
        // Closed hat chokes open hat, as on the 808.
        if track == Voice::ClosedHat.index() {
            self.voices[Voice::OpenHat.index()].choke();
        }
        let p = self.voice_params(track);
        self.voices[track].trigger(velocity.clamp(0.0, 1.0), p);
        emit(Feedback::Trigger { track: track as u8, velocity, time: self.time(), step });
    }

    /// Length of step `index` in samples, including swing. Swing delays every
    /// second 16th: pairs keep their total length, the first note of each pair
    /// takes 50%..75% of it as swing goes 0..1.
    fn step_samples(&self, index: u32) -> f64 {
        let tempo = self.params[TEMPO].clamp(20.0, 300.0) as f64;
        let sixteenth = 60.0 / tempo / 4.0 * self.sr as f64;
        let ratio = 0.5 + 0.25 * self.params[SWING].clamp(0.0, 1.0) as f64;
        if index % 2 == 0 { 2.0 * sixteenth * ratio } else { 2.0 * sixteenth * (1.0 - ratio) }
    }

    fn fire_step(&mut self, emit: &mut impl FnMut(Feedback)) {
        let length = (self.params[LENGTH].round() as u32).clamp(1, MAX_STEPS as u32);
        if self.next_step >= length {
            self.next_step = 0;
        }
        let step = self.next_step;
        emit(Feedback::Step { step, time: self.time() });
        for track in 0..NUM_TRACKS {
            let level = self.pattern[track][step as usize];
            if level != STEP_OFF {
                let vel = if level == STEP_ACCENT { VELOCITY_ACCENT } else { VELOCITY_ON };
                self.trigger(track, vel, Some(step), emit);
            }
        }
        self.next_step_at += self.step_samples(step);
        self.next_step = step + 1;
    }

    /// Render interleaved audio into `out` (`channels` >= 1; channels beyond
    /// 2 are filled with silence).
    pub fn render(&mut self, out: &mut [f32], channels: usize, emit: &mut impl FnMut(Feedback)) {
        let channels = channels.max(1);
        let frames = out.len() / channels;
        let mut frame = 0;
        while frame < frames {
            let mut n = frames - frame;
            if self.playing {
                let until = (self.next_step_at - self.pos as f64).ceil();
                if until <= 0.0 {
                    self.fire_step(emit);
                    continue;
                }
                n = n.min(until as usize);
            }
            self.render_frames(&mut out[frame * channels..(frame + n) * channels], channels);
            frame += n;
            self.pos += n as u64;
            self.tick_meters(n as u32, emit);
        }
    }

    /// Per-channel (gain, left, right) targets from volume, pan, mute, and
    /// solo, plus the master gain.
    fn mix_targets(&self) -> ([(f32, f32, f32); NUM_TRACKS], f32) {
        let any_solo = (0..NUM_TRACKS).any(|t| self.params[mixer_param(t, MixerParam::Solo)] >= 0.5);
        let mut targets = [(0.0f32, 0.0f32, 0.0f32); NUM_TRACKS];
        for (t, target) in targets.iter_mut().enumerate() {
            let muted = self.params[mixer_param(t, MixerParam::Mute)] >= 0.5;
            let soloed = self.params[mixer_param(t, MixerParam::Solo)] >= 0.5;
            let audible = !muted && (!any_solo || soloed);
            let gain = if audible { volume_to_gain(self.params[mixer_param(t, MixerParam::Volume)]) } else { 0.0 };
            let pan = self.params[mixer_param(t, MixerParam::Pan)].clamp(-1.0, 1.0);
            let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
            *target = (gain, angle.cos(), angle.sin());
        }
        (targets, volume_to_gain(self.params[MASTER_VOLUME]))
    }

    fn render_frames(&mut self, out: &mut [f32], channels: usize) {
        let (targets, master_target) = self.mix_targets();

        for f in out.chunks_exact_mut(channels) {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for t in 0..NUM_TRACKS {
                let s = self.voices[t].process();
                let ch = &mut self.channels[t];
                let (gt, plt, prt) = targets[t];
                let g = ch.gain.next(gt);
                let pl = ch.pan_l.next(plt);
                let pr = ch.pan_r.next(prt);
                let x = s * g;
                ch.peak = ch.peak.max(x.abs());
                l += x * pl;
                r += x * pr;
            }
            let m = self.master.next(master_target);
            l = soft_clip(l * m);
            r = soft_clip(r * m);
            self.master_peak[0] = self.master_peak[0].max(l.abs());
            self.master_peak[1] = self.master_peak[1].max(r.abs());
            if channels == 1 {
                f[0] = 0.5 * (l + r);
            } else {
                f[0] = l;
                f[1] = r;
                for x in &mut f[2..] {
                    *x = 0.0;
                }
            }
        }
    }

    fn tick_meters(&mut self, frames: u32, emit: &mut impl FnMut(Feedback)) {
        if self.meter_countdown > frames {
            self.meter_countdown -= frames;
            return;
        }
        self.meter_countdown = (self.sr / METER_HZ) as u32;
        let mut tracks = [0.0; NUM_TRACKS];
        for (t, ch) in self.channels.iter_mut().enumerate() {
            tracks[t] = ch.peak;
            ch.peak = 0.0;
        }
        emit(Feedback::Meters { tracks, master: self.master_peak });
        self.master_peak = [0.0; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fours_protocol::STEP_ON;

    fn render_secs(e: &mut Engine, secs: f32, fb: &mut Vec<Feedback>) -> Vec<f32> {
        let frames = (secs * e.sample_rate() as f32) as usize;
        let mut out = vec![0.0; frames * 2];
        for chunk in out.chunks_mut(512) {
            e.render(chunk, 2, &mut |f| fb.push(f));
        }
        out
    }

    #[test]
    fn every_voice_sounds_and_decays() {
        for v in Voice::ALL {
            let mut e = Engine::new(48000);
            let mut fb = vec![];
            e.apply(Command::Trigger { track: v.index() as u8, velocity: 1.0 }, &mut |f| fb.push(f));
            let out = render_secs(&mut e, 0.2, &mut fb);
            let peak = out.iter().fold(0.0f32, |a, x| a.max(x.abs()));
            assert!(peak > 0.02, "{v:?} too quiet: {peak}");
            assert!(peak <= 1.0, "{v:?} clipped: {peak}");
            let tail = render_secs(&mut e, 3.0, &mut fb);
            let end_peak = tail[tail.len() - 4800..].iter().fold(0.0f32, |a, x| a.max(x.abs()));
            assert!(end_peak < 0.001, "{v:?} did not decay: {end_peak}");
        }
    }

    #[test]
    fn sequencer_fires_on_time() {
        let mut e = Engine::new(48000);
        let mut fb = vec![];
        let mut steps = [STEP_OFF; MAX_STEPS];
        steps[0] = STEP_ON;
        steps[4] = STEP_ON;
        e.apply(Command::SetTrack { track: 0, steps }, &mut |_| {});
        e.apply(Command::Play, &mut |_| {});
        // 120 bpm: 16th = 0.125s, one 16-step bar = 2s.
        render_secs(&mut e, 4.0, &mut fb);
        let kicks: Vec<f64> = fb
            .iter()
            .filter_map(|f| match f {
                Feedback::Trigger { track: 0, time, .. } => Some(*time),
                _ => None,
            })
            .collect();
        let expect = [0.0, 0.5, 2.0, 2.5];
        assert_eq!(kicks.len(), expect.len(), "{kicks:?}");
        for (k, x) in kicks.iter().zip(expect) {
            assert!((k - x).abs() < 1.0 / 48000.0 * 2.0, "{k} vs {x}");
        }
        let steps: Vec<u32> = fb.iter().filter_map(|f| if let Feedback::Step { step, .. } = f { Some(*step) } else { None }).collect();
        assert_eq!(&steps[..17], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0]);
    }

    #[test]
    fn swing_delays_offbeats() {
        let mut e = Engine::new(48000);
        let mut fb = vec![];
        e.apply(Command::SetParam { id: SWING, value: 1.0 }, &mut |_| {});
        e.apply(Command::Play, &mut |_| {});
        render_secs(&mut e, 0.6, &mut fb);
        let times: Vec<f64> = fb.iter().filter_map(|f| if let Feedback::Step { time, .. } = f { Some(*time) } else { None }).collect();
        // Pair = 0.25s; full swing puts the offbeat at 75% of the pair.
        assert!((times[1] - 0.1875).abs() < 1e-3, "{times:?}");
        assert!((times[2] - 0.25).abs() < 1e-3, "{times:?}");
    }
}
