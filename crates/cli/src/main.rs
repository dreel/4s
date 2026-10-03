//! 4s: command line client for the 4S daemon. Every UI action has a CLI
//! equivalent; see docs/api-parity.md.
//!
//! User-facing numbering in the CLI is 1-based (steps, rows, columns, knobs,
//! pages), matching how musicians count. The RPC API is 0-based.

mod client;

use anyhow::{Result, anyhow, bail};
use clap::{Parser, Subcommand, ValueEnum};
use client::Client;
use fours_protocol::*;
use serde_json::Value;

#[derive(Parser)]
#[command(name = "4s", version, about = "Control the 4S daemon (4sd)")]
struct Cli {
    /// Daemon URL.
    #[arg(long, env = "FOURS_URL", default_value = concat!("ws://", "127.0.0.1:4440"), global = true)]
    url: String,
    /// Auth token, if the daemon requires one.
    #[arg(long, env = "FOURS_TOKEN", global = true)]
    token: Option<String>,
    /// Print raw JSON results.
    #[arg(long, global = true)]
    json: bool,
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
    /// Read a parameter, e.g. `4s get mixer.3.volume`.
    Get { path: String },
    /// Set a parameter, e.g. `4s set mixer.3.volume 35%`. Accepts numbers,
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
    /// Show or edit the pattern.
    Pattern {
        #[command(subcommand)]
        cmd: Option<PatternCmd>,
    },
    /// Play a voice now.
    Trigger {
        voice: String,
        #[arg(long)]
        velocity: Option<f32>,
    },
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
    /// Call any RPC method with JSON params.
    Call { method: String, params: Option<String> },
    /// List all RPC methods.
    Methods,
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
}

