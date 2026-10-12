//! The engine: a shared tick clock (`PPQ` ticks per quarter, swing applied
//! per 16th), a fixed table of instrument slots each with a clip, and a fixed
//! pool of stereo mixer channels. `Engine::render` and `Engine::apply` are
//! called from the audio thread (or offline) and must not allocate, lock, or
//! block. Instruments are built and dropped on the control side; clips live
//! in storage preallocated here and are edited by small commands.
//!
//! Sequencing (RFC 0007): on every tick, each slot's clip releases the notes
//! that end there, then starts the events that begin there. A clip loops at
//! its own length.
//!
//! Recording (RFC 0008): live notes report the song tick the player heard
//! when they played them (`Feedback::Live`), the metronome clicks on beats,
//! and `Play` can run a count-in of clicks before tick 0.
//!
//! The song (RFC 0008 phase B, RFC 0009): clips live in one preallocated
//! table shared by every slot. The transport plays each slot's placements
//! on the song timeline from the locate point, looping the song or a range
//! (`song.*` globals), or stopping at its end. A slot with a launched clip
//! loops that clip instead, from the tick it started; launches and returns
//! to the arrangement wait for the next `LAUNCH_QUANTIZE` boundary of the
//! song position while playing.
//!
//! Signal flow per block: each instrument renders its outputs; each routed
//! output is summed into its channel with its own pan law (constant-power pan
//! for mono, balance for stereo); each channel runs its insert chain of audio
//! effects (RFC 0010), then applies its fader and mute/solo; channels sum
//! into the master, which runs its own chain, then its fader and a soft
//! clipper.

use crate::dsp::{Smoother, soft_clip};
use crate::effect::{AudioEffect, COMMON_PARAMS, ProcessContext};
use crate::instrument::{Instrument, MAX_BLOCK, MAX_OUTPUTS};
use crate::modulator::{ModContext, Modulator};
use crate::params::*;
use fours_protocol::{
    ClipEvent, DEFAULT_CLIP_LENGTH, MAX_CHANNELS, MAX_CLIP_TICKS, MAX_CLIPS, MAX_EFFECTS, MAX_EVENTS, MAX_INSERTS,
    MAX_INSTRUMENTS, MAX_MOD_ROUTES, MAX_MODULATORS, MAX_PLACEMENTS, OutputWidth, PPQ, TICKS_PER_BAR, TICKS_PER_STEP,
};
use std::f32::consts::TAU;

/// Most notes a clip can hold sounding at once.
const MAX_ACTIVE: usize = 64;

/// Meter feedback rate.
const METER_HZ: f32 = 30.0;

/// Where a parameter lives inside the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParamTarget {
    Global(usize),
    Instrument { slot: u8, index: u16 },
    /// `ch` is the 0-based channel index (channel number - 1).
    Channel { ch: u8, index: u8 },
    /// An audio effect's parameter (`bypass`, `mix`, then its own).
    Effect { slot: u8, index: u16 },
    /// A track channel's send level to a return channel (0-based indexes).
    Send { ch: u8, ret: u8 },
    /// A modulator's parameter.
    Modulator { slot: u8, index: u16 },
}

/// A modulation route as the engine runs it: a modulator output moving a
/// target by `depth` of its range (`min..max`) around `base`, the value
/// last set on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModRoute {
    pub source: u8,
    pub output: u8,
    pub target: ParamTarget,
    pub depth: f32,
    pub min: f32,
    pub max: f32,
    pub base: f32,
}

/// The insert chain index of the master (channels are 0..MAX_CHANNELS).
pub const MASTER_CHAIN: u8 = MAX_CHANNELS as u8;

/// Something the audio thread hands back to be dropped on the control side.
pub enum Returned {
    Instrument(Box<dyn Instrument>),
    Effect(Box<dyn AudioEffect>),
    Modulator(Box<dyn Modulator>),
}

/// Control messages into the engine. Channel fields are 0-based indexes.
pub enum Command {
    SetParam { target: ParamTarget, value: f32 },
    AddInstrument { slot: u8, instrument: Box<dyn Instrument> },
    RemoveInstrument { slot: u8 },
    SetRoute { slot: u8, output: u8, channel: Option<u8> },
    SetChannelActive { ch: u8, active: bool },
    /// Make a channel a return bus (fed by sends, mixed after the track
    /// channels) or a track channel.
    SetChannelReturn { ch: u8, ret: bool },
    /// Put an audio effect in an effect slot (replacing any there).
    AddEffect { slot: u8, effect: Box<dyn AudioEffect> },
    RemoveEffect { slot: u8 },
    /// A channel's (or, `MASTER_CHAIN`, the master's) insert chain: the
    /// first `len` effect slots of `slots`, in order.
    SetChain { ch: u8, slots: [u8; MAX_INSERTS], len: u8 },
    /// Put a modulator in a modulator slot (replacing any there).
    AddModulator { slot: u8, modulator: Box<dyn Modulator> },
    RemoveModulator { slot: u8 },
    /// Set modulation route `index` (its target's current value is its
    /// base), or (`None`) clear it, putting its target back at its base.
    SetModRoute { index: u8, route: Option<ModRoute> },
    /// Remove every event from a clip (an index in the clip table).
    ClearClip { clip: u16 },
    /// Add an event (replacing one at the same tick and note).
    AddEvent { clip: u16, event: ClipEvent },
    RemoveEvent { clip: u16, tick: u32, note: u8 },
    /// Clip length in ticks.
    SetClipLength { clip: u16, length: u32 },
    /// Loop a clip on a slot instead of its arrangement, or (`None`) go back
    /// to the arrangement: while playing, at the next `LAUNCH_QUANTIZE`
    /// boundary (`now`: the next tick); while stopped, at once.
    Launch { slot: u8, clip: Option<u16>, now: bool },
    /// Add a placement to a slot's arrangement (replacing one with the same
    /// start). The control side keeps them from overlapping.
    AddPlacement { slot: u8, placement: Placement },
    RemovePlacement { slot: u8, start: u32 },
    /// Where play starts from; while playing, jump there.
    Locate { tick: u32 },
    /// A note; with `gate` it releases after half a step at the current tempo.
    NoteOn { slot: u8, note: u8, velocity: f32, gate: bool },
    NoteOff { slot: u8, note: u8 },
    PitchBend { slot: u8, semitones: f32 },
    /// Start from tick 0, after `count_in` ticks of metronome only.
    Play { count_in: u32 },
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
            Command::SetChannelReturn { ch, ret } => write!(f, "SetChannelReturn({ch}, {ret})"),
            Command::AddEffect { slot, .. } => write!(f, "AddEffect({slot})"),
            Command::RemoveEffect { slot } => write!(f, "RemoveEffect({slot})"),
            Command::SetChain { ch, slots, len } => write!(f, "SetChain({ch}, {:?})", &slots[..(*len as usize).min(MAX_INSERTS)]),
            Command::AddModulator { slot, .. } => write!(f, "AddModulator({slot})"),
            Command::RemoveModulator { slot } => write!(f, "RemoveModulator({slot})"),
            Command::SetModRoute { index, route } => write!(f, "SetModRoute({index}, {route:?})"),
            Command::ClearClip { clip } => write!(f, "ClearClip({clip})"),
            Command::AddEvent { clip, event } => write!(f, "AddEvent({clip}, {event:?})"),
            Command::RemoveEvent { clip, tick, note } => write!(f, "RemoveEvent({clip}, {tick}, {note})"),
            Command::SetClipLength { clip, length } => write!(f, "SetClipLength({clip}, {length:?})"),
            Command::Launch { slot, clip, now } => write!(f, "Launch({slot}, {clip:?}, {now})"),
            Command::AddPlacement { slot, placement } => write!(f, "AddPlacement({slot}, {placement:?})"),
            Command::RemovePlacement { slot, start } => write!(f, "RemovePlacement({slot}, {start})"),
            Command::Locate { tick } => write!(f, "Locate({tick})"),
            Command::NoteOn { slot, note, .. } => write!(f, "NoteOn({slot}, {note})"),
            Command::NoteOff { slot, note } => write!(f, "NoteOff({slot}, {note})"),
            Command::PitchBend { slot, semitones } => write!(f, "PitchBend({slot}, {semitones})"),
            Command::Play { count_in } => write!(f, "Play({count_in})"),
            Command::Stop => write!(f, "Stop"),
        }
    }
}

