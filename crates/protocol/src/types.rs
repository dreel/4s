use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Maximum number of steps a pattern can hold. The active length is the
/// `sequencer.length` parameter.
pub const MAX_STEPS: usize = 64;

/// Number of drum tracks in a `tr808` (one per voice, one per Livid Block
/// row).
pub const NUM_TRACKS: usize = 8;

/// Most instruments the engine can hold at once.
pub const MAX_INSTRUMENTS: usize = 16;

/// Most mixer channels. Channel numbers are 1..=MAX_CHANNELS.
pub const MAX_CHANNELS: usize = 32;

/// Step levels.
pub const STEP_OFF: u8 = 0;
pub const STEP_ON: u8 = 1;
pub const STEP_ACCENT: u8 = 2;

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

/// The eight TR-808-style drum voices. Order is the track order.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Voice {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
    LowTom,
    HighTom,
    Cowbell,
}

impl Voice {
    pub const ALL: [Voice; NUM_TRACKS] = [
        Voice::Kick,
        Voice::Snare,
        Voice::Clap,
        Voice::ClosedHat,
        Voice::OpenHat,
        Voice::LowTom,
        Voice::HighTom,
        Voice::Cowbell,
    ];

    pub fn index(self) -> usize {
        Voice::ALL.iter().position(|v| *v == self).unwrap()
    }

    pub fn from_index(i: usize) -> Option<Voice> {
        Voice::ALL.get(i).copied()
    }

    /// Stable id used in parameter paths and on the wire, e.g. `closed_hat`.
    pub fn id(self) -> &'static str {
        match self {
            Voice::Kick => "kick",
            Voice::Snare => "snare",
            Voice::Clap => "clap",
            Voice::ClosedHat => "closed_hat",
            Voice::OpenHat => "open_hat",
            Voice::LowTom => "low_tom",
            Voice::HighTom => "high_tom",
            Voice::Cowbell => "cowbell",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Voice::Kick => "Kick",
            Voice::Snare => "Snare",
            Voice::Clap => "Clap",
            Voice::ClosedHat => "Closed Hat",
            Voice::OpenHat => "Open Hat",
            Voice::LowTom => "Low Tom",
            Voice::HighTom => "High Tom",
            Voice::Cowbell => "Cowbell",
        }
    }

    /// Parse an id (`closed_hat`), short alias (`ch`, `bd`), or 1-based track
    /// number (`4`).
    pub fn parse(s: &str) -> Option<Voice> {
        let s = s.trim().to_ascii_lowercase();
        if let Ok(n) = s.parse::<usize>() {
            return n.checked_sub(1).and_then(Voice::from_index);
        }
        let v = match s.as_str() {
            "kick" | "bd" => Voice::Kick,
            "snare" | "sd" => Voice::Snare,
            "clap" | "cp" => Voice::Clap,
            "closed_hat" | "closedhat" | "ch" | "hh" => Voice::ClosedHat,
            "open_hat" | "openhat" | "oh" => Voice::OpenHat,
            "low_tom" | "lowtom" | "lt" => Voice::LowTom,
            "high_tom" | "hightom" | "ht" => Voice::HighTom,
            "cowbell" | "cb" => Voice::Cowbell,
            _ => return None,
        };
        Some(v)
    }

    /// General MIDI drum note for this voice (used for generic MIDI input).
    pub fn gm_note(self) -> u8 {
        match self {
            Voice::Kick => 36,
            Voice::Snare => 38,
            Voice::Clap => 39,
            Voice::ClosedHat => 42,
            Voice::OpenHat => 46,
            Voice::LowTom => 45,
            Voice::HighTom => 50,
            Voice::Cowbell => 56,
        }
    }
}

// ---------------------------------------------------------------------------
// Instruments, channels, routing
// ---------------------------------------------------------------------------

/// An instrument type. Instances are created with `instrument.add`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentType {
    /// TR-808-style drum machine: eight voices, a step pattern per voice.
    Tr808,
    /// TB-303-style bass synth: one monophonic voice, a note pattern.
    Tb303,
}

impl InstrumentType {
    pub const ALL: [InstrumentType; 2] = [InstrumentType::Tr808, InstrumentType::Tb303];

