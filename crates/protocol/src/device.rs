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

/// Pads in a `sampler`.
pub const SAMPLER_PADS: u32 = 16;
/// The largest chunk of a `sample.upload`, decoded from base64 (its
/// base64 then fits a 16 MiB WebSocket frame).
pub const MAX_UPLOAD_BYTES: usize = 11 * 1024 * 1024;
/// The largest file an upload can send, in chunks.
pub const MAX_UPLOAD_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// A loaded pad of a sampler (RFC 0010, regions RFC 0011).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SamplerPad {
    /// 1..16.
    pub pad: u32,
    /// The file's name, without its folders.
    pub name: String,
    /// Where it is on the engine host.
    pub path: String,
    /// The region of the file the pad plays, in seconds.
    pub start: f64,
    pub end: f64,
    /// The region's length (`end - start`).
    pub seconds: f64,
    /// The whole file's length.
    pub file_seconds: f64,
    /// For a project's own copy of a region: the file and region it was
    /// cut from, to edit again if that file is still there.
    pub origin: Option<SampleOrigin>,
}

/// Where a region came from: a file on the engine host and the seconds
/// of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SampleOrigin {
    pub path: String,
    pub start: f64,
    pub end: f64,
}

/// An audio file on the engine host (`sample.info`), decoded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SampleInfo {
    pub path: String,
    pub name: String,
    pub seconds: f64,
    /// Codec, rate, and channels, e.g. `mp3, 44100 Hz, 2 ch`.
    pub format: String,
}

/// A waveform overview (`sample.peaks`): `bins` (min, max) pairs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SamplePeaks {
    pub start: f64,
    pub end: f64,
    pub peaks: Vec<[f32; 2]>,
}

/// What an audition plays (`sample.play`): the region as it was cut.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SamplePlaying {
    pub path: String,
    pub start: f64,
    pub end: f64,
}

/// Where an upload is (`sample.upload`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SampleUploadResult {
    pub upload: String,
    /// Bytes received so far.
    #[ts(type = "number")]
    pub received: u64,
    /// Once done: where the file is stored on the engine host.
    pub path: Option<String>,
}

/// A sampler's loaded pads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SamplerInfo {
    pub instrument: String,
    pub pads: Vec<SamplerPad>,
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64, with padding.
pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decode standard base64 (whitespace ignored).
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        let v = B64.iter().position(|b| *b == c).ok_or("invalid base64")? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for data in [&b""[..], b"f", b"fo", b"foo", b"foob", b"fooba", b"foobar", &[0, 255, 128, 7]] {
            let e = base64_encode(data);
            assert_eq!(base64_decode(&e).unwrap(), data, "{e}");
        }
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
        assert!(base64_decode("@@").is_err());
    }
}
