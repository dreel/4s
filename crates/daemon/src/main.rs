//! 4sd: the 4S daemon. Hosts the audio engine, owns all state, talks MIDI, and
//! serves the JSON-RPC API that the CLI and UI use.

mod audio;
mod controller;
mod core;
mod midi;
mod server;

use anyhow::Result;
use clap::Parser;
use fours_engine::{Engine, RtEngine};
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
    /// Disable MIDI device auto-connect.
    #[arg(long)]
    no_midi: bool,
    /// Data directory (projects, renders, controller maps). Default: ~/.4s
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Require clients to present this token in session.hello.
    #[arg(long, env = "FOURS_TOKEN")]
    token: Option<String>,
    /// Project bundle to load at startup.
    #[arg(long)]
    project: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    let data_dir = args.data_dir.clone().unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".4s")
    });
    std::fs::create_dir_all(&data_dir)?;

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
    let fours_engine::EngineLink { commands, mut feedback } = link;

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
        let p = &p;
        let mut c = core.lock().unwrap();
        if let Err(e) = c.project_load(p, "engine") {
            anyhow::bail!("failed to load project {p}: {}", e.message);
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
                if !batch.is_empty() {
                    let mut c = core.lock().unwrap();
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

    // MIDI hotplug: auto-connect a Livid Block when one appears.
    if !args.no_midi {
        let core = core.clone();
        std::thread::Builder::new().name("4s-midi-scan".into()).spawn(move || {
            loop {
                core.lock().unwrap().midi_autoconnect();
                std::thread::sleep(Duration::from_secs(2));
            }
        })?;
    }

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async move {
        let listener = tokio::net::TcpListener::bind(&args.listen).await?;
        let addr = listener.local_addr()?;
        if !addr.ip().is_loopback() && args.token.is_none() {
            tracing::warn!("listening on a non-loopback address without --token; anyone on the network can control 4S");
        }
        // Machine-readable line for tests and scripts.
        println!("4sd listening on ws://{addr}");
        std::io::stdout().flush()?;
        tokio::select! {
            _ = server::serve(core, listener, args.token) => {}
            _ = tokio::signal::ctrl_c() => tracing::info!("shutting down"),
        }
        Ok(())
    })
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
