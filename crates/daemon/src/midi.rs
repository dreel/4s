//! MIDI device connections (midir). Input callbacks only forward raw messages
//! to a channel; all handling happens on the daemon's MIDI worker thread so
//! no locks are taken inside midir callbacks.

use anyhow::{Context, Result, anyhow};
use fours_protocol::{DeviceProfile, MidiConnection};
use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::sync::mpsc::Sender;

const CLIENT: &str = "4S";

pub struct MidiMessage {
    pub port: String,
    pub data: Vec<u8>,
}

struct Conn {
    info: MidiConnection,
    _input: MidiInputConnection<()>,
    output: Option<MidiOutputConnection>,
}

#[derive(Default)]
pub struct Midi {
    conns: Vec<Conn>,
}

/// macOS: CoreMIDI only updates a process's view of MIDI devices through
/// notifications delivered on the run loop of the thread that created the
/// process's *first* MIDI client. Without a running run loop, devices plugged
/// in after startup never appear. Call this once at startup, before any other
/// MIDI use: it creates that first client on a dedicated thread that runs a
/// CoreFoundation run loop forever.
#[cfg(target_os = "macos")]
pub fn start_device_watcher() {
    use std::ffi::c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFRunLoopDefaultMode: *const c_void;
        fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
    }
    const FINISHED: i32 = 1; // kCFRunLoopRunFinished: no sources on this run loop

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("4s-coremidi".into())
        .spawn(move || {
            let client = MidiInput::new(CLIENT);
            if let Err(e) = &client {
                tracing::warn!("MIDI unavailable: {e}");
            }
            let _ = tx.send(());
            let _keep = client;
            loop {
                let r = unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, 0) };
                if r == FINISHED {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            }
        })
        .expect("spawn CoreMIDI thread");
    let _ = rx.recv();
}

/// Other platforms (ALSA) see hotplugged devices without extra work.
#[cfg(not(target_os = "macos"))]
pub fn start_device_watcher() {}

pub fn list_ports() -> (Vec<String>, Vec<String>) {
    let inputs = MidiInput::new(CLIENT)
        .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect())
        .unwrap_or_default();
    let outputs = MidiOutput::new(CLIENT)
        .map(|m| m.ports().iter().filter_map(|p| m.port_name(p).ok()).collect())
        .unwrap_or_default();
    (inputs, outputs)
}

/// Exact match first, then case-insensitive substring.
fn find_port(names: &[String], query: &str) -> Option<String> {
    if let Some(n) = names.iter().find(|n| *n == query) {
        return Some(n.clone());
    }
    let q = query.to_lowercase();
    names.iter().find(|n| n.to_lowercase().contains(&q)).cloned()
}

impl Midi {
    pub fn connections(&self) -> Vec<MidiConnection> {
        self.conns.iter().map(|c| c.info.clone()).collect()
    }

    pub fn is_connected(&self, input: &str) -> bool {
        self.conns.iter().any(|c| c.info.input == input)
    }

    /// The input port a query names (exact, then case-insensitive
    /// substring).
    pub fn find_input(input_query: &str) -> Result<String> {
        let (inputs, _) = list_ports();
        find_port(&inputs, input_query)
            .ok_or_else(|| anyhow!("no MIDI input matching '{input_query}' (inputs: {inputs:?})"))
    }

    /// Connect the input port `input_name` (as found by `find_input`) as
    /// logical device `device`.
    pub fn connect(
        &mut self,
        input_name: &str,
        output_query: Option<&str>,
        device: &str,
        profile: DeviceProfile,
        tx: Sender<MidiMessage>,
    ) -> Result<MidiConnection> {
        let (_, outputs) = list_ports();
        let input_name = input_name.to_string();
        if self.is_connected(&input_name) {
            return Err(anyhow!("'{input_name}' is already connected"));
        }
        let output_name = match output_query {
            Some(q) => Some(
                find_port(&outputs, q).ok_or_else(|| anyhow!("no MIDI output matching '{q}'"))?,
            ),
            None if profile == DeviceProfile::LividBlock => find_port(&outputs, &input_name),
            None => None,
        };

        let mi = MidiInput::new(CLIENT).context("MIDI init")?;
        let port = mi
            .ports()
            .into_iter()
            .find(|p| mi.port_name(p).ok().as_deref() == Some(&input_name))
            .ok_or_else(|| anyhow!("input port disappeared"))?;
        let port_name = input_name.clone();
        let input = mi
            .connect(
                &port,
                "4S input",
                move |_stamp, data, _| {
                    let _ = tx.send(MidiMessage { port: port_name.clone(), data: data.to_vec() });
                },
                (),
            )
            .map_err(|e| anyhow!("connect input: {e}"))?;

        let output = match &output_name {
            Some(name) => {
                let mo = MidiOutput::new(CLIENT).context("MIDI init")?;
                let port = mo
                    .ports()
                    .into_iter()
                    .find(|p| mo.port_name(p).ok().as_deref() == Some(name))
                    .ok_or_else(|| anyhow!("output port disappeared"))?;
                Some(mo.connect(&port, "4S output").map_err(|e| anyhow!("connect output: {e}"))?)
            }
            None => None,
        };

        let info = MidiConnection { input: input_name, output: output_name, device: device.to_string(), profile };
        self.conns.push(Conn { info: info.clone(), _input: input, output });
        Ok(info)
    }

    /// Disconnect by port or logical name. Returns the port name.
    pub fn disconnect(&mut self, input_query: &str) -> Result<String> {
        let name = self.connected_port(input_query).ok_or_else(|| anyhow!("'{input_query}' is not connected"))?;
        self.conns.retain(|c| c.info.input != name);
        Ok(name)
    }

    /// The connected port a query names: a logical device name, else a port
    /// name (exact or substring).
    pub fn connected_port(&self, query: &str) -> Option<String> {
        if let Some(c) = self.conns.iter().find(|c| c.info.device == query) {
            return Some(c.info.input.clone());
        }
        let names: Vec<String> = self.conns.iter().map(|c| c.info.input.clone()).collect();
        find_port(&names, query)
    }

    pub fn connection(&self, port: &str) -> Option<&MidiConnection> {
        self.conns.iter().find(|c| c.info.input == port).map(|c| &c.info)
    }

    pub fn by_device(&self, device: &str) -> Option<&MidiConnection> {
        self.conns.iter().find(|c| c.info.device == device).map(|c| &c.info)
    }

    pub fn rename(&mut self, port: &str, name: &str) {
        for c in &mut self.conns {
            if c.info.input == port {
                c.info.device = name.to_string();
            }
        }
    }

    /// Drop connections whose ports no longer exist. Returns true if any were removed.
    pub fn prune(&mut self, inputs: &[String]) -> bool {
        let before = self.conns.len();
        self.conns.retain(|c| inputs.contains(&c.info.input));
        before != self.conns.len()
    }

    /// The connected Livid Block, if any.
    pub fn block_name(&self) -> Option<String> {
        self.conns.iter().find(|c| c.info.profile == DeviceProfile::LividBlock).map(|c| c.info.input.clone())
    }

    /// Send to every Livid Block output.
    pub fn send_block(&mut self, msg: &[u8]) {
        for c in &mut self.conns {
            if c.info.profile == DeviceProfile::LividBlock
                && let Some(out) = &mut c.output
                && let Err(e) = out.send(msg)
            {
                tracing::warn!("MIDI send to {} failed: {e}", c.info.input);
            }
        }
    }
}
