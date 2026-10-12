//! Offline (faster than real time) rendering and simple audio analysis, used
//! for exports and for agent-driven validation of what the engine produces.

use crate::effect;
use crate::engine::{Command, Engine, Feedback, ModRoute, ParamTarget, Placement};
use crate::modulator;
use crate::instrument;
use crate::params::{NUM_GLOBALS, TEMPO};
use fours_protocol::{ClipEvent, DeviceType, InstrumentType, MAX_INSERTS, RenderTrigger, Voice};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct RenderInstrument {
    pub id: String,
    pub kind: InstrumentType,
    /// Parameter values in `instrument::params` order.
    pub params: Vec<f32>,
    /// Channel index (0-based) per output, main first.
    pub routes: Vec<Option<u8>>,
    /// The clip it loops instead of its arrangement, if launched (an index
    /// in the clip table).
    pub launched: Option<u16>,
    /// Its arrangement, by start.
    pub placements: Vec<Placement>,
    /// A sampler's samples by pad (0-based).
    pub samples: Vec<(u8, std::sync::Arc<[f32]>)>,
}

/// A clip in the engine's clip table: length in ticks and events.
#[derive(Clone, Debug)]
pub struct RenderClip {
    pub index: u16,
    pub length: u32,
    pub events: Vec<ClipEvent>,
    pub lanes: Vec<RenderLane>,
}

/// A clip's automation lane, resolved: its target, the target's value set
/// by hand, its range, and its (tick, value) points in tick order.
#[derive(Clone, Debug)]
pub struct RenderLane {
    pub target: ParamTarget,
    pub base: f32,
    pub min: f32,
    pub max: f32,
    pub points: Vec<(u32, f32)>,
}

/// An audio effect in its engine slot, with its parameter values (in
/// `effect::params` order).
#[derive(Clone, Debug)]
pub struct RenderEffect {
    pub slot: u8,
    pub kind: DeviceType,
    pub params: Vec<f32>,
}

/// Everything needed to render the current project, copied out of the
/// daemon's state.
#[derive(Clone, Debug)]
pub struct RenderSpec {
    pub globals: [f32; NUM_GLOBALS],
    /// Active channels: (0-based index, parameter values).
    pub channels: Vec<(u8, [f32; crate::params::CHANNEL_PARAMS])>,
    pub instruments: Vec<RenderInstrument>,
    pub clips: Vec<RenderClip>,
    pub effects: Vec<RenderEffect>,
    /// Return channels (0-based) and the send levels into them (from, to,
    /// level).
    pub returns: Vec<u8>,
    pub sends: Vec<(u8, u8, f32)>,
    /// Modulators (as effects: slot, kind, parameter values) and the
    /// modulation routes by index.
    pub modulators: Vec<RenderEffect>,
    pub routes: Vec<ModRoute>,
    /// Insert chains: engine chain index (`MASTER_CHAIN` for the master)
    /// -> effect slots in order.
    pub chains: Vec<(u8, Vec<u8>)>,
    /// Where play starts from.
    pub start: u32,
}

