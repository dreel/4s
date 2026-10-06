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
pub const RESERVED_IDS: &[&str] = &["transport", "sequencer", "mixer", "controller"];

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

    /// Parameter path knob `track` (0-based) controls in this mode, on the
    /// drum instrument `target`.
    pub fn param_path(self, target: &str, track: usize) -> String {
        let voice = Voice::from_index(track).unwrap_or(Voice::Kick);
        let param = match self {
            KnobMode::Volume => "level",
            KnobMode::Tune => "tune",
            KnobMode::Decay => "decay",
            KnobMode::Tone => "tone",
        };
        format!("{target}.{}.{param}", voice.id())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ControllerState {
    /// The drum instrument the grid and knobs control. `None` when there is
    /// no `tr808`: the grid is dark and input does nothing.
    pub target: Option<String>,
    pub knob_mode: KnobMode,
    /// Parameter path each knob currently controls (knob N -> track N).
    /// Empty when there is no target.
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
    /// General MIDI drum notes trigger voices of the controller target.
    GenericDrums,
    /// Note on/off plays a note instrument (default: the first `tb303`).
    Keyboard,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct MidiConnection {
    pub input: String,
    pub output: Option<String>,
    pub kind: DeviceKind,
    /// Instrument a `keyboard` plays; `None` = the first `tb303`.
    #[serde(default)]
    pub instrument: Option<String>,
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
    pub graph: Graph,
    /// One pattern per instrument, in instrument order.
    pub patterns: Vec<InstrumentPattern>,
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
    StepChanged { instrument: String, voice: Voice, step: u32, level: u8 },
    PatternChanged { instrument: String, voice: Voice, steps: Vec<u8> },
    /// A note pattern changed (all `MAX_STEPS` steps).
    NotesChanged { instrument: String, steps: Vec<NoteStep> },
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
            Event::NotesChanged { .. } => "notes_changed",
            Event::Graph { .. } => "graph",
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
