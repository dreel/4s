//! Parameter specs. On the wire, parameters are addressed by path; inside the
//! engine by a `ParamTarget` (see `engine.rs`). The daemon builds the live
//! registry from the pieces here: the globals, each active channel, and each
//! instrument's parameters (`instrument::params`).

use fours_protocol::{PPQ, ParamInfo, ParamKind, TICKS_PER_BAR};

/// Global parameters, indexed by `ParamTarget::Global`.
pub const TEMPO: usize = 0;
pub const SWING: usize = 1;
pub const MASTER_VOLUME: usize = 2;
/// The metronome clicks while the transport plays (it always clicks during
/// a count-in).
pub const METRONOME: usize = 3;
pub const METRONOME_LEVEL: usize = 4;
/// What the transport loops (RFC 0008): 0 nothing (stop at the song's end),
/// 1 the whole song, 2 the range `LOOP_START..LOOP_END` (bars).
pub const LOOP: usize = 5;
pub const LOOP_START: usize = 6;
pub const LOOP_END: usize = 7;
/// Where a clip launch or a return to the arrangement lands while playing
/// (RFC 0009): an index into `LAUNCH_QUANTIZE_TICKS`.
pub const LAUNCH_QUANTIZE: usize = 8;
pub const NUM_GLOBALS: usize = 9;

/// `transport.launch_quantize` values in ticks: none (the next tick), a
/// beat, a bar, two bars, four bars.
pub const LAUNCH_QUANTIZE_TICKS: [u32; 5] = [0, PPQ, TICKS_PER_BAR, 2 * TICKS_PER_BAR, 4 * TICKS_PER_BAR];

/// Channel parameters, indexed by `ParamTarget::Channel`.
pub const CH_VOLUME: usize = 0;
pub const CH_PAN: usize = 1;
pub const CH_MUTE: usize = 2;
pub const CH_SOLO: usize = 3;
pub const CHANNEL_PARAMS: usize = 4;

pub fn cont(path: String, label: String, min: f64, max: f64, default: f64, unit: Option<&str>) -> ParamInfo {
    ParamInfo { path, label, kind: ParamKind::Continuous { min, max }, default, unit: unit.map(str::to_string) }
}

fn integer(path: &str, label: &str, min: i32, max: i32, default: f64, unit: Option<&str>) -> ParamInfo {
    ParamInfo {
        path: path.into(),
        label: label.into(),
        kind: ParamKind::Integer { min, max },
        default,
        unit: unit.map(str::to_string),
    }
}

pub fn toggle(path: String, label: String) -> ParamInfo {
    ParamInfo { path, label, kind: ParamKind::Toggle, default: 0.0, unit: None }
}

/// Global parameters, in index order.
pub fn globals() -> Vec<ParamInfo> {
    vec![
        cont("transport.tempo".into(), "Tempo".into(), 20.0, 300.0, 120.0, Some("bpm")),
        cont("transport.swing".into(), "Swing".into(), 0.0, 1.0, 0.0, None),
        cont("mixer.master.volume".into(), "Master Volume".into(), 0.0, 1.0, 0.8, None),
        toggle("metronome.on".into(), "Metronome".into()),
        cont("metronome.level".into(), "Metronome Level".into(), 0.0, 1.0, 0.6, None),
        integer("song.loop", "Song Loop", 0, 2, 1.0, None),
        integer("song.loop_start", "Loop Start", 0, 998, 0.0, Some("bar")),
        integer("song.loop_end", "Loop End", 1, 999, 4.0, Some("bar")),
        integer("transport.launch_quantize", "Launch Quantize", 0, 4, 2.0, None),
    ]
}

/// Parameters of mixer channel `n` (1-based), in index order. The fader
/// defaults to unity: levels are set inside instruments.
pub fn channel(n: u32, name: &str) -> Vec<ParamInfo> {
    vec![
        cont(format!("mixer.{n}.volume"), format!("{name} Volume"), 0.0, 1.0, 1.0, None),
        cont(format!("mixer.{n}.pan"), format!("{name} Pan"), -1.0, 1.0, 0.0, None),
        toggle(format!("mixer.{n}.mute"), format!("{name} Mute")),
        toggle(format!("mixer.{n}.solo"), format!("{name} Solo")),
    ]
}

pub fn channel_defaults() -> [f32; CHANNEL_PARAMS] {
    [1.0, 0.0, 0.0, 0.0]
}

pub fn global_defaults() -> [f32; NUM_GLOBALS] {
    let mut d = [0.0; NUM_GLOBALS];
    for (i, p) in globals().iter().enumerate() {
        d[i] = p.default as f32;
    }
    d
}

/// Fader position (0..1) to linear gain. Squared for a more natural taper:
/// 0.8 -> 0.64 (about -3.9 dB), 0.5 -> 0.25 (-12 dB).
pub fn volume_to_gain(v: f32) -> f32 {
    v * v
}

/// Constant-power pan for a mono source: (left, right) gains, -3 dB each at
/// center.
pub fn mono_pan(pan: f32) -> (f32, f32) {
    let angle = (pan.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
    (angle.cos(), angle.sin())
}

/// Balance for a stereo source: unity at center; moving toward one side
/// attenuates only the other side.
pub fn balance(pan: f32) -> (f32, f32) {
    let p = pan.clamp(-1.0, 1.0);
    ((1.0 - p).min(1.0), (1.0 + p).min(1.0))
}