impl RenderSpec {
    /// Build an engine in this state (non-real-time).
    pub fn build(&self, sample_rate: u32) -> Engine {
        let mut e = Engine::new(sample_rate);
        let mut fb = |_| {};
        for (i, v) in self.globals.iter().enumerate() {
            let _ = e.apply(Command::SetParam { target: ParamTarget::Global(i), value: *v }, &mut fb);
        }
        for (ch, params) in &self.channels {
            let _ = e.apply(Command::SetChannelActive { ch: *ch, active: true }, &mut fb);
            for (i, v) in params.iter().enumerate() {
                let target = ParamTarget::Channel { ch: *ch, index: i as u8 };
                let _ = e.apply(Command::SetParam { target, value: *v }, &mut fb);
            }
        }
        for (slot, inst) in self.instruments.iter().enumerate() {
            let slot = slot as u8;
            let instrument = instrument::make(inst.kind, sample_rate as f32);
            let _ = e.apply(Command::AddInstrument { slot, instrument }, &mut fb);
            for (i, v) in inst.params.iter().enumerate() {
                let target = ParamTarget::Instrument { slot, index: i as u16 };
                let _ = e.apply(Command::SetParam { target, value: *v }, &mut fb);
            }
            for (o, ch) in inst.routes.iter().enumerate() {
                let _ = e.apply(Command::SetRoute { slot, output: o as u8, channel: *ch }, &mut fb);
            }
            for (pad, sample) in &inst.samples {
                let _ = e.apply(Command::SetSample { slot, pad: *pad, sample: Some(sample.clone()) }, &mut fb);
            }
            if inst.launched.is_some() {
                let _ = e.apply(Command::Launch { slot, clip: inst.launched, now: true }, &mut fb);
            }
            for placement in &inst.placements {
                let _ = e.apply(Command::AddPlacement { slot, placement: *placement }, &mut fb);
            }
        }
        for ch in &self.returns {
            let _ = e.apply(Command::SetChannelReturn { ch: *ch, ret: true }, &mut fb);
        }
        for (ch, ret, v) in &self.sends {
            let _ = e.apply(Command::SetParam { target: ParamTarget::Send { ch: *ch, ret: *ret }, value: *v }, &mut fb);
        }
        for fx in &self.effects {
            let _ = e.apply(Command::AddEffect { slot: fx.slot, effect: effect::make(fx.kind, sample_rate as f32) }, &mut fb);
            for (i, v) in fx.params.iter().enumerate() {
                let target = ParamTarget::Effect { slot: fx.slot, index: i as u16 };
                let _ = e.apply(Command::SetParam { target, value: *v }, &mut fb);
            }
        }
        for m in &self.modulators {
            let _ = e.apply(Command::AddModulator { slot: m.slot, modulator: modulator::make(m.kind) }, &mut fb);
            for (i, v) in m.params.iter().enumerate() {
                let target = ParamTarget::Modulator { slot: m.slot, index: i as u16 };
                let _ = e.apply(Command::SetParam { target, value: *v }, &mut fb);
            }
        }
        for (i, r) in self.routes.iter().enumerate() {
            let _ = e.apply(Command::SetModRoute { index: i as u8, route: Some(*r) }, &mut fb);
        }
        for (ch, chain) in &self.chains {
            let mut slots = [0u8; MAX_INSERTS];
            let len = chain.len().min(MAX_INSERTS);
            slots[..len].copy_from_slice(&chain[..len]);
            let _ = e.apply(Command::SetChain { ch: *ch, slots, len: len as u8 }, &mut fb);
        }
        for c in &self.clips {
            let _ = e.apply(Command::SetClipLength { clip: c.index, length: c.length }, &mut fb);
            for event in &c.events {
                let _ = e.apply(Command::AddEvent { clip: c.index, event: *event }, &mut fb);
            }
            for (k, l) in c.lanes.iter().enumerate() {
                let len = l.points.len() as u8;
                let lane = Command::SetLane { clip: c.index, lane: k as u8, target: Some(l.target), base: l.base, min: l.min, max: l.max, len };
                let _ = e.apply(lane, &mut fb);
                for (i, (tick, value)) in l.points.iter().enumerate() {
                    let point = Command::SetLanePoint { clip: c.index, lane: k as u8, index: i as u8, tick: *tick, value: *value };
                    let _ = e.apply(point, &mut fb);
                }
            }
        }
        let _ = e.apply(Command::Locate { tick: self.start }, &mut fb);
        e.snap();
        e
    }
}

/// A note played live during an offline render (as a key would be).
#[derive(Clone, Copy, Debug)]
pub struct LiveNote {
    /// Seconds from play.
    pub time: f64,
    pub slot: u8,
    pub note: u8,
    pub velocity: f32,
    /// Seconds held.
    pub duration: f64,
}

pub struct OfflineRender {
    pub sample_rate: u32,
    /// Interleaved stereo.
    pub samples: Vec<f32>,
    pub triggers: Vec<RenderTrigger>,
    /// `Step` and `Live` feedback, in order (what a recording sees).
    pub feedback: Vec<Feedback>,
}

/// Render `bars` 4/4 bars (16 steps each, at the current tempo) from step 1,
/// plus `tail` seconds, playing `input` notes at their times
/// (sample-accurately).
pub fn render_graph(spec: &RenderSpec, sample_rate: u32, bars: f64, tail: f64, input: &[LiveNote]) -> OfflineRender {
    let mut engine = spec.build(sample_rate);
    let tempo = spec.globals[TEMPO].clamp(20.0, 300.0) as f64;
    let bar_secs = 4.0 * 60.0 / tempo;
    let play_frames = (bars.max(0.0) * bar_secs * sample_rate as f64).round() as usize;
    let tail_frames = (tail.max(0.0) * sample_rate as f64).round() as usize;
    let total = play_frames + tail_frames;

    // Note ons and offs by frame; a note's off before a later on at the
    // same frame.
    let at = |t: f64| ((t.max(0.0) * sample_rate as f64).round() as usize).min(total);
    let mut cmds: Vec<(usize, bool, Command)> = Vec::new();
    for n in input {
        let on = Command::NoteOn { slot: n.slot, note: n.note, velocity: n.velocity, gate: false };
        cmds.push((at(n.time), true, on));
        cmds.push((at(n.time + n.duration.max(0.0)), false, Command::NoteOff { slot: n.slot, note: n.note }));
    }
    cmds.push((play_frames, false, Command::Stop));
    cmds.sort_by_key(|(f, on, _)| (*f, *on));

    let mut fb = Vec::new();
    let mut samples = vec![0.0f32; total * 2];
    let _ = engine.apply(Command::Play { count_in: 0 }, &mut |f| fb.push(f));
    let mut frame = 0;
    let mut pending = cmds.into_iter().peekable();
    while frame < total || pending.peek().is_some() {
        while let Some((_, _, cmd)) = pending.next_if(|(f, _, _)| *f <= frame) {
            let _ = engine.apply(cmd, &mut |f| fb.push(f));
        }
        let until = pending.peek().map(|(f, _, _)| *f).unwrap_or(total).min(frame + 1024).min(total);
        if until <= frame {
            if frame >= total {
                break;
            }
            continue;
        }
        engine.render(&mut samples[frame * 2..until * 2], 2, &mut |f| fb.push(f));
        frame = until;
    }

    let feedback = fb.iter().filter(|f| matches!(f, Feedback::Step { .. } | Feedback::Live { .. })).copied().collect();
    let triggers = fb
        .into_iter()
        .filter_map(|f| match f {
            Feedback::Trigger { slot, voice, note, velocity, time, step: Some(step) } => Some(RenderTrigger {
                time,
                step,
                instrument: spec.instruments.get(slot as usize)?.id.clone(),
                voice: voice.and_then(|v| Voice::from_index(v as usize)),
                note,
                velocity,
            }),
            _ => None,
        })
        .collect();
    OfflineRender { sample_rate, samples, triggers, feedback }
}