#[derive(Subcommand, Debug, Clone)]
enum MidiCmd {
    /// List ports and connections.
    Ports,
    /// Connect a MIDI input as a device.
    Connect {
        input: String,
        #[arg(long)]
        output: Option<String>,
        #[arg(long, value_enum, default_value = "block")]
        kind: Kind,
    },
    /// Disconnect a MIDI input.
    Disconnect { input: String },
    /// Print raw incoming MIDI (for discovering controller mappings).
    Monitor {
        #[arg(long)]
        count: Option<usize>,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum Kind {
    Block,
    Drums,
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
    /// Change knob mode, page (1-based), or follow.
    Mode {
        #[arg(long, value_enum)]
        knobs: Option<KnobArg>,
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

#[derive(ValueEnum, Debug, Clone, Copy)]
enum KnobArg {
    Volume,
    Tune,
    Decay,
    Tone,
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
        Cmd::Pattern { cmd } => match cmd.clone().unwrap_or(PatternCmd::Show { voice: None }) {
            PatternCmd::Show { voice: v } => vec![Request::PatternGet(PatternGetParams {
                voice: v.as_deref().map(voice).transpose()?,
            })],
            PatternCmd::Set { voice: v, steps } => vec![Request::PatternSet(PatternSetParams {
                voice: voice(&v)?,
                steps: parse_steps(&steps).map_err(|e| anyhow!(e))?,
            })],
            PatternCmd::Step { voice: v, step, level } => vec![Request::PatternSetStep(SetStepParams {
                voice: voice(&v)?,
                step: step_index(step)?,
                level: match level {
                    Level::Off => STEP_OFF,
                    Level::On => STEP_ON,
                    Level::Accent => STEP_ACCENT,
                },
            })],
            PatternCmd::Toggle { voice: v, step } => vec![Request::PatternToggleStep(ToggleStepParams {
                voice: voice(&v)?,
                step: step_index(step)?,
            })],
            PatternCmd::Clear { voice: v } => vec![Request::PatternClear(PatternClearParams {
                voice: v.as_deref().map(voice).transpose()?,
            })],
        },
        Cmd::Trigger { voice: v, velocity } => {
            vec![Request::VoiceTrigger(TriggerParams { voice: voice(v)?, velocity: *velocity })]
        }
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
        },
        Cmd::Midi { cmd } => match cmd {
            MidiCmd::Ports => vec![Request::MidiPorts(e)],
            MidiCmd::Connect { input, output, kind } => vec![Request::MidiConnect(MidiConnectParams {
                input: input.clone(),
                output: output.clone(),
                kind: match kind {
                    Kind::Block => DeviceKind::LividBlock,
                    Kind::Drums => DeviceKind::GenericDrums,
                },
            })],
            MidiCmd::Disconnect { input } => {
                vec![Request::MidiDisconnect(MidiDisconnectParams { input: input.clone() })]
            }
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
            ControllerCmd::Mode { knobs, page, follow } => vec![Request::ControllerSetMode(ControllerModeParams {
                knob_mode: knobs.map(|k| match k {
                    KnobArg::Volume => KnobMode::Volume,
                    KnobArg::Tune => KnobMode::Tune,
                    KnobArg::Decay => KnobMode::Decay,
                    KnobArg::Tone => KnobMode::Tone,
                }),
                page: page.map(|p| one_based(p, "page")).transpose()?,
                follow,
            })],
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
    for t in &p.tracks {
        println!("{}", grid_line(t.voice.id(), &t.steps, p.length as usize));
    }
}

fn print_leds(c: &ControllerState) {
    let page = c.page + 1;
    println!(
        "knobs: {:?}  page: {page}  follow: {}  device: {}",
        c.knob_mode,
        c.follow,
        c.device.as_deref().unwrap_or("(virtual only)")
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
    for t in &s.pattern {
        let ch = t.voice.index() + 1;
        let vol = p(&format!("mixer.{ch}.volume"));
        let mut flags = String::new();
        if p(&format!("mixer.{ch}.mute")) >= 0.5 {
            flags.push_str(" M");
        }
        if p(&format!("mixer.{ch}.solo")) >= 0.5 {
            flags.push_str(" S");
        }
        println!("{}  vol {:>3.0}%{flags}", grid_line(t.voice.id(), &t.steps, length), vol * 100.0);
    }
    println!("master: {:.0}%", p("mixer.master.volume") * 100.0);
    print_status_lines(s);
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
        println!("midi: {} ({:?}) out: {}", m.input, m.kind, m.output.as_deref().unwrap_or("-"));
    }
    println!(
        "project: {}{}",
        s.project.path.as_deref().unwrap_or("(unsaved)"),
        if s.project.dirty { " *modified*" } else { "" }
    );
}

fn print_event(e: &EventEnvelope, json: bool) {
    if json {
        println!("{}", serde_json::to_string(e).unwrap());
        return;
    }
    let body = match &e.event {
        Event::ParamChanged { path, value } => format!("{path} = {value}"),
        Event::StepChanged { voice, step, level } => format!("{} step {} = {}", voice.id(), step + 1, level),
        Event::PatternChanged { voice, steps } => format!("{} = {}", voice.id(), format_steps(steps)),
        Event::Transport { playing } => if *playing { "playing".into() } else { "stopped".into() },
        Event::Playhead { step, time } => format!("step {} @ {time:.3}s", step + 1),
        Event::Trigger { voice, velocity, time } => format!("{} vel {velocity:.2} @ {time:.3}s", voice.id()),
        Event::Meters { tracks, master } => format!(
            "tracks [{}] master [{:.2} {:.2}]",
            tracks.iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>().join(" "),
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
        Cmd::Pattern { cmd } => match cmd {
            None | Some(PatternCmd::Show { .. }) | Some(PatternCmd::Clear { .. }) => {
                print_pattern(&serde_json::from_value(last)?)
            }
            Some(PatternCmd::Set { .. }) => {
                let t: TrackPattern = serde_json::from_value(last)?;
                println!("{}", grid_line(t.voice.id(), &t.steps, 16.max(t.steps.iter().rposition(|s| *s != 0).map(|i| i + 1).unwrap_or(0))));
            }
            Some(_) => {
                let s: StepResult = serde_json::from_value(last)?;
                println!("{} step {} = {}", s.voice.id(), s.step + 1, ["off", "on", "accent"][s.level as usize]);
            }
        },
        Cmd::Trigger { .. } => println!("ok"),
        Cmd::Render { .. } => {
            let r: RenderResult = serde_json::from_value(last)?;
            println!("wrote {} ({:.2}s @ {} Hz)", r.path, r.duration, r.sample_rate);
            println!("peak {:.3}  rms {:.4}", r.peak, r.rms);
            println!("triggers: {}  detected onsets: {}", r.triggers.len(), r.onsets.len());
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
                println!("  {} ({:?}) out: {}", c.input, c.kind, c.output.as_deref().unwrap_or("-"));
            }
        }
        Cmd::Controller { .. } => print_leds(&serde_json::from_value(last)?),
        _ => println!("{}", serde_json::to_string_pretty(&last)?),
    }
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

async fn run(cli: Cli) -> Result<()> {
    if let Cmd::Methods = cli.cmd {
        for (m, doc) in METHOD_DOCS {
            println!("{m:<22} {doc}");
        }
        return Ok(());
    }
    let reqs = plan(&cli.cmd)?;
    let mut client = Client::connect(&cli.url, cli.token.clone(), "cli").await?;

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
    for r in &reqs {
        results.push(client.call(r).await?);
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
            "status", "state", "params", "get mixer.1.volume", "set mixer.3.volume 35%",
            "play", "stop", "tempo 128", "pattern", "pattern show kick",
            "pattern set kick x---x---", "pattern set sd ----x---", "set mixer.1.pan -0.5", "pattern step kick 1 accent", "pattern toggle sd 5",
            "pattern clear", "trigger kick", "watch", "render --bars 2", "project new",
            "project save beat", "project load beat", "project list", "midi ports",
            "midi connect Block", "midi disconnect Block", "midi monitor", "controller",
            "controller press 1 1", "controller knob 1 50%", "controller mode --knobs tune",
        ];
        let mut covered: BTreeSet<&str> = commands.iter().flat_map(|c| methods_for(c)).collect();
        covered.insert("session.hello"); // sent by every command on connect
        let all: BTreeSet<&str> = METHODS.iter().copied().collect();
        let missing: Vec<_> = all.difference(&covered).collect();
        assert!(missing.is_empty(), "RPC methods without a CLI command: {missing:?}");
    }

    #[test]
    fn values_and_numbering() {
        assert_eq!(parse_value("35%").unwrap(), 0.35);
        assert_eq!(parse_value("on").unwrap(), 1.0);
        assert_eq!(parse_value("0.5").unwrap(), 0.5);
        assert!(parse_value("loud").is_err());
        let cli = Cli::try_parse_from(["4s", "pattern", "step", "kick", "1", "on"]).unwrap();
        match &plan(&cli.cmd).unwrap()[0] {
            Request::PatternSetStep(p) => assert_eq!(p.step, 0),
            other => panic!("{other:?}"),
        }
        let cli = Cli::try_parse_from(["4s", "pattern", "step", "kick", "0", "on"]).unwrap();
        assert!(plan(&cli.cmd).is_err());
    }
}
