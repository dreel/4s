//! The engine: a shared step clock, a fixed table of instrument slots, and a
//! fixed pool of stereo mixer channels. `Engine::render` and `Engine::apply`
//! are called from the audio thread (or offline) and must not allocate, lock,
//! or block. Instruments are built and dropped on the control side.
//!
//! Signal flow per block: each instrument renders its outputs; each routed
//! output is summed into its channel with its own pan law (constant-power pan
//! for mono, balance for stereo); each channel applies its fader and
//! mute/solo; channels sum into the master, which has a soft clipper.

use crate::dsp::{Smoother, soft_clip};
use crate::instrument::{Instrument, MAX_BLOCK, MAX_OUTPUTS};
use crate::params::*;
use fours_protocol::{MAX_CHANNELS, MAX_INSTRUMENTS, MAX_STEPS, NoteStep, OutputWidth};

/// Meter feedback rate.
const METER_HZ: f32 = 30.0;

/// Where a parameter lives inside the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamTarget {
    Global(usize),
    Instrument { slot: u8, index: u16 },
    /// `ch` is the 0-based channel index (channel number - 1).
    Channel { ch: u8, index: u8 },
}

/// Control messages into the engine. Channel fields are 0-based indexes.
pub enum Command {
    SetParam { target: ParamTarget, value: f32 },
    AddInstrument { slot: u8, instrument: Box<dyn Instrument> },
    RemoveInstrument { slot: u8 },
    SetRoute { slot: u8, output: u8, channel: Option<u8> },
    SetChannelActive { ch: u8, active: bool },
    SetDrumStep { slot: u8, track: u8, step: u8, level: u8 },
    SetDrumTrack { slot: u8, track: u8, steps: [u8; MAX_STEPS] },
    SetNotes { slot: u8, steps: [NoteStep; MAX_STEPS] },
    /// A note; with `gate` it releases after half a step at the current tempo.
    NoteOn { slot: u8, note: u8, velocity: f32, gate: bool },
    NoteOff { slot: u8, note: u8 },
    Play,
    Stop,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Command::SetParam { target, value } => write!(f, "SetParam({target:?}, {value})"),
            Command::AddInstrument { slot, .. } => write!(f, "AddInstrument({slot})"),
            Command::RemoveInstrument { slot } => write!(f, "RemoveInstrument({slot})"),
            Command::SetRoute { slot, output, channel } => write!(f, "SetRoute({slot}, {output}, {channel:?})"),
            Command::SetChannelActive { ch, active } => write!(f, "SetChannelActive({ch}, {active})"),
            Command::SetDrumStep { slot, track, step, level } => {
                write!(f, "SetDrumStep({slot}, {track}, {step}, {level})")
            }
            Command::SetDrumTrack { slot, track, .. } => write!(f, "SetDrumTrack({slot}, {track})"),
            Command::SetNotes { slot, .. } => write!(f, "SetNotes({slot})"),
            Command::NoteOn { slot, note, .. } => write!(f, "NoteOn({slot}, {note})"),
            Command::NoteOff { slot, note } => write!(f, "NoteOff({slot}, {note})"),
            Command::Play => write!(f, "Play"),
            Command::Stop => write!(f, "Stop"),
        }
    }
}

/// Messages out of the engine. Times are engine time in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Feedback {
    Step { step: u32, time: f64 },
    Trigger { slot: u8, voice: Option<u8>, note: Option<u8>, velocity: f32, time: f64, step: Option<u32> },
    Stopped { time: f64 },
    /// Peak (left, right) per channel index since the last meter message.
    Meters { channels: [[f32; 2]; MAX_CHANNELS], master: [f32; 2] },
}

struct Slot {
    instrument: Box<dyn Instrument>,
    routes: [Option<u8>; MAX_OUTPUTS],
}

struct Channel {
    active: bool,
    params: [f32; CHANNEL_PARAMS],
    gain: Smoother,
    mono_l: Smoother,
    mono_r: Smoother,
    bal_l: Smoother,
    bal_r: Smoother,
    /// Per-frame smoothed coefficients for the current block: gain, mono
    /// left/right, balance left/right.
    coef: Vec<[f32; 5]>,
    bus: Vec<f32>,
    peak: [f32; 2],
}