    pub fn id(self) -> &'static str {
        match self {
            InstrumentType::Tr808 => "tr808",
            InstrumentType::Tb303 => "tb303",
        }
    }

    /// Id given to a new instance when none is chosen (`drums2`, ... after).
    pub fn default_id(self) -> &'static str {
        match self {
            InstrumentType::Tr808 => "drums",
            InstrumentType::Tb303 => "bass",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            InstrumentType::Tr808 => "Drums",
            InstrumentType::Tb303 => "Bass",
        }
    }

    pub fn parse(s: &str) -> Option<InstrumentType> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tr808" | "808" | "drums" => Some(InstrumentType::Tr808),
            "tb303" | "303" | "bass" => Some(InstrumentType::Tb303),
            _ => None,
        }
    }
}

/// Top-level path segments that cannot be instrument ids.
pub const RESERVED_IDS: &[&str] = &["transport", "sequencer", "mixer", "controller", "focus"];

/// Check an instrument id: `[a-z][a-z0-9_]*`, not reserved.
pub fn validate_instrument_id(id: &str) -> Result<(), String> {
    let mut chars = id.chars();
    let ok = chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if !ok {
        return Err(format!("invalid instrument id '{id}': use [a-z][a-z0-9_]*"));
    }
    if RESERVED_IDS.contains(&id) {
        return Err(format!("instrument id '{id}' is reserved"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputWidth {
    Mono,
    Stereo,
}

/// One routable output of an instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct OutputInfo {
    /// Source name used by `route.set`: `drums` (main) or `drums.kick` (direct).
    pub source: String,
    pub label: String,
    pub width: OutputWidth,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct InstrumentInfo {
    pub id: String,
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: InstrumentType,
    pub name: String,
    /// Main output first, then any direct outs.
    pub outputs: Vec<OutputInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ChannelInfo {
    /// Stable channel number (1..=MAX_CHANNELS); paths are `mixer.<n>.*`.
    pub n: u32,
    pub name: String,
}

/// Instruments, mixer channels, and the routes between them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct Graph {
    /// In creation order.
    pub instruments: Vec<InstrumentInfo>,
    /// In display (creation) order.
    pub channels: Vec<ChannelInfo>,
    /// Source -> channel number. Unrouted sources are absent.
    pub routes: BTreeMap<String, u32>,
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

/// How a parameter's numeric value should be interpreted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParamKind {
    Continuous { min: f64, max: f64 },
    Integer { min: i32, max: i32 },
    Toggle,
}

/// Registry entry describing one addressable parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ParamInfo {
    /// Stable path, e.g. `mixer.3.volume` or `drums.kick.decay`.
    pub path: String,
    pub label: String,
    #[serde(flatten)]
    #[ts(flatten)]
    pub kind: ParamKind,
    pub default: f64,
    pub unit: Option<String>,
}

impl ParamInfo {
    /// Clamp (and round, for integers/toggles) a value into this param's range.
    pub fn clamp(&self, v: f64) -> f64 {
        match self.kind {
            ParamKind::Continuous { min, max } => v.clamp(min, max),
            ParamKind::Integer { min, max } => v.round().clamp(min as f64, max as f64),
            ParamKind::Toggle => {
                if v >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ParamValue {
    pub path: String,
    pub value: f64,
}

// ---------------------------------------------------------------------------
// Pattern / transport
// ---------------------------------------------------------------------------

/// Steps for one track. Each entry is 0 (off), 1 (on), or 2 (accent).
/// Always `MAX_STEPS` long; `sequencer.length` sets how many are played.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct TrackPattern {
    pub voice: Voice,
    pub steps: Vec<u8>,
}

/// One step of a note pattern. `note` is a MIDI number; `None` is a rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct NoteStep {
    pub note: Option<u8>,
    #[serde(default)]
    pub accent: bool,
    /// Glide into the next step's note without retriggering.
    #[serde(default)]
    pub slide: bool,
}

/// An instrument's pattern, by kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatternData {
    Drums { tracks: Vec<TrackPattern> },
    /// Always `MAX_STEPS` long.
    Notes { steps: Vec<NoteStep> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct InstrumentPattern {
    pub instrument: String,
    pub pattern: PatternData,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct TransportState {
    pub playing: bool,
    /// Current step while playing.
    pub step: Option<u32>,
}

// ---------------------------------------------------------------------------
// Controller (Livid Block, real or virtual)
// ---------------------------------------------------------------------------

/// A named set of up to 8 parameters that knobs following focus control.
/// Each instrument type declares its pages (RFC 0007).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct KnobPage {
    /// Stable id, e.g. `decay`.
    pub id: String,
    pub label: String,
    /// Full parameter paths, knob 1 first.
    pub params: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ControllerState {
    /// The seat the engine host's own devices (the Block) belong to.
    pub seat: String,
    /// That seat's focused instrument: the grid edits it when it is a
    /// `tr808` (otherwise the grid is dark) and the knobs control its page.
    pub focus: Option<String>,
    /// Knob page in use (an id from `knob_pages`).
    pub knob_page: Option<String>,
    /// Pages the focused instrument offers.
    pub knob_pages: Vec<String>,
    /// Parameter path each knob currently controls. Empty without focus.
    pub knob_params: Vec<String>,
    /// Which 8-step page the grid shows (0 = steps 1-8, 1 = steps 9-16, ...).
    pub page: u32,
    /// When true, the page follows the playhead while playing.
    pub follow: bool,
    /// 8 rows x 8 columns of LED values (0 = off, 1 = on).
    pub leds: Vec<Vec<u8>>,
    /// Hardware currently attached as a Livid Block, if any.
    pub device: Option<String>,
}

// ---------------------------------------------------------------------------
// MIDI devices and seats (RFC 0007)
// ---------------------------------------------------------------------------

/// How a device's input is interpreted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceProfile {
    /// Notes and CCs, routed by the seat's bindings and CC maps.
    #[default]
    Generic,
    /// Grid + knobs as a surface for the seat's focus, LEDs driven.
    LividBlock,
}

/// A physical port's hardware entry, kept per machine in
/// `<data-dir>/midi-devices.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct MidiDevice {
    pub port: String,
    /// Logical device name that seats bind to, e.g. `keys`.
    pub name: String,
    pub profile: DeviceProfile,
    /// Connect automatically when the port appears.
    pub auto_connect: bool,
    /// Known device model (`midi.models`) this port belongs to.
    #[serde(default)]
    pub model: Option<String>,
    /// The port's role in its model: the model's name for it (e.g.
    /// `mpk_daw`), which its default layout refers to.
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct MidiConnection {
    pub input: String,
    pub output: Option<String>,
    /// Logical device name.
    pub device: String,
    pub profile: DeviceProfile,
    /// Known device model, if any.
    #[serde(default)]
    pub model: Option<String>,
    /// The port's role in its model (see `MidiDevice::role`).
    #[serde(default)]
    pub role: Option<String>,
}

/// A known controller (RFC 0007, device models): which of its ports to use
/// and how, and a default layout used while a seat has no bindings for it.
/// Shipped as data in `crates/daemon/devices/`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct DeviceModel {
    /// Stable id, e.g. `akai_mpk_mini_iv`.
    pub id: String,
    pub label: String,
    /// Ports whose name contains this (any case) belong to the model.
    #[serde(rename = "match")]
    #[ts(rename = "match")]
    pub matches: String,
    /// Port name part (any case; `""` matches any port of the model) ->
    /// logical device name for that port, or `ignore` to leave it alone.
    pub ports: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub profile: DeviceProfile,
    /// Bindings, CC maps, following knobs, and pitch bend, naming the
    /// model's ports by their names in `ports`.
    #[serde(default)]
    pub layout: SeatConfig,
}

impl DeviceModel {
    /// The logical name this model gives `port` (`None`: not this model's
    /// port, or ignored).
    pub fn role(&self, port: &str) -> Option<&str> {
        let p = port.to_lowercase();
        if !p.contains(&self.matches.to_lowercase()) {
            return None;
        }
        let (_, name) = self
            .ports
            .iter()
            .filter(|(k, _)| p.contains(&k.to_lowercase()))
            .max_by_key(|(k, _)| k.len())?;
        (name != "ignore").then_some(name.as_str())
    }
}

/// Check a logical device or seat name: `[a-z][a-z0-9_]*`, at most 32
/// characters.
pub fn validate_name(what: &str, name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let ok = name.len() <= 32
        && chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok { Ok(()) } else { Err(format!("invalid {what} name '{name}': use [a-z][a-z0-9_]*, at most 32 characters")) }
}

/// Turn any label (a port name, a user name) into a valid name:
/// `Arturia KeyStep 37` -> `arturia_keystep_37`.
pub fn slug(label: &str) -> String {
    let mut s = String::new();
    for c in label.trim().to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('_') && !s.is_empty() {
            s.push('_');
        }
    }
    let mut s = s.trim_end_matches('_').to_string();
    if !s.starts_with(|c: char| c.is_ascii_lowercase()) {
        s.insert(0, 'd');
    }
    s.truncate(32);
    s.trim_end_matches('_').to_string()
}

/// Route notes from a device into an instrument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct NoteBinding {
    /// Logical device name.
    pub device: String,
    /// MIDI channel 1-16; any channel if absent.
    #[serde(default)]
    pub channel: Option<u8>,
    /// Lowest and highest input note (inclusive); all notes if absent.
    #[serde(default)]
    pub low: Option<u8>,
    #[serde(default)]
    pub high: Option<u8>,
    /// Semitones added to each note.
    #[serde(default)]
    pub transpose: i8,
    /// Output note for input notes `low`, `low + 1`, ... (from 0 without
    /// `low`), e.g. pads to drum voices. Overrides `transpose`; a note past
    /// the list plays nothing.
    #[serde(default)]
    pub remap: Option<Vec<u8>>,
    /// An instrument id, `focus` for the seat's focused instrument, or
    /// `@<type>` for the first instrument of a type (`@tr808`).
    pub target: String,
}

impl NoteBinding {
    pub fn matches(&self, device: &str, channel: u8, note: u8) -> bool {
        self.device == device
            && self.channel.is_none_or(|c| c == channel)
            && self.low.is_none_or(|l| note >= l)
            && self.high.is_none_or(|h| note <= h)
    }

    /// The note an input note plays, if any.
    pub fn output(&self, note: u8) -> Option<u8> {
        match &self.remap {
            Some(map) => map.get(note.checked_sub(self.low.unwrap_or(0))? as usize).copied(),
            None => u8::try_from(note as i16 + self.transpose as i16).ok().filter(|n| *n <= 127),
        }
    }
}

/// How a CC's value moves a parameter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CcMode {
    /// 0..127 is the parameter's range (a pot or fader).
    #[default]
    Absolute,
    /// Endless encoder: 1..63 turn up, 65..127 turn down (127 = -1).
    Relative,
}

/// Bind one CC to one parameter, scaled over its range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct CcMap {
    pub device: String,
    #[serde(default)]
    pub channel: Option<u8>,
    pub cc: u8,
    /// Parameter path; `focus.<param>` follows the seat's focus (e.g.
    /// `focus.cutoff`; nothing when the focus has no such parameter).
    pub param: String,
    /// Only take over once the knob passes the current value (absolute).
    #[serde(default = "yes")]
    pub pickup: bool,
    #[serde(default)]
    pub mode: CcMode,
}

/// CCs of a device that control the focused instrument's knob page
/// (knob 1 = the first CC).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct KnobFollow {
    pub device: String,
    #[serde(default)]
    pub channel: Option<u8>,
    pub ccs: Vec<u8>,
    #[serde(default = "yes")]
    pub pickup: bool,
    #[serde(default)]
    pub mode: CcMode,
}

