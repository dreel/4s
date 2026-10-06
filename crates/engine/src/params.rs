//! Parameter specs. On the wire, parameters are addressed by path; inside the
//! engine by a `ParamTarget` (see `engine.rs`). The daemon builds the live
//! registry from the pieces here: the globals, each active channel, and each
//! instrument's parameters (`instrument::params`).

use fours_protocol::{ParamInfo, ParamKind};

/// Global parameters, indexed by `ParamTarget::Global`.
pub const TEMPO: usize = 0;
pub const SWING: usize = 1;
pub const LENGTH: usize = 2;
pub const MASTER_VOLUME: usize = 3;
pub const NUM_GLOBALS: usize = 4;

/// Channel parameters, indexed by `ParamTarget::Channel`.
pub const CH_VOLUME: usize = 0;
pub const CH_PAN: usize = 1;
pub const CH_MUTE: usize = 2;
pub const CH_SOLO: usize = 3;
pub const CHANNEL_PARAMS: usize = 4;

pub fn cont(path: String, label: String, min: f64, max: f64, default: f64, unit: Option<&str>) -> ParamInfo {
    ParamInfo { path, label, kind: ParamKind::Continuous { min, max }, default, unit: unit.map(str::to_string) }
}

pub fn toggle(path: String, label: String) -> ParamInfo {
    ParamInfo { path, label, kind: ParamKind::Toggle, default: 0.0, unit: None }
}

/// Global parameters, in index order.
pub fn globals() -> Vec<ParamInfo> {
    vec![
        cont("transport.tempo".into(), "Tempo".into(), 20.0, 300.0, 120.0, Some("bpm")),
        cont("transport.swing".into(), "Swing".into(), 0.0, 1.0, 0.0, None),
        ParamInfo {
            path: "sequencer.length".into(),
            label: "Length".into(),
            kind: ParamKind::Integer { min: 1, max: fours_protocol::MAX_STEPS as i32 },
            default: 16.0,
            unit: Some("steps".into()),
        },
        cont("mixer.master.volume".into(), "Master Volume".into(), 0.0, 1.0, 0.8, None),
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
