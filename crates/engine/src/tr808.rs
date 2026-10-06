//! `tr808`: eight drum voices with a step pattern per voice and an internal
//! mix. Output 0 is the stereo main mix (each voice after level, mute, and
//! constant-power pan). Outputs 1..=8 are mono direct outs, one per voice
//! (after level and mute, before pan). A direct out that is routed leaves the
//! main mix, like a normalled individual-out jack.

use crate::dsp::Smoother;
use crate::instrument::{Hit, Instrument, MAX_BLOCK};
use crate::params::{cont, mono_pan, toggle, volume_to_gain};
use crate::voices::{DrumVoice, VoiceParams, default_decay, make_voice};
use fours_protocol::{MAX_STEPS, NUM_TRACKS, OutputInfo, OutputWidth, ParamInfo, STEP_ACCENT, STEP_OFF, Voice};

pub const VELOCITY_ON: f32 = 0.7;
pub const VELOCITY_ACCENT: f32 = 1.0;

/// Per-voice parameters, in index order (`track * VOICE_PARAMS + p`).
pub const TUNE: usize = 0;
pub const DECAY: usize = 1;
pub const TONE: usize = 2;
pub const LEVEL: usize = 3;
pub const PAN: usize = 4;
pub const MUTE: usize = 5;
pub const VOICE_PARAMS: usize = 6;

struct Strip {
    gain: Smoother,
    pan_l: Smoother,
    pan_r: Smoother,
}

pub struct Tr808 {
    voices: Vec<Box<dyn DrumVoice>>,
    strips: Vec<Strip>,
    params: [f32; NUM_TRACKS * VOICE_PARAMS],
    pattern: [[u8; MAX_STEPS]; NUM_TRACKS],
    routed: [bool; NUM_TRACKS],
    /// Output buffers: main, then one per voice. Interleaved stereo.
    bufs: Vec<Vec<f32>>,
}

impl Tr808 {
    pub fn new(sr: f32) -> Self {
        let mut params = [0.0; NUM_TRACKS * VOICE_PARAMS];
        for (i, p) in Self::params("drums").iter().enumerate() {
            params[i] = p.default as f32;
        }
        let strips = (0..NUM_TRACKS)
            .map(|t| {
                let (l, r) = mono_pan(params[t * VOICE_PARAMS + PAN]);
                Strip {
                    gain: Smoother::new(sr, 0.01, volume_to_gain(params[t * VOICE_PARAMS + LEVEL])),
                    pan_l: Smoother::new(sr, 0.01, l),
                    pan_r: Smoother::new(sr, 0.01, r),
                }
            })
            .collect();
        Self {
            voices: Voice::ALL.iter().map(|v| make_voice(*v, sr)).collect(),
            strips,
            params,
            pattern: [[STEP_OFF; MAX_STEPS]; NUM_TRACKS],
            routed: [false; NUM_TRACKS],
            bufs: (0..=NUM_TRACKS).map(|_| vec![0.0; MAX_BLOCK * 2]).collect(),
        }
    }

    pub fn params(id: &str) -> Vec<ParamInfo> {
        let mut r = Vec::with_capacity(NUM_TRACKS * VOICE_PARAMS);
        for v in Voice::ALL {
            let (vid, l) = (v.id(), v.label());
            r.push(cont(format!("{id}.{vid}.tune"), format!("{l} Tune"), -12.0, 12.0, 0.0, Some("st")));
            r.push(cont(format!("{id}.{vid}.decay"), format!("{l} Decay"), 0.0, 1.0, default_decay(v), None));
            r.push(cont(format!("{id}.{vid}.tone"), format!("{l} Tone"), 0.0, 1.0, 0.5, None));
            r.push(cont(format!("{id}.{vid}.level"), format!("{l} Level"), 0.0, 1.0, 0.8, None));
            r.push(cont(format!("{id}.{vid}.pan"), format!("{l} Pan"), -1.0, 1.0, 0.0, None));
            r.push(toggle(format!("{id}.{vid}.mute"), format!("{l} Mute")));
        }
        r
    }