fn yes() -> bool {
    true
}

/// One performer's setup: saved in the project.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SeatConfig {
    /// Instrument that `focus` bindings, the Block, and following knobs
    /// play. The first instrument if absent.
    #[serde(default)]
    pub focus: Option<String>,
    /// Knob page id; the focused instrument's first page if absent or not
    /// one of its pages.
    #[serde(default)]
    pub knob_page: Option<String>,
    #[serde(default)]
    pub bindings: Vec<NoteBinding>,
    #[serde(default)]
    pub cc: Vec<CcMap>,
    #[serde(default)]
    pub knobs: Vec<KnobFollow>,
    /// Where pitch bend goes: `focus`, an instrument id, or `@<type>`.
    /// Default: the focus.
    #[serde(default)]
    pub pitch_bend: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct Seat {
    pub name: String,
    /// Saved with the project. A seat made with "ignore" is not.
    pub saved: bool,
    pub config: SeatConfig,
    /// Client ids (`name#n`) sitting in this seat.
    pub occupants: Vec<String>,
    /// Parameter that the next CC moved on this seat's devices will be
    /// mapped to (`seat.learn_cc`).
    pub learning: Option<String>,
    /// Devices on this engine that play here with their model's default
    /// layout (no bindings of the seat's own), as `device (Model)`.
    #[serde(default)]
    pub defaults: Vec<String>,
}

