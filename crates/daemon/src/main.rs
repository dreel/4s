//! 4sd: the 4S daemon. Hosts the audio engine, owns all state, talks MIDI, and
//! serves the JSON-RPC API that the CLI and UI use.
//!
//! Normally started in the background by `4s daemon start` or by the Electron
//! app (see docs/lifecycle.md); running it directly keeps it in the foreground.

mod audio;
mod controller;
mod core;
mod midi;
mod runtime;
mod server;

use anyhow::{Context, Result, bail};
use clap::Parser;
use fours_engine::{Engine, RtEngine};
use fours_protocol::{DaemonInfo, PROTOCOL_VERSION, Role};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "4sd", version, about = "4S daemon: audio engine, sequencer, MIDI, and RPC server")]
struct Args {
    /// Address to listen on. Use 127.0.0.1:0 for a random free port.
    #[arg(long, default_value = fours_protocol::DEFAULT_LISTEN)]
    listen: String,
    /// Run without an audio device (engine still runs in real time).
    #[arg(long)]
    no_audio: bool,
    /// Disable Livid Block auto-connect (manual `midi connect` still works).
    #[arg(long)]
    no_midi: bool,
    /// Data directory (projects, renders, controller maps, runtime file).
    /// Default: ~/.4s
    #[arg(long, env = "FOURS_DATA_DIR")]
    data_dir: Option<PathBuf>,
    /// Write logs to this file instead of stderr.
    #[arg(long)]
    log_file: Option<PathBuf>,
    /// Require clients to present this token in session.hello.
    #[arg(long, env = "FOURS_TOKEN")]
    token: Option<String>,
    /// Project bundle to load at startup.
    #[arg(long)]
    project: Option<String>,
    /// Exit when this process exits (used by an app that owns the daemon, so
    /// a crashed app never leaves an orphaned daemon behind).
    #[arg(long)]
    parent_pid: Option<u32>,
}

fn init_logging(log_file: Option<&PathBuf>) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match log_file {
        Some(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .with_context(|| format!("open log file {}", path.display()))?;
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(Mutex::new(file))
                .init();
        }
        None => tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init(),
    }
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(args.log_file.as_ref())?;
    if let Err(e) = run(args) {
        tracing::error!("{e:#}");
        return Err(e);
    }
    Ok(())
}

