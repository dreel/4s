//! 4s: command line client for the 4S daemon. Every UI action has a CLI
//! equivalent; see docs/api-parity.md.
//!
//! User-facing numbering in the CLI is 1-based (steps, rows, columns, knobs,
//! pages), matching how musicians count. The RPC API is 0-based.

mod client;
mod daemon_ctl;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use client::{Client, Seating};
use daemon_ctl::StartArgs;
use std::path::PathBuf;
use fours_protocol::*;
use serde_json::Value;

#[derive(Parser)]
#[command(name = "4s", version, about = "Control the 4S daemon (4sd)")]
struct Cli {
    /// Daemon URL. Default: the running daemon for --data-dir, else
    /// ws://127.0.0.1:4440.
    #[arg(long, env = "FOURS_URL", global = true)]
    url: Option<String>,
    /// Data directory of the local daemon (runtime file, logs). Default: ~/.4s
    #[arg(long, env = "FOURS_DATA_DIR", global = true)]
    data_dir: Option<PathBuf>,
    /// Auth token, if the daemon requires one.
    #[arg(long, env = "FOURS_TOKEN", global = true)]
    token: Option<String>,
    /// Print raw JSON results.
    #[arg(long, global = true)]
    json: bool,
    /// Who you are. The seat with this name is joined automatically
    /// (default: your OS user).
    #[arg(long, env = "FOURS_USER", global = true)]
    user: Option<String>,
    /// Join this seat (seat commands then act on it).
    #[arg(long, global = true)]
    seat: Option<String>,
    /// Create a new seat, named after --user, and join it.
    #[arg(long, global = true, conflicts_with = "seat")]
    new_seat: bool,
    /// Do not join a seat.
    #[arg(long, global = true, conflicts_with_all = ["seat", "new_seat"])]
    no_seat: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug, Clone)]
enum Cmd {
    /// Engine, transport, and project status.
    Status,
    /// Full state snapshot.
    State,
    /// List parameters with current values.
    Params { prefix: Option<String> },
    /// Read a parameter, e.g. `4s get drums.kick.level`.
    Get { path: String },
    /// Set a parameter, e.g. `4s set mixer.1.volume 35%`. Accepts numbers,
    /// percentages, and on/off.
    Set {
        path: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Start playback.
    Play,
    /// Stop playback.
    Stop,
    /// Set tempo in BPM (shorthand for `set transport.tempo`).
    Tempo { bpm: f64 },
    /// Show or edit a drum pattern.
    Pattern {
        /// Drum instrument (default: the first tr808).
        #[arg(long)]
        instrument: Option<String>,
        #[command(subcommand)]
        cmd: Option<PatternCmd>,
    },
    /// Show or set a note pattern, e.g. `4s notes bass "C2 C2! D#2~ - G1"`.
    /// Tokens: `C2` note, `!` accent, `~` slide into the next step, `-` rest.
    Notes {
        /// Note instrument id (e.g. `bass`).
        instrument: String,
        /// New pattern; omit to show the current one.
        #[arg(allow_hyphen_values = true)]
        pattern: Option<String>,
    },
    /// Set one step (1-based) of a note pattern, e.g. `4s note bass 3 D#2!~`
    /// (`-` for a rest).
    Note {
        instrument: String,
        step: u32,
        #[arg(allow_hyphen_values = true)]
        note: String,
    },
    /// Play a drum voice (`4s trigger kick`) or a note (`4s trigger --note C2`) now.
    Trigger {
        voice: Option<String>,
        /// Note to play, e.g. C2 (a drum machine plays the voice on that GM
        /// note).
        #[arg(long)]
        note: Option<String>,
        /// Instrument (default: your seat's focus; for a voice, the first
        /// tr808 unless the focus is one).
        #[arg(long)]
        instrument: Option<String>,
        #[arg(long)]
        velocity: Option<f32>,
    },
    /// Hold a note like a keyboard key, then release it:
    /// `4s key C2 --for 0.5`. (A held note belongs to its connection, so
    /// it is released when this command exits.)
    Key {
        note: String,
        /// Seconds to hold the note (default 1).
        #[arg(long = "for", default_value_t = 1.0)]
        secs: f64,
        /// Instrument (default: your seat's focus).
        #[arg(long)]
        instrument: Option<String>,
        /// 0..1 (default 1; 0.95 and up is accented).
        #[arg(long)]
        velocity: Option<f32>,
    },
    /// Add, remove, and list instruments.
    Instrument {
        #[command(subcommand)]
        cmd: InstrumentCmd,
    },
    /// Add, remove, and rename mixer channels.
    Channel {
        #[command(subcommand)]
        cmd: ChannelCmd,
    },
    /// Route an instrument output to a channel, e.g. `4s route drums.kick 2`,
    /// or unroute it with `none` (a direct out returns to the main mix).
    Route { source: String, channel: String },
    /// Show the mixer: channels, their sources, levels, mute/solo.
    Mixer,
    /// Stream events. Ctrl-C to stop.
    Watch {
        /// Only these event types (repeatable), e.g. --type param_changed.
        #[arg(long = "type")]
        types: Vec<String>,
        /// Exit after this many events.
        #[arg(long)]
        count: Option<usize>,
    },
    /// Render the pattern offline to a WAV (on the engine host) and analyze it.
    Render {
        #[arg(long, default_value_t = 1.0)]
        bars: f64,
        #[arg(long)]
        tail: Option<f64>,
        /// Output path (engine-side). Relative paths go under the data dir.
        #[arg(long)]
        out: Option<String>,
        #[arg(long)]
        sample_rate: Option<u32>,
    },
    /// Project files.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
    },
    /// MIDI ports and devices.
    Midi {
        #[command(subcommand)]
        cmd: MidiCmd,
    },
    /// Livid Block (real or virtual) controller.
    Controller {
        #[command(subcommand)]
        cmd: Option<ControllerCmd>,
    },
    /// Seats: each performer's focus, bindings, and CC maps (RFC 0006).
    /// `4s seat` lists them.
    Seat {
        #[command(subcommand)]
        cmd: Option<SeatCmd>,
    },
    /// Focus an instrument for your seat: devices without bindings, `focus`
    /// bindings, the Block, and following knobs play it.
    Focus { instrument: String },
    /// Route a device's notes to an instrument for your seat, e.g.
    /// `4s bind keys --notes C1..B2 --to bass`. A device with no bindings
    /// plays your focus.
    Bind {
        /// Logical device name (see `4s midi ports`).
        device: String,
        /// MIDI channel 1-16 (default: any).
        #[arg(long)]
        channel: Option<u8>,
        /// Input note range, e.g. C1..B2 or 36..47 (default: all).
        #[arg(long)]
        notes: Option<String>,
        /// Semitones to add.
        #[arg(long, allow_hyphen_values = true)]
        transpose: Option<i8>,
        /// Instrument id, or `focus`.
        #[arg(long, default_value = "focus")]
        to: String,
    },
    /// Remove one of your seat's bindings (1-based, as `4s seat` lists them).
    Unbind { n: u32 },
    /// Map CCs to parameters for your seat.
    Cc {
        #[command(subcommand)]
        cmd: CcCmd,
    },
    /// Knobs that follow your seat's focus.
    Knobs {
        #[command(subcommand)]
        cmd: KnobsCmd,
    },
    /// Start, stop, and inspect the background daemon.
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Call any RPC method with JSON params.
    Call { method: String, params: Option<String> },
    /// List all RPC methods.
    Methods,
}