// ---------------------------------------------------------------------------
// Engine / project status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct AudioStatus {
    /// `cpal` for a real device, `null` for headless real-time pacing.
    pub backend: String,
    pub device: Option<String>,
    pub sample_rate: u32,
    pub channels: u32,
    pub running: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ProjectInfo {
    /// Engine-side path of the project bundle, if saved/loaded.
    pub path: Option<String>,
    pub dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Engine,
    Bridge,
}

/// Identity and location of a running daemon. Returned by `daemon.info` and
/// also written to `<data-dir>/4sd.json` while the daemon runs, so local tools
/// can find it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct DaemonInfo {
    pub pid: u32,
    pub version: String,
    pub protocol_version: u32,
    /// WebSocket URL the daemon is listening on.
    pub url: String,
    pub role: Role,
    pub data_dir: String,
    pub log_file: Option<String>,
    /// Unix time (seconds) the daemon started.
    pub started_at: f64,
    pub uptime: f64,
}

/// Name of the runtime file inside the data dir.
pub const RUNTIME_FILE: &str = "4sd.json";

/// Every seat, and which one the engine host's own devices use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct SeatsState {
    pub seats: Vec<Seat>,
    /// Seat of the MIDI devices attached to the engine host.
    pub host: String,
}

/// Full engine state. Subscribers apply events with `seq` greater than this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct Snapshot {
    #[ts(type = "number")]
    pub seq: u64,
    pub transport: TransportState,
    pub params: BTreeMap<String, f64>,
    pub graph: Graph,
    /// One step view per instrument, in instrument order.
    pub patterns: Vec<InstrumentPattern>,
    /// One clip per instrument, in instrument order (RFC 0007).
    pub clips: Vec<crate::clip::Clip>,
    pub controller: ControllerState,
    pub midi: Vec<MidiConnection>,
    pub seats: SeatsState,
    pub project: ProjectInfo,
    pub audio: AudioStatus,
}