impl Channel {
    fn new(sr: f32) -> Self {
        let (ml, mr) = mono_pan(0.0);
        Self {
            active: false,
            params: channel_defaults(),
            gain: Smoother::new(sr, 0.01, 0.0),
            mono_l: Smoother::new(sr, 0.01, ml),
            mono_r: Smoother::new(sr, 0.01, mr),
            bal_l: Smoother::new(sr, 0.01, 1.0),
            bal_r: Smoother::new(sr, 0.01, 1.0),
            coef: vec![[0.0; 5]; MAX_BLOCK],
            bus: vec![0.0; MAX_BLOCK * 2],
            peak: [0.0; 2],
        }
    }

    /// Targets: gain (with mute/solo), mono pan, balance.
    fn targets(&self, any_solo: bool) -> [f32; 5] {
        let muted = self.params[CH_MUTE] >= 0.5;
        let soloed = self.params[CH_SOLO] >= 0.5;
        let audible = !muted && (!any_solo || soloed);
        let gain = if audible { volume_to_gain(self.params[CH_VOLUME]) } else { 0.0 };
        let (ml, mr) = mono_pan(self.params[CH_PAN]);
        let (bl, br) = balance(self.params[CH_PAN]);
        [gain, ml, mr, bl, br]
    }

    fn snap(&mut self, any_solo: bool) {
        let [g, ml, mr, bl, br] = self.targets(any_solo);
        self.gain.value = g;
        self.mono_l.value = ml;
        self.mono_r.value = mr;
        self.bal_l.value = bl;
        self.bal_r.value = br;
    }
}