/// Messages out of the engine. Times are engine time in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Feedback {
    /// A step of the song started: `tick` counted from play (tick 0 is the
    /// first tick after any count-in), `pos` the song position.
    Step { tick: u64, pos: u64, time: f64 },
    /// A live note (not from a clip) was played or released. `tick` is the
    /// tick from play the player heard at that moment (the output latency
    /// taken off; negative during a count-in), and `pos` the song position
    /// heard; `None` when stopped. `gate` notes release by themselves after
    /// half a step.
    Live { slot: u8, note: u8, velocity: f32, on: bool, gate: bool, tick: Option<f64>, pos: Option<f64>, time: f64 },
    /// A slot switched to a launched clip (or, `None`, back to its
    /// arrangement) at `tick` from play, song position `pos`; both 0 and
    /// the start point when stopped or counting in.
    Launched { slot: u8, clip: Option<u16>, tick: u64, pos: u64, time: f64 },
    /// Playing without a loop reached the song's end (with no launched
    /// clips).
    SongEnd { time: f64 },
    Trigger { slot: u8, voice: Option<u8>, note: Option<u8>, velocity: f32, time: f64, step: Option<u32> },
    /// `Play` took effect: feedback after this counts from the new start.
    Started { time: f64 },
    Stopped { time: f64 },
    /// Peak (left, right) per channel index since the last meter message.
    Meters { channels: [[f32; 2]; MAX_CHANNELS], master: [f32; 2] },
}

struct Slot {
    instrument: Box<dyn Instrument>,
    routes: [Option<u8>; MAX_OUTPUTS],
}

/// An audio effect and the two parameters the engine applies around it.
struct EffectSlot {
    effect: Box<dyn AudioEffect>,
    bypass: bool,
    mix: f32,
    /// The wet share, smoothed toward `mix` (0 when bypassed).
    wet: Smoother,
    /// Bypassed and faded out: it did not run, so it starts clean.
    idle: bool,
}

/// Run an insert chain over `bus` (interleaved stereo, `n` frames), keeping
/// each effect's dry signal in `dry` to blend by its mix.
fn run_chain(effects: &mut [Option<EffectSlot>], chain: &[u8], bus: &mut [f32], dry: &mut [f32], n: usize, ctx: &ProcessContext) {
    for &slot in chain {
        let Some(Some(e)) = effects.get_mut(slot as usize) else { continue };
        let target = if e.bypass { 0.0 } else { e.mix };
        if target == 0.0 && e.wet.value < 1e-4 {
            // Bypassed and faded out: the effect does not run.
            e.idle = true;
            continue;
        }
        if e.idle {
            // Back from a bypass: no echoes or tail from before it.
            e.effect.reset();
            e.idle = false;
        }
        dry[..n * 2].copy_from_slice(&bus[..n * 2]);
        e.effect.process(&mut bus[..n * 2], n, ctx);
        for f in 0..n {
            let w = e.wet.next(target);
            for c in 0..2 {
                let i = f * 2 + c;
                bus[i] = dry[i] * (1.0 - w) + bus[i] * w;
            }
        }
    }
}

/// A clip in the shared table. Preallocated: edits insert into spare
/// capacity and never allocate.
struct ClipStore {
    /// Sorted by (tick, note); at most `MAX_EVENTS`.
    events: Vec<ClipEvent>,
    length: u32,
}

impl ClipStore {
    fn position(&self, e: &ClipEvent) -> Result<usize, usize> {
        self.events.binary_search_by_key(&e.key(), ClipEvent::key)
    }
}

/// A placement as the engine plays it: `clip` indexes the clip table.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Placement {
    pub start: u32,
    pub length: u32,
    pub offset: u32,
    pub clip: u16,
}

/// A slot's sequencing: its arrangement, the clip it loops instead (if
/// launched), and the notes its clips are playing. Preallocated.
struct Track {
    /// Sorted by start, not overlapping; at most `MAX_PLACEMENTS`.
    placements: Vec<Placement>,
    /// The launched clip and the tick from play (after any count-in) it
    /// started at.
    launched: Option<(u16, u64)>,
    /// A switch waiting for its boundary: to a clip, or back (`None`), and
    /// whether it lands on the next tick.
    pending: Option<(Option<u16>, bool)>,
    /// (absolute tick it ends at, note), for notes the clips started.
    active: [(u64, u8); MAX_ACTIVE],
    num_active: usize,
}

impl Track {
    fn reset(&mut self) {
        self.placements.clear();
        self.launched = None;
        self.pending = None;
        self.num_active = 0;
    }

    fn new() -> Self {
        Self {
            placements: Vec::with_capacity(MAX_PLACEMENTS),
            launched: None,
            pending: None,
            active: [(0, 0); MAX_ACTIVE],
            num_active: 0,
        }
    }

    /// Plays a launched clip, or will once its launch lands.
    fn launching(&self) -> bool {
        match self.pending {
            Some((clip, _)) => clip.is_some() || self.launched.is_some(),
            None => self.launched.is_some(),
        }
    }

    /// The placement playing at song tick `pos`.
    fn placement_at(&self, pos: u64) -> Option<Placement> {
        let i = self.placements.partition_point(|p| p.start as u64 <= pos);
        let p = *self.placements.get(i.checked_sub(1)?)?;
        (pos < p.start as u64 + p.length as u64).then_some(p)
    }
}

/// The metronome's click: a short decaying sine, higher on a bar's
/// downbeat.
struct Click {
    phase: f32,
    inc: f32,
    env: f32,
    decay: f32,
}

impl Click {
    fn new(sr: f32) -> Self {
        // About 25 ms to -60 dB.
        Self { phase: 0.0, inc: 0.0, env: 0.0, decay: (-6.9 / (0.025 * sr)).exp() }
    }

    fn start(&mut self, sr: f32, downbeat: bool) {
        self.phase = 0.0;
        self.inc = TAU * if downbeat { 1500.0 } else { 1000.0 } / sr;
        self.env = 1.0;
    }

    fn next(&mut self) -> f32 {
        if self.env < 1e-4 {
            return 0.0;
        }
        let s = self.phase.sin() * self.env;
        self.phase = (self.phase + self.inc) % TAU;
        self.env *= self.decay;
        s
    }
}

/// Whether any track channel, or any return, is soloed.
#[derive(Clone, Copy)]
struct Solo {
    tracks: bool,
    returns: bool,
}

struct Channel {
    active: bool,
    /// A return bus: fed by sends, not silenced by another channel's solo.
    ret: bool,
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
            ret: false,
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
    fn targets(&self, any_solo: Solo) -> [f32; 5] {
        let muted = self.params[CH_MUTE] >= 0.5;
        let soloed = self.params[CH_SOLO] >= 0.5;
        // Track solos gate the tracks, return solos the returns: soloing a
        // track keeps the returns it feeds, soloing a return keeps the
        // tracks that feed it.
        let gate = if self.ret { any_solo.returns } else { any_solo.tracks };
        let audible = !muted && (!gate || soloed);
        let gain = if audible { volume_to_gain(self.params[CH_VOLUME]) } else { 0.0 };
        let (ml, mr) = mono_pan(self.params[CH_PAN]);
        let (bl, br) = balance(self.params[CH_PAN]);
        [gain, ml, mr, bl, br]
    }