// ---------------------------------------------------------------------------
// Journal and undo history
// ---------------------------------------------------------------------------

/// One change to an addressable piece of project state, e.g.
/// `param:mixer.2.volume`, `step:drums.kick.3`, `note:bass.5`,
/// `instrument:bass`, `channel:2`, `channels:order`, `route:drums.kick`.
/// `null` means absent (or, for params and steps, the default).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct Change {
    pub key: String,
    #[ts(type = "unknown")]
    pub before: serde_json::Value,
    #[ts(type = "unknown")]
    pub after: serde_json::Value,
}

/// Transport and controller state when a journal entry was recorded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct JournalContext {
    pub playing: bool,
    pub step: Option<u32>,
    /// Controller page (pad columns map to steps through it).
    pub page: u32,
}

/// One request that could change state, as the daemon applied it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct JournalEntry {
    #[ts(type = "number")]
    pub seq: u64,
    /// Unix time in seconds.
    pub time: f64,
    /// Who owns the change (the undo stack it lands on).
    pub user: String,
    /// The client (or `midi:<port>`) it came from.
    pub origin: String,
    /// RPC method; MIDI input is recorded as its equivalent RPC.
    pub method: String,
    #[ts(type = "unknown")]
    pub params: serde_json::Value,
    pub context: JournalContext,
    pub changes: Vec<Change>,
    /// For `history.undo` / `history.redo`: the entry this one reverts.
    #[ts(type = "number | null")]
    pub reverts: Option<u64>,
    pub error: Option<String>,
}