#[derive(Subcommand, Debug, Clone)]
enum DaemonCmd {
    /// Start 4sd in the background (no-op if already running).
    Start(StartArgs),
    /// Stop the daemon (graceful; --force kills it if it does not respond).
    Stop {
        #[arg(long)]
        force: bool,
    },
    /// Show whether the daemon is running, and its pid, URL, and uptime.
    /// Exits with code 3 when not running.
    Status,
    /// Stop (if running) and start again.
    Restart(StartArgs),
    /// Print the end of the daemon log.
    Logs {
        #[arg(short = 'n', long, default_value_t = 40)]
        lines: usize,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum InstrumentCmd {
    /// Instrument types that can be added.
    Types,
    /// Instruments in the project.
    List,
    /// Add an instrument, by default on a new channel.
    Add {
        /// tr808 or tb303.
        kind: String,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// Route its main output to this existing channel.
        #[arg(long)]
        channel: Option<u32>,
        /// Leave its main output unrouted.
        #[arg(long)]
        no_channel: bool,
    },
    /// Remove an instrument and the channels left empty.
    Rm {
        id: String,
        /// Keep channels even if nothing feeds them any more.
        #[arg(long)]
        keep_channels: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum ChannelCmd {
    /// Add a channel (lowest free number).
    Add {
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a channel; its sources become unrouted.
    Rm { n: u32 },
    /// Rename a channel.
    Rename { n: u32, name: String },
}

#[derive(Subcommand, Debug, Clone)]
enum PatternCmd {
    /// Show the pattern (all voices or one).
    Show { voice: Option<String> },
    /// Replace a voice's steps, e.g. `4s pattern set kick "x---x---X---x---"`.
    Set {
        voice: String,
        #[arg(allow_hyphen_values = true)]
        steps: String,
    },
    /// Set one step (1-based) to off/on/accent.
    Step { voice: String, step: u32, level: Level },
    /// Toggle one step (1-based).
    Toggle { voice: String, step: u32 },
    /// Clear one voice or everything.
    Clear { voice: Option<String> },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum Level {
    Off,
    On,
    Accent,
}

#[derive(Subcommand, Debug, Clone)]
enum ProjectCmd {
    /// Start a fresh empty project.
    New,
    /// Save (to the current path, or a new one).
    Save { path: Option<String> },
    /// Load a project bundle.
    Load { path: String },
    /// List saved projects.
    List,
    /// Print the current project's location and show it in Finder/Explorer
    /// (local daemon only).
    Reveal {
        /// Only print the path.
        #[arg(long)]
        no_open: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum MidiCmd {
    /// List ports, connections, and this machine's device names.
    Ports,
    /// Connect a MIDI input as a named device.
    Connect {
        input: String,
        #[arg(long)]
        output: Option<String>,
        /// Logical name seats bind to (default: the saved one, else from
        /// the port name).
        #[arg(long)]
        name: Option<String>,
        /// Default: the saved one, else `block` for ports named like a
        /// Block, else `generic`.
        #[arg(long, value_enum)]
        profile: Option<ProfileArg>,
    },
    /// Disconnect a MIDI input (by port or device name).
    Disconnect { input: String },
    /// Rename a device (the logical name seats bind to).
    Rename { device: String, name: String },
    /// Pin the seat this machine's devices play in; no seat unpins (they
    /// follow the local user).
    Seat { seat: Option<String> },
    /// Send one raw MIDI message as if from a device, e.g.
    /// `4s midi send keys 90 3C 64`.
    Send {
        device: String,
        /// Hex bytes.
        #[arg(required = true)]
        bytes: Vec<String>,
    },
    /// Print raw incoming MIDI (for discovering controller mappings).
    Monitor {
        #[arg(long)]
        count: Option<usize>,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum ProfileArg {
    /// Notes and CCs, routed by seat bindings and CC maps.
    Generic,
    /// Livid Block: grid + knobs drive the seat's focus, LEDs lit.
    Block,
}

#[derive(Subcommand, Debug, Clone)]
enum SeatCmd {
    /// List seats, who sits where, and their bindings.
    List,
    /// Join an existing seat (for this command; use --seat for others).
    Claim { name: String },
    /// Create a seat (default name: --user) and join it.
    Create {
        name: Option<String>,
        /// For this session only: not saved with the project.
        #[arg(long)]
        ignore: bool,
    },
    /// Leave your seat.
    Leave,
    /// Delete a seat.
    Rm { name: String },
}

#[derive(Subcommand, Debug, Clone)]
enum CcCmd {
    /// Map a device's CC to a parameter, e.g. `4s cc map knobs 21 bass.cutoff`.
    Map {
        device: String,
        cc: u8,
        param: String,
        /// MIDI channel 1-16 (default: any).
        #[arg(long)]
        channel: Option<u8>,
        /// Jump to the knob's position instead of picking up.
        #[arg(long)]
        no_pickup: bool,
    },
    /// Remove a CC map.
    Unmap {
        device: String,
        cc: u8,
        #[arg(long)]
        channel: Option<u8>,
    },
    /// Map the next CC you move to a parameter; no parameter cancels.
    Learn { param: Option<String> },
}

#[derive(Subcommand, Debug, Clone)]
enum KnobsCmd {
    /// Choose the knob page of your focused instrument, e.g. `decay`.
    Page { page: String },
    /// Make a device's CCs (knob 1 first) control the focused instrument's
    /// page; no CCs stops it, e.g. `4s knobs follow knobs 21 22 23 24`.
    Follow {
        device: String,
        ccs: Vec<u8>,
        #[arg(long)]
        channel: Option<u8>,
        #[arg(long)]
        no_pickup: bool,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum ControllerCmd {
    /// Show LEDs, mode, and page.
    Show,
    /// Tap a pad (1-based row and column).
    Press {
        row: u32,
        col: u32,
        /// Only press (down) or release (up) instead of a full tap.
        #[arg(long, value_enum)]
        only: Option<PadEdge>,
    },
    /// Turn a knob (1-based) to a position 0..1 (or a percentage).
    Knob {
        index: u32,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    /// Change the grid page (1-based) or follow. Focus and knob page are
    /// your seat's: `4s focus`, `4s knobs page`.
    Mode {
        #[arg(long)]
        page: Option<u32>,
        #[arg(long)]
        follow: Option<bool>,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum PadEdge {
    Down,
    Up,
}


// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

fn voice(s: &str) -> Result<Voice> {
    Voice::parse(s).ok_or_else(|| {
        let ids: Vec<_> = Voice::ALL.iter().map(|v| v.id()).collect();
        anyhow!("unknown voice '{s}' (use {} or 1-8)", ids.join(", "))
    })
}

fn instrument_type(s: &str) -> Result<InstrumentType> {
    InstrumentType::parse(s).ok_or_else(|| {
        let ids: Vec<_> = InstrumentType::ALL.iter().map(|t| t.id()).collect();
        anyhow!("unknown instrument type '{s}' (use {})", ids.join(", "))
    })
}

/// `C1..B2`, `36..47`, or one note.
fn note_range(s: &str) -> Result<(u8, u8)> {
    let one = |t: &str| -> Result<u8> {
        let t = t.trim();
        t.parse::<u8>().ok().filter(|n| *n <= 127).map(Ok).unwrap_or_else(|| parse_note(t).map_err(|e| anyhow!(e)))
    };
    let (lo, hi) = match s.split_once("..") {
        Some((a, b)) => (one(a)?, one(b)?),
        None => (one(s)?, one(s)?),
    };
    if lo > hi {
        bail!("note range {s} is backwards");
    }
    Ok((lo, hi))
}

fn hex_bytes(bytes: &[String]) -> Result<Vec<u8>> {
    bytes.iter().map(|b| u8::from_str_radix(b, 16).map_err(|_| anyhow!("not a hex byte: {b}"))).collect()
}

fn one_based(n: u32, what: &str) -> Result<u32> {
    n.checked_sub(1).ok_or_else(|| anyhow!("{what} numbers start at 1"))
}

fn step_index(n: u32) -> Result<u32> {
    if n == 0 || n as usize > MAX_STEPS {
        bail!("step must be 1..{MAX_STEPS}");
    }
    Ok(n - 1)
}

/// Parse `0.35`, `35%`, `on`/`off`, `true`/`false`.
fn parse_value(s: &str) -> Result<f64> {
    let t = s.trim().to_ascii_lowercase();
    match t.as_str() {
        "on" | "true" | "yes" => return Ok(1.0),
        "off" | "false" | "no" => return Ok(0.0),
        _ => {}
    }
    if let Some(p) = t.strip_suffix('%') {
        return Ok(p.trim().parse::<f64>()? / 100.0);
    }
    Ok(t.parse::<f64>().map_err(|_| anyhow!("invalid value '{s}'"))?)
}

// ---------------------------------------------------------------------------
// Command -> requests (pure, so parity can be tested without a daemon)
// ---------------------------------------------------------------------------

fn plan(cmd: &Cmd) -> Result<Vec<Request>> {
    let e = Empty {};
    Ok(match cmd {
        Cmd::Status => vec![Request::StateGet(e.clone()), Request::EngineStatus(e)],
        Cmd::State => vec![Request::StateGet(e)],
        Cmd::Params { prefix } => vec![
            Request::ParamList(ParamListParams { prefix: prefix.clone() }),
            Request::StateGet(e),
        ],
        Cmd::Get { path } => vec![Request::ParamGet(ParamGetParams { path: path.clone() })],
        Cmd::Set { path, value } => {
            vec![Request::ParamSet(ParamSetParams { path: path.clone(), value: parse_value(value)? })]
        }
        Cmd::Play => vec![Request::TransportPlay(e)],
        Cmd::Stop => vec![Request::TransportStop(e)],
        Cmd::Tempo { bpm } => {
            vec![Request::ParamSet(ParamSetParams { path: "transport.tempo".into(), value: *bpm })]
        }
        Cmd::Pattern { instrument, cmd } => match cmd.clone().unwrap_or(PatternCmd::Show { voice: None }) {
            PatternCmd::Show { voice: v } => vec![Request::PatternGet(PatternGetParams {
                instrument: instrument.clone(),
                voice: v.as_deref().map(voice).transpose()?,
            })],
            PatternCmd::Set { voice: v, steps } => vec![Request::PatternSet(PatternSetParams {
                instrument: instrument.clone(),
                voice: voice(&v)?,
                steps: parse_steps(&steps).map_err(|e| anyhow!(e))?,
            })],
            PatternCmd::Step { voice: v, step, level } => vec![Request::PatternSetStep(SetStepParams {
                instrument: instrument.clone(),
                voice: voice(&v)?,
                step: step_index(step)?,
                level: match level {
                    Level::Off => STEP_OFF,
                    Level::On => STEP_ON,
                    Level::Accent => STEP_ACCENT,
                },
            })],
            PatternCmd::Toggle { voice: v, step } => vec![Request::PatternToggleStep(ToggleStepParams {
                instrument: instrument.clone(),
                voice: voice(&v)?,
                step: step_index(step)?,
            })],
            PatternCmd::Clear { voice: v } => vec![Request::PatternClear(PatternClearParams {
                instrument: instrument.clone(),
                voice: v.as_deref().map(voice).transpose()?,
            })],
        },
        Cmd::Notes { instrument, pattern } => match pattern {
            None => vec![Request::PatternGetNotes(NotesGetParams { instrument: Some(instrument.clone()) })],
            Some(p) => vec![Request::PatternSetNotes(NotesSetParams {
                instrument: Some(instrument.clone()),
                steps: parse_notes(p).map_err(|e| anyhow!(e))?,
            })],
        },
        Cmd::Note { instrument, step, note } => {
            let parsed = parse_notes(note).map_err(|e| anyhow!(e))?;
            if note.split_whitespace().count() != 1 {
                bail!("give one note token, e.g. C2, D#2!~, or -");
            }
            vec![Request::PatternSetNote(NoteSetParams {
                instrument: Some(instrument.clone()),
                step: step_index(*step)?,
                note: parsed[0],
            })]
        }
        Cmd::Trigger { voice: v, note, instrument, velocity } => {
            if v.is_some() == note.is_some() {
                bail!("give a voice (e.g. `4s trigger kick`) or --note (e.g. `4s trigger --note C2`)");
            }
            vec![Request::VoiceTrigger(TriggerParams {
                instrument: instrument.clone(),
                voice: v.as_deref().map(voice).transpose()?,
                note: note.as_deref().map(parse_note).transpose().map_err(|e| anyhow!(e))?,
                velocity: *velocity,
            })]
        }
        Cmd::Key { note, instrument, velocity, .. } => {
            let note = parse_note(note).map_err(|e| anyhow!(e))?;
            vec![
                Request::VoiceNoteOn(NoteParams { instrument: instrument.clone(), note, velocity: *velocity }),
                Request::VoiceNoteOff(NoteParams { instrument: instrument.clone(), note, velocity: None }),
            ]
        }
        Cmd::Instrument { cmd } => match cmd {
            InstrumentCmd::Types => vec![Request::InstrumentTypes(e)],
            InstrumentCmd::List => vec![Request::InstrumentList(e)],
            InstrumentCmd::Add { kind, id, name, channel, no_channel } => {
                vec![Request::InstrumentAdd(InstrumentAddParams {
                    kind: instrument_type(kind)?,
                    id: id.clone(),
                    name: name.clone(),
                    channel: *channel,
                    no_channel: *no_channel,
                })]
            }
            InstrumentCmd::Rm { id, keep_channels } => vec![Request::InstrumentRemove(InstrumentRemoveParams {
                id: id.clone(),
                keep_channels: *keep_channels,
            })],
        },
        Cmd::Channel { cmd } => match cmd {
            ChannelCmd::Add { name } => vec![Request::ChannelAdd(ChannelAddParams { name: name.clone() })],
            ChannelCmd::Rm { n } => vec![Request::ChannelRemove(ChannelRemoveParams { n: *n })],
            ChannelCmd::Rename { n, name } => {
                vec![Request::ChannelRename(ChannelRenameParams { n: *n, name: name.clone() })]
            }
        },
        Cmd::Route { source, channel } => {
            let channel = match channel.trim().to_ascii_lowercase().as_str() {
                "none" | "-" => None,
                n => Some(n.parse::<u32>().map_err(|_| anyhow!("channel must be a number or `none`"))?),
            };
            vec![Request::RouteSet(RouteSetParams { source: source.clone(), channel })]
        }
        Cmd::Mixer => vec![Request::StateGet(e)],
        Cmd::Watch { types, .. } => vec![
            Request::EventsSubscribe(SubscribeParams {
                types: if types.is_empty() { None } else { Some(types.clone()) },
            }),
            Request::EventsUnsubscribe(e),
        ],
        Cmd::Render { bars, tail, out, sample_rate } => vec![Request::RenderOffline(RenderParams {
            bars: Some(*bars),
            tail: *tail,
            path: out.clone(),
            sample_rate: *sample_rate,
        })],
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::New => vec![Request::ProjectNew(e)],
            ProjectCmd::Save { path } => vec![Request::ProjectSave(ProjectSaveParams { path: path.clone() })],
            ProjectCmd::Load { path } => vec![Request::ProjectLoad(ProjectLoadParams { path: path.clone() })],
            ProjectCmd::List => vec![Request::ProjectList(e)],
            ProjectCmd::Reveal { .. } => vec![Request::StateGet(e)],
        },
        Cmd::Midi { cmd } => match cmd {
            MidiCmd::Ports => vec![Request::MidiPorts(e)],
            MidiCmd::Connect { input, output, name, profile } => vec![Request::MidiConnect(MidiConnectParams {
                input: input.clone(),
                output: output.clone(),
                name: name.clone(),
                profile: profile.map(|p| match p {
                    ProfileArg::Generic => DeviceProfile::Generic,
                    ProfileArg::Block => DeviceProfile::LividBlock,
                }),
            })],
            MidiCmd::Disconnect { input } => {
                vec![Request::MidiDisconnect(MidiDisconnectParams { input: input.clone() })]
            }
            MidiCmd::Rename { device, name } => {
                vec![Request::MidiRename(MidiRenameParams { device: device.clone(), name: name.clone() })]
            }
            MidiCmd::Seat { seat } => vec![Request::MidiSetSeat(MidiSetSeatParams { seat: seat.clone() })],
            MidiCmd::Send { device, bytes } => vec![Request::MidiInput(MidiInputParams {
                device: device.clone(),
                data: hex_bytes(bytes)?,
                seat: None,
            })],
            MidiCmd::Monitor { .. } => vec![
                Request::EventsSubscribe(SubscribeParams { types: Some(vec!["midi_in".into()]) }),
                Request::EventsUnsubscribe(e),
            ],
        },
        Cmd::Controller { cmd } => match cmd.clone().unwrap_or(ControllerCmd::Show) {
            ControllerCmd::Show => vec![Request::ControllerGet(e)],
            ControllerCmd::Press { row, col, only } => vec![Request::ControllerPress(PadParams {
                row: one_based(row, "row")?,
                col: one_based(col, "column")?,
                pressed: only.map(|o| matches!(o, PadEdge::Down)),
            })],
            ControllerCmd::Knob { index, value } => vec![Request::ControllerKnob(KnobParams {
                index: one_based(index, "knob")?,
                value: parse_value(&value)?,
            })],
            ControllerCmd::Mode { page, follow } => vec![Request::ControllerSetMode(ControllerModeParams {
                page: page.map(|p| one_based(p, "page")).transpose()?,
                follow,
            })],
        },
        Cmd::Seat { cmd } => vec![match cmd.clone().unwrap_or(SeatCmd::List) {
            SeatCmd::List => Request::SeatList(e),
            SeatCmd::Claim { name } => Request::SeatClaim(SeatNameParams { name }),
            SeatCmd::Create { name, ignore } => Request::SeatCreate(SeatCreateParams { name, saved: Some(!ignore) }),
            SeatCmd::Leave => Request::SeatLeave(e),
            SeatCmd::Rm { name } => Request::SeatRemove(SeatNameParams { name }),
        }],
        Cmd::Focus { instrument } => {
            vec![Request::SeatFocus(SeatFocusParams { seat: None, instrument: instrument.clone() })]
        }
        Cmd::Bind { device, channel, notes, transpose, to } => {
            let (low, high) = match notes {
                Some(n) => {
                    let (l, h) = note_range(n)?;
                    (Some(l), Some(h))
                }
                None => (None, None),
            };
            vec![Request::SeatBind(SeatBindParams {
                seat: None,
                binding: NoteBinding {
                    device: device.clone(),
                    channel: *channel,
                    low,
                    high,
                    transpose: transpose.unwrap_or(0),
                    target: to.clone(),
                },
            })]
        }
        Cmd::Unbind { n } => vec![Request::SeatUnbind(SeatUnbindParams { seat: None, index: one_based(*n, "binding")? })],
        Cmd::Cc { cmd } => vec![match cmd {
            CcCmd::Map { device, cc, param, channel, no_pickup } => Request::SeatMapCc(SeatMapCcParams {
                seat: None,
                map: CcMap { device: device.clone(), channel: *channel, cc: *cc, param: param.clone(), pickup: !no_pickup },
            }),
            CcCmd::Unmap { device, cc, channel } => Request::SeatUnmapCc(SeatUnmapCcParams {
                seat: None,
                device: device.clone(),
                cc: *cc,
                channel: *channel,
            }),
            CcCmd::Learn { param } => Request::SeatLearnCc(SeatLearnCcParams { seat: None, param: param.clone() }),
        }],
        Cmd::Knobs { cmd } => vec![match cmd {
            KnobsCmd::Page { page } => Request::SeatPage(SeatPageParams { seat: None, page: page.clone() }),
            KnobsCmd::Follow { device, ccs, channel, no_pickup } => Request::SeatFollowKnobs(SeatFollowKnobsParams {
                seat: None,
                follow: KnobFollow { device: device.clone(), channel: *channel, ccs: ccs.clone(), pickup: !no_pickup },
            }),
        }],
        Cmd::Daemon { cmd } => match cmd {
            DaemonCmd::Status => vec![Request::DaemonInfo(e.clone()), Request::EngineStatus(e)],
            DaemonCmd::Stop { .. } => vec![Request::DaemonShutdown(e)],
            // Local process management; no RPC.
            DaemonCmd::Start(_) | DaemonCmd::Restart(_) | DaemonCmd::Logs { .. } => vec![],
        },
        Cmd::Call { method, params } => {
            let p = params.as_deref().map(serde_json::from_str::<Value>).transpose()?;
            vec![parse_request(method, p).map_err(|e| anyhow!(e))?]
        }
        Cmd::Methods => vec![],
    })
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn grid_line(label: &str, steps: &[u8], length: usize) -> String {
    let s = format_steps(&steps[..length.min(steps.len())]);
    let chunks: Vec<String> = s.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    format!("{label:<11} {}", chunks.join(" "))
}

fn print_pattern(p: &PatternResult) {
    println!("{}:", p.instrument);
    for t in &p.tracks {
        println!("{}", grid_line(t.voice.id(), &t.steps, p.length as usize));
    }
}

fn print_leds(c: &ControllerState) {
    let page = c.page + 1;
    println!(
        "focus: {}  knobs: {}  page: {page}  follow: {}  device: {}  seat: {}",
        c.focus.as_deref().unwrap_or("(none)"),
        c.knob_page.as_deref().unwrap_or("-"),
        c.follow,
        c.device.as_deref().unwrap_or("(virtual only)"),
        c.seat
    );
    for (r, row) in c.leds.iter().enumerate() {
        let cells: String = row.iter().map(|v| if *v != 0 { "# " } else { ". " }).collect();
        let label = Voice::from_index(r).map(|v| v.id()).unwrap_or("");
        println!("  {cells} {label}");
    }
}

fn print_state(s: &Snapshot) {
    let p = |k: &str| s.params.get(k).copied().unwrap_or(0.0);
    println!(
        "transport: {}{}  tempo: {} bpm  swing: {:.0}%  length: {}",
        if s.transport.playing { "playing" } else { "stopped" },
        s.transport.step.map(|x| format!(" (step {})", x + 1)).unwrap_or_default(),
        p("transport.tempo"),
        p("transport.swing") * 100.0,
        p("sequencer.length")
    );
    let length = p("sequencer.length") as usize;
    for ip in &s.patterns {
        let kind = s.graph.instruments.iter().find(|i| i.id == ip.instrument).map(|i| i.kind.id()).unwrap_or("?");
        println!("{} ({kind}):", ip.instrument);
        match &ip.pattern {
            PatternData::Drums { tracks } => {
                for t in tracks {
                    let id = &ip.instrument;
                    let vid = t.voice.id();
                    let level = p(&format!("{id}.{vid}.level"));
                    let muted = if p(&format!("{id}.{vid}.mute")) >= 0.5 { " M" } else { "" };
                    println!("  {}  lvl {:>3.0}%{muted}", grid_line(vid, &t.steps, length), level * 100.0);
                }
            }
            PatternData::Notes { steps } => println!("  {}", format_notes(steps, length)),
        }
    }
    print_mixer(s);
    print_status_lines(s);
}

fn fmt_pan(v: f64) -> String {
    if v.abs() < 0.005 {
        "C".into()
    } else if v < 0.0 {
        format!("L{:.0}", -v * 100.0)
    } else {
        format!("R{:.0}", v * 100.0)
    }
}

fn print_mixer(s: &Snapshot) {
    let p = |k: &str| s.params.get(k).copied().unwrap_or(0.0);
    for c in &s.graph.channels {
        let n = c.n;
        let sources: Vec<&str> =
            s.graph.routes.iter().filter(|(_, ch)| **ch == n).map(|(src, _)| src.as_str()).collect();
        let mut flags = String::new();
        if p(&format!("mixer.{n}.mute")) >= 0.5 {
            flags.push_str(" M");
        }
        if p(&format!("mixer.{n}.solo")) >= 0.5 {
            flags.push_str(" S");
        }
        println!(
            "ch {n:<2} {:<12} vol {:>3.0}%  pan {:<4}{flags:<5}  <- {}",
            c.name,
            p(&format!("mixer.{n}.volume")) * 100.0,
            fmt_pan(p(&format!("mixer.{n}.pan"))),
            if sources.is_empty() { "(nothing)".to_string() } else { sources.join(", ") }
        );
    }
    println!("master        vol {:>3.0}%", p("mixer.master.volume") * 100.0);
}

fn print_graph(g: &Graph) {
    for c in &g.channels {
        let sources: Vec<&str> =
            g.routes.iter().filter(|(_, ch)| **ch == c.n).map(|(src, _)| src.as_str()).collect();
        println!("ch {:<2} {:<12} <- {}", c.n, c.name, if sources.is_empty() { "(nothing)".into() } else { sources.join(", ") });
    }
    let unrouted: Vec<&str> = g
        .instruments
        .iter()
        .map(|i| i.id.as_str())
        .filter(|id| !g.routes.contains_key(*id))
        .collect();
    if !unrouted.is_empty() {
        println!("unrouted main outs: {}", unrouted.join(", "));
    }
}

fn print_instrument(i: &InstrumentInfo) {
    let direct = i.outputs.len().saturating_sub(1);
    let extra = if direct > 0 { format!(", {direct} direct outs") } else { String::new() };
    println!("{:<10} {:<6} {:<12} ({:?} main{extra})", i.id, i.kind.id(), i.name, i.outputs[0].width);
}

fn print_status_lines(s: &Snapshot) {
    let a = &s.audio;
    println!(
        "audio: {} {} @ {} Hz{}",
        a.backend,
        a.device.as_deref().unwrap_or("-"),
        a.sample_rate,
        a.error.as_ref().map(|e| format!(" (fallback: {e})")).unwrap_or_default()
    );
    if s.midi.is_empty() {
        println!("midi: no devices connected");
    }
    for m in &s.midi {
        println!("midi: {} as {} ({:?}) out: {}", m.input, m.device, m.profile, m.output.as_deref().unwrap_or("-"));
    }
    println!("devices on this engine play in seat: {}", s.seats.host);
    println!(
        "project: {}{}",
        s.project.path.as_deref().unwrap_or("(unsaved)"),
        if s.project.dirty { " *modified*" } else { "" }
    );
}

fn fmt_note_range(b: &NoteBinding) -> String {
    match (b.low, b.high) {
        (None, None) => "all notes".into(),
        (l, h) => format!("{}..{}", note_name(l.unwrap_or(0)), note_name(h.unwrap_or(127))),
    }
}

fn print_seat(s: &Seat, host: bool) {
    let mut flags = Vec::new();
    if !s.saved {
        flags.push("this session only".to_string());
    }
    if host {
        flags.push("this engine's devices".to_string());
    }
    if let Some(p) = &s.learning {
        flags.push(format!("learning a CC for {p}"));
    }
    let flags = if flags.is_empty() { String::new() } else { format!("  ({})", flags.join(", ")) };
    let who = if s.occupants.is_empty() { "nobody".into() } else { s.occupants.join(", ") };
    println!("{}: {who}{flags}", s.name);
    println!(
        "  focus: {}  knobs: {}",
        s.config.focus.as_deref().unwrap_or("(first instrument)"),
        s.config.knob_page.as_deref().unwrap_or("(first page)")
    );
    for (i, b) in s.config.bindings.iter().enumerate() {
        let ch = b.channel.map(|c| format!("ch {c}")).unwrap_or("any ch".into());
        let tr = if b.transpose != 0 { format!(" {:+} st", b.transpose) } else { String::new() };
        println!("  bind {}: {} {ch} {}{tr} -> {}", i + 1, b.device, fmt_note_range(b), b.target);
    }
    for m in &s.config.cc {
        let ch = m.channel.map(|c| format!("ch {c}")).unwrap_or("any ch".into());
        let pickup = if m.pickup { "" } else { " (no pickup)" };
        println!("  cc: {} {ch} cc {} -> {}{pickup}", m.device, m.cc, m.param);
    }
    for k in &s.config.knobs {
        let ccs: Vec<String> = k.ccs.iter().map(|c| c.to_string()).collect();
        println!("  knobs: {} cc {} follow focus", k.device, ccs.join(" "));
    }
}

fn print_event(e: &EventEnvelope, json: bool) {
    if json {
        println!("{}", serde_json::to_string(e).unwrap());
        return;
    }
    let body = match &e.event {
        Event::ParamChanged { path, value } => format!("{path} = {value}"),
        Event::StepChanged { instrument, voice, step, level } => {
            format!("{instrument}.{} step {} = {}", voice.id(), step + 1, level)
        }
        Event::PatternChanged { instrument, voice, steps } => {
            format!("{instrument}.{} = {}", voice.id(), format_steps(steps))
        }
        Event::NotesChanged { instrument, steps } => {
            let last = steps.iter().rposition(|s| s.note.is_some()).map(|i| i + 1).unwrap_or(0);
            format!("{instrument} = {}", format_notes(steps, last.max(16)))
        }
        Event::Graph { graph } => format!(
            "{} instruments, {} channels, {} routes",
            graph.instruments.len(),
            graph.channels.len(),
            graph.routes.len()
        ),
        Event::Transport { playing } => if *playing { "playing".into() } else { "stopped".into() },
        Event::Playhead { step, time } => format!("step {} @ {time:.3}s", step + 1),
        Event::Trigger { instrument, voice, note, velocity, time } => {
            let what = match (voice, note) {
                (Some(v), _) => v.id().to_string(),
                (None, Some(n)) => note_name(*n),
                _ => "?".into(),
            };
            format!("{instrument} {what} vel {velocity:.2} @ {time:.3}s")
        }
        Event::Meters { channels, master } => format!(
            "channels [{}] master [{:.2} {:.2}]",
            channels.iter().map(|c| format!("{}:{:.2}/{:.2}", c.channel, c.left, c.right)).collect::<Vec<_>>().join(" "),
            master.first().unwrap_or(&0.0),
            master.get(1).unwrap_or(&0.0)
        ),
        Event::MidiIn { port, data } => {
            let hex: Vec<String> = data.iter().map(|b| format!("{b:02X}")).collect();
            format!("{port}: {}", hex.join(" "))
        }
        other => serde_json::to_string(other).unwrap(),
    };
    println!("[{}] {:<10} {:<15} {body}", e.seq, e.origin, e.event.type_name());
}

fn present(cmd: &Cmd, results: &[Value], json: bool) -> Result<()> {
    let last = results.last().cloned().unwrap_or(Value::Null);
    if json {
        let out = match cmd {
            Cmd::Params { .. } => params_json(results)?,
            Cmd::Status => serde_json::json!({ "state": results[0], "audio": last }),
            _ => last,
        };
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    match cmd {
        Cmd::Status => {
            let mut s: Snapshot = serde_json::from_value(results[0].clone())?;
            s.audio = serde_json::from_value(last)?;
            let p = |k: &str| s.params.get(k).copied().unwrap_or(0.0);
            println!(
                "transport: {}  tempo: {} bpm",
                if s.transport.playing { "playing" } else { "stopped" },
                p("transport.tempo")
            );
            print_status_lines(&s);
        }
        Cmd::State => print_state(&serde_json::from_value(last)?),
        Cmd::Params { .. } => {
            for p in params_json(results)?.as_array().unwrap() {
                let info: ParamInfo = serde_json::from_value(p["info"].clone())?;
                let range = match info.kind {
                    ParamKind::Continuous { min, max } => format!("{min}..{max}"),
                    ParamKind::Integer { min, max } => format!("{min}..{max} (int)"),
                    ParamKind::Toggle => "on/off".into(),
                };
                let unit = info.unit.map(|u| format!(" {u}")).unwrap_or_default();
                println!("{:<24} {:>8}{unit:<6}  [{range}]", info.path, p["value"]);
            }
        }
        Cmd::Get { .. } | Cmd::Set { .. } | Cmd::Tempo { .. } => {
            let v: ParamValue = serde_json::from_value(last)?;
            println!("{} = {}", v.path, v.value);
        }
        Cmd::Play | Cmd::Stop => {
            let t: TransportState = serde_json::from_value(last)?;
            println!("{}", if t.playing { "playing" } else { "stopped" });
        }
        Cmd::Pattern { cmd, .. } => match cmd {
            None | Some(PatternCmd::Show { .. }) | Some(PatternCmd::Clear { .. }) => {
                print_pattern(&serde_json::from_value(last)?)
            }
            Some(PatternCmd::Set { .. }) => {
                // `pattern set` returns one track; the instrument is implied.
                let t: TrackPattern = serde_json::from_value(last)?;
                println!("{}", grid_line(t.voice.id(), &t.steps, 16.max(t.steps.iter().rposition(|s| *s != 0).map(|i| i + 1).unwrap_or(0))));
            }
            Some(_) => {
                let s: StepResult = serde_json::from_value(last)?;
                println!(
                    "{}.{} step {} = {}",
                    s.instrument,
                    s.voice.id(),
                    s.step + 1,
                    ["off", "on", "accent"][s.level as usize]
                );
            }
        },
        Cmd::Notes { .. } | Cmd::Note { .. } => {
            let n: NotesResult = serde_json::from_value(last)?;
            let last_note = n.steps.iter().rposition(|s| s.note.is_some()).map(|i| i + 1).unwrap_or(0);
            println!("{}: {}", n.instrument, format_notes(&n.steps, (n.length as usize).max(last_note)));
        }
        Cmd::Trigger { .. } | Cmd::Key { .. } | Cmd::Midi { cmd: MidiCmd::Send { .. } } => println!("ok"),
        Cmd::Instrument { cmd: InstrumentCmd::Types } => {
            let r: InstrumentTypesResult = serde_json::from_value(last)?;
            for t in r.types {
                println!(
                    "{:<6} {:<6} default id `{}`, {} params, {} outputs",
                    t.kind.id(),
                    t.label,
                    t.default_id,
                    t.params.len(),
                    t.outputs.len()
                );
            }
        }
        Cmd::Instrument { cmd: InstrumentCmd::List } => {
            let r: InstrumentListResult = serde_json::from_value(last)?;
            if r.instruments.is_empty() {
                println!("(no instruments)");
            }
            for i in &r.instruments {
                print_instrument(i);
            }
        }
        Cmd::Instrument { cmd: InstrumentCmd::Add { .. } } => {
            print_instrument(&serde_json::from_value(last)?);
        }
        Cmd::Instrument { .. } | Cmd::Route { .. } | Cmd::Channel { cmd: ChannelCmd::Rm { .. } } => {
            print_graph(&serde_json::from_value(last)?);
        }
        Cmd::Channel { .. } => {
            let c: ChannelInfo = serde_json::from_value(last)?;
            println!("ch {} {}", c.n, c.name);
        }
        Cmd::Mixer => print_mixer(&serde_json::from_value(last)?),
        Cmd::Render { .. } => {
            let r: RenderResult = serde_json::from_value(last)?;
            println!("wrote {} ({:.2}s @ {} Hz)", r.path, r.duration, r.sample_rate);
            println!("peak {:.3}  rms {:.4}", r.peak, r.rms);
            println!(
                "left: peak {:.3} rms {:.4}  right: peak {:.3} rms {:.4}",
                r.left.peak, r.left.rms, r.right.peak, r.right.rms
            );
            let mut per: std::collections::BTreeMap<&str, usize> = Default::default();
            for t in &r.triggers {
                *per.entry(t.instrument.as_str()).or_default() += 1;
            }
            let per: Vec<String> = per.iter().map(|(i, n)| format!("{i} {n}")).collect();
            println!(
                "triggers: {} ({})  detected onsets: {}",
                r.triggers.len(),
                if per.is_empty() { "none".into() } else { per.join(", ") },
                r.onsets.len()
            );
            let onsets: Vec<String> = r.onsets.iter().map(|t| format!("{t:.3}")).collect();
            println!("onsets (s): {}", onsets.join(" "));
        }
        Cmd::Project { cmd: ProjectCmd::List } => {
            let r: ProjectListResult = serde_json::from_value(last)?;
            if r.projects.is_empty() {
                println!("(no projects)");
            }
            for p in r.projects {
                println!("{p}");
            }
        }
        Cmd::Project { .. } => {
            let p: ProjectInfo = serde_json::from_value(last)?;
            println!("project: {}{}", p.path.as_deref().unwrap_or("(unsaved)"), if p.dirty { " *modified*" } else { "" });
        }
        Cmd::Midi { .. } => {
            let r: MidiPortsResult = serde_json::from_value(last)?;
            println!("inputs:");
            for i in &r.inputs {
                println!("  {i}");
            }
            println!("outputs:");
            for o in &r.outputs {
                println!("  {o}");
            }
            println!("connected:");
            if r.connections.is_empty() {
                println!("  (none)");
            }
            for c in &r.connections {
                println!("  {} as {} ({:?}) out: {}", c.input, c.device, c.profile, c.output.as_deref().unwrap_or("-"));
            }
            let pinned = if r.pinned_seat.is_some() { " (pinned)" } else { "" };
            println!("devices play in seat: {}{pinned}", r.seat);
        }
        Cmd::Controller { .. } => print_leds(&serde_json::from_value(last)?),
        Cmd::Seat { .. } => {
            let r: SeatListResult = serde_json::from_value(last)?;
            if r.seats.is_empty() {
                println!("(no seats)");
            }
            for s in &r.seats {
                print_seat(s, s.name == r.host);
            }
            println!("you: {}", r.you.as_deref().unwrap_or("(no seat)"));
        }
        Cmd::Focus { .. } | Cmd::Bind { .. } | Cmd::Unbind { .. } | Cmd::Cc { .. } | Cmd::Knobs { .. } => {
            print_seat(&serde_json::from_value(last)?, false)
        }
        _ => println!("{}", serde_json::to_string_pretty(&last)?),
    }
    Ok(())
}

/// Local desktop action (not an RPC): show the project bundle in the OS file
/// manager. Only possible when the daemon -- and so the file -- is on this
/// machine.
fn reveal_project(snap: &Snapshot, url: &str, no_open: bool) -> Result<()> {
    let Some(path) = &snap.project.path else {
        bail!("project is not saved yet; save it first with `4s project save NAME`");
    };
    println!("{path}");
    if no_open {
        return Ok(());
    }
    let host = url.trim_start_matches("ws://").trim_start_matches("wss://");
    let local = ["127.0.0.1", "localhost", "[::1]"].iter().any(|h| host.starts_with(h));
    if !local {
        bail!("the project is on the engine host ({url}), not this machine");
    }
    if !std::path::Path::new(path).exists() {
        bail!("not found on this machine: {path}");
    }
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg("-R").arg(path).status()
    } else if cfg!(windows) {
        std::process::Command::new("explorer").arg(format!("/select,{path}")).status()
    } else {
        let dir = std::path::Path::new(path).parent().unwrap_or(std::path::Path::new("/"));
        std::process::Command::new("xdg-open").arg(dir).status()
    };
    status.map_err(|e| anyhow!("could not open the file manager: {e}"))?;
    Ok(())
}

/// Join param.list with current values: `[{info, value}]`.
fn params_json(results: &[Value]) -> Result<Value> {
    let list: ParamListResult = serde_json::from_value(results[0].clone())?;
    let snap: Snapshot = serde_json::from_value(results[1].clone())?;
    Ok(Value::Array(
        list.params
            .into_iter()
            .map(|p| {
                let value = snap.params.get(&p.path).copied().unwrap_or(p.default);
                serde_json::json!({ "info": p, "value": value })
            })
            .collect(),
    ))
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli).await {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Explicit --url / FOURS_URL, else the local daemon's runtime file, else the
/// default address -- but only for the default data dir. An explicitly chosen
/// data dir with no daemon is an error, so commands never silently reach some
/// other daemon on the default port.
fn resolve_url(cli: &Cli, data_dir: &std::path::Path) -> Result<String> {
    if let Some(u) = &cli.url {
        return Ok(u.clone());
    }
    match daemon_ctl::live(data_dir) {
        Some(info) => Ok(info.url),
        None if cli.data_dir.is_some() => Err(anyhow!(
            "no 4sd running for data dir {}\nhint: start the daemon with `4s daemon start`",
            data_dir.display()
        )),
        None => Ok(format!("ws://{}", DEFAULT_LISTEN)),
    }
}

/// Seat options from the command line. The user defaults to the OS user,
/// as the daemon's host seat does.
fn seating(cli: &Cli) -> Seating {
    let user = cli
        .user
        .clone()
        .or_else(|| ["USER", "USERNAME"].iter().find_map(|k| std::env::var(k).ok()))
        .filter(|u| !u.trim().is_empty());
    Seating { user, seat: cli.seat.clone(), auto: !cli.no_seat && !cli.new_seat }
}

async fn connect(cli: &Cli, url: &str) -> Result<Client> {
    let mut c = Client::connect(url, cli.token.clone(), "cli", &seating(cli)).await.map_err(|e| {
        if cli.url.is_none() {
            anyhow!("{e:#}\nhint: start the daemon with `4s daemon start`")
        } else {
            e
        }
    })?;
    if cli.new_seat {
        c.call(&Request::SeatCreate(SeatCreateParams { name: None, saved: Some(true) })).await?;
    }
    Ok(c)
}

async fn run_daemon(cli: &Cli, cmd: &DaemonCmd, data_dir: &std::path::Path) -> Result<()> {
    match cmd {
        DaemonCmd::Start(args) => {
            if args.restart_if_stale
                && let Some(old) = daemon_ctl::live(data_dir)
                && daemon_ctl::is_stale(&old, args.bin.as_deref())
            {
                return restart_stale(cli, data_dir, args, old).await;
            }
            let (info, started) = daemon_ctl::start(data_dir, args, cli.token.as_deref())?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else if started {
                println!("4sd started (pid {}) on {}", info.pid, info.url);
            } else {
                println!("4sd already running (pid {}) on {}", info.pid, info.url);
            }
        }
        DaemonCmd::Stop { force } => {
            let Some(info) = daemon_ctl::live(data_dir) else {
                if cli.url.is_some() {
                    // Remote daemon: we can only ask it to stop.
                    let url = resolve_url(cli, data_dir)?;
                    connect(cli, &url).await?.call(&Request::DaemonShutdown(Empty {})).await?;
                    println!("shutdown requested");
                } else {
                    println!("4sd not running");
                }
                return Ok(());
            };
            let graceful = match Client::connect(&info.url, cli.token.clone(), "cli", &Seating::none()).await {
                Ok(mut c) => c.call(&Request::DaemonShutdown(Empty {})).await.is_ok(),
                Err(_) => false,
            };
            if !(graceful && daemon_ctl::wait_exit(info.pid, std::time::Duration::from_secs(5))) {
                daemon_ctl::signal_stop(info.pid, *force)?;
            }
            let _ = daemon_ctl::live(data_dir); // clears a stale runtime file
            println!("4sd stopped (pid {})", info.pid);
        }
        DaemonCmd::Status => {
            if cli.url.is_none() && daemon_ctl::live(data_dir).is_none() {
                if cli.json {
                    println!("{{\"running\": false}}");
                } else {
                    println!("4sd not running (data dir {})", data_dir.display());
                }
                std::process::exit(3);
            }
            let url = resolve_url(cli, data_dir)?;
            let mut client = connect(cli, &url).await?;
            let info: DaemonInfo = serde_json::from_value(client.call(&Request::DaemonInfo(Empty {})).await?)?;
            let audio: AudioStatus = serde_json::from_value(client.call(&Request::EngineStatus(Empty {})).await?)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "running": true, "info": info, "audio": audio }))?);
            } else {
                println!("4sd running (pid {}) on {}", info.pid, info.url);
                println!("version {} (protocol {}), up {}", info.version, info.protocol_version, daemon_ctl::format_uptime(info.uptime));
                println!("audio: {} {} @ {} Hz", audio.backend, audio.device.as_deref().unwrap_or("-"), audio.sample_rate);
                println!("data dir: {}", info.data_dir);
                if let Some(l) = &info.log_file {
                    println!("log: {l}");
                }
            }
        }
        DaemonCmd::Restart(args) => {
            if let Some(info) = daemon_ctl::live(data_dir) {
                if let Ok(mut c) = Client::connect(&info.url, cli.token.clone(), "cli", &Seating::none()).await {
                    let _ = c.call(&Request::DaemonShutdown(Empty {})).await;
                }
                if !daemon_ctl::wait_exit(info.pid, std::time::Duration::from_secs(5)) {
                    daemon_ctl::signal_stop(info.pid, false)?;
                }
            }
            let (info, _) = daemon_ctl::start(data_dir, args, cli.token.as_deref())?;
            println!("4sd started (pid {}) on {}", info.pid, info.url);
        }
        DaemonCmd::Logs { lines } => {
            let path = daemon_ctl::live(data_dir)
                .and_then(|i| i.log_file.map(PathBuf::from))
                .unwrap_or_else(|| daemon_ctl::log_path(data_dir));
            if !path.exists() {
                bail!("no log file at {}", path.display());
            }
            println!("{}", daemon_ctl::tail(&path, *lines));
        }
    }
    Ok(())
}

/// Ask a yes/no question on the terminal (default no). Without a terminal
/// there is nobody to ask, so the answer is no.
fn confirm(question: &str) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Restart a daemon running an old build, carrying its session over: unsaved
/// or modified work is saved to `<data-dir>/autosave/dev-session.4s` first.
/// If the old daemon speaks an incompatible protocol, or its session cannot
/// be loaded by the new build, offer to start fresh instead.
async fn restart_stale(cli: &Cli, data_dir: &std::path::Path, args: &StartArgs, old: DaemonInfo) -> Result<()> {
    // Talking to the old daemon at all is what fails when its build is
    // incompatible (a different protocol version). Only then is discarding
    // its session on the table; any later failure (e.g. saving) just stops.
    let session = match Client::connect(&old.url, cli.token.clone(), "cli", &Seating::none()).await {
        Ok(mut c) => {
            let session = save_session(&mut c, data_dir).await?;
            let _ = c.call(&Request::DaemonShutdown(Empty {})).await;
            Some(session)
        }
        Err(e) => {
            println!("the running 4sd (pid {}, {}) is from an incompatible build: {e:#}", old.pid, old.version);
            if !confirm("Stop it and start fresh, discarding its unsaved session?") {
                bail!(
                    "4sd (pid {}) left running. Start fresh with `scripts/dev.sh --fresh` or `4s daemon stop`",
                    old.pid
                );
            }
            None
        }
    };
    match session {
        // Asked to shut down; signal only if it does not exit.
        Some(_) => {
            if !daemon_ctl::wait_exit(old.pid, std::time::Duration::from_secs(5)) {
                daemon_ctl::signal_stop(old.pid, false)?;
            }
        }
        // It cannot be asked, and its session is being discarded anyway.
        None => daemon_ctl::signal_stop(old.pid, true)?,
    }
    let mut args = args.clone();
    let restored = args.project.is_none() && session.is_some();
    if restored {
        args.project = session.clone();
    }
    let info = match daemon_ctl::start(data_dir, &args, cli.token.as_deref()) {
        Ok((info, _)) => info,
        Err(e) if restored => {
            println!("could not restore the session from {}:\n{e:#}", session.as_deref().unwrap_or("?"));
            if !confirm("Start with a new project instead?") {
                bail!("4sd not started. Start fresh with `scripts/dev.sh --fresh`");
            }
            args.project = None;
            let (info, _) = daemon_ctl::start(data_dir, &args, cli.token.as_deref())?;
            println!("started 4sd (pid {}) on {} with a new project", info.pid, info.url);
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    if cli.json {
        println!("{}", serde_json::to_string_pretty(&info)?);
    } else {
        println!("restarted 4sd (code changed; pid {} -> {}) on {}", old.pid, info.pid, info.url);
        match (&session, restored) {
            (Some(s), true) => println!("session restored from {s}"),
            (None, _) => println!("previous session discarded"),
            _ => {}
        }
    }
    Ok(())
}

/// Save the old daemon's session if it is unsaved or modified. Returns the
/// project path to restore.
async fn save_session(c: &mut Client, data_dir: &std::path::Path) -> Result<String> {
    let snap: Snapshot = serde_json::from_value(c.call(&Request::StateGet(Empty {})).await?)?;
    Ok(match (&snap.project.path, snap.project.dirty) {
        (Some(p), false) => p.clone(),
        _ => {
            let p = data_dir.join("autosave").join("dev-session.4s").to_string_lossy().into_owned();
            c.call(&Request::ProjectSave(ProjectSaveParams { path: Some(p.clone()) }))
                .await
                .map_err(|e| anyhow!("could not save the running session to {p}: {e:#}; 4sd left running"))?;
            p
        }
    })
}

async fn run(cli: Cli) -> Result<()> {
    if let Cmd::Methods = cli.cmd {
        for (m, doc) in METHOD_DOCS {
            println!("{m:<22} {doc}");
        }
        return Ok(());
    }
    let data_dir = cli.data_dir.clone().unwrap_or_else(daemon_ctl::default_data_dir);
    if let Cmd::Daemon { cmd } = &cli.cmd {
        return run_daemon(&cli, cmd, &data_dir).await;
    }
    let reqs = plan(&cli.cmd)?;
    let url = resolve_url(&cli, &data_dir)?;
    let mut client = connect(&cli, &url).await?;

    let stream_count = match &cli.cmd {
        Cmd::Watch { count, .. } => Some(*count),
        Cmd::Midi { cmd: MidiCmd::Monitor { count } } => Some(*count),
        _ => None,
    };
    if let Some(count) = stream_count {
        client.call(&reqs[0]).await?;
        let mut seen = 0;
        loop {
            let e = client.next_event().await?;
            print_event(&e, cli.json);
            seen += 1;
            if count.is_some_and(|c| seen >= c) {
                break;
            }
        }
        client.call(&reqs[1]).await?;
        return Ok(());
    }

    let mut results = Vec::new();
    for (i, r) in reqs.iter().enumerate() {
        if let (Cmd::Key { secs, .. }, 1) = (&cli.cmd, i) {
            tokio::time::sleep(std::time::Duration::from_secs_f64(secs.clamp(0.0, 3600.0))).await;
        }
        results.push(client.call(r).await?);
    }
    if let Cmd::Project { cmd: ProjectCmd::Reveal { no_open } } = &cli.cmd {
        let snap: Snapshot = serde_json::from_value(results.remove(0))?;
        return reveal_project(&snap, &url, *no_open);
    }
    if results.is_empty() {
        bail!("nothing to do");
    }
    present(&cli.cmd, &results, cli.json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn methods_for(args: &str) -> Vec<&'static str> {
        let argv = std::iter::once("4s").chain(args.split_whitespace());
        let cli = Cli::try_parse_from(argv).unwrap_or_else(|e| panic!("{args}: {e}"));
        plan(&cli.cmd).unwrap().iter().map(|r| r.method()).collect()
    }

    /// Every RPC method must be reachable through a dedicated CLI command
    /// (not just `4s call`). Adding a method to the API without a CLI
    /// command fails this test.
    #[test]
    fn cli_covers_every_method() {
        let commands = [
            "status", "state", "params", "get mixer.1.volume", "set drums.kick.level 35%",
            "instrument types", "instrument list", "instrument add tb303 --id bass", "instrument rm bass",
            "channel add --name Hat", "channel rm 2", "channel rename 1 Kit", "route drums.closed_hat 2",
            "mixer", "notes bass C2", "notes bass", "note bass 3 D#2!~", "key C2 --for 0.1", "trigger --note C2",
            "play", "stop", "tempo 128", "pattern", "pattern show kick",
            "pattern set kick x---x---", "pattern set sd ----x---", "set mixer.1.pan -0.5", "pattern step kick 1 accent", "pattern toggle sd 5",
            "pattern clear", "trigger kick", "watch", "render --bars 2", "project new",
            "project save beat", "project load beat", "project list", "midi ports",
            "midi connect Block", "midi disconnect Block", "midi monitor", "controller",
            "midi rename Block pads", "midi seat sam", "midi send keys 90 3C 64",
            "controller press 1 1", "controller knob 1 50%", "controller mode --page 2",
            "seat", "seat claim sam", "seat create --ignore", "seat leave", "seat rm sam",
            "focus bass", "bind keys --notes C1..B2 --to bass", "unbind 1", "cc map knobs 21 bass.cutoff",
            "cc unmap knobs 21", "cc learn bass.cutoff", "knobs page decay", "knobs follow knobs 21 22",
            "daemon status", "daemon stop",
        ];
        let mut covered: BTreeSet<&str> = commands.iter().flat_map(|c| methods_for(c)).collect();
        covered.insert("session.hello"); // sent by every command on connect
        let all: BTreeSet<&str> = METHODS.iter().copied().collect();
        let missing: Vec<_> = all.difference(&covered).collect();
        assert!(missing.is_empty(), "RPC methods without a CLI command: {missing:?}");
    }
}
