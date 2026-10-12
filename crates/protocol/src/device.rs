//! Devices (RFC 0010): every kind of node the engine runs. Instruments keep
//! their own type (`InstrumentType`); `DeviceType` covers the other kinds
//! (audio effects now, modulators next). Every device has an id from the
//! instruments' namespace (the first segment of its parameter paths),
//! typed ports, and parameters.

use crate::layout::NoteLayout;
use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Most audio effects in a project.
pub const MAX_EFFECTS: usize = 64;
/// Most modulators in a project.
pub const MAX_MODULATORS: usize = 8;
/// Most modulation routes in a project.
pub const MAX_MOD_ROUTES: usize = 64;
/// Most inserts on one channel (or the master).
pub const MAX_INSERTS: usize = 8;
/// The channel number that means the master bus in a device placement.
pub const MASTER: u32 = 0;

/// What a device does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Notes in, audio out.
    Instrument,
    /// Audio in, audio out: an insert on a channel or the master.
    AudioEffect,
    /// Modulation out, routed to parameters.
    Modulator,
}

/// A device type that is not an instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceType {
    /// State-variable filter: low, band, or high pass.
    Filter,
    /// Stereo delay, free or tempo-synced, with feedback and ping-pong.
    Delay,
    /// A small Freeverb-style room.
    Reverb,
    /// Four LFOs, free or synced to the transport (a modulator).
    Lfo,
}

impl DeviceType {
    pub const ALL: [DeviceType; 4] = [DeviceType::Filter, DeviceType::Delay, DeviceType::Reverb, DeviceType::Lfo];

    pub fn id(self) -> &'static str {
        match self {
            DeviceType::Filter => "filter",
            DeviceType::Delay => "delay",
            DeviceType::Reverb => "reverb",
            DeviceType::Lfo => "lfo",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DeviceType::Filter => "Filter",
            DeviceType::Delay => "Delay",
            DeviceType::Reverb => "Reverb",
            DeviceType::Lfo => "LFO",
        }
    }

    /// Id given to a new instance when none is chosen (`delay2`, ...).
    pub fn default_id(self) -> &'static str {
        self.id()
    }

    pub fn kind(self) -> DeviceKind {
        match self {
            DeviceType::Lfo => DeviceKind::Modulator,
            _ => DeviceKind::AudioEffect,
        }
    }

    pub fn parse(s: &str) -> Option<DeviceType> {
        match s.trim().to_ascii_lowercase().as_str() {
            "filter" | "svf" => Some(DeviceType::Filter),
            "delay" | "echo" => Some(DeviceType::Delay),
            "reverb" | "verb" => Some(DeviceType::Reverb),
            "lfo" => Some(DeviceType::Lfo),
            _ => None,
        }
    }

    /// Its ports: audio effects are stereo in, stereo out; an LFO bank has
    /// four modulation outputs.
    pub fn ports(self) -> Vec<PortInfo> {
        match self.kind() {
            DeviceKind::Modulator => (1..=4)
                .map(|k| PortInfo { id: k.to_string(), kind: PortKind::Modulation, direction: PortDirection::Out, width: None })
                .collect(),
            _ => vec![PortInfo::audio("in", PortDirection::In), PortInfo::audio("out", PortDirection::Out)],
        }
    }
}

impl InstrumentType {
    /// Its ports: notes in, then its audio outputs.
    pub fn ports(self) -> Vec<PortInfo> {
        vec![PortInfo { id: "notes".into(), kind: PortKind::Notes, direction: PortDirection::In, width: None }]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortKind {
    Audio,
    Notes,
    Modulation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PortDirection {
    In,
    Out,
}

/// A port a device declares. Connections are implied by where the device
/// sits for now (RFC 0010); a patching graph wires ports later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct PortInfo {
    pub id: String,
    pub kind: PortKind,
    pub direction: PortDirection,
    /// Audio ports: mono or stereo.
    pub width: Option<OutputWidth>,
}

impl PortInfo {
    fn audio(id: &str, direction: PortDirection) -> Self {
        PortInfo { id: id.into(), kind: PortKind::Audio, direction, width: Some(OutputWidth::Stereo) }
    }
}

/// A device that is not an instrument, and where it sits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct DeviceInfo {
    pub id: String,
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: DeviceType,
    pub name: String,
    /// Audio effects: the channel whose insert chain holds it (0 = the
    /// master); modulators: none.
    pub channel: Option<u32>,
    /// Its place in that chain (or, a modulator, in the modulator rack),
    /// from 0.
    pub position: u32,
}

/// A modulation route (RFC 0010): a modulator output moving a continuous
/// parameter by `depth` of its range around the value it is set to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ModRoute {
    /// A modulator output, `<id>.<k>` (`lfo.1`).
    pub source: String,
    /// A parameter path.
    pub target: String,
    /// -1..1.
    pub depth: f32,
}

/// One entry of the device registry (`device.types`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct DeviceTypeInfo {
    /// The type's id (`tr808`, `delay`).
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub id: String,
    pub kind: DeviceKind,
    pub label: String,
    pub default_id: String,
    pub ports: Vec<PortInfo>,
    /// Parameters of an instance with the default id.
    pub params: Vec<ParamInfo>,
    pub knob_pages: Vec<KnobPage>,
    /// Instruments: how their notes show in a step view.
    pub layout: Option<NoteLayout>,
}