pub struct Engine {
    sr: f32,
    globals: [f32; NUM_GLOBALS],
    slots: Vec<Option<Slot>>,
    channels: Vec<Channel>,
    master: Smoother,
    master_bus: Vec<f32>,
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
    /// An engine with no instruments and no active channels.
    pub fn new(sample_rate: u32) -> Self {
        let sr = sample_rate as f32;
        let globals = global_defaults();
        Self {
            sr,
            globals,
            slots: (0..MAX_INSTRUMENTS).map(|_| None).collect(),
            channels: (0..MAX_CHANNELS).map(|_| Channel::new(sr)).collect(),
            master: Smoother::new(sr, 0.01, volume_to_gain(globals[MASTER_VOLUME])),
            master_bus: vec![0.0; MAX_BLOCK * 2],
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

    /// Jump every smoother to its target, so an offline render starts at the
    /// loaded mix instead of fading in (non-real-time use).
    pub fn snap(&mut self) {
        let any_solo = self.any_solo();
        for ch in &mut self.channels {
            ch.snap(any_solo);
        }
        self.master.value = volume_to_gain(self.globals[MASTER_VOLUME]);
        for slot in self.slots.iter_mut().flatten() {
            slot.instrument.snap();
        }
    }

    fn any_solo(&self) -> bool {
        self.channels.iter().any(|c| c.active && c.params[CH_SOLO] >= 0.5)
    }

    /// Apply a command. A removed instrument is handed back so the caller can
    /// return it to the control side to be dropped there.
    #[must_use]
    pub fn apply(&mut self, cmd: Command, emit: &mut impl FnMut(Feedback)) -> Option<Box<dyn Instrument>> {
        match cmd {
            Command::SetParam { target, value } => match target {
                ParamTarget::Global(i) => {
                    if let Some(g) = self.globals.get_mut(i) {
                        *g = value;
                    }
                }
                ParamTarget::Instrument { slot, index } => {
                    if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                        s.instrument.set_param(index as usize, value);
                    }
                }
                ParamTarget::Channel { ch, index } => {
                    if let Some(c) = self.channels.get_mut(ch as usize)
                        && (index as usize) < CHANNEL_PARAMS
                    {
                        c.params[index as usize] = value;
                    }
                }
            },
            Command::AddInstrument { slot, instrument } => {
                // Never drop an instrument here: an invalid slot hands it
                // straight back to be freed on the control side.
                let Some(s) = self.slots.get_mut(slot as usize) else { return Some(instrument) };
                return s.replace(Slot { instrument, routes: [None; MAX_OUTPUTS] }).map(|s| s.instrument);
            }
            Command::RemoveInstrument { slot } => {
                return self.slots.get_mut(slot as usize)?.take().map(|s| s.instrument);
            }
            Command::SetRoute { slot, output, channel } => {
                if let Some(Some(s)) = self.slots.get_mut(slot as usize)
                    && (output as usize) < MAX_OUTPUTS
                {
                    s.routes[output as usize] = channel.filter(|c| (*c as usize) < MAX_CHANNELS);
                    s.instrument.set_routed(output as usize, s.routes[output as usize].is_some());
                }
            }
            Command::SetChannelActive { ch, active } => {
                let any_solo = self.any_solo();
                if let Some(c) = self.channels.get_mut(ch as usize) {
                    c.active = active;
                    c.params = channel_defaults();
                    c.peak = [0.0; 2];
                    // A reused channel starts at its defaults, not ramping
                    // from the previous channel's gain and pan.
                    c.snap(any_solo);
                }
            }
            Command::SetDrumStep { slot, track, step, level } => {
                if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                    s.instrument.set_drum_step(track as usize, step as usize, level);
                }
            }
            Command::SetDrumTrack { slot, track, steps } => {
                if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                    s.instrument.set_drum_track(track as usize, &steps);
                }
            }
            Command::SetNotes { slot, steps } => {
                if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                    s.instrument.set_notes(&steps);
                }
            }
            Command::NoteOn { slot, note, velocity, gate } => {
                let time = self.time();
                let gate = gate.then(|| self.sixteenth() * 0.5);
                if let Some(Some(s)) = self.slots.get_mut(slot as usize)
                    && let Some(h) = s.instrument.note_on(note, velocity, gate)
                {
                    emit(Feedback::Trigger { slot, voice: h.voice, note: h.note, velocity: h.velocity, time, step: None });
                }
            }
            Command::NoteOff { slot, note } => {
                if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                    s.instrument.note_off(note);
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
                    for s in self.slots.iter_mut().flatten() {
                        s.instrument.on_stop();
                    }
                    emit(Feedback::Stopped { time: self.time() });
                }
            }
        }
        None
    }

    fn sixteenth(&self) -> f64 {
        let tempo = self.globals[TEMPO].clamp(20.0, 300.0) as f64;
        60.0 / tempo / 4.0 * self.sr as f64
    }

    /// Length of step `index` in samples, including swing. Swing delays every
    /// second 16th: pairs keep their total length, the first note of each pair
    /// takes 50%..75% of it as swing goes 0..1.
    fn step_samples(&self, index: u32) -> f64 {
        let sixteenth = self.sixteenth();
        let ratio = 0.5 + 0.25 * self.globals[SWING].clamp(0.0, 1.0) as f64;
        if index % 2 == 0 { 2.0 * sixteenth * ratio } else { 2.0 * sixteenth * (1.0 - ratio) }
    }

    fn fire_step(&mut self, emit: &mut impl FnMut(Feedback)) {
        let length = (self.globals[LENGTH].round() as u32).clamp(1, MAX_STEPS as u32);
        if self.next_step >= length {
            self.next_step = 0;
        }
        let step = self.next_step;
        let time = self.time();
        let samples = self.step_samples(step);
        emit(Feedback::Step { step, time });
        for (slot, s) in self.slots.iter_mut().enumerate() {
            if let Some(s) = s {
                s.instrument.on_step(step as usize, samples, &mut |h| {
                    emit(Feedback::Trigger {
                        slot: slot as u8,
                        voice: h.voice,
                        note: h.note,
                        velocity: h.velocity,
                        time,
                        step: Some(step),
                    })
                });
            }
        }
        self.next_step_at += samples;
        self.next_step = step + 1;
    }

    /// Render interleaved audio into `out` (`channels` >= 1; channels beyond
    /// 2 are filled with silence).
    pub fn render(&mut self, out: &mut [f32], channels: usize, emit: &mut impl FnMut(Feedback)) {
        let channels = channels.max(1);
        let frames = out.len() / channels;
        let mut frame = 0;
        while frame < frames {
            let mut n = (frames - frame).min(MAX_BLOCK);
            if self.playing {
                let until = (self.next_step_at - self.pos as f64).ceil();
                if until <= 0.0 {
                    self.fire_step(emit);
                    continue;
                }
                n = n.min(until as usize);
            }
            self.render_block(&mut out[frame * channels..(frame + n) * channels], channels, n);
            frame += n;
            self.pos += n as u64;
            self.tick_meters(n as u32, emit);
        }
    }

    fn render_block(&mut self, out: &mut [f32], channels: usize, n: usize) {
        for s in self.slots.iter_mut().flatten() {
            s.instrument.render(n);
        }

        let any_solo = self.any_solo();
        for ch in self.channels.iter_mut().filter(|c| c.active) {
            let t = ch.targets(any_solo);
            for c in &mut ch.coef[..n] {
                *c = [
                    ch.gain.next(t[0]),
                    ch.mono_l.next(t[1]),
                    ch.mono_r.next(t[2]),
                    ch.bal_l.next(t[3]),
                    ch.bal_r.next(t[4]),
                ];
            }
            ch.bus[..n * 2].fill(0.0);
        }

        for s in self.slots.iter().flatten() {
            for (o, route) in s.routes.iter().enumerate().take(s.instrument.num_outputs()) {
                let Some(c) = route else { continue };
                let ch = &mut self.channels[*c as usize];
                if !ch.active {
                    continue;
                }
                let buf = s.instrument.output(o);
                let stereo = s.instrument.output_width(o) == OutputWidth::Stereo;
                for f in 0..n {
                    let k = &ch.coef[f];
                    let (l, r) = if stereo {
                        (buf[f * 2] * k[3], buf[f * 2 + 1] * k[4])
                    } else {
                        (buf[f * 2] * k[1], buf[f * 2] * k[2])
                    };
                    ch.bus[f * 2] += l;
                    ch.bus[f * 2 + 1] += r;
                }
            }
        }

        let master = &mut self.master_bus[..n * 2];
        master.fill(0.0);
        for ch in self.channels.iter_mut().filter(|c| c.active) {
            for f in 0..n {
                let g = ch.coef[f][0];
                let l = ch.bus[f * 2] * g;
                let r = ch.bus[f * 2 + 1] * g;
                ch.peak[0] = ch.peak[0].max(l.abs());
                ch.peak[1] = ch.peak[1].max(r.abs());
                master[f * 2] += l;
                master[f * 2 + 1] += r;
            }
        }

        let master_target = volume_to_gain(self.globals[MASTER_VOLUME]);
        for (f, o) in out.chunks_exact_mut(channels).enumerate().take(n) {
            let m = self.master.next(master_target);
            let l = soft_clip(self.master_bus[f * 2] * m);
            let r = soft_clip(self.master_bus[f * 2 + 1] * m);
            self.master_peak[0] = self.master_peak[0].max(l.abs());
            self.master_peak[1] = self.master_peak[1].max(r.abs());
            if channels == 1 {
                o[0] = 0.5 * (l + r);
            } else {
                o[0] = l;
                o[1] = r;
                for x in &mut o[2..] {
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
        let mut channels = [[0.0; 2]; MAX_CHANNELS];
        for (i, ch) in self.channels.iter_mut().enumerate() {
            channels[i] = ch.peak;
            ch.peak = [0.0; 2];
        }
        emit(Feedback::Meters { channels, master: self.master_peak });
        self.master_peak = [0.0; 2];
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::instrument;
    use fours_protocol::{InstrumentType, STEP_OFF, STEP_ON, Voice};

    /// An engine with a `tr808` in slot 0, its main out on channel index 0.
    pub(crate) fn drum_engine() -> Engine {
        let mut e = Engine::new(48000);
        let mut fb = |_| {};
        let _ = e.apply(Command::SetChannelActive { ch: 0, active: true }, &mut fb);
        let instrument = instrument::make(InstrumentType::Tr808, 48000.0);
        let _ = e.apply(Command::AddInstrument { slot: 0, instrument }, &mut fb);
        let _ = e.apply(Command::SetRoute { slot: 0, output: 0, channel: Some(0) }, &mut fb);
        e.snap();
        e
    }

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
            let mut e = drum_engine();
            let mut fb = vec![];
            let _ = e.apply(Command::NoteOn { slot: 0, note: v.gm_note(), velocity: 1.0, gate: false }, &mut |f| fb.push(f));
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
        let mut e = drum_engine();
        let mut fb = vec![];
        let mut steps = [STEP_OFF; MAX_STEPS];
        steps[0] = STEP_ON;
        steps[4] = STEP_ON;
        let _ = e.apply(Command::SetDrumTrack { slot: 0, track: 0, steps }, &mut |_| {});
        let _ = e.apply(Command::Play, &mut |_| {});
        // 120 bpm: 16th = 0.125s, one 16-step bar = 2s.
        render_secs(&mut e, 4.0, &mut fb);
        let kicks: Vec<f64> = fb
            .iter()
            .filter_map(|f| match f {
                Feedback::Trigger { voice: Some(0), time, .. } => Some(*time),
                _ => None,
            })
            .collect();
        let expect = [0.0, 0.5, 2.0, 2.5];
        assert_eq!(kicks.len(), expect.len(), "{kicks:?}");
        for (k, x) in kicks.iter().zip(expect) {
            assert!((k - x).abs() < 1.0 / 48000.0 * 2.0, "{k} vs {x}");
        }
        let steps: Vec<u32> =
            fb.iter().filter_map(|f| if let Feedback::Step { step, .. } = f { Some(*step) } else { None }).collect();
        assert_eq!(&steps[..17], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0]);
    }

    #[test]
    fn swing_delays_offbeats() {
        let mut e = drum_engine();
        let mut fb = vec![];
        let _ = e.apply(Command::SetParam { target: ParamTarget::Global(SWING), value: 1.0 }, &mut |_| {});
        let _ = e.apply(Command::Play, &mut |_| {});
        render_secs(&mut e, 0.6, &mut fb);
        let times: Vec<f64> =
            fb.iter().filter_map(|f| if let Feedback::Step { time, .. } = f { Some(*time) } else { None }).collect();
        // Pair = 0.25s; full swing puts the offbeat at 75% of the pair.
        assert!((times[1] - 0.1875).abs() < 1e-3, "{times:?}");
        assert!((times[2] - 0.25).abs() < 1e-3, "{times:?}");
    }

    fn note_engine() -> Engine {
        let mut e = Engine::new(48000);
        let mut fb = |_| {};
        let _ = e.apply(Command::SetChannelActive { ch: 0, active: true }, &mut fb);
        let instrument = instrument::make(InstrumentType::Tb303, 48000.0);
        let _ = e.apply(Command::AddInstrument { slot: 0, instrument }, &mut fb);
        let _ = e.apply(Command::SetRoute { slot: 0, output: 0, channel: Some(0) }, &mut fb);
        e.snap();
        e
    }

    #[test]
    fn tb303_sounds_and_decays() {
        for (square, accent) in [(0.0, false), (1.0, false), (0.0, true), (1.0, true)] {
            let mut e = note_engine();
            let mut fb = vec![];
            let _ = e.apply(Command::SetParam { target: ParamTarget::Instrument { slot: 0, index: 1 }, value: square }, &mut |_| {});
            let _ = e.apply(Command::SetParam { target: ParamTarget::Instrument { slot: 0, index: 3 }, value: 1.0 }, &mut |_| {});
            let velocity = if accent { 1.0 } else { 0.7 };
            let _ = e.apply(Command::NoteOn { slot: 0, note: 36, velocity, gate: true }, &mut |f| fb.push(f));
            let out = render_secs(&mut e, 0.1, &mut fb);
            let peak = out.iter().fold(0.0f32, |a, x| a.max(x.abs()));
            assert!(peak > 0.05, "square={square} accent={accent}: too quiet {peak}");
            assert!(peak <= 1.0, "square={square} accent={accent}: clipped {peak}");
            let tail = render_secs(&mut e, 0.5, &mut fb);
            let end_peak = tail[tail.len() - 4800..].iter().fold(0.0f32, |a, x| a.max(x.abs()));
            assert!(end_peak < 0.001, "square={square} accent={accent}: did not release {end_peak}");
        }
    }

    /// Regression: dragging the cutoff around while low, sliding square-wave
    /// notes play used to drive the ladder's state to infinity after about a
    /// minute, leaving a stuck, inaudible DC output until the instrument was
    /// rebuilt. The output must keep moving (never a constant level) and stay
    /// audible bar after bar.
    #[test]
    fn tb303_filter_never_sticks() {
        let sr = 44100;
        let mut e = Engine::new(sr);
        let mut fb = |_| {};
        let _ = e.apply(Command::SetChannelActive { ch: 0, active: true }, &mut fb);
        let instrument = instrument::make(InstrumentType::Tb303, sr as f32);
        let _ = e.apply(Command::AddInstrument { slot: 0, instrument }, &mut fb);
        let _ = e.apply(Command::SetRoute { slot: 0, output: 0, channel: Some(0) }, &mut fb);
        let notes = fours_protocol::parse_notes("D1~ D#1~ C1~ F#1~ A#3~ A#3 A#3 - - - - D#1 - C#1 - G1").unwrap();
        let mut steps = [NoteStep::default(); MAX_STEPS];
        steps.copy_from_slice(&notes);
        let _ = e.apply(Command::SetNotes { slot: 0, steps }, &mut fb);
        // Square wave, and the decay/accent the bug was found with.
        for (index, value) in [(1u16, 1.0f32), (3, 0.5), (4, 0.5), (5, 0.62), (6, 0.74)] {
            let _ = e.apply(Command::SetParam { target: ParamTarget::Instrument { slot: 0, index }, value }, &mut fb);
        }
        let _ = e.apply(Command::Play, &mut fb);
        let mut out = vec![0.0f32; 512 * 2];
        let mut rng = 12345u32;
        let (mut bar_peak, mut silent_bars) = (0.0f32, 0);
        let blocks = sr as usize * 90 / 512; // 90 s; it stuck at ~59 s before the fix
        for block in 0..blocks {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            // A slider: slow drags across the range, with occasional jumps.
            let t = block as f32 * 512.0 / sr as f32;
            let cutoff = if rng % 50 == 0 {
                (rng % 1000) as f32 / 999.0
            } else {
                0.5 + 0.5 * (t * 0.37).sin() * (t * 0.05).cos()
            };
            let target = ParamTarget::Instrument { slot: 0, index: 2 };
            let _ = e.apply(Command::SetParam { target, value: cutoff }, &mut fb);
            e.render(&mut out, 2, &mut |_| {});
            let (lo, hi) = out.iter().step_by(2).fold((f32::MAX, f32::MIN), |(l, h), x| (l.min(*x), h.max(*x)));
            assert!(lo.is_finite() && hi.is_finite(), "non-finite output at {t:.1}s");
            assert!(!(hi - lo < 1e-6 && hi.abs() > 0.3), "stuck DC output {hi} at {t:.1}s (cutoff {cutoff:.2})");
            bar_peak = bar_peak.max(hi.abs()).max(lo.abs());
            if block % 172 == 171 {
                // About one bar at 120 bpm.
                silent_bars += (bar_peak < 0.01) as u32;
                bar_peak = 0.0;
            }
        }
        assert_eq!(silent_bars, 0, "the bass went silent");
    }

    /// A slid note glides in without retriggering the filter envelope: the
    /// level stays continuous across the step boundary, and no gap opens.
    #[test]
    fn tb303_slide_holds_the_gate() {
        let mut steps = [NoteStep::default(); MAX_STEPS];
        steps[0] = NoteStep { note: Some(36), accent: false, slide: true };
        steps[1] = NoteStep { note: Some(43), accent: false, slide: false };
        let mut slid = note_engine();
        let _ = slid.apply(Command::SetNotes { slot: 0, steps }, &mut |_| {});
        steps[0].slide = false;
        let mut plain = note_engine();
        let _ = plain.apply(Command::SetNotes { slot: 0, steps }, &mut |_| {});
        // Step 0 is 0..0.125s; without slide its gate closes at 0.0625s.
        let gap_level = |e: &mut Engine| {
            let _ = e.apply(Command::Play, &mut |_| {});
            let out = render_secs(e, 0.25, &mut vec![]);
            let (a, b) = ((0.10 * 48000.0) as usize * 2, (0.12 * 48000.0) as usize * 2);
            out[a..b].iter().fold(0.0f32, |m, x| m.max(x.abs()))
        };
        let held = gap_level(&mut slid);
        let released = gap_level(&mut plain);
        assert!(held > 0.05, "slide should hold the gate: {held}");
        assert!(released < 0.01, "without slide the gate should close: {released}");
    }
}