    fn snap(&mut self, any_solo: Solo) {
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
    tracks: Vec<Track>,
    clips: Vec<ClipStore>,
    channels: Vec<Channel>,
    effects: Vec<Option<EffectSlot>>,
    /// Insert chains: one per channel, then the master's (`MASTER_CHAIN`).
    chains: [[u8; MAX_INSERTS]; MAX_CHANNELS + 1],
    chain_len: [u8; MAX_CHANNELS + 1],
    /// Scratch for the dry signal of the effect running.
    dry: Vec<f32>,
    /// Send levels, track channel -> return channel, and the levels the last
    /// block ended at (each block ramps between them).
    sends: [[f32; MAX_CHANNELS]; MAX_CHANNELS],
    sends_at: [[f32; MAX_CHANNELS]; MAX_CHANNELS],
    /// Scratch for a track channel's post-fader signal.
    post: Vec<f32>,
    modulators: Vec<Option<Box<dyn Modulator>>>,
    routes: [Option<ModRoute>; MAX_MOD_ROUTES],
    /// Beats since the song's tick 0, advanced every block (synced LFOs
    /// follow it; it runs while stopped too).
    beat: f64,
    master: Smoother,
    master_bus: Vec<f32>,
    master_peak: [f32; 2],
    playing: bool,
    /// The next tick to fire, counted from play, count-in included.
    tick: u64,
    /// Count-in ticks before the song's tick 0.
    count_in: u64,
    /// The song tick of the next tick to fire.
    song_pos: u64,
    /// Where play starts from (`Locate`).
    start_pos: u64,
    /// `SongEnd` was sent for this run.
    ended: bool,
    /// The tick (from play) at which the song last wrapped at a loop's end.
    wrapped_at: Option<u64>,
    /// Absolute sample position at which it fires.
    next_tick_at: f64,
    /// Samples between the last tick fired and the next.
    tick_len: f64,
    /// Seconds from rendering a sample to hearing it (set by the audio
    /// backend).
    output_latency: f64,
    click: Click,
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
            tracks: (0..MAX_INSTRUMENTS).map(|_| Track::new()).collect(),
            clips: (0..MAX_CLIPS)
                .map(|_| ClipStore { events: Vec::with_capacity(MAX_EVENTS), length: DEFAULT_CLIP_LENGTH })
                .collect(),
            channels: (0..MAX_CHANNELS).map(|_| Channel::new(sr)).collect(),
            effects: (0..MAX_EFFECTS).map(|_| None).collect(),
            chains: [[0; MAX_INSERTS]; MAX_CHANNELS + 1],
            chain_len: [0; MAX_CHANNELS + 1],
            dry: vec![0.0; MAX_BLOCK * 2],
            sends: [[0.0; MAX_CHANNELS]; MAX_CHANNELS],
            sends_at: [[0.0; MAX_CHANNELS]; MAX_CHANNELS],
            post: vec![0.0; MAX_BLOCK * 2],
            modulators: (0..MAX_MODULATORS).map(|_| None).collect(),
            routes: [None; MAX_MOD_ROUTES],
            beat: 0.0,
            master: Smoother::new(sr, 0.01, volume_to_gain(globals[MASTER_VOLUME])),
            master_bus: vec![0.0; MAX_BLOCK * 2],
            master_peak: [0.0; 2],
            playing: false,
            tick: 0,
            count_in: 0,
            song_pos: 0,
            start_pos: 0,
            ended: false,
            wrapped_at: None,
            next_tick_at: 0.0,
            tick_len: 1.0,
            output_latency: 0.0,
            click: Click::new(sr),
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

    /// How long rendered audio takes to be heard; live notes are placed
    /// that much earlier (`Feedback::Live`).
    pub fn set_output_latency(&mut self, secs: f64) {
        self.output_latency = secs.clamp(0.0, 1.0);
    }

    /// The tick from play (fractional, negative in a count-in) whose sound
    /// reaches the listener now, and the song position it is, or `None`
    /// when stopped.
    fn heard_tick(&self) -> Option<(f64, f64)> {
        if !self.playing {
            return None;
        }
        let tick_len = self.tick_len.max(1.0);
        // The last tick fired is `tick - 1`, `tick_len` samples before the
        // next one is due.
        let back = (self.next_tick_at - self.pos as f64) / tick_len + self.output_latency * self.sr as f64 / tick_len;
        let tick = self.tick as f64 - self.count_in as f64 - back;
        if tick < 0.0 {
            return Some((tick, self.start_pos as f64 + tick));
        }
        let mut pos = self.song_pos as f64 - back;
        // Just past a loop's wrap, what is heard is still before it.
        if let Some((ls, le)) = self.song_loop()
            && pos < ls as f64
            && self.wrapped_at.is_some_and(|w| (self.tick - w) as f64 <= back.ceil() + 1.0)
        {
            pos += (le - ls) as f64;
        }
        Some((tick, pos))
    }

    /// The song's end: the end of the last placement on any slot.
    fn song_end(&self) -> u64 {
        self.tracks.iter().filter_map(|t| t.placements.last()).map(|p| p.start as u64 + p.length as u64).max().unwrap_or(0)
    }

    /// The span the transport loops, if any.
    fn song_loop(&self) -> Option<(u64, u64)> {
        let bar = TICKS_PER_BAR as u64;
        match self.globals[LOOP].round() as i32 {
            1 => Some((0, self.song_end())).filter(|(_, e)| *e > 0),
            2 => {
                let (s, e) = (self.globals[LOOP_START].max(0.0) as u64 * bar, self.globals[LOOP_END].max(0.0) as u64 * bar);
                (e > s).then_some((s, e))
            }
            _ => None,
        }
    }

    /// Jump every smoother to its target, so an offline render starts at the
    /// loaded mix instead of fading in (non-real-time use).
    pub fn snap(&mut self) {
        let any_solo = self.any_solo();
        for ch in &mut self.channels {
            ch.snap(any_solo);
        }
        self.sends_at = self.sends;
        self.master.value = volume_to_gain(self.globals[MASTER_VOLUME]);
        for slot in self.slots.iter_mut().flatten() {
            slot.instrument.snap();
        }
        let ctx = ProcessContext { tempo: self.globals[TEMPO].clamp(20.0, 300.0) };
        for e in self.effects.iter_mut().flatten() {
            e.effect.snap(&ctx);
            e.wet.value = if e.bypass { 0.0 } else { e.mix };
        }
    }

    fn any_solo(&self) -> Solo {
        let soloed = |ret: bool| self.channels.iter().any(|c| c.active && c.ret == ret && c.params[CH_SOLO] >= 0.5);
        Solo { tracks: soloed(false), returns: soloed(true) }
    }

    /// Apply a command. A removed instrument is handed back so the caller can
    /// return it to the control side to be dropped there.
    #[must_use]
    pub fn apply(&mut self, cmd: Command, emit: &mut impl FnMut(Feedback)) -> Option<Returned> {
        match cmd {
            Command::SetParam { target, value } => {
                // A modulated target's routes move around the new value.
                for r in self.routes.iter_mut().flatten().filter(|r| r.target == target) {
                    r.base = value;
                }
                self.write_param(target, value);
            }
            Command::AddModulator { slot, modulator } => {
                let Some(s) = self.modulators.get_mut(slot as usize) else { return Some(Returned::Modulator(modulator)) };
                return s.replace(modulator).map(Returned::Modulator);
            }
            Command::RemoveModulator { slot } => {
                // Its routes go first, their targets back at their bases.
                for i in 0..MAX_MOD_ROUTES {
                    if let Some(r) = self.routes[i].filter(|r| r.source == slot) {
                        self.routes[i] = None;
                        self.write_param(r.target, r.base);
                    }
                }
                return self.modulators.get_mut(slot as usize)?.take().map(Returned::Modulator);
            }
            Command::SetModRoute { index, route } => {
                if let Some(cell) = self.routes.get_mut(index as usize) {
                    let old = std::mem::replace(cell, route);
                    if let Some(o) = old.filter(|o| route.is_none_or(|r| r.target != o.target)) {
                        self.write_param(o.target, o.base);
                    }
                }
            }
            Command::AddEffect { slot, effect } => {
                // Never drop an effect here: an invalid slot hands it back.
                let Some(s) = self.effects.get_mut(slot as usize) else { return Some(Returned::Effect(effect)) };
                // Settled at this tempo, so a synced delay does not glide in.
                let mut effect = effect;
                effect.snap(&ProcessContext { tempo: self.globals[TEMPO].clamp(20.0, 300.0) });
                let wet = Smoother::new(self.sr, 0.01, 0.0);
                return s.replace(EffectSlot { effect, bypass: false, mix: 0.0, wet, idle: false }).map(|e| Returned::Effect(e.effect));
            }
            Command::RemoveEffect { slot } => {
                // Out of every chain first, so nothing runs a missing slot.
                for c in 0..=MAX_CHANNELS {
                    let len = self.chain_len[c] as usize;
                    let mut k = 0;
                    for i in 0..len {
                        if self.chains[c][i] != slot {
                            self.chains[c][k] = self.chains[c][i];
                            k += 1;
                        }
                    }
                    self.chain_len[c] = k as u8;
                }
                return self.effects.get_mut(slot as usize)?.take().map(|e| Returned::Effect(e.effect));
            }
            Command::SetChain { ch, slots, len } => {
                if (ch as usize) <= MAX_CHANNELS {
                    self.chains[ch as usize] = slots;
                    self.chain_len[ch as usize] = len.min(MAX_INSERTS as u8);
                }
            }
            Command::AddInstrument { slot, instrument } => {
                // Never drop an instrument here: an invalid slot hands it
                // straight back to be freed on the control side.
                let Some(s) = self.slots.get_mut(slot as usize) else { return Some(Returned::Instrument(instrument)) };
                self.tracks[slot as usize].reset();
                return s.replace(Slot { instrument, routes: [None; MAX_OUTPUTS] }).map(|s| Returned::Instrument(s.instrument));
            }
            Command::RemoveInstrument { slot } => {
                if let Some(t) = self.tracks.get_mut(slot as usize) {
                    t.reset();
                }
                return self.slots.get_mut(slot as usize)?.take().map(|s| Returned::Instrument(s.instrument));
            }
            Command::SetRoute { slot, output, channel } => {
                if let Some(Some(s)) = self.slots.get_mut(slot as usize)
                    && (output as usize) < MAX_OUTPUTS
                {
                    s.routes[output as usize] = channel.filter(|c| (*c as usize) < MAX_CHANNELS);
                    s.instrument.set_routed(output as usize, s.routes[output as usize].is_some());
                }
            }
            Command::SetChannelReturn { ch, ret } => {
                if let Some(c) = self.channels.get_mut(ch as usize) {
                    c.ret = ret;
                }
            }
            Command::SetChannelActive { ch, active } => {
                let any_solo = self.any_solo();
                // A reused channel number starts as a track with no sends.
                if (ch as usize) < MAX_CHANNELS {
                    for row in self.sends.iter_mut().chain(self.sends_at.iter_mut()) {
                        row[ch as usize] = 0.0;
                    }
                    self.sends[ch as usize] = [0.0; MAX_CHANNELS];
                    self.sends_at[ch as usize] = [0.0; MAX_CHANNELS];
                }
                if let Some(c) = self.channels.get_mut(ch as usize) {
                    c.ret = false;
                    c.active = active;
                    c.params = channel_defaults();
                    c.peak = [0.0; 2];
                    // A reused channel starts at its defaults, not ramping
                    // from the previous channel's gain and pan.
                    c.snap(any_solo);
                }
            }
            Command::ClearClip { clip } => {
                if let Some(c) = self.clips.get_mut(clip as usize) {
                    c.events.clear();
                    c.length = DEFAULT_CLIP_LENGTH;
                }
            }
            Command::AddEvent { clip, event } => {
                if let Some(c) = self.clips.get_mut(clip as usize)
                    && event.tick < MAX_CLIP_TICKS
                {
                    match c.position(&event) {
                        Ok(i) => c.events[i] = event,
                        // Within the preallocated capacity: no allocation.
                        Err(i) if c.events.len() < MAX_EVENTS => c.events.insert(i, event),
                        Err(_) => {}
                    }
                }
            }
            Command::RemoveEvent { clip, tick, note } => {
                if let Some(c) = self.clips.get_mut(clip as usize)
                    && let Ok(i) = c.position(&ClipEvent { tick, note, ..Default::default() })
                {
                    c.events.remove(i);
                }
            }
            Command::SetClipLength { clip, length } => {
                if let Some(c) = self.clips.get_mut(clip as usize) {
                    c.length = length.clamp(1, MAX_CLIP_TICKS);
                }
            }
            Command::Launch { slot, clip, now } => {
                if (slot as usize) < self.tracks.len() && clip.is_none_or(|c| (c as usize) < MAX_CLIPS) {
                    if self.playing && self.tick >= self.count_in {
                        self.tracks[slot as usize].pending = Some((clip, now));
                    } else {
                        // Stopped or counting in: launched clips start with
                        // the song.
                        self.switch(slot as usize, clip, 0, emit);
                    }
                }
            }
            Command::AddPlacement { slot, placement } => {
                if let Some(t) = self.tracks.get_mut(slot as usize)
                    && (placement.clip as usize) < MAX_CLIPS
                    && placement.length > 0
                {
                    match t.placements.binary_search_by_key(&placement.start, |p| p.start) {
                        Ok(i) => t.placements[i] = placement,
                        Err(i) if t.placements.len() < MAX_PLACEMENTS => t.placements.insert(i, placement),
                        Err(_) => {}
                    }
                }
            }
            Command::RemovePlacement { slot, start } => {
                if let Some(t) = self.tracks.get_mut(slot as usize)
                    && let Ok(i) = t.placements.binary_search_by_key(&start, |p| p.start)
                {
                    t.placements.remove(i);
                }
            }
            Command::Locate { tick } => {
                self.start_pos = tick as u64;
                // During a count-in the song has not started: it starts here.
                if self.playing && self.tick < self.count_in {
                    self.song_pos = tick as u64;
                } else if self.playing {
                    // Launched clips play on.
                    self.release_arrangement_notes();
                    self.song_pos = tick as u64;
                    self.ended = false;
                }
            }
            Command::NoteOn { slot, note, velocity, gate: gated } => {
                let time = self.time();
                let (tick, pos) = self.heard_tick().unzip();
                let gate = gated.then(|| self.sixteenth() * 0.5);
                if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                    if let Some(h) = s.instrument.note_on(note, velocity, gate) {
                        emit(Feedback::Trigger { slot, voice: h.voice, note: h.note, velocity: h.velocity, time, step: None });
                    }
                    emit(Feedback::Live { slot, note, velocity, on: true, gate: gated, tick, pos, time });
                }
            }
            Command::NoteOff { slot, note } => {
                let (time, (tick, pos)) = (self.time(), self.heard_tick().unzip());
                if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                    s.instrument.note_off(note);
                    emit(Feedback::Live { slot, note, velocity: 0.0, on: false, gate: false, tick, pos, time });
                }
            }
            Command::PitchBend { slot, semitones } => {
                if let Some(Some(s)) = self.slots.get_mut(slot as usize) {
                    s.instrument.pitch_bend(semitones);
                }
            }
            Command::Play { count_in } => {
                self.release_clip_notes(None);
                // Waiting switches land; launched clips start with the song.
                for slot in 0..self.tracks.len() {
                    let t = &self.tracks[slot];
                    let clip = t.pending.map_or(t.launched.map(|l| l.0), |p| p.0);
                    if t.pending.is_some() || t.launched.is_some_and(|l| l.1 != 0) {
                        self.switch(slot, clip, 0, emit);
                    }
                }
                self.playing = true;
                self.tick = 0;
                // Whole bars, so swing pairs and beats line up with the song.
                self.count_in = (count_in as u64).div_ceil(TICKS_PER_BAR as u64) * TICKS_PER_BAR as u64;
                self.song_pos = self.start_pos;
                // Synced modulators count from the song's start (the
                // count-in is before it).
                self.beat = (self.start_pos as f64 - self.count_in as f64) / PPQ as f64;
                self.ended = false;
                self.wrapped_at = None;
                self.next_tick_at = self.pos as f64;
                self.tick_len = self.step_samples(0) / TICKS_PER_STEP as f64;
                emit(Feedback::Started { time: self.time() });
            }
            Command::Stop => {
                if self.playing {
                    self.playing = false;
                    // The clips' notes end; notes held by keys do not.
                    self.release_clip_notes(None);
                    // Waiting switches land.
                    for slot in 0..self.tracks.len() {
                        if let Some((clip, _)) = self.tracks[slot].pending {
                            self.switch(slot, clip, 0, emit);
                        }
                    }
                    emit(Feedback::Stopped { time: self.time() });
                }
            }
        }
        None
    }

    /// Set a parameter in the engine (no base update: modulation uses it
    /// too).
    fn write_param(&mut self, target: ParamTarget, value: f32) {
        match target {
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
            ParamTarget::Send { ch, ret } => {
                if (ch as usize) < MAX_CHANNELS && (ret as usize) < MAX_CHANNELS {
                    self.sends[ch as usize][ret as usize] = value.clamp(0.0, 1.0);
                }
            }
            ParamTarget::Effect { slot, index } => {
                if let Some(Some(e)) = self.effects.get_mut(slot as usize) {
                    match index as usize {
                        0 => e.bypass = value >= 0.5,
                        1 => e.mix = value.clamp(0.0, 1.0),
                        i => e.effect.set_param(i - COMMON_PARAMS, value),
                    }
                }
            }
            ParamTarget::Modulator { slot, index } => {
                if let Some(Some(m)) = self.modulators.get_mut(slot as usize) {
                    m.set_param(index as usize, value);
                }
            }
        }
    }

    /// Advance the modulators by `n` frames and move every modulated target
    /// to its base plus its routes' sum.
    fn modulate(&mut self, n: usize) {
        let ctx = ModContext { sr: self.sr, tempo: self.globals[TEMPO].clamp(20.0, 300.0), beat: self.beat };
        self.beat += n as f64 / self.sr as f64 * ctx.tempo as f64 / 60.0;
        if self.routes.iter().all(Option::is_none) {
            return;
        }
        for m in self.modulators.iter_mut().flatten() {
            m.advance(n, &ctx);
        }
        for i in 0..MAX_MOD_ROUTES {
            let Some(r) = self.routes[i] else { continue };
            // Each target once: at its first route.
            if self.routes[..i].iter().flatten().any(|q| q.target == r.target) {
                continue;
            }
            let mut v = r.base;
            for q in self.routes[i..].iter().flatten().filter(|q| q.target == r.target) {
                let out = self.modulators.get(q.source as usize).and_then(|m| m.as_ref()).map_or(0.0, |m| m.output(q.output as usize));
                v += q.depth * (q.max - q.min) * out;
            }
            self.write_param(r.target, v.clamp(r.min, r.max));
        }
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

    /// Release the notes clips are playing: all of them, or those that end
    /// by tick `until`.
    fn release_clip_notes(&mut self, until: Option<u64>) {
        for slot in 0..self.tracks.len() {
            self.release_track_notes(slot, until);
        }
    }

    /// Release the notes of the slots that follow their arrangement (a
    /// locate or a loop's wrap; launched clips play on).
    fn release_arrangement_notes(&mut self) {
        for slot in 0..self.tracks.len() {
            if self.tracks[slot].launched.is_none() {
                self.release_track_notes(slot, None);
            }
        }
    }

    /// Release one slot's clip notes: all, or those that end by `until`.
    fn release_track_notes(&mut self, slot: usize, until: Option<u64>) {
        let t = &mut self.tracks[slot];
        let mut i = 0;
        while i < t.num_active {
            let (end, note) = t.active[i];
            if until.is_none_or(|u| end <= u) {
                t.num_active -= 1;
                t.active[i] = t.active[t.num_active];
                // A later clip note on the same pitch is still sounding:
                // ending this one must not cut it short.
                let still = t.active[..t.num_active].iter().any(|(e, n)| *n == note && until.is_some_and(|u| *e > u));
                if !still && let Some(Some(s)) = self.slots.get_mut(slot) {
                    s.instrument.note_off(note);
                }
            } else {
                i += 1;
            }
        }
    }

    /// Switch a slot to a launched clip (or back to its arrangement) from
    /// `tick` (from play, after any count-in), ending its clips' notes.
    fn switch(&mut self, slot: usize, clip: Option<u16>, tick: u64, emit: &mut impl FnMut(Feedback)) {
        self.release_track_notes(slot, None);
        let t = &mut self.tracks[slot];
        t.pending = None;
        t.launched = clip.map(|c| (c, tick));
        let pos = if self.playing && self.tick >= self.count_in { self.song_pos } else { self.start_pos };
        emit(Feedback::Launched { slot: slot as u8, clip, tick, pos, time: self.pos as f64 / self.sr as f64 });
    }

    /// Fire one tick: notes that end here are released, waiting launches
    /// that land here switch, then each slot's clip starts its events at
    /// this position.
    fn fire_tick(&mut self, emit: &mut impl FnMut(Feedback)) {
        let tick = self.tick;
        // Beats click from the count-in's first tick; `count_in` is whole
        // bars, so the song's downbeats are the count-in's too.
        if tick % PPQ as u64 == 0 && (tick < self.count_in || self.globals[METRONOME] >= 0.5) {
            self.click.start(self.sr, tick % TICKS_PER_BAR as u64 == 0);
        }
        if tick < self.count_in {
            self.tick_len = self.step_samples((tick / TICKS_PER_STEP as u64) as u32) / TICKS_PER_STEP as f64;
            self.next_tick_at += self.tick_len;
            self.tick = tick + 1;
            return;
        }
        // Ticks from the song's start, after any count-in.
        let song = tick - self.count_in;
        let time = self.time();
        let looping = self.song_loop();
        // A range loops when the song reaches its end (playing from past it
        // goes on); the whole song, from anywhere past it.
        let mut wrapped = false;
        if let Some((ls, le)) = looping
            && (self.song_pos == le || (self.globals[LOOP].round() as i32 == 1 && self.song_pos >= le))
        {
            self.release_arrangement_notes();
            self.song_pos = ls;
            self.wrapped_at = Some(tick);
            wrapped = true;
        }
        let song_pos = self.song_pos;
        self.release_clip_notes(Some(tick));
        // Launches and returns that land here: on a quantize boundary, or a
        // loop's wrap (a loop shorter than the quantum may hold none).
        let quantize = LAUNCH_QUANTIZE_TICKS[(self.globals[LAUNCH_QUANTIZE].round().max(0.0) as usize).min(4)] as u64;
        for slot in 0..self.tracks.len() {
            if let Some((clip, now)) = self.tracks[slot].pending
                && (now || wrapped || quantize == 0 || song_pos % quantize == 0)
            {
                self.switch(slot, clip, song, emit);
            }
        }
        // An empty song plays on (there is nothing to end), as do launched
        // clips.
        if looping.is_none()
            && !self.ended
            && self.song_end() > 0
            && song_pos >= self.song_end()
            && !self.tracks.iter().any(Track::launching)
        {
            self.ended = true;
            emit(Feedback::SongEnd { time });
        }
        if song_pos % TICKS_PER_STEP as u64 == 0 {
            emit(Feedback::Step { tick: song, pos: song_pos, time });
        }
        for (slot, t) in self.tracks.iter_mut().enumerate() {
            let Some(Some(s)) = self.slots.get_mut(slot) else { continue };
            // What plays here: a launched clip, looping from where it
            // started; else the placement under the song position (notes
            // end with it).
            let (clip, pos, until) = match t.launched {
                Some((c, since)) => {
                    let c = &self.clips[c as usize];
                    (c, ((song - since) % c.length.max(1) as u64) as u32, None)
                }
                None => {
                    let Some(p) = t.placement_at(song_pos) else { continue };
                    let c = &self.clips[p.clip as usize];
                    let into = song_pos - p.start as u64;
                    (c, ((into + p.offset as u64) % c.length.max(1) as u64) as u32, Some(p.length as u64 - into))
                }
            };
            let first = clip.events.partition_point(|e| e.tick < pos);
            for e in clip.events[first..].iter().take_while(|e| e.tick == pos) {
                let velocity = e.velocity as f32 / 127.0;
                if let Some(h) = s.instrument.note_on(e.note, velocity, None) {
                    emit(Feedback::Trigger {
                        slot: slot as u8,
                        voice: h.voice,
                        note: h.note,
                        velocity: h.velocity,
                        time,
                        step: Some(pos / TICKS_PER_STEP),
                    });
                }
                if t.num_active < MAX_ACTIVE {
                    let len = until.map_or(e.len.max(1) as u64, |u| (e.len.max(1) as u64).min(u));
                    t.active[t.num_active] = (tick + len, e.note);
                    t.num_active += 1;
                } else {
                    // Too many sounding: end this one at once rather than
                    // leave it stuck.
                    s.instrument.note_off(e.note);
                }
            }
        }
        // Swing pairs count from the song's first step.
        self.tick_len = self.step_samples((song_pos / TICKS_PER_STEP as u64) as u32) / TICKS_PER_STEP as f64;
        self.next_tick_at += self.tick_len;
        self.tick = tick + 1;
        self.song_pos = song_pos + 1;
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
                let until = (self.next_tick_at - self.pos as f64).ceil();
                if until <= 0.0 {
                    self.fire_tick(emit);
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
        self.modulate(n);
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

        let ctx = ProcessContext { tempo: self.globals[TEMPO].clamp(20.0, 300.0) };
        let master = &mut self.master_bus[..n * 2];
        master.fill(0.0);
        // Track channels first (their sends feed the returns), then the
        // returns: inserts, then the fader, into the master. Soloing only
        // returns hears just them: the tracks still send, but their dry
        // signal leaves the master.
        let solo = self.any_solo();
        let dry_to_master = if solo.returns && !solo.tracks { 0.0 } else { 1.0 };
        for returns in [false, true] {
            for i in 0..MAX_CHANNELS {
                let ch = &mut self.channels[i];
                if !ch.active || ch.ret != returns {
                    continue;
                }
                let len = self.chain_len[i] as usize;
                if len > 0 {
                    run_chain(&mut self.effects, &self.chains[i][..len], &mut ch.bus, &mut self.dry, n, &ctx);
                }
                for f in 0..n {
                    let g = ch.coef[f][0];
                    let l = ch.bus[f * 2] * g;
                    let r = ch.bus[f * 2 + 1] * g;
                    ch.peak[0] = ch.peak[0].max(l.abs());
                    ch.peak[1] = ch.peak[1].max(r.abs());
                    let to_master = if returns { 1.0 } else { dry_to_master };
                    self.master_bus[f * 2] += l * to_master;
                    self.master_bus[f * 2 + 1] += r * to_master;
                    self.post[f * 2] = l;
                    self.post[f * 2 + 1] = r;
                }
                if returns {
                    continue;
                }
                // Post-fader sends, ramped across the block, into each
                // return's bus through its balance.
                for j in 0..MAX_CHANNELS {
                    let (from, to) = (self.sends_at[i][j], self.sends[i][j]);
                    self.sends_at[i][j] = to;
                    let rc = &mut self.channels[j];
                    if (from == 0.0 && to == 0.0) || !rc.active || !rc.ret {
                        continue;
                    }
                    for f in 0..n {
                        let s = from + (to - from) * (f + 1) as f32 / n as f32;
                        let k = &rc.coef[f];
                        rc.bus[f * 2] += self.post[f * 2] * s * k[3];
                        rc.bus[f * 2 + 1] += self.post[f * 2 + 1] * s * k[4];
                    }
                }
            }
        }

        let len = self.chain_len[MASTER_CHAIN as usize] as usize;
        if len > 0 {
            run_chain(&mut self.effects, &self.chains[MASTER_CHAIN as usize][..len], &mut self.master_bus, &mut self.dry, n, &ctx);
        }

        let master_target = volume_to_gain(self.globals[MASTER_VOLUME]);
        let click_gain = 0.5 * volume_to_gain(self.globals[METRONOME_LEVEL].clamp(0.0, 1.0));
        for (f, o) in out.chunks_exact_mut(channels).enumerate().take(n) {
            let m = self.master.next(master_target);
            let l = soft_clip(self.master_bus[f * 2] * m);
            let r = soft_clip(self.master_bus[f * 2 + 1] * m);
            self.master_peak[0] = self.master_peak[0].max(l.abs());
            self.master_peak[1] = self.master_peak[1].max(r.abs());
            // The click is not part of the mix: after the master and its
            // meter.
            let c = self.click.next() * click_gain;
            let (l, r) = ((l + c).clamp(-1.0, 1.0), (r + c).clamp(-1.0, 1.0));
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
    use fours_protocol::{InstrumentType, NoteStep, STEP_ON, Voice, drum_event, note_event};

    /// Load clip 0 and launch it on slot 0.
    fn add_events(e: &mut Engine, events: impl IntoIterator<Item = ClipEvent>) {
        let _ = e.apply(Command::Launch { slot: 0, clip: Some(0), now: false }, &mut |_| {});
        load_clip(e, 0, events);
    }

    fn load_clip(e: &mut Engine, clip: u16, events: impl IntoIterator<Item = ClipEvent>) {
        for event in events {
            let _ = e.apply(Command::AddEvent { clip, event }, &mut |_| {});
        }
    }

    /// (voice, time) of each drum trigger.
    fn drum_hits(fb: &[Feedback]) -> Vec<(u8, f64)> {
        fb.iter().filter_map(|f| if let Feedback::Trigger { voice: Some(v), time, .. } = f { Some((*v, *time)) } else { None }).collect()
    }

    fn same_hits(a: &[(u8, f64)], b: &[(u8, f64)]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.0 == y.0 && (x.1 - y.1).abs() < 1e-3)
    }

    /// Load 303 note steps into slot 0's clip.
    fn add_notes(e: &mut Engine, steps: &[NoteStep]) {
        add_events(e, steps.iter().enumerate().filter_map(|(i, s)| note_event(i, s)));
    }

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
        add_events(&mut e, [0, 4].into_iter().filter_map(|s| drum_event(Voice::Kick, s, STEP_ON)));
        let _ = e.apply(Command::Play { count_in: 0 }, &mut |_| {});
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
        let steps: Vec<u64> =
            fb.iter().filter_map(|f| if let Feedback::Step { pos, .. } = f { Some(*pos / 24) } else { None }).collect();
        assert_eq!(&steps[..17], &(0..17).collect::<Vec<u64>>()[..]);
    }

    #[test]
    fn swing_delays_offbeats() {
        let mut e = drum_engine();
        let mut fb = vec![];
        let _ = e.apply(Command::SetParam { target: ParamTarget::Global(SWING), value: 1.0 }, &mut |_| {});
        let _ = e.apply(Command::Play { count_in: 0 }, &mut |_| {});
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
        add_notes(&mut e, &notes);
        // Square wave, and the decay/accent the bug was found with.
        for (index, value) in [(1u16, 1.0f32), (3, 0.5), (4, 0.5), (5, 0.62), (6, 0.74)] {
            let _ = e.apply(Command::SetParam { target: ParamTarget::Instrument { slot: 0, index }, value }, &mut fb);
        }
        let _ = e.apply(Command::Play { count_in: 0 }, &mut fb);
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

    /// Pitch bend moves a sounding 303 note: +2 semitones raises the
    /// waveform's rate by 2^(2/12) (counted as rising zero crossings of a
    /// bright saw).
    #[test]
    fn pitch_bend_bends_the_303() {
        let crossings = |bend: f32| {
            let mut e = note_engine();
            for (index, value) in [(2u16, 1.0f32), (4, 0.0)] {
                // Cutoff open, no envelope sweep: the saw passes through.
                let _ = e.apply(Command::SetParam { target: ParamTarget::Instrument { slot: 0, index }, value }, &mut |_| {});
            }
            let _ = e.apply(Command::NoteOn { slot: 0, note: 45, velocity: 0.7, gate: false }, &mut |_| {});
            let _ = e.apply(Command::PitchBend { slot: 0, semitones: bend }, &mut |_| {});
            render_secs(&mut e, 0.2, &mut vec![]); // let the bend settle
            let out = render_secs(&mut e, 0.5, &mut vec![]);
            let left: Vec<f32> = out.iter().step_by(2).copied().collect();
            left.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count() as f32
        };
        let ratio = crossings(2.0) / crossings(0.0);
        assert!((ratio - 2f32.powf(2.0 / 12.0)).abs() < 0.03, "ratio {ratio}");
    }

    /// A clip note that ends while a later one on the same pitch sounds must
    /// not cut that one short (both are C2; the first ends at tick 30, the
    /// second at 36 = 0.1875 s at 120 bpm).
    #[test]
    fn overlapping_same_note_is_not_cut_short() {
        let level = |events: &[ClipEvent]| {
            let mut e = note_engine();
            add_events(&mut e, events.iter().copied());
            let _ = e.apply(Command::Play { count_in: 0 }, &mut |_| {});
            let out = render_secs(&mut e, 0.2, &mut vec![]);
            let (a, b) = ((0.165 * 48000.0) as usize * 2, (0.18 * 48000.0) as usize * 2);
            out[a..b].iter().fold(0.0f32, |m, x| m.max(x.abs()))
        };
        let c2 = |tick, len| ClipEvent { tick, len, note: 36, velocity: 89 };
        let overlapped = level(&[c2(0, 30), c2(24, 12)]);
        let alone = level(&[c2(0, 30)]);
        assert!(overlapped > 0.05, "the second C2 should still sound: {overlapped}");
        assert!(alone < overlapped * 0.5, "without it the note has ended: {alone} vs {overlapped}");
    }

    /// A slid note glides in without retriggering the filter envelope: the
    /// level stays continuous across the step boundary, and no gap opens.
    #[test]
    fn tb303_slide_holds_the_gate() {
        let mut steps = [NoteStep::default(); 2];
        steps[0] = NoteStep { note: Some(36), accent: false, slide: true };
        steps[1] = NoteStep { note: Some(43), accent: false, slide: false };
        let mut slid = note_engine();
        add_notes(&mut slid, &steps);
        steps[0].slide = false;
        let mut plain = note_engine();
        add_notes(&mut plain, &steps);
        // Step 0 is 0..0.125s; without slide its gate closes at 0.0625s.
        let gap_level = |e: &mut Engine| {
            let _ = e.apply(Command::Play { count_in: 0 }, &mut |_| {});
            let out = render_secs(e, 0.25, &mut vec![]);
            let (a, b) = ((0.10 * 48000.0) as usize * 2, (0.12 * 48000.0) as usize * 2);
            out[a..b].iter().fold(0.0f32, |m, x| m.max(x.abs()))
        };
        let held = gap_level(&mut slid);
        let released = gap_level(&mut plain);
        assert!(held > 0.05, "slide should hold the gate: {held}");
        assert!(released < 0.01, "without slide the gate should close: {released}");
    }

    /// Live notes report the song tick the player heard: after a one-bar
    /// count-in (which only clicks), minus the output latency. 120 bpm =
    /// 192 ticks a second.
    #[test]
    fn live_notes_report_the_heard_tick() {
        let mut e = note_engine();
        let mut fb = vec![];
        e.set_output_latency(0.05);
        let _ = e.apply(Command::Play { count_in: TICKS_PER_BAR }, &mut |_| {});
        let out = render_secs(&mut e, 1.0, &mut fb);
        assert!(out.iter().any(|x| x.abs() > 0.1), "the count-in clicks");
        assert!(!fb.iter().any(|f| matches!(f, Feedback::Step { .. })), "no steps during the count-in");
        let live = |e: &mut Engine| {
            let mut got = None;
            let _ = e.apply(Command::NoteOn { slot: 0, note: 36, velocity: 0.7, gate: false }, &mut |f| {
                if let Feedback::Live { tick, .. } = f {
                    got = tick;
                }
            });
            got.expect("a live note while playing has a tick")
        };
        let t = live(&mut e);
        assert!((t - (-192.0 - 9.6)).abs() < 1.0, "1 s into a 2 s count-in, 50 ms latency: {t}");
        render_secs(&mut e, 1.5, &mut fb);
        let first = fb.iter().find_map(|f| if let Feedback::Step { tick, time, .. } = f { Some((*tick, *time)) } else { None });
        assert_eq!(first.map(|(t, _)| t), Some(0));
        assert!((first.unwrap().1 - 2.0).abs() < 1e-3, "tick 0 starts after the count-in: {first:?}");
        let t = live(&mut e);
        assert!((t - (96.0 - 9.6)).abs() < 1.0, "half a second into the song: {t}");
    }

    /// The transport plays placements from the locate point: a kick clip in
    /// bar 1 and a snare clip in bar 3 (120 bpm: a bar is 2 s). Without a
    /// loop it ends after bar 3; looping bars 2-3 it comes back to bar 2 at
    /// 6 s, so the snare plays again at 8 s.
    #[test]
    fn the_transport_plays_the_arrangement() {
        let hits = |looping: f32| {
            let mut e = drum_engine();
            load_clip(&mut e, 0, drum_event(Voice::Kick, 0, STEP_ON));
            load_clip(&mut e, 1, drum_event(Voice::Snare, 0, STEP_ON));
            let mut fb = vec![];
            let mut apply = |c| {
                let _ = e.apply(c, &mut |_| {});
            };
            apply(Command::AddPlacement { slot: 0, placement: Placement { start: 0, length: 384, offset: 0, clip: 0 } });
            apply(Command::AddPlacement { slot: 0, placement: Placement { start: 768, length: 384, offset: 0, clip: 1 } });
            for (g, v) in [(LOOP, looping), (LOOP_START, 1.0), (LOOP_END, 3.0)] {
                apply(Command::SetParam { target: ParamTarget::Global(g), value: v });
            }
            apply(Command::Play { count_in: 0 });
            render_secs(&mut e, 9.0, &mut fb);
            let end = fb.iter().find_map(|f| if let Feedback::SongEnd { time } = f { Some(*time) } else { None });
            (drum_hits(&fb), end)
        };
        let (once, end) = hits(0.0);
        assert!(same_hits(&once, &[(0, 0.0), (1, 4.0)]), "{once:?}");
        assert!(end.is_some_and(|t| (t - 6.0).abs() < 1e-3), "{end:?}");
        let (looped, end) = hits(2.0);
        assert!(same_hits(&looped, &[(0, 0.0), (1, 4.0), (1, 8.0)]), "{looped:?}");
        assert_eq!(end, None);
    }

    /// Clip launching (RFC 0009): the arrangement has a snare clip over
    /// bars 1-4; a kick clip (half a bar long) launched while playing, at
    /// 0.3 s, waits for bar 2 (2 s) and loops from there every second,
    /// whatever the song does. Going back at 4.3 s lands on bar 4 (6 s),
    /// past the song's end (bar 3): launched tracks kept it playing, and it
    /// ends as soon as the track is back.
    #[test]
    fn launched_clips_replace_the_arrangement_from_a_bar() {
        let mut e = drum_engine();
        load_clip(&mut e, 1, [0, 4, 8, 12].into_iter().filter_map(|s| drum_event(Voice::Snare, s, STEP_ON)));
        load_clip(&mut e, 2, drum_event(Voice::Kick, 0, STEP_ON));
        let mut fb = vec![];
        let apply = |e: &mut Engine, c, fb: &mut Vec<Feedback>| {
            let _ = e.apply(c, &mut |f| fb.push(f));
        };
        apply(&mut e, Command::SetClipLength { clip: 2, length: 192 }, &mut fb);
        apply(&mut e, Command::AddPlacement { slot: 0, placement: Placement { start: 0, length: 3 * 384, offset: 0, clip: 1 } }, &mut fb);
        apply(&mut e, Command::SetParam { target: ParamTarget::Global(LOOP), value: 0.0 }, &mut fb);
        apply(&mut e, Command::Play { count_in: 0 }, &mut fb);
        render_secs(&mut e, 0.3, &mut fb);
        apply(&mut e, Command::Launch { slot: 0, clip: Some(2), now: false }, &mut fb);
        render_secs(&mut e, 4.0, &mut fb);
        apply(&mut e, Command::Launch { slot: 0, clip: None, now: false }, &mut fb);
        render_secs(&mut e, 4.0, &mut fb);
        let kicks: Vec<f64> = drum_hits(&fb).iter().filter(|h| h.0 == 0).map(|h| h.1).collect();
        assert_eq!(kicks.len(), 4, "{kicks:?}");
        for (k, x) in kicks.iter().zip([2.0, 3.0, 4.0, 5.0]) {
            assert!((k - x).abs() < 1e-3, "{kicks:?}");
        }
        let snares: Vec<f64> = drum_hits(&fb).iter().filter(|h| h.0 == 1).map(|h| h.1).collect();
        assert!(same_hits(&snares.iter().map(|t| (1, *t)).collect::<Vec<_>>(), &[(1, 0.0), (1, 0.5), (1, 1.0), (1, 1.5)]), "{snares:?}");
        let launched: Vec<(Option<u16>, u64)> =
            fb.iter().filter_map(|f| if let Feedback::Launched { clip, tick, .. } = f { Some((*clip, *tick)) } else { None }).collect();
        assert_eq!(launched, [(Some(2), 384), (None, 1152)]);
        // Bar 4 is past the song's end: it ends as soon as the track is back.
        let end = fb.iter().find_map(|f| if let Feedback::SongEnd { time } = f { Some(*time) } else { None });
        assert!(end.is_some_and(|t| (t - 6.0).abs() < 1e-3), "{end:?}");
    }

    /// A send feeds its return after the fader: the kick on channel 0 sent
    /// to a return on channel 1 shows on the return's meter, and not when
    /// the track is muted (post-fader) or the send is at zero.
    #[test]
    fn sends_feed_returns_after_the_fader() {
        let return_peak = |send: f32, mute: f32| {
            let mut e = drum_engine();
            let mut fb = vec![];
            let mut apply = |c| {
                let _ = e.apply(c, &mut |_| {});
            };
            apply(Command::SetChannelActive { ch: 1, active: true });
            apply(Command::SetChannelReturn { ch: 1, ret: true });
            apply(Command::SetParam { target: ParamTarget::Send { ch: 0, ret: 1 }, value: send });
            apply(Command::SetParam { target: ParamTarget::Channel { ch: 0, index: CH_MUTE as u8 }, value: mute });
            e.snap();
            let _ = e.apply(Command::NoteOn { slot: 0, note: 36, velocity: 1.0, gate: false }, &mut |_| {});
            render_secs(&mut e, 0.2, &mut fb);
            fb.iter().filter_map(|f| if let Feedback::Meters { channels, .. } = f { Some(channels[1][0]) } else { None }).fold(0.0f32, f32::max)
        };
        assert!(return_peak(1.0, 0.0) > 0.05, "{}", return_peak(1.0, 0.0));
        assert!(return_peak(1.0, 1.0) < 1e-4, "muted: {}", return_peak(1.0, 1.0));
        assert!(return_peak(0.0, 0.0) < 1e-4, "no send: {}", return_peak(0.0, 0.0));
    }

    /// A modulation route moves its target around the value set on it: an
    /// LFO on channel 0's pan swings the kick between the sides; clearing
    /// the route puts the pan back where it was set.
    #[test]
    fn a_route_modulates_around_the_base() {
        let mut e = drum_engine();
        let pan = ParamTarget::Channel { ch: 0, index: CH_PAN as u8 };
        let mut apply = |e: &mut Engine, c| {
            let _ = e.apply(c, &mut |_| {});
        };
        apply(&mut e, Command::AddModulator { slot: 0, modulator: crate::modulator::make(fours_protocol::DeviceType::Lfo) });
        apply(&mut e, Command::SetParam { target: pan, value: 0.2 });
        let route = ModRoute { source: 0, output: 0, target: pan, depth: 0.5, min: -1.0, max: 1.0, base: 0.2 };
        apply(&mut e, Command::SetModRoute { index: 0, route: Some(route) });
        apply(&mut e, Command::Play { count_in: 0 });
        // One bar's sine at 120 bpm: a beat in (0.5 s) it is at its top.
        render_secs(&mut e, 0.5, &mut vec![]);
        let top = e.channels[0].params[CH_PAN];
        assert!((top - 1.0).abs() < 0.05, "base 0.2 + 0.5 x range 2: {top}");
        render_secs(&mut e, 1.0, &mut vec![]);
        let bottom = e.channels[0].params[CH_PAN];
        assert!((bottom + 0.8).abs() < 0.05, "0.2 - 1.0 three beats in: {bottom}");
        // Setting the pan moves the base; clearing the route goes back to it.
        apply(&mut e, Command::SetParam { target: pan, value: -0.3 });
        apply(&mut e, Command::SetModRoute { index: 0, route: None });
        assert_eq!(e.channels[0].params[CH_PAN], -0.3);
    }

    /// A waiting launch lands on a loop's wrap: a one-bar loop over bar 2
    /// holds no multiple of a two-bar quantum (768 ticks), so without that
    /// it would wait forever. Played from bar 2 (2 s at 120 bpm a bar), the
    /// launch at 0.3 s lands when the loop comes round, at 2 s.
    #[test]
    fn a_launch_lands_on_a_loop_wrap() {
        let mut e = drum_engine();
        load_clip(&mut e, 2, drum_event(Voice::Kick, 0, STEP_ON));
        let mut fb = vec![];
        let mut apply = |c| {
            let _ = e.apply(c, &mut |_| {});
        };
        for (g, v) in [(LOOP, 2.0), (LOOP_START, 1.0), (LOOP_END, 2.0), (LAUNCH_QUANTIZE, 3.0)] {
            apply(Command::SetParam { target: ParamTarget::Global(g), value: v });
        }
        apply(Command::Locate { tick: 384 });
        apply(Command::Play { count_in: 0 });
        render_secs(&mut e, 0.3, &mut fb);
        let _ = e.apply(Command::Launch { slot: 0, clip: Some(2), now: false }, &mut |_| {});
        render_secs(&mut e, 2.0, &mut fb);
        assert!(same_hits(&drum_hits(&fb), &[(0, 2.0)]), "{:?}", drum_hits(&fb));
    }

    /// Back to the arrangement mid-placement plays the placement from where
    /// the song is, and `now` does not wait for the bar.
    #[test]
    fn back_now_picks_up_the_placement_mid_way() {
        let mut e = drum_engine();
        load_clip(&mut e, 1, [0, 4, 8, 12].into_iter().filter_map(|s| drum_event(Voice::Snare, s, STEP_ON)));
        load_clip(&mut e, 2, drum_event(Voice::Kick, 0, STEP_ON));
        let mut fb = vec![];
        let _ = e.apply(Command::AddPlacement { slot: 0, placement: Placement { start: 0, length: 384, offset: 0, clip: 1 } }, &mut |_| {});
        let _ = e.apply(Command::Launch { slot: 0, clip: Some(2), now: false }, &mut |_| {});
        let _ = e.apply(Command::Play { count_in: 0 }, &mut |_| {});
        render_secs(&mut e, 0.9, &mut fb);
        let _ = e.apply(Command::Launch { slot: 0, clip: None, now: true }, &mut |_| {});
        render_secs(&mut e, 1.0, &mut fb);
        assert!(same_hits(&drum_hits(&fb), &[(0, 0.0), (1, 1.0), (1, 1.5)]), "{:?}", drum_hits(&fb));
    }

    /// Entering a bar loop from before it is not a wrap: a note heard just
    /// before the loop's start stays there. Only after a real wrap is what
    /// is heard placed at the loop's end. (120 bpm: 192 ticks a second; 50
    /// ms of latency is 9.6 ticks.)
    #[test]
    fn heard_position_wraps_only_after_a_wrap() {
        let mut e = note_engine();
        e.set_output_latency(0.05);
        for (g, v) in [(LOOP, 2.0), (LOOP_START, 1.0), (LOOP_END, 2.0)] {
            let _ = e.apply(Command::SetParam { target: ParamTarget::Global(g), value: v }, &mut |_| {});
        }
        let _ = e.apply(Command::Play { count_in: 0 }, &mut |_| {});
        let pos = |e: &mut Engine| e.heard_tick().map(|(_, p)| p).unwrap();
        render_secs(&mut e, 2.01, &mut vec![]);
        let p = pos(&mut e);
        assert!((p - (384.0 + 1.92 - 9.6)).abs() < 2.0, "entering the loop: {p}");
        // Past the wrap at bar 3 (4 s), back at bar 2.
        render_secs(&mut e, 2.0, &mut vec![]);
        let p = pos(&mut e);
        assert!((p - (768.0 + 1.92 - 9.6)).abs() < 2.0, "just after the wrap, heard before it: {p}");
    }
}