/// Peak and RMS of one lane (0 = left, 1 = right) of interleaved stereo.
pub fn lane_level(samples: &[f32], lane: usize) -> (f32, f32) {
    let lane: Vec<f32> = samples.iter().skip(lane).step_by(2).copied().collect();
    let peak = lane.iter().fold(0.0f32, |a, x| a.max(x.abs()));
    let rms = if lane.is_empty() {
        0.0
    } else {
        (lane.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / lane.len() as f64).sqrt() as f32
    };
    (peak, rms)
}

pub struct Analysis {
    pub peak: f32,
    pub rms: f32,
    pub onsets: Vec<f64>,
}

/// Peak, RMS, and energy-based onset detection on interleaved stereo audio.
/// Onsets are reported where short-term energy jumps well above the recent
/// average; good enough to check that hits land where expected.
pub fn analyze(samples: &[f32], sample_rate: u32) -> Analysis {
    let peak = samples.iter().fold(0.0f32, |a, x| a.max(x.abs()));
    let rms = if samples.is_empty() {
        0.0
    } else {
        (samples.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / samples.len() as f64).sqrt() as f32
    };

    // Two detection bands: broadband (catches kicks/toms) and the first
    // difference, which emphasizes high frequencies so hats and snares are
    // found even under a ringing kick. Each band is reduced to a peak
    // envelope over a window longer than one period of the lowest drum
    // (~49 Hz), so a decaying low sine reads as smoothly falling instead of
    // pulsing with its waveform. An onset is a frame whose envelope jumps
    // well above the previous frame's.
    const HOP: usize = 128;
    let window = (0.025 * sample_rate as f64) as usize;
    let mono: Vec<f32> = samples.chunks_exact(2).map(|f| 0.5 * (f[0] + f[1])).collect();
    let diff: Vec<f32> = std::iter::once(0.0).chain(mono.windows(2).map(|w| w[1] - w[0])).collect();
    let envelope = |x: &[f32]| -> Vec<f32> {
        (0..x.len().div_ceil(HOP))
            .map(|i| {
                let end = ((i + 1) * HOP).min(x.len());
                let start = (i * HOP).saturating_sub(window);
                x[start..end].iter().fold(0.0f32, |a, v| a.max(v.abs()))
            })
            .collect()
    };
    let bands = [(envelope(&mono), 0.003f32), (envelope(&diff), 0.001f32)];
    let jumps = |(env, floor): &(Vec<f32>, f32), i: usize| -> bool {
        let prev = if i == 0 { 0.0 } else { env[i - 1] };
        env[i] > *floor && env[i] > prev * 2.0
    };
    let mut onsets = Vec::new();
    let min_gap = (0.04 * sample_rate as f64 / HOP as f64) as usize;
    let mut last: Option<usize> = None;
    for i in 0..bands[0].0.len() {
        if bands.iter().any(|b| jumps(b, i)) && last.is_none_or(|l| i - l >= min_gap) {
            onsets.push((i * HOP) as f64 / sample_rate as f64);
            last = Some(i);
        }
    }
    Analysis { peak, rms, onsets }
}

pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), hound::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample(*s)?;
    }
    w.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{channel_defaults, global_defaults};
    use crate::tr808::{DECAY, VOICE_PARAMS};
    use fours_protocol::{MAX_STEPS, NUM_TRACKS, STEP_ACCENT, STEP_OFF, STEP_ON, drum_event};

    /// A `tr808` on channel index 0 playing a drum grid (as clip events).
    fn drums_spec(pattern: [[u8; MAX_STEPS]; NUM_TRACKS]) -> RenderSpec {
        let events = Voice::ALL
            .iter()
            .flat_map(|v| (0..MAX_STEPS).filter_map(move |s| drum_event(*v, s, pattern[v.index()][s])))
            .collect::<Vec<_>>();
        let events = fours_protocol::normalize_events(&events).unwrap();
        let params = instrument::params(InstrumentType::Tr808, "drums").iter().map(|p| p.default as f32).collect();
        let mut routes = vec![None; 1 + NUM_TRACKS];
        routes[0] = Some(0);
        RenderSpec {
            globals: global_defaults(),
            channels: vec![(0u8, channel_defaults())],
            instruments: vec![RenderInstrument {
                id: "drums".into(),
                kind: InstrumentType::Tr808,
                params,
                routes,
                launched: Some(0),
                placements: Vec::new(),
                samples: Vec::new(),
            }],
            clips: vec![RenderClip { index: 0, length: fours_protocol::DEFAULT_CLIP_LENGTH, events, lanes: Vec::new() }],
            effects: Vec::new(),
            chains: Vec::new(),
            returns: Vec::new(),
            sends: Vec::new(),
            modulators: Vec::new(),
            routes: Vec::new(),
            start: 0,
        }
    }

    #[test]
    fn render_and_detect_four_on_the_floor() {
        let mut pattern = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
        for s in [0, 4, 8, 12] {
            pattern[0][s] = STEP_ON;
        }
        let r = render_graph(&drums_spec(pattern), 48000, 1.0, 0.0, &[]);
        assert_eq!(r.samples.len(), 48000 * 2 * 2); // 2s at 120 bpm, stereo
        assert_eq!(r.triggers.len(), 4);
        assert_eq!(r.triggers[0].instrument, "drums");
        let a = analyze(&r.samples, r.sample_rate);
        assert!(a.peak > 0.1);
        assert_eq!(a.onsets.len(), 4, "{:?}", a.onsets);
        for (o, t) in a.onsets.iter().zip(&r.triggers) {
            assert!((o - t.time).abs() < 0.01, "onset {o} vs trigger {}", t.time);
        }
    }

    #[test]
    fn detects_mixed_pattern_onsets() {
        let mut pattern = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
        let set = |p: &mut [[u8; MAX_STEPS]; NUM_TRACKS], v: Voice, s: &str| {
            let steps = fours_protocol::parse_steps(s).unwrap();
            p[v.index()][..steps.len()].copy_from_slice(&steps);
        };
        set(&mut pattern, Voice::Kick, "X---x---X---x---");
        set(&mut pattern, Voice::Snare, "----x-------x---");
        set(&mut pattern, Voice::ClosedHat, "x-x-x-x-x-x-x-xX");
        set(&mut pattern, Voice::Cowbell, "---------------X");
        let mut spec = drums_spec(pattern);
        spec.globals[TEMPO] = 128.0;
        let r = render_graph(&spec, 48000, 1.0, 0.0, &[]);
        let mut hit_times: Vec<f64> = r.triggers.iter().map(|t| t.time).collect();
        hit_times.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        let a = analyze(&r.samples, r.sample_rate);
        assert_eq!(a.onsets.len(), hit_times.len(), "onsets {:?} vs hits {:?}", a.onsets, hit_times);
        for (o, t) in a.onsets.iter().zip(&hit_times) {
            assert!((o - t).abs() < 0.01, "onset {o} vs hit {t}");
        }
    }

    /// Long, low sounds must not produce extra onsets as they ring out.
    #[test]
    fn long_tails_do_not_retrigger() {
        for voice in [Voice::Kick, Voice::LowTom, Voice::OpenHat, Voice::Cowbell, Voice::Clap] {
            let mut pattern = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
            pattern[voice.index()][0] = STEP_ACCENT;
            pattern[voice.index()][8] = STEP_ON;
            let mut spec = drums_spec(pattern);
            spec.instruments[0].params[voice.index() * VOICE_PARAMS + DECAY] = 1.0;
            let r = render_graph(&spec, 48000, 1.0, 1.0, &[]);
            let a = analyze(&r.samples, r.sample_rate);
            assert_eq!(a.onsets.len(), 2, "{voice:?}: {:?}", a.onsets);
        }
    }

    #[test]
    fn silence_has_no_onsets() {
        let r = render_graph(&drums_spec([[STEP_OFF; MAX_STEPS]; NUM_TRACKS]), 48000, 1.0, 0.0, &[]);
        let a = analyze(&r.samples, r.sample_rate);
        assert_eq!(a.peak, 0.0);
        assert!(a.onsets.is_empty());
    }
}