    pub fn outputs(id: &str, name: &str) -> Vec<OutputInfo> {
        let mut o = vec![OutputInfo { source: id.to_string(), label: name.to_string(), width: OutputWidth::Stereo }];
        for v in Voice::ALL {
            o.push(OutputInfo {
                source: format!("{id}.{}", v.id()),
                label: format!("{name} {}", v.label()),
                width: OutputWidth::Mono,
            });
        }
        o
    }

    fn p(&self, track: usize, p: usize) -> f32 {
        self.params[track * VOICE_PARAMS + p]
    }

    fn play(&mut self, track: usize, velocity: f32) {
        // Closed hat chokes open hat, as on the 808.
        if track == Voice::ClosedHat.index() {
            self.voices[Voice::OpenHat.index()].choke();
        }
        let vp = VoiceParams { tune: self.p(track, TUNE), decay: self.p(track, DECAY), tone: self.p(track, TONE) };
        self.voices[track].trigger(velocity.clamp(0.0, 1.0), vp);
    }

    /// (gain, left, right) targets per voice.
    fn targets(&self) -> [(f32, f32, f32); NUM_TRACKS] {
        let mut t = [(0.0, 0.0, 0.0); NUM_TRACKS];
        for (i, x) in t.iter_mut().enumerate() {
            let gain = if self.p(i, MUTE) >= 0.5 { 0.0 } else { volume_to_gain(self.p(i, LEVEL)) };
            let (l, r) = mono_pan(self.p(i, PAN));
            *x = (gain, l, r);
        }
        t
    }
}

impl Instrument for Tr808 {
    fn num_outputs(&self) -> usize {
        1 + NUM_TRACKS
    }

    fn output_width(&self, output: usize) -> OutputWidth {
        if output == 0 { OutputWidth::Stereo } else { OutputWidth::Mono }
    }

    fn set_param(&mut self, index: usize, value: f32) {
        if index < self.params.len() {
            self.params[index] = value;
        }
    }

    fn set_routed(&mut self, output: usize, routed: bool) {
        if (1..=NUM_TRACKS).contains(&output) {
            self.routed[output - 1] = routed;
        }
    }

    fn set_drum_step(&mut self, track: usize, step: usize, level: u8) {
        if track < NUM_TRACKS && step < MAX_STEPS {
            self.pattern[track][step] = level;
        }
    }

    fn set_drum_track(&mut self, track: usize, steps: &[u8; MAX_STEPS]) {
        if track < NUM_TRACKS {
            self.pattern[track] = *steps;
        }
    }

    fn on_step(&mut self, step: usize, _step_samples: f64, emit: &mut dyn FnMut(Hit)) {
        for track in 0..NUM_TRACKS {
            let level = self.pattern[track][step];
            if level != STEP_OFF {
                let velocity = if level == STEP_ACCENT { VELOCITY_ACCENT } else { VELOCITY_ON };
                self.play(track, velocity);
                emit(Hit { voice: Some(track as u8), note: None, velocity });
            }
        }
    }

    fn trigger(&mut self, voice: usize, velocity: f32) -> bool {
        if voice >= NUM_TRACKS {
            return false;
        }
        self.play(voice, velocity);
        true
    }

    fn render(&mut self, frames: usize) {
        let frames = frames.min(MAX_BLOCK);
        let targets = self.targets();
        for b in &mut self.bufs {
            b[..frames * 2].fill(0.0);
        }
        let (main, direct) = self.bufs.split_at_mut(1);
        let main = &mut main[0];
        for f in 0..frames {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            for t in 0..NUM_TRACKS {
                let s = self.voices[t].process();
                let st = &mut self.strips[t];
                let (gt, plt, prt) = targets[t];
                let x = s * st.gain.next(gt);
                let pl = st.pan_l.next(plt);
                let pr = st.pan_r.next(prt);
                if self.routed[t] {
                    direct[t][f * 2] = x;
                } else {
                    l += x * pl;
                    r += x * pr;
                }
            }
            main[f * 2] = l;
            main[f * 2 + 1] = r;
        }
    }

    fn output(&self, output: usize) -> &[f32] {
        &self.bufs[output]
    }

    fn snap(&mut self) {
        let targets = self.targets();
        for (st, (g, l, r)) in self.strips.iter_mut().zip(targets) {
            st.gain.value = g;
            st.pan_l.value = l;
            st.pan_r.value = r;
        }
    }
}