/// A user's undo and redo stacks (labels, newest first).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct HistoryInfo {
    pub user: String,
    pub undo: Vec<String>,
    pub redo: Vec<String>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    ParamChanged { path: String, value: f64 },
    StepChanged { instrument: String, voice: Voice, step: u32, level: u8 },
    PatternChanged { instrument: String, voice: Voice, steps: Vec<u8> },
    /// A note pattern changed (all `MAX_STEPS` steps).
    NotesChanged { instrument: String, steps: Vec<NoteStep> },
    /// An instrument's clip changed (any edit, including through a step
    /// view, which also sends the view's events).
    ClipChanged { clip: crate::clip::Clip },
    /// Instruments, channels, or routes changed. Parameters may have been
    /// added or removed: refetch `state.get` and `param.list`.
    Graph { graph: Graph },
    Transport { playing: bool },
    /// A step started. `time` is engine time in seconds.
    Playhead { step: u32, time: f64 },
    /// An instrument played (from the sequencer, a pad, MIDI, or an
    /// audition). Drums set `voice`; note instruments set `note`.
    Trigger { instrument: String, voice: Option<Voice>, note: Option<u8>, velocity: f32, time: f64 },
    /// Peak levels since the last meter event (linear, 0..1+), per active
    /// channel (post fader, pan, and mute/solo) and for the master.
    Meters { channels: Vec<ChannelLevel>, master: Vec<f32> },
    Controller { state: ControllerState },
    Midi { connections: Vec<MidiConnection> },
    /// Seats, their bindings, or who sits where changed.
    Seats { state: SeatsState },
    /// Raw incoming MIDI, for discovering controller mappings. `port` is
    /// the port name (for `midi.input`, the logical device name).
    MidiIn { port: String, data: Vec<u8> },
    Project { info: ProjectInfo },
    /// A user's undo/redo summary changed.
    History { user: String, undo_label: Option<String>, redo_label: Option<String>, undo_count: u32, redo_count: u32 },
    /// A request was recorded in the journal.
    Journal { entry: JournalEntry },
    /// State was replaced wholesale (project load/new); refetch `state.get`.
    Reset,
    /// This subscriber fell behind and missed events; refetch `state.get`.
    Lagged { missed: u32 },
}

