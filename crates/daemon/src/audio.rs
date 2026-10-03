//! Audio output: a cpal device stream, or a "null" backend that runs the engine
//! in real time without a device (headless servers, CI, agent tests).

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use fours_engine::RtEngine;
use fours_protocol::AudioStatus;
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub const NULL_SAMPLE_RATE: u32 = 48000;
const NULL_BLOCK: usize = 256;
/// Max frames per callback we preallocate scratch for (non-f32 devices).
const MAX_FRAMES: usize = 16384;

/// Query the default output device's sample rate (to construct the engine).
pub fn default_device_config() -> Result<(String, u32, u16)> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or_else(|| anyhow!("no default output device"))?;
    let config = device.default_output_config()?;
    Ok((device.to_string(), config.sample_rate(), config.channels()))
}

/// Start the engine on the default device. The stream lives on its own thread
/// (streams are not `Send` on every platform).
pub fn start_device(mut engine: RtEngine) -> Result<AudioStatus> {
    let (tx, rx) = mpsc::channel::<Result<AudioStatus>>();
    std::thread::Builder::new().name("4s-audio".into()).spawn(move || {
        let run = || -> Result<(cpal::Stream, AudioStatus)> {
            let host = cpal::default_host();
            let device = host.default_output_device().ok_or_else(|| anyhow!("no default output device"))?;
            let supported = device.default_output_config()?;
            let format = supported.sample_format();
            let config: cpal::StreamConfig = supported.into();
            if config.sample_rate != engine.sample_rate() {
                return Err(anyhow!("device rate {} != engine rate {}", config.sample_rate, engine.sample_rate()));
            }
            let channels = config.channels as usize;
            let err_fn = |e: cpal::Error| tracing::warn!("audio stream error: {e}");
            let stream = match format {
                cpal::SampleFormat::F32 => device.build_output_stream(
                    config.clone(),
                    move |data: &mut [f32], _: &cpal::OutputCallbackInfo| engine.process(data, channels),
                    err_fn,
                    None,
                )?,
                cpal::SampleFormat::I16 => build_converted::<i16>(&device, &config, engine, err_fn)?,
                cpal::SampleFormat::I32 => build_converted::<i32>(&device, &config, engine, err_fn)?,
                other => return Err(anyhow!("unsupported sample format {other}")),
            };
            stream.play()?;
            let status = AudioStatus {
                backend: "cpal".into(),
                device: Some(device.to_string()),
                sample_rate: config.sample_rate,
                channels: config.channels as u32,
                running: true,
                error: None,
            };
            Ok((stream, status))
        };
        match run() {
            Ok((stream, status)) => {
                let _ = tx.send(Ok(status));
                // Keep the stream alive for the life of the process.
                let _stream = stream;
                loop {
                    std::thread::park();
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e));
            }
        }
    })?;
    rx.recv().map_err(|_| anyhow!("audio thread exited"))?
}

fn build_converted<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut engine: RtEngine,
    err_fn: impl FnMut(cpal::Error) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut scratch = vec![0.0f32; MAX_FRAMES * channels];
    Ok(device.build_output_stream(
        config.clone(),
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for chunk in data.chunks_mut(scratch.len()) {
                let buf = &mut scratch[..chunk.len()];
                engine.process(buf, channels);
                for (o, s) in chunk.iter_mut().zip(buf.iter()) {
                    *o = T::from_sample(*s);
                }
            }
        },
        err_fn,
        None,
    )?)
}

/// Run the engine paced to real time with no audio device.
pub fn start_null(mut engine: RtEngine) -> AudioStatus {
    let sr = engine.sample_rate();
    std::thread::Builder::new()
        .name("4s-audio-null".into())
        .spawn(move || {
            let mut buf = vec![0.0f32; NULL_BLOCK * 2];
            let block = Duration::from_secs_f64(NULL_BLOCK as f64 / sr as f64);
            let mut deadline = Instant::now();
            loop {
                engine.process(&mut buf, 2);
                deadline += block;
                let now = Instant::now();
                if deadline > now {
                    std::thread::sleep(deadline - now);
                } else if now - deadline > Duration::from_millis(250) {
                    deadline = now; // fell far behind (e.g. suspended); don't burst
                }
            }
        })
        .expect("spawn null audio thread");
    AudioStatus {
        backend: "null".into(),
        device: None,
        sample_rate: sr,
        channels: 2,
        running: true,
        error: None,
    }
}
