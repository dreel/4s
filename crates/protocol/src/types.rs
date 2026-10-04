use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Maximum number of steps a pattern can hold. The active length is the
/// `sequencer.length` parameter.
pub const MAX_STEPS: usize = 64;

/// Number of drum tracks (one per voice, one per Livid Block row).
pub const NUM_TRACKS: usize = 8;

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct TransportState {
    pub playing: bool,
    /// Current step while playing.
    pub step: Option<u32>,
}

// ---------------------------------------------------------------------------
// Controller (Livid Block, real or virtual)
// ---------------------------------------------------------------------------

/// What the 8 knobs control. Knob N always maps to track N.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KnobMode {
    Volume,
    Tune,
    Decay,
    Tone,
}

impl KnobMode {
    pub const ALL: [KnobMode; 4] = [KnobMode::Volume, KnobMode::Tune, KnobMode::Decay, KnobMode::Tone];

    /// Parameter path knob `track` (0-based) controls in this mode.
    pub fn param_path(self, track: usize) -> String {
        let voice = Voice::from_index(track).unwrap_or(Voice::Kick);
        match self {
            KnobMode::Volume => format!("mixer.{}.volume", track + 1),
            KnobMode::Tune => format!("drums.{}.tune", voice.id()),
            KnobMode::Decay => format!("drums.{}.decay", voice.id()),
            KnobMode::Tone => format!("drums.{}.tone", voice.id()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ControllerState {
    pub knob_mode: KnobMode,
    /// Parameter path each knob currently controls (knob N -> track N).
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
// MIDI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Grid + knobs mapped onto the sequencer, LEDs driven.
    LividBlock,
    /// General MIDI drum notes trigger voices.
    GenericDrums,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct MidiConnection {
    pub input: String,
    pub output: Option<String>,
    pub kind: DeviceKind,
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

/// Full engine state. Subscribers apply events with `seq` greater than this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct Snapshot {
    #[ts(type = "number")]
    pub seq: u64,
    pub transport: TransportState,
    pub params: BTreeMap<String, f64>,
    pub pattern: Vec<TrackPattern>,
    pub controller: ControllerState,
    pub midi: Vec<MidiConnection>,
    pub project: ProjectInfo,
    pub audio: AudioStatus,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    ParamChanged { path: String, value: f64 },
    StepChanged { voice: Voice, step: u32, level: u8 },
    PatternChanged { voice: Voice, steps: Vec<u8> },
    Transport { playing: bool },
    /// A step started. `time` is engine time in seconds.
    Playhead { step: u32, time: f64 },
    /// A voice fired (from the sequencer, a pad, or MIDI).
    Trigger { voice: Voice, velocity: f32, time: f64 },
    /// Peak levels since the last meter event (linear, 0..1+).
    Meters { tracks: Vec<f32>, master: Vec<f32> },
    Controller { state: ControllerState },
    Midi { connections: Vec<MidiConnection> },
    /// Raw incoming MIDI, for discovering controller mappings.
    MidiIn { port: String, data: Vec<u8> },
    Project { info: ProjectInfo },
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
            Event::Transport { .. } => "transport",
            Event::Playhead { .. } => "playhead",
            Event::Trigger { .. } => "trigger",
            Event::Meters { .. } => "meters",
            Event::Controller { .. } => "controller",
            Event::Midi { .. } => "midi",
            Event::MidiIn { .. } => "midi_in",
            Event::Project { .. } => "project",
            Event::Reset => "reset",
            Event::Lagged { .. } => "lagged",
        }
    }
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
    pub voice: Voice,
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

#[cfg(test)]
mod tests {
    use super::*;

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
            event: Event::ParamChanged { path: "mixer.3.volume".into(), value: 0.35 },
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["event"]["type"], "param_changed");
        assert_eq!(j["event"]["path"], "mixer.3.volume");
        let back: EventEnvelope = serde_json::from_value(j).unwrap();
        assert_eq!(back, e);
    }
}