fn run(args: Args) -> Result<()> {
    let data_dir = args.data_dir.clone().unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".4s")
    });
    std::fs::create_dir_all(&data_dir)?;
    let data_dir = std::fs::canonicalize(&data_dir).unwrap_or(data_dir);
    let pid = std::process::id();

    // One daemon per data dir.
    if let Some(existing) = runtime::read(&data_dir)
        && existing.pid != pid
        && runtime::pid_alive(existing.pid)
    {
        bail!(
            "4sd already running for {} (pid {}, {}); stop it with `4s daemon stop`",
            data_dir.display(),
            existing.pid,
            existing.url
        );
    }

    // Bind before touching audio/MIDI so a port conflict fails fast.
    let listener = std::net::TcpListener::bind(&args.listen).with_context(|| {
        format!("cannot listen on {} (is another 4sd running? see `4s daemon status`)", args.listen)
    })?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    if !addr.ip().is_loopback() && args.token.is_none() {
        tracing::warn!("listening on a non-loopback address without --token; anyone on the network can control 4S");
    }

    // Audio: try the default device unless disabled; fall back to null.
    let (audio_status, link) = {
        let device = if args.no_audio { None } else { Some(audio::default_device_config()) };
        match device {
            Some(Ok((name, sr, _ch))) => {
                let (rt, link) = RtEngine::new(Engine::new(sr));
                match audio::start_device(rt) {
                    Ok(status) => {
                        tracing::info!("audio: {name} @ {sr} Hz");
                        (status, link)
                    }
                    Err(e) => null_audio(Some(format!("{e}"))),
                }
            }
            Some(Err(e)) => null_audio(Some(format!("{e}"))),
            None => null_audio(None),
        }
    };
    let fours_engine::EngineLink { commands, mut feedback, mut returns } = link;

    // Must precede any other MIDI use so hotplugged devices are seen.
    midi::start_device_watcher();
    let (midi_tx, midi_rx) = std::sync::mpsc::channel();
    let core = Arc::new(Mutex::new(core::Core::new(commands, midi_tx, data_dir.clone(), audio_status)));
    tracing::info!("data dir: {}", data_dir.display());

    if let Some(p) = &args.project {
        // A local command-line path: resolve against the working directory
        // when it exists there (RPC paths resolve under the data dir instead).
        let p = match std::fs::canonicalize(p) {
            Ok(abs) => abs.to_string_lossy().into_owned(),
            Err(_) => p.clone(),
        };
        let mut c = core.lock().unwrap();
        if let Err(e) = c.project_load(&p, "engine") {
            bail!("failed to load project {p}: {}", e.message);
        }
    }

    // Engine feedback -> state + events.
    {
        let core = core.clone();
        std::thread::Builder::new().name("4s-feedback".into()).spawn(move || {
            let mut batch = Vec::with_capacity(256);
            loop {
                std::thread::sleep(Duration::from_millis(4));
                while let Ok(f) = feedback.pop() {
                    batch.push(f);
                }
                // Removed instruments come back from the audio thread to be
                // dropped here.
                let mut returned = 0;
                while let Ok(instrument) = returns.pop() {
                    drop(instrument);
                    returned += 1;
                }
                if !batch.is_empty() || returned > 0 {
                    let mut c = core.lock().unwrap();
                    for _ in 0..returned {
                        c.instrument_returned();
                    }
                    for f in batch.drain(..) {
                        c.handle_feedback(f);
                    }
                }
            }
        })?;
    }

    // MIDI input -> core.
    {
        let core = core.clone();
        std::thread::Builder::new().name("4s-midi".into()).spawn(move || {
            for msg in midi_rx {
                core.lock().unwrap().handle_midi(msg);
            }
        })?;
    }

    // MIDI hotplug: drop unplugged devices, and auto-connect a Livid Block
    // when one appears (unless --no-midi).
    {
        let core = core.clone();
        let auto = !args.no_midi;
        std::thread::Builder::new().name("4s-midi-scan".into()).spawn(move || {
            loop {
                core.lock().unwrap().midi_autoconnect(auto);
                std::thread::sleep(Duration::from_secs(2));
            }
        })?;
    }

    let started_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let info = DaemonInfo {
        pid,
        version: env!("CARGO_PKG_VERSION").into(),
        protocol_version: PROTOCOL_VERSION,
        url: format!("ws://{addr}"),
        role: Role::Engine,
        data_dir: data_dir.to_string_lossy().into_owned(),
        log_file: args
            .log_file
            .as_ref()
            .map(|p| std::fs::canonicalize(p).unwrap_or(p.clone()).to_string_lossy().into_owned()),
        started_at,
        uptime: 0.0,
    };
    runtime::write(&data_dir, &info).context("write runtime file")?;

    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(async {
        let listener = tokio::net::TcpListener::from_std(listener)?;
        let shutdown = Arc::new(tokio::sync::Notify::new());
        if let Some(ppid) = args.parent_pid {
            let shutdown = shutdown.clone();
            std::thread::Builder::new().name("4s-parent-watch".into()).spawn(move || {
                while runtime::pid_alive(ppid) {
                    std::thread::sleep(Duration::from_millis(500));
                }
                tracing::info!("parent process {ppid} exited");
                shutdown.notify_one();
            })?;
        }
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        // Machine-readable line for tests and scripts.
        println!("4sd listening on {} (pid {pid})", info.url);
        std::io::stdout().flush()?;
        tracing::info!("listening on {} (pid {pid})", info.url);
        tokio::select! {
            _ = server::serve(core, listener, args.token.clone(), Arc::new(info.clone()), shutdown.clone()) => {}
            _ = shutdown.notified() => tracing::info!("shutting down"),
            _ = tokio::signal::ctrl_c() => tracing::info!("interrupted"),
            _ = sigterm.recv() => tracing::info!("terminated"),
        }
        anyhow::Ok(())
    });
    runtime::remove(&data_dir, pid);
    tracing::info!("stopped");
    // Audio/MIDI threads are detached; exit now rather than waiting on them.
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => Err(e),
    }
}

fn null_audio(error: Option<String>) -> (fours_protocol::AudioStatus, fours_engine::EngineLink) {
    if let Some(e) = &error {
        tracing::warn!("audio device unavailable ({e}); running headless");
    } else {
        tracing::info!("audio: disabled (--no-audio); running headless");
    }
    let (rt, link) = RtEngine::new(Engine::new(audio::NULL_SAMPLE_RATE));
    let mut status = audio::start_null(rt);
    status.error = error;
    (status, link)
}
