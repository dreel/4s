//! 4s: command line client for the 4S daemon. Every UI action has a CLI
//! equivalent; see docs/api-parity.md.
//!
//! User-facing numbering in the CLI is 1-based (steps, rows, columns, knobs,
//! pages), matching how musicians count. The RPC API is 0-based.

mod client;
mod daemon_ctl;
mod replay;

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
    /// Who you are: owns your undo history, and the seat with this name is
    /// joined automatically. Default: $USER.
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

impl Cli {
    fn user(&self) -> Option<String> {
        self.user
            .clone()
            .or_else(|| ["USER", "USERNAME"].iter().find_map(|k| std::env::var(k).ok()))
            .filter(|u| !u.trim().is_empty())
    }
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
    /// Record live notes into an instrument's clip (RFC 0008): `4s record`
    /// starts (playing after the count-in if stopped), `4s record --off`
    /// ends it, keeping what was played. Settings stay for the next take.
    Record {
        /// End the take.
        #[arg(long, conflicts_with = "show")]
        off: bool,
        /// Only show (or, with settings, change) the record state.
        #[arg(long)]
        show: bool,
        /// Instrument to record into (default: your seat's focus).
        #[arg(long)]
        to: Option<String>,
        #[command(flatten)]
        settings: RecordArgs,
    },
    /// The metronome: `4s metronome on`, `4s metronome --level 40%`. It
    /// always clicks during a recording's count-in.
    Metronome {
        /// on or off.
        state: Option<String>,
        #[arg(long)]
        level: Option<String>,
    },
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
    /// Add, remove, rename, and reorder mixer channels.
    Channel {
        #[command(subcommand)]
        cmd: ChannelCmd,
    },
    /// Route an instrument output to a channel, e.g. `4s route drums.kick 2`,
    /// or unroute it with `none` (a direct out returns to the main mix).
    Route {
        source: String,
        channel: String,
        /// Move whatever else feeds the channel to this source's old channel
        /// (if it had none, they stay and share the channel).
        #[arg(long)]
        swap: bool,
    },
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
        /// In song mode, the bar (or bar.beat) to start from (default: the
        /// locate point).
        #[arg(long)]
        from: Option<String>,
        /// Include the metronome click.
        #[arg(long)]
        metronome: bool,
        /// Notes to play live during the render, as `secs:note[:dur[:vel]]`
        /// tokens (seconds from play), e.g. `"0.51:C2 1.0:D#2:0.25:127"`.
        #[arg(long, allow_hyphen_values = true)]
        input: Option<String>,
        /// Record `--input` and show the clip it would leave (the project
        /// is not changed).
        #[arg(long)]
        record: bool,
        /// Instrument the input plays and records into (default: your focus).
        #[arg(long)]
        to: Option<String>,
        #[command(flatten)]
        settings: RecordArgs,
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
    /// Show or edit an instrument's clip: timed note events (RFC 0007).
    /// Ticks: 96 per quarter note, 24 per step. `4s clip` shows your focus.
    Clip {
        /// Which clip in the instrument's pool (default: its selected clip).
        #[arg(long, global = true)]
        clip: Option<u32>,
        #[command(subcommand)]
        cmd: Option<ClipCmd>,
    },
    /// The song (RFC 0008): each track's arrangement of clips. `4s song`
    /// shows it. Positions are bars, or bar.beat (`3.2`), from 1.
    Song {
        #[command(subcommand)]
        cmd: Option<SongCmd>,
    },
    /// Where song mode plays from, e.g. `4s locate 5` (bar 5) or `5.3`.
    Locate { at: String },
    /// Several requests as one journal entry and undo step: a JSON array
    /// of `{"method": ..., "params": ...}`, e.g.
    /// `4s batch '[{"method":"tempo"...}]'` (or `-` to read it from stdin).
    Batch { requests: String },
    /// Seats: each performer's focus, bindings, and CC maps (RFC 0007).
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
        /// Instrument id, `focus`, or `@<type>` (the first tr808: `@tr808`).
        #[arg(long, default_value = "focus")]
        to: String,
        /// Output notes for the input notes from the range's low end, e.g.
        /// pads to drum voices: `--remap 36,38,39,42`.
        #[arg(long, value_delimiter = ',')]
        remap: Option<Vec<String>>,
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
    /// Undo your last change (changes someone else made since are kept).
    Undo,
    /// Redo your last undo.
    Redo,
    /// Your undo and redo stacks.
    History,
    /// The journal: every request that could change state, who sent it, and
    /// what it changed. Times are UTC. See docs/journal.md.
    Journal {
        #[command(subcommand)]
        cmd: Option<JournalCmd>,
        /// Only entries after this seq.
        #[arg(long)]
        since: Option<u64>,
        /// Only this user's entries.
        #[arg(long = "for")]
        for_user: Option<String>,
        /// At most this many (newest).
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Keep printing new entries as they are recorded.
        #[arg(long, conflicts_with_all = ["since", "for_user"])]
        follow: bool,
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
    /// Add an instrument, by default on the first empty channel, else a new one.
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
    /// Move a channel to a display position (1 = leftmost).
    Move { n: u32, position: u32 },
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
enum JournalCmd {
    /// Save this session (since daemon start or the last project
    /// new/load/import) as a replayable recording.
    Export {
        /// Write here (on this machine); default stdout.
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "json")]
        format: ExportFormat,
    },
    /// Replay a recording (or a <data-dir>/journal/*.jsonl file) into the
    /// daemon and check it ends up the same, entry by entry. Replaces the
    /// daemon's project: use an isolated daemon.
    Replay {
        file: PathBuf,
        /// Wait between entries as long as the recording did.
        #[arg(long)]
        realtime: bool,
        /// For a .jsonl file: which segment (0-based; default the last).
        #[arg(long)]
        segment: Option<usize>,
        /// Replay even if the daemon has unsaved changes.
        #[arg(long)]
        force: bool,
        /// If it diverges, rewrite the recording with what the replay did
        /// (after a deliberate behavior change; review with `git diff`).
        #[arg(long)]
        accept: bool,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum ExportFormat {
    /// A recording for `4s journal replay`.
    Json,
    /// A shell script of `4s call` lines (approximate, editable).
    Sh,
}

#[derive(Subcommand, Debug, Clone)]
enum ProjectCmd {
    /// Start a fresh empty project.
    New,
    /// Save (to the current path, or a new one).
    Save { path: Option<String> },
    /// Load a project bundle.
    Load { path: String },
    /// Load a project file from this machine (a bundle dir or its
    /// project.json), sent inline. Works with a remote daemon.
    Import { file: PathBuf },
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
    /// Known device models (controllers with a default layout).
    Models,
    /// Show the default layout of a connected device's model; `--apply`
    /// copies it into your seat to edit.
    Layout {
        device: String,
        #[arg(long)]
        apply: bool,
    },
    /// Send one raw MIDI message as if from a device, e.g.
    /// `4s midi send keys 90 3C 64`.
    Send {
        device: String,
        /// Hex bytes.
        #[arg(required = true)]
        bytes: Vec<String>,
        /// Read the bytes as this kind of device (default: the connected
        /// device's kind, else generic).
        #[arg(long, value_enum)]
        profile: Option<ProfileArg>,
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

/// Record settings, shared by `4s record` and `4s render --record`.
#[derive(clap::Args, Debug, Clone, Default)]
struct RecordArgs {
    /// Each pass clears the part of the loop it covers (default: overdub).
    #[arg(long, conflicts_with = "overdub")]
    replace: bool,
    /// Add to what is in the clip.
    #[arg(long)]
    overdub: bool,
    /// Record quantize: 1/4, 1/8, 1/8t, 1/16, 1/16t, 1/32, ticks, or off.
    #[arg(long)]
    quantize: Option<String>,
    /// How far quantize moves notes toward the grid: 0..1 or a percentage.
    #[arg(long)]
    strength: Option<String>,
    /// Bars of metronome before recording from a stop (0..4).
    #[arg(long)]
    count_in: Option<u32>,
    /// Extra input latency to compensate, in milliseconds.
    #[arg(long)]
    offset_ms: Option<f32>,
}

impl RecordArgs {
    fn params(&self, arm: Option<bool>, instrument: Option<String>) -> Result<RecordParams> {
        let mode = match (self.replace, self.overdub) {
            (true, _) => Some(RecordMode::Replace),
            (_, true) => Some(RecordMode::Overdub),
            _ => None,
        };
        Ok(RecordParams {
            arm,
            instrument,
            mode,
            quantize: self.quantize.as_deref().map(|q| grid_arg(q).map(|g| g.unwrap_or(0))).transpose()?,
            strength: self.strength.as_deref().map(|v| parse_value(v).map(|v| v as f32)).transpose()?,
            count_in: self.count_in,
            offset_ms: self.offset_ms,
        })
    }
}

fn device_profile(p: ProfileArg) -> DeviceProfile {
    match p {
        ProfileArg::Generic => DeviceProfile::Generic,
        ProfileArg::Block => DeviceProfile::LividBlock,
    }
}

#[derive(Subcommand, Debug, Clone)]
enum SongCmd {
    /// Show every track's arrangement.
    Show,
    /// Place a clip (default: the selected one) on a track at a bar, e.g.
    /// `4s song place drums --at 1 --bars 4`; it cuts what it overlaps.
    Place {
        instrument: String,
        #[arg(long)]
        at: String,
        /// Bars long (default: the clip's length).
        #[arg(long)]
        bars: Option<f64>,
        /// Ticks into the clip it starts at.
        #[arg(long)]
        offset: Option<u32>,
        /// Which clip (default: the selected one).
        #[arg(long)]
        clip: Option<u32>,
    },
    /// Remove the placement that starts at a bar.
    Rm { instrument: String, at: String },
    /// Move the placement that starts at a bar to another.
    Mv { instrument: String, at: String, to: String },
    /// Song mode plays the arrangement; pattern mode loops each track's
    /// selected clip.
    Mode { mode: String },
    /// What song mode loops: `song`, `off` (stop at the end), or bars, e.g.
    /// `4s song loop 2 5` for bars 2 to 5.
    Loop {
        what: String,
        to: Option<u32>,
    },
}

#[derive(Subcommand, Debug, Clone)]
enum ClipCmd {
    /// Show a clip (default: your focus).
    Show { instrument: Option<String> },
    /// The clips in an instrument's pool (default: every instrument's).
    List { instrument: Option<String> },
    /// A new empty clip, selected unless --keep.
    New {
        instrument: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// Steps (default: follow sequencer.length).
        #[arg(long)]
        steps: Option<u32>,
        /// Keep the selected clip selected.
        #[arg(long)]
        keep: bool,
    },
    /// Copy a clip into a new one, and select it.
    Dup { instrument: Option<String> },
    Rename { instrument: String, name: String },
    /// Delete a clip and its placements.
    Del { instrument: String },
    /// Select the clip pattern mode plays and the step editors edit.
    Select { instrument: String, id: u32 },
    /// Replace a clip's events: `tick:note[:len[:vel]]` tokens, e.g.
    /// `4s clip set bass "0:C2:12 24:D#2:25:127 36:G1"`.
    Set {
        instrument: String,
        #[arg(allow_hyphen_values = true)]
        events: String,
    },
    /// Add a note (replacing one at the same tick and note), e.g.
    /// `4s clip add bass 36 C3 --len 6`.
    Add {
        instrument: String,
        tick: u32,
        note: String,
        /// Ticks (default: one step, 24).
        #[arg(long)]
        len: Option<u32>,
        /// 1..127 (default 89; 121 and up is accented).
        #[arg(long)]
        vel: Option<u8>,
    },
    /// Remove the note at a tick.
    Rm { instrument: String, tick: u32, note: String },
    /// Remove and add notes in one edit (one undo step), e.g.
    /// `4s clip update bass --rm "24:D#2" --add "36:C3:6:100"`.
    Update {
        instrument: String,
        /// `tick:note` tokens to remove.
        #[arg(long, allow_hyphen_values = true)]
        rm: Option<String>,
        /// `tick:note[:len[:vel]]` tokens to add.
        #[arg(long, allow_hyphen_values = true)]
        add: Option<String>,
    },
    /// Set a clip's length in steps (`auto` follows sequencer.length), e.g.
    /// `4s clip length bass 12` for a 12-step loop against a 16-step beat.
    Length { instrument: String, steps: String },
    /// Remove every note.
    Clear { instrument: String },
    /// Snap notes to a grid: 1/4, 1/8, 1/8t, 1/16 (default), 1/16t, 1/32,
    /// or `<n>t` ticks.
    Quantize {
        instrument: String,
        #[arg(default_value = "1/16")]
        grid: String,
        /// How far to move notes toward it: 0..1 or a percentage (default
        /// 100%).
        #[arg(long)]
        strength: Option<String>,
    },
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
    /// Delete a seat. In a project with no other saved seats, the host
    /// user's seat comes straight back: this machine's devices need one.
    Rm { name: String },
}

#[derive(Subcommand, Debug, Clone)]
enum CcCmd {
    /// Map a device's CC to a parameter, e.g. `4s cc map knobs 21 bass.cutoff`
    /// (`focus.cutoff` follows your focus).
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
        /// An endless encoder sending steps (1..63 up, 65..127 down).
        #[arg(long)]
        relative: bool,
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
        /// Endless encoders sending steps (1..63 up, 65..127 down).
        #[arg(long)]
        relative: bool,
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

/// A note by name (`C2`) or number (`36`).
fn note_arg(s: &str) -> Result<u8> {
    match s.parse::<u8>() {
        Ok(n) if n <= 127 => Ok(n),
        _ => parse_note(s).map_err(|e| anyhow!(e)),
    }
}

/// A grid in ticks: `1/16`, `1/8t` (triplets), `off` (`None`), `16` (1/16),
/// or `<n>t` ticks.
fn grid_arg(s: &str) -> Result<Option<u32>> {
    if let Ok(g) = parse_grid(s) {
        return Ok(g);
    }
    if let Some(t) = s.strip_suffix('t').filter(|t| !t.contains('/')) {
        return t.parse().map(Some).map_err(|_| anyhow!("invalid grid '{s}'"));
    }
    // Any other `1/<n>` (or bare `<n>`) that divides a bar evenly.
    let d: u32 = s
        .trim_start_matches("1/")
        .parse()
        .map_err(|_| anyhow!("grid is 1/4, 1/8, 1/8t, 1/16, 1/16t, 1/32, 1/<n>, <n>t, or off"))?;
    if d == 0 || TICKS_PER_BAR % d != 0 {
        bail!("grid 1/{d} is not a whole number of ticks");
    }
    Ok(Some(TICKS_PER_BAR / d))
}

/// A song position: bar (`3`) or bar.beat (`3.2`), from 1, as ticks.
fn pos_arg(s: &str) -> Result<u32> {
    let bad = || anyhow!("invalid position '{s}' (a bar, or bar.beat, from 1: `3`, `3.2`)");
    let (bar, beat) = match s.split_once('.') {
        Some((b, t)) => (b.parse::<u32>().map_err(|_| bad())?, t.parse::<u32>().map_err(|_| bad())?),
        None => (s.parse::<u32>().map_err(|_| bad())?, 1),
    };
    if bar == 0 || !(1..=4).contains(&beat) {
        return Err(bad());
    }
    Ok((bar - 1) * TICKS_PER_BAR + (beat - 1) * PPQ)
}

/// A song tick as bar.beat (`3.2`), with any leftover ticks (`3.2+12`).
fn fmt_pos(tick: u32) -> String {
    let (bar, beat, rest) = (tick / TICKS_PER_BAR + 1, tick % TICKS_PER_BAR / PPQ + 1, tick % PPQ);
    match (beat, rest) {
        (1, 0) => format!("{bar}"),
        (_, 0) => format!("{bar}.{beat}"),
        _ => format!("{bar}.{beat}+{rest}"),
    }
}

/// A length in ticks as bars (`4 bars`), else ticks.
fn fmt_bars(ticks: u32) -> String {
    if ticks % TICKS_PER_BAR == 0 {
        let n = ticks / TICKS_PER_BAR;
        format!("{n} bar{}", if n == 1 { "" } else { "s" })
    } else {
        format!("{ticks} ticks")
    }
}

fn print_track(t: &TrackInfo) {
    let clips: Vec<String> = t
        .clips
        .iter()
        .map(|c| {
            let sel = if c.id == t.selected { "*" } else { "" };
            if c.name == c.id.to_string() { format!("{sel}{}", c.id) } else { format!("{sel}{} \"{}\"", c.id, c.name) }
        })
        .collect();
    println!("{}: clips {}", t.instrument, clips.join(", "));
    for p in &t.arrangement {
        let name = t.clips.iter().find(|c| c.id == p.clip).map(|c| c.name.as_str()).unwrap_or("?");
        let offset = if p.offset > 0 { format!(" from tick {}", p.offset) } else { String::new() };
        println!("  bar {:<7} clip {} ({name}), {}{offset}", fmt_pos(p.start), p.clip, fmt_bars(p.length));
    }
}

fn print_song(s: &SongInfo, snap: Option<&Snapshot>) {
    if let Some(snap) = snap {
        let p = |k: &str| snap.params.get(k).copied().unwrap_or(0.0);
        let mode = if p("song.mode") >= 0.5 { "song" } else { "pattern" };
        let looping = match p("song.loop") as i32 {
            0 => "off".to_string(),
            1 => "the song".to_string(),
            _ => format!("bars {} to {}", p("song.loop_start") as u32 + 1, p("song.loop_end") as u32),
        };
        println!("mode {mode}, loop {looping}, plays from bar {}", fmt_pos(snap.transport.start));
    }
    println!("song: {}", if s.length == 0 { "empty".to_string() } else { fmt_bars(s.length.div_ceil(TICKS_PER_BAR) * TICKS_PER_BAR) });
    for t in &s.tracks {
        print_track(t);
    }
}

/// `secs:note[:dur[:vel]]` tokens: notes to play into a render.
fn parse_input(s: &str) -> Result<Vec<RenderNote>> {
    s.split_whitespace()
        .map(|tok| {
            let parts: Vec<&str> = tok.split(':').collect();
            if !(2..=4).contains(&parts.len()) {
                bail!("invalid input note '{tok}' (expected secs:note[:dur[:vel]], e.g. 0.5:C2:0.1:100)");
            }
            let secs = |p: &str| p.parse::<f64>().map_err(|_| anyhow!("invalid seconds in '{tok}'"));
            Ok(RenderNote {
                time: secs(parts[0])?,
                note: note_arg(parts[1])?,
                duration: parts.get(2).map(|p| secs(p)).transpose()?,
                velocity: parts.get(3).map(|p| p.parse::<u8>().map_err(|_| anyhow!("invalid velocity in '{tok}'"))).transpose()?,
            })
        })
        .collect()
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
        Cmd::Record { off, show, to, settings } => {
            let arm = if *off { Some(false) } else if *show { None } else { Some(true) };
            vec![Request::TransportRecord(settings.params(arm, to.clone())?)]
        }
        Cmd::Metronome { state, level } => {
            let mut reqs = Vec::new();
            if let Some(s) = state {
                reqs.push(Request::ParamSet(ParamSetParams { path: "metronome.on".into(), value: parse_value(s)? }));
            }
            if let Some(l) = level {
                reqs.push(Request::ParamSet(ParamSetParams { path: "metronome.level".into(), value: parse_value(l)? }));
            }
            reqs.push(Request::StateGet(e));
            reqs
        }
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
            ChannelCmd::Move { n, position } => {
                vec![Request::ChannelMove(ChannelMoveParams { n: *n, position: *position })]
            }
        },
        Cmd::Route { source, channel, swap } => {
            let channel = match channel.trim().to_ascii_lowercase().as_str() {
                "none" | "-" => None,
                n => Some(n.parse::<u32>().map_err(|_| anyhow!("channel must be a number or `none`"))?),
            };
            vec![Request::RouteSet(RouteSetParams { source: source.clone(), channel, swap: *swap })]
        }
        Cmd::Mixer => vec![Request::StateGet(e)],
        Cmd::Watch { types, .. } => vec![
            Request::EventsSubscribe(SubscribeParams {
                types: if types.is_empty() { None } else { Some(types.clone()) },
            }),
            Request::EventsUnsubscribe(e),
        ],
        Cmd::Render { bars, tail, out, sample_rate, from, metronome, input, record, to, settings } => {
            let record = (*record || to.is_some()).then(|| settings.params(None, to.clone())).transpose()?;
            vec![Request::RenderOffline(RenderParams {
                bars: Some(*bars),
                tail: *tail,
                path: out.clone(),
                sample_rate: *sample_rate,
                from: from.as_deref().map(pos_arg).transpose()?,
                metronome: metronome.then_some(true),
                input: input.as_deref().map(parse_input).transpose()?,
                record,
            })]
        }
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::New => vec![Request::ProjectNew(e)],
            ProjectCmd::Save { path } => vec![Request::ProjectSave(ProjectSaveParams { path: path.clone() })],
            ProjectCmd::Load { path } => vec![Request::ProjectLoad(ProjectLoadParams { path: path.clone() })],
            ProjectCmd::Import { file } => {
                let path = if file.is_dir() { file.join(PROJECT_FILE_NAME) } else { file.clone() };
                let json = std::fs::read_to_string(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;
                let file = parse_project(&json).map_err(|e| anyhow!("{}: {e}", path.display()))?;
                vec![Request::ProjectImport(ProjectImportParams { file })]
            }
            ProjectCmd::List => vec![Request::ProjectList(e)],
            ProjectCmd::Reveal { .. } => vec![Request::StateGet(e)],
        },
        Cmd::Midi { cmd } => match cmd {
            MidiCmd::Ports => vec![Request::MidiPorts(e)],
            MidiCmd::Connect { input, output, name, profile } => vec![Request::MidiConnect(MidiConnectParams {
                input: input.clone(),
                output: output.clone(),
                name: name.clone(),
                profile: profile.map(device_profile),
            })],
            MidiCmd::Disconnect { input } => {
                vec![Request::MidiDisconnect(MidiDisconnectParams { input: input.clone() })]
            }
            MidiCmd::Rename { device, name } => {
                vec![Request::MidiRename(MidiRenameParams { device: device.clone(), name: name.clone() })]
            }
            MidiCmd::Seat { seat } => vec![Request::MidiSetSeat(MidiSetSeatParams { seat: seat.clone() })],
            MidiCmd::Models => vec![Request::MidiModels(e)],
            MidiCmd::Layout { apply: false, .. } => vec![Request::MidiPorts(e.clone()), Request::MidiModels(e)],
            MidiCmd::Layout { device, apply: true } => {
                vec![Request::SeatApplyLayout(SeatApplyLayoutParams {
                    seat: None,
                    device: device.clone(),
                    model: None,
                    ports: vec![],
                })]
            }
            MidiCmd::Send { device, bytes, profile } => vec![Request::MidiInput(MidiInputParams {
                device: device.clone(),
                data: hex_bytes(bytes)?,
                seat: None,
                profile: profile.map(device_profile),
                model: None,
                role: None,
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
        Cmd::Clip { clip, cmd } => vec![match cmd.clone().unwrap_or(ClipCmd::Show { instrument: None }) {
            ClipCmd::Show { instrument } => Request::ClipGet(ClipGetParams { instrument, clip: *clip }),
            ClipCmd::List { .. } => Request::SongGet(e),
            ClipCmd::New { instrument, name, steps, keep } => Request::ClipNew(ClipNewParams {
                instrument,
                name,
                length: steps.map(|s| s * TICKS_PER_STEP),
                select: Some(!keep),
            }),
            ClipCmd::Dup { instrument } => Request::ClipDuplicate(ClipRefParams { instrument, clip: *clip }),
            ClipCmd::Rename { instrument, name } => {
                Request::ClipRename(ClipRenameParams { instrument: Some(instrument), clip: *clip, name })
            }
            ClipCmd::Del { instrument } => Request::ClipDelete(ClipRefParams { instrument: Some(instrument), clip: *clip }),
            ClipCmd::Select { instrument, id } => {
                Request::ClipSelect(ClipRefParams { instrument: Some(instrument), clip: Some(id) })
            }
            ClipCmd::Set { instrument, events } => Request::ClipSet(ClipEventsParams {
                instrument: Some(instrument),
                clip: *clip,
                events: parse_events(&events).map_err(|e| anyhow!(e))?,
            }),
            ClipCmd::Add { instrument, tick, note, len, vel } => Request::ClipAdd(ClipEventsParams {
                instrument: Some(instrument),
                clip: *clip,
                events: vec![ClipEvent {
                    tick,
                    note: note_arg(&note)?,
                    len: len.unwrap_or(TICKS_PER_STEP),
                    velocity: vel.unwrap_or(VEL_ON),
                }],
            }),
            ClipCmd::Rm { instrument, tick, note } => Request::ClipRemove(ClipRemoveParams {
                instrument: Some(instrument),
                clip: *clip,
                events: vec![EventKey { tick, note: note_arg(&note)? }],
            }),
            ClipCmd::Update { instrument, rm, add } => Request::ClipUpdate(ClipUpdateParams {
                instrument: Some(instrument),
                clip: *clip,
                remove: rm
                    .as_deref()
                    .unwrap_or_default()
                    .split_whitespace()
                    .map(|tok| match tok.split_once(':') {
                        Some((t, n)) => Ok(EventKey {
                            tick: t.parse().map_err(|_| anyhow!("invalid tick in '{tok}'"))?,
                            note: note_arg(n)?,
                        }),
                        None => bail!("invalid event key '{tok}' (expected tick:note, e.g. 24:D#2)"),
                    })
                    .collect::<Result<_>>()?,
                add: add.as_deref().map(parse_events).transpose().map_err(|e| anyhow!(e))?.unwrap_or_default(),
                recorded: false,
            }),
            ClipCmd::Length { instrument, steps } => Request::ClipLength(ClipLengthParams {
                instrument: Some(instrument),
                clip: *clip,
                length: match steps.as_str() {
                    "auto" => None,
                    n => match n.parse::<u32>() {
                        Ok(steps) if (1..=MAX_STEPS as u32).contains(&steps) => Some(steps * TICKS_PER_STEP),
                        _ => bail!("length is 1..{MAX_STEPS} steps or `auto`"),
                    },
                },
            }),
            ClipCmd::Clear { instrument } => Request::ClipClear(ClipGetParams { instrument: Some(instrument), clip: *clip }),
            ClipCmd::Quantize { instrument, grid, strength } => Request::ClipQuantize(ClipQuantizeParams {
                instrument: Some(instrument),
                clip: *clip,
                grid: grid_arg(&grid)?.ok_or_else(|| anyhow!("quantize needs a grid"))?,
                strength: strength.as_deref().map(|v| parse_value(v).map(|v| v as f32)).transpose()?,
            }),
        }],
        Cmd::Song { cmd } => match cmd.clone().unwrap_or(SongCmd::Show) {
            SongCmd::Show => vec![Request::StateGet(e.clone()), Request::SongGet(e)],
            SongCmd::Place { instrument, at, bars, offset, clip } => vec![Request::SongPlace(SongPlaceParams {
                instrument: Some(instrument),
                clip,
                start: pos_arg(&at)?,
                length: bars.map(|b| (b * TICKS_PER_BAR as f64).round() as u32),
                offset,
            })],
            SongCmd::Rm { instrument, at } => {
                vec![Request::SongRemove(SongRemoveParams { instrument: Some(instrument), start: pos_arg(&at)? })]
            }
            SongCmd::Mv { instrument, at, to } => vec![Request::SongMove(SongMoveParams {
                instrument: Some(instrument),
                start: pos_arg(&at)?,
                to: pos_arg(&to)?,
            })],
            SongCmd::Mode { mode } => {
                let value = match mode.as_str() {
                    "song" | "on" => 1.0,
                    "pattern" | "off" => 0.0,
                    _ => bail!("mode is `song` or `pattern`"),
                };
                vec![
                    Request::ParamSet(ParamSetParams { path: "song.mode".into(), value }),
                    Request::StateGet(e.clone()),
                    Request::SongGet(e),
                ]
            }
            SongCmd::Loop { what, to } => {
                let set = |path: &str, value: f64| Request::ParamSet(ParamSetParams { path: path.into(), value });
                let mut reqs = match (what.as_str(), to) {
                    ("off", None) => vec![set("song.loop", 0.0)],
                    ("song", None) => vec![set("song.loop", 1.0)],
                    (from, Some(to)) => {
                        let from: u32 = from.parse().map_err(|_| anyhow!("loop `off`, `song`, or <from bar> <to bar>"))?;
                        if from == 0 || to < from {
                            bail!("bars count from 1, and the loop ends at or after it starts");
                        }
                        vec![set("song.loop_start", (from - 1) as f64), set("song.loop_end", to as f64), set("song.loop", 2.0)]
                    }
                    _ => bail!("loop `off`, `song`, or <from bar> <to bar>"),
                };
                reqs.push(Request::StateGet(e.clone()));
                reqs.push(Request::SongGet(e));
                reqs
            }
        },
        Cmd::Locate { at } => vec![Request::TransportLocate(LocateParams { tick: pos_arg(at)? })],
        Cmd::Batch { requests } => {
            let text = if requests == "-" { std::io::read_to_string(std::io::stdin())? } else { requests.clone() };
            let requests: Vec<serde_json::Value> =
                serde_json::from_str(&text).map_err(|e| anyhow!("batch: a JSON array of requests: {e}"))?;
            vec![Request::Batch(BatchParams { requests })]
        }
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
        Cmd::Bind { device, channel, notes, transpose, to, remap } => {
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
                    remap: remap.as_ref().map(|r| r.iter().map(|n| note_arg(n)).collect::<Result<Vec<_>>>()).transpose()?,
                    target: to.clone(),
                },
            })]
        }
        Cmd::Unbind { n } => vec![Request::SeatUnbind(SeatUnbindParams { seat: None, index: one_based(*n, "binding")? })],
        Cmd::Cc { cmd } => vec![match cmd {
            CcCmd::Map { device, cc, param, channel, no_pickup, relative } => Request::SeatMapCc(SeatMapCcParams {
                seat: None,
                map: CcMap {
                    device: device.clone(),
                    channel: *channel,
                    cc: *cc,
                    param: param.clone(),
                    pickup: !no_pickup,
                    mode: if *relative { CcMode::Relative } else { CcMode::Absolute },
                },
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
            KnobsCmd::Follow { device, ccs, channel, no_pickup, relative } => {
                Request::SeatFollowKnobs(SeatFollowKnobsParams {
                    seat: None,
                    follow: KnobFollow {
                        device: device.clone(),
                        channel: *channel,
                        ccs: ccs.clone(),
                        pickup: !no_pickup,
                        mode: if *relative { CcMode::Relative } else { CcMode::Absolute },
                    },
                })
            }
        }],
        Cmd::Daemon { cmd } => match cmd {
            DaemonCmd::Status => vec![Request::DaemonInfo(e.clone()), Request::EngineStatus(e)],
            DaemonCmd::Stop { .. } => vec![Request::DaemonShutdown(e)],
            // Local process management; no RPC.
            DaemonCmd::Start(_) | DaemonCmd::Restart(_) | DaemonCmd::Logs { .. } => vec![],
        },
        Cmd::Undo => vec![Request::HistoryUndo(e)],
        Cmd::Redo => vec![Request::HistoryRedo(e)],
        Cmd::History => vec![Request::HistoryGet(e)],
        Cmd::Journal { cmd: Some(JournalCmd::Export { .. }), .. } => vec![Request::JournalExport(e)],
        // Client-side: see `replay::run`.
        Cmd::Journal { cmd: Some(JournalCmd::Replay { .. }), .. } => vec![],
        Cmd::Journal { follow: true, .. } => vec![
            Request::EventsSubscribe(SubscribeParams { types: Some(vec!["journal".into()]) }),
            Request::EventsUnsubscribe(e),
        ],
        Cmd::Journal { since, for_user, limit, .. } => vec![Request::JournalGet(JournalGetParams {
            since: *since,
            limit: Some(*limit),
            user: for_user.clone(),
        })],
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
    print!("record: ");
    print_record(&s.record);
    println!(
        "project: {}{}",
        s.project.path.as_deref().unwrap_or("(unsaved)"),
        if s.project.dirty { " *modified*" } else { "" }
    );
}

fn print_record(r: &RecordState) {
    let quantize = match r.quantize {
        Some(_) => format!("quantize {} at {:.0}%", format_grid(r.quantize), r.strength * 100.0),
        None => "no quantize".into(),
    };
    let mode = match r.mode {
        RecordMode::Overdub => "overdub",
        RecordMode::Replace => "replace",
    };
    let bars = if r.count_in == 1 { "bar" } else { "bars" };
    let offset = if r.offset_ms > 0.0 { format!(", {} ms earlier", r.offset_ms) } else { String::new() };
    let settings = format!("{mode}, {quantize}, count-in {} {bars}{offset}", r.count_in);
    match &r.instrument {
        Some(i) if r.recording => {
            println!("recording into {i} for {} ({settings})", r.user.as_deref().unwrap_or("?"))
        }
        _ => println!("not recording (next take: {settings})"),
    }
}

fn print_clip(c: &Clip) {
    let length = match c.length {
        Some(l) if l % TICKS_PER_STEP == 0 => format!("{} steps", l / TICKS_PER_STEP),
        Some(l) => format!("{l} ticks"),
        None => "sequencer.length".into(),
    };
    let name = if c.name == c.id.to_string() { String::new() } else { format!(" \"{}\"", c.name) };
    println!("{} clip {}{name}: {} notes, length {length}", c.instrument, c.id, c.events.len());
    for e in &c.events {
        let (step, sub) = (e.tick / TICKS_PER_STEP + 1, e.tick % TICKS_PER_STEP);
        let at = if sub == 0 { format!("step {step}") } else { format!("step {step} +{sub}") };
        let name = if (NOTE_MIN..=NOTE_MAX).contains(&e.note) { note_name(e.note) } else { e.note.to_string() };
        println!("  {:>5}  {at:<12} {name:<4} len {:<3} vel {}", e.tick, e.len, e.velocity);
    }
    println!("  {}", format_events(&c.events));
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
    print_config(&s.config);
    for d in &s.defaults {
        println!("  default layout: {d}");
    }
}

/// Bindings, CC maps, following knobs, and pitch bend of a seat or layout.
fn print_config(c: &SeatConfig) {
    for (i, b) in c.bindings.iter().enumerate() {
        let ch = b.channel.map(|c| format!("ch {c}")).unwrap_or("any ch".into());
        let tr = if b.transpose != 0 { format!(" {:+} st", b.transpose) } else { String::new() };
        let remap = match &b.remap {
            Some(r) => format!(" as {}", r.iter().map(|n| note_name(*n)).collect::<Vec<_>>().join(",")),
            None => String::new(),
        };
        println!("  bind {}: {} {ch} {}{tr}{remap} -> {}", i + 1, b.device, fmt_note_range(b), b.target);
    }
    let mode = |m: CcMode| if m == CcMode::Relative { " (relative)" } else { "" };
    for m in &c.cc {
        let ch = m.channel.map(|c| format!("ch {c}")).unwrap_or("any ch".into());
        let pickup = if m.pickup || m.mode == CcMode::Relative { "" } else { " (no pickup)" };
        println!("  cc: {} {ch} cc {} -> {}{pickup}{}", m.device, m.cc, m.param, mode(m.mode));
    }
    for k in &c.knobs {
        let ccs: Vec<String> = k.ccs.iter().map(|c| c.to_string()).collect();
        println!("  knobs: {} cc {} follow focus{}", k.device, ccs.join(" "), mode(k.mode));
    }
    if let Some(t) = &c.pitch_bend {
        println!("  pitch bend -> {t}");
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
        Event::Record { state } => match &state.instrument {
            Some(i) if state.recording => format!("recording into {i}"),
            _ => "not recording".into(),
        },
        Event::Playhead { step, tick, time } => format!("step {} (bar {}) @ {time:.3}s", step + 1, fmt_pos(*tick)),
        Event::Track { track } => format!("{}: {} clips, {} placed", track.instrument, track.clips.len(), track.arrangement.len()),
        Event::ClipDeleted { instrument, id } => format!("{instrument} clip {id} deleted"),
        Event::Located { tick } => format!("song plays from bar {}", fmt_pos(*tick)),
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
        Event::Journal { entry } => {
            println!("{}", journal_line(entry));
            return;
        }
        other => serde_json::to_string(other).unwrap(),
    };
    println!("[{}] {:<10} {:<15} {body}", e.seq, e.origin, e.event.type_name());
}

/// `seq  time  user  origin  method params -> changed keys`, plus what an
/// undo reverts and any error.
fn journal_line(e: &JournalEntry) -> String {
    let ms = (e.time * 1000.0).round() as u64 % 86_400_000;
    let time = format!("{:02}:{:02}:{:02}.{:03}Z", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    let params = match &e.params {
        Value::Object(m) if m.is_empty() => String::new(),
        Value::Null => String::new(),
        p => {
            let p = p.to_string();
            match p.char_indices().nth(100) {
                Some((i, _)) => format!(" {}...", &p[..i]),
                None => format!(" {p}"),
            }
        }
    };
    let keys: Vec<&str> = e.changes.iter().map(|c| c.key.as_str()).collect();
    let shown = if keys.len() > 6 { format!("{} (+{} more)", keys[..6].join(" "), keys.len() - 6) } else { keys.join(" ") };
    let mut line = format!("{:>5}  {time}  {}  {}  {}{params}", e.seq, e.user, e.origin, e.method);
    if !keys.is_empty() {
        line.push_str(&format!(" -> {shown}"));
    }
    if let Some(r) = e.reverts {
        line.push_str(&format!("  (reverts #{r})"));
    }
    if let Some(err) = &e.error {
        line.push_str(&format!("  error: {err}"));
    }
    line
}

fn print_history(h: &HistoryInfo) {
    println!("user: {}", h.user);
    println!("undo: {}", if h.undo.is_empty() { "(empty)".into() } else { h.undo.join(" | ") });
    println!("redo: {}", if h.redo.is_empty() { "(empty)".into() } else { h.redo.join(" | ") });
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
        Cmd::Record { .. } => print_record(&serde_json::from_value(last)?),
        Cmd::Metronome { .. } => {
            let s: Snapshot = serde_json::from_value(last)?;
            let on = s.params.get("metronome.on").copied().unwrap_or(0.0) >= 0.5;
            let level = s.params.get("metronome.level").copied().unwrap_or(0.0);
            println!("metronome {} (level {:.0}%)", if on { "on" } else { "off" }, level * 100.0);
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
        Cmd::Instrument { .. }
        | Cmd::Route { .. }
        | Cmd::Channel { cmd: ChannelCmd::Rm { .. } | ChannelCmd::Move { .. } } => {
            print_graph(&serde_json::from_value(last)?);
        }
        Cmd::Channel { .. } => {
            let c: ChannelInfo = serde_json::from_value(last)?;
            println!("ch {} {}", c.n, c.name);
        }
        Cmd::Mixer => print_mixer(&serde_json::from_value(last)?),
        Cmd::Undo | Cmd::Redo => {
            let r: HistoryStepResult = serde_json::from_value(last)?;
            let verb = if matches!(cmd, Cmd::Undo) { "undid" } else { "redid" };
            let what = if matches!(cmd, Cmd::Undo) { "undo" } else { "redo" };
            match r.label {
                Some(l) if r.changed.is_empty() => println!("could not {what}: {l}"),
                Some(l) => println!("{verb}: {l}"),
                None => println!("nothing to {what}"),
            }
            if !r.skipped.is_empty() {
                println!("skipped: {} (changed by someone else)", r.skipped.join(" "));
            }
        }
        Cmd::History => print_history(&serde_json::from_value(last)?),
        Cmd::Journal { cmd: Some(JournalCmd::Export { out, format }), .. } => {
            let r: Recording = serde_json::from_value(last)?;
            let text = match format {
                ExportFormat::Json => serde_json::to_string_pretty(&r)? + "\n",
                ExportFormat::Sh => replay::to_script(&r),
            };
            match out {
                Some(path) => {
                    std::fs::write(path, text).map_err(|e| anyhow!("write {}: {e}", path.display()))?;
                    println!("wrote {} ({} entries, digest {})", path.display(), r.entries.len(), r.digest);
                }
                None => print!("{text}"),
            }
        }
        Cmd::Journal { .. } => {
            let r: JournalGetResult = serde_json::from_value(last)?;
            for e in &r.entries {
                println!("{}", journal_line(e));
            }
        }
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
            for c in &r.recorded {
                print!("recorded ");
                print_clip(c);
            }
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
        Cmd::Midi { cmd: MidiCmd::Models } => {
            let r: MidiModelsResult = serde_json::from_value(last)?;
            for m in &r.models {
                let ports: Vec<String> = m
                    .ports
                    .iter()
                    .filter(|(_, n)| *n != "ignore")
                    .map(|(p, n)| if p.is_empty() { n.clone() } else { format!("{p} as {n}") })
                    .collect();
                println!("{:<18} {}: ports matching \"{}\": {}", m.id, m.label, m.matches, ports.join(", "));
            }
        }
        Cmd::Midi { cmd: MidiCmd::Layout { device, apply: false } } => {
            let ports: MidiPortsResult = serde_json::from_value(results[0].clone())?;
            let models: MidiModelsResult = serde_json::from_value(last)?;
            let c = ports.connections.iter().find(|c| &c.device == device).ok_or_else(|| anyhow!("no connected device '{device}'"))?;
            let m = c
                .model
                .as_ref()
                .and_then(|id| models.models.iter().find(|m| &m.id == id))
                .ok_or_else(|| anyhow!("'{device}' is not of a known model (see 4s midi models)"))?;
            println!("{} ({}) default layout; this port is `{}`:", m.label, m.id, c.role.as_deref().unwrap_or("-"));
            print_config(&m.layout);
            println!("used while your seat has no bindings for its devices; `--apply` copies it into your seat");
        }
        Cmd::Midi { cmd: MidiCmd::Layout { apply: true, .. } } => print_seat(&serde_json::from_value(last)?, false),
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
                let model = c.model.as_deref().map(|m| format!(" model {m}")).unwrap_or_default();
                println!("  {} as {} ({:?}){model} out: {}", c.input, c.device, c.profile, c.output.as_deref().unwrap_or("-"));
            }
            let pinned = if r.pinned_seat.is_some() { " (pinned)" } else { "" };
            println!("devices play in seat: {}{pinned}", r.seat);
        }
        Cmd::Controller { .. } => print_leds(&serde_json::from_value(last)?),
        Cmd::Clip { cmd: Some(ClipCmd::List { instrument }), .. } => {
            let song: SongInfo = serde_json::from_value(last)?;
            for t in song.tracks.iter().filter(|t| instrument.as_deref().is_none_or(|w| w == t.instrument)) {
                print_track(t);
            }
        }
        Cmd::Clip { cmd: Some(ClipCmd::Del { .. } | ClipCmd::Select { .. }), .. } => {
            print_track(&serde_json::from_value(last)?)
        }
        Cmd::Clip { .. } => print_clip(&serde_json::from_value(last)?),
        Cmd::Song { cmd: Some(SongCmd::Place { .. } | SongCmd::Rm { .. } | SongCmd::Mv { .. }) } => {
            print_track(&serde_json::from_value(last)?)
        }
        Cmd::Song { .. } => {
            let snap: Snapshot = serde_json::from_value(results[results.len() - 2].clone())?;
            print_song(&serde_json::from_value(last)?, Some(&snap));
        }
        Cmd::Locate { .. } => {
            let t: TransportState = serde_json::from_value(last)?;
            println!("song plays from bar {}", fmt_pos(t.start));
        }
        Cmd::Batch { .. } => {
            let r: BatchResult = serde_json::from_value(last)?;
            println!("{} requests applied", r.results.len());
        }
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
    Seating { user: cli.user(), seat: cli.seat.clone(), auto: !cli.no_seat && !cli.new_seat }
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
            let graceful = match Client::connect(&info.url, cli.token.clone(), "cli", &Seating::unseated(cli.user())).await {
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
                if let Ok(mut c) = Client::connect(&info.url, cli.token.clone(), "cli", &Seating::unseated(cli.user())).await {
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
    let session = match Client::connect(&old.url, cli.token.clone(), "cli", &Seating::unseated(cli.user())).await {
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
    if let Cmd::Journal { cmd: Some(JournalCmd::Replay { file, realtime, segment, force, accept }), .. } = &cli.cmd {
        return replay::run(replay::Options {
            url: &url,
            token: cli.token.clone(),
            file,
            realtime: *realtime,
            segment: *segment,
            force: *force,
            accept: *accept,
        })
        .await;
    }
    let mut client = connect(&cli, &url).await?;

    let stream_count = match &cli.cmd {
        Cmd::Watch { count, .. } => Some(*count),
        Cmd::Midi { cmd: MidiCmd::Monitor { count } } => Some(*count),
        Cmd::Journal { cmd: None, follow: true, .. } => Some(None),
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
            "channel add --name Hat", "channel rm 2", "channel rename 1 Kit", "channel move 2 1", "route drums.closed_hat 2",
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
            "daemon status", "daemon stop", "undo", "redo", "history", "journal",
            "clip", "clip show bass", "clip set bass 0:C2:12", "clip add bass 36 C3 --len 6", "clip rm bass 36 C3",
            "clip length bass 12", "clip clear bass", "clip quantize bass 1/8", "clip update bass --rm 0:C2", "clip new bass", "clip dup bass",
            "clip rename bass B --clip 2", "clip del bass --clip 2", "clip select bass 1", "clip list", "song",
            "song place drums --at 1 --bars 4", "song rm drums 1", "song mv drums 1 3", "locate 5",
            "batch []", "record", "record --off",
            "metronome on", "render --record --input 0.5:C2",
            "midi models", "midi layout mpk --apply",
        ];
        let mut covered: BTreeSet<&str> = commands.iter().flat_map(|c| methods_for(c)).collect();
        let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/../protocol/fixtures/project-v2.json");
        covered.extend(methods_for(&format!("project import {fixture}")));
        covered.extend(methods_for("journal export"));
        covered.insert("session.hello"); // sent by every command on connect
        let all: BTreeSet<&str> = METHODS.iter().copied().collect();
        let missing: Vec<_> = all.difference(&covered).collect();
        assert!(missing.is_empty(), "RPC methods without a CLI command: {missing:?}");
    }
}