impl Event {
    pub fn type_name(&self) -> &'static str {
        match self {
            Event::ParamChanged { .. } => "param_changed",
            Event::StepChanged { .. } => "step_changed",
            Event::PatternChanged { .. } => "pattern_changed",
            Event::NotesChanged { .. } => "notes_changed",
            Event::ClipChanged { .. } => "clip_changed",
            Event::Graph { .. } => "graph",
            Event::Transport { .. } => "transport",
            Event::Playhead { .. } => "playhead",
            Event::Trigger { .. } => "trigger",
            Event::Meters { .. } => "meters",
            Event::Controller { .. } => "controller",
            Event::Midi { .. } => "midi",
            Event::Seats { .. } => "seats",
            Event::MidiIn { .. } => "midi_in",
            Event::Project { .. } => "project",
            Event::History { .. } => "history",
            Event::Journal { .. } => "journal",
            Event::Reset => "reset",
            Event::Lagged { .. } => "lagged",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ChannelLevel {
    pub channel: u32,
    pub left: f32,
    pub right: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct EventEnvelope {
    #[ts(type = "number")]
    pub seq: u64,
    /// Who caused this change: a client name, `midi:<port>`, or `engine`.
    pub origin: String,
    pub event: Event,
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct RenderTrigger {
    pub time: f64,
    pub step: u32,
    pub instrument: String,
    pub voice: Option<Voice>,
    pub note: Option<u8>,
    pub velocity: f32,
}

// ---------------------------------------------------------------------------
// Pattern string helpers (used by the CLI and project files)
// ---------------------------------------------------------------------------

/// Parse `x---X---` style strings: `x`/`1` = on, `X`/`2`/`!` = accent,
/// `-`/`.`/`0` = off. Whitespace and `|` are ignored.
pub fn parse_steps(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    for c in s.chars() {
        let level = match c {
            'x' | '1' | 'o' => STEP_ON,
            'X' | '2' | '!' | 'O' => STEP_ACCENT,
            '-' | '.' | '0' | '_' => STEP_OFF,
            ' ' | '|' => continue,
            other => return Err(format!("invalid step character '{other}'")),
        };
        out.push(level);
    }
    if out.len() > MAX_STEPS {
        return Err(format!("pattern longer than {MAX_STEPS} steps"));
    }
    Ok(out)
}

pub fn format_steps(steps: &[u8]) -> String {
    steps
        .iter()
        .map(|s| match *s {
            STEP_ON => 'x',
            STEP_ACCENT => 'X',
            _ => '-',
        })
        .collect()
}

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Lowest and highest notes a note pattern may hold (C0, C8).
pub const NOTE_MIN: u8 = 12;
pub const NOTE_MAX: u8 = 108;

/// Note name for a MIDI number, scientific pitch with C4 = 60 (`C2` = 36).
pub fn note_name(note: u8) -> String {
    format!("{}{}", NOTE_NAMES[(note % 12) as usize], (note / 12) as i32 - 1)
}

/// Parse a note name (`C2`, `D#3`, `Eb1`) to a MIDI number.
pub fn parse_note(s: &str) -> Result<u8, String> {
    let mut chars = s.chars();
    let letter = chars.next().ok_or("empty note")?.to_ascii_uppercase();
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return Err(format!("invalid note '{s}'")),
    };
    let rest: String = chars.collect();
    let (shift, octave) = if let Some(o) = rest.strip_prefix('#') {
        (1, o)
    } else if let Some(o) = rest.strip_prefix('b') {
        (-1, o)
    } else {
        (0, rest.as_str())
    };
    let octave: i32 = octave.parse().map_err(|_| format!("invalid note '{s}' (expected e.g. C2, D#3)"))?;
    let n = (octave + 1) * 12 + base + shift;
    if n < NOTE_MIN as i32 || n > NOTE_MAX as i32 {
        return Err(format!("note '{s}' out of range ({}..{})", note_name(NOTE_MIN), note_name(NOTE_MAX)));
    }
    Ok(n as u8)
}

/// Parse a note pattern: space-separated tokens, `C2` note, `C2!` accent,
/// `C2~` slide into the next step, `C2!~` both, `-` rest. Padded with rests
/// to `MAX_STEPS`.
pub fn parse_notes(s: &str) -> Result<Vec<NoteStep>, String> {
    let mut out = Vec::new();
    for tok in s.split_whitespace().filter(|t| *t != "|") {
        if out.len() == MAX_STEPS {
            return Err(format!("note pattern longer than {MAX_STEPS} steps"));
        }
        if tok == "-" || tok == "." {
            out.push(NoteStep::default());
            continue;
        }
        let mut name = tok;
        let (mut accent, mut slide) = (false, false);
        while let Some(c) = name.chars().last() {
            match c {
                '!' => accent = true,
                '~' => slide = true,
                _ => break,
            }
            name = &name[..name.len() - 1];
        }
        out.push(NoteStep { note: Some(parse_note(name)?), accent, slide });
    }
    out.resize(MAX_STEPS, NoteStep::default());
    Ok(out)
}

/// Format the first `len` steps of a note pattern (inverse of `parse_notes`).
pub fn format_notes(steps: &[NoteStep], len: usize) -> String {
    steps
        .iter()
        .take(len)
        .map(|s| match s.note {
            None => "-".to_string(),
            Some(n) => {
                format!("{}{}{}", note_name(n), if s.accent { "!" } else { "" }, if s.slide { "~" } else { "" })
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_round_trip() {
        let s = "C2 C2! D#2~ - G1 A#1!~ C3 -";
        let steps = parse_notes(s).unwrap();
        assert_eq!(steps.len(), MAX_STEPS);
        assert_eq!(steps[0].note, Some(36));
        assert_eq!(steps[5], NoteStep { note: Some(34), accent: true, slide: true });
        assert_eq!(format_notes(&steps, 8), s);
        assert_eq!(parse_notes("Eb2").unwrap()[0].note, Some(39));
        assert!(parse_notes("C9").is_err());
        assert!(parse_notes(&"C2 ".repeat(65)).is_err());
    }

    #[test]
    fn steps_round_trip() {
        let s = "x---X---x-x-----";
        assert_eq!(format_steps(&parse_steps(s).unwrap()), s);
        assert_eq!(parse_steps("x--- x---|X").unwrap().len(), 9);
        assert!(parse_steps("x?").is_err());
    }

    #[test]
    fn event_wire_format() {
        let e = EventEnvelope {
            seq: 3,
            origin: "cli".into(),
            event: Event::ParamChanged { path: "mixer.1.volume".into(), value: 0.35 },
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["event"]["type"], "param_changed");
        assert_eq!(j["event"]["path"], "mixer.1.volume");
        let back: EventEnvelope = serde_json::from_value(j).unwrap();
        assert_eq!(back, e);
    }
}
