//! The parameter registry. Parameters are identified on the wire by path and
//! inside the engine by a dense index (`ParamId`) into a flat value array.
//! Index layout is fixed by the functions below; `registry()` produces entries
//! in exactly that order.

use crate::voices::default_decay;
use fours_protocol::{NUM_TRACKS, ParamInfo, ParamKind, Voice};

pub type ParamId = usize;

pub const TEMPO: ParamId = 0;
pub const SWING: ParamId = 1;
pub const LENGTH: ParamId = 2;
const VOICE_BASE: ParamId = 3;
const VOICE_PARAMS: usize = 3;
const MIXER_BASE: ParamId = VOICE_BASE + NUM_TRACKS * VOICE_PARAMS;
const MIXER_PARAMS: usize = 4;
pub const MASTER_VOLUME: ParamId = MIXER_BASE + NUM_TRACKS * MIXER_PARAMS;
pub const NUM_PARAMS: usize = MASTER_VOLUME + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoiceParam {
    Tune = 0,
    Decay = 1,
    Tone = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixerParam {
    Volume = 0,
    Pan = 1,
    Mute = 2,
    Solo = 3,
}

pub fn voice_param(track: usize, p: VoiceParam) -> ParamId {
    VOICE_BASE + track * VOICE_PARAMS + p as usize
}

pub fn mixer_param(track: usize, p: MixerParam) -> ParamId {
    MIXER_BASE + track * MIXER_PARAMS + p as usize
}

fn cont(path: String, label: String, min: f64, max: f64, default: f64, unit: Option<&str>) -> ParamInfo {
    ParamInfo {
        path,
        label,
        kind: ParamKind::Continuous { min, max },
        default,
        unit: unit.map(str::to_string),
    }
}

fn toggle(path: String, label: String) -> ParamInfo {
    ParamInfo { path, label, kind: ParamKind::Toggle, default: 0.0, unit: None }
}

/// All parameters, in `ParamId` order.
pub fn registry() -> Vec<ParamInfo> {
    let mut r = Vec::with_capacity(NUM_PARAMS);
    r.push(cont("transport.tempo".into(), "Tempo".into(), 20.0, 300.0, 120.0, Some("bpm")));
    r.push(cont("transport.swing".into(), "Swing".into(), 0.0, 1.0, 0.0, None));
    r.push(ParamInfo {
        path: "sequencer.length".into(),
        label: "Length".into(),
        kind: ParamKind::Integer { min: 1, max: fours_protocol::MAX_STEPS as i32 },
        default: 16.0,
        unit: Some("steps".into()),
    });
    for v in Voice::ALL {
        let (id, l) = (v.id(), v.label());
        r.push(cont(format!("drums.{id}.tune"), format!("{l} Tune"), -12.0, 12.0, 0.0, Some("st")));
        r.push(cont(format!("drums.{id}.decay"), format!("{l} Decay"), 0.0, 1.0, default_decay(v), None));
        r.push(cont(format!("drums.{id}.tone"), format!("{l} Tone"), 0.0, 1.0, 0.5, None));
    }
    for (i, v) in Voice::ALL.iter().enumerate() {
        let n = i + 1;
        let l = v.label();
        r.push(cont(format!("mixer.{n}.volume"), format!("{l} Volume"), 0.0, 1.0, 0.8, None));
        r.push(cont(format!("mixer.{n}.pan"), format!("{l} Pan"), -1.0, 1.0, 0.0, None));
        r.push(toggle(format!("mixer.{n}.mute"), format!("{l} Mute")));
        r.push(toggle(format!("mixer.{n}.solo"), format!("{l} Solo")));
    }
    r.push(cont("mixer.master.volume".into(), "Master Volume".into(), 0.0, 1.0, 0.8, None));
    debug_assert_eq!(r.len(), NUM_PARAMS);
    r
}

pub fn default_values() -> Vec<f32> {
    registry().iter().map(|p| p.default as f32).collect()
}

/// Fader position (0..1) to linear gain. Squared for a more natural taper:
/// 0.8 -> 0.64 (about -3.9 dB), 0.5 -> 0.25 (-12 dB).
pub fn volume_to_gain(v: f32) -> f32 {
    v * v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_registry() {
        let r = registry();
        assert_eq!(r.len(), NUM_PARAMS);
        assert_eq!(r[TEMPO].path, "transport.tempo");
        assert_eq!(r[SWING].path, "transport.swing");
        assert_eq!(r[LENGTH].path, "sequencer.length");
        assert_eq!(r[voice_param(3, VoiceParam::Decay)].path, "drums.closed_hat.decay");
        assert_eq!(r[mixer_param(2, MixerParam::Volume)].path, "mixer.3.volume");
        assert_eq!(r[mixer_param(7, MixerParam::Solo)].path, "mixer.8.solo");
        assert_eq!(r[MASTER_VOLUME].path, "mixer.master.volume");
        let mut paths: Vec<_> = r.iter().map(|p| &p.path).collect();
        paths.sort();
        paths.dedup();
        assert_eq!(paths.len(), NUM_PARAMS, "paths must be unique");
    }
}
