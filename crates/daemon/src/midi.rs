//! MIDI device connections (midir). Input callbacks only forward raw messages
//! to a channel; all handling happens on the daemon's MIDI worker thread so
//! no locks are taken inside midir callbacks.

use anyhow::{Context, Result, anyhow};
use fours_protocol::{DeviceProfile, MidiConnection};
use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};
use std::sync::Mutex;
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

/// The clients `list_ports` enumerates through, made once and kept. Making
/// (and dropping) a CoreMIDI client on every hotplug scan, every 2 s for the
/// life of every daemon, wore down macOS's MIDIServer until it stopped
/// answering, and the scan hung inside CoreMIDI.
static LISTER: Mutex<Option<(MidiInput, MidiOutput)>> = Mutex::new(None);

/// Drop the kept listing clients so the next `list_ports` makes new ones:
/// clients made before macOS's MIDIServer restarted list nothing.
pub fn reset_lister() {
    *LISTER.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// The names of the MIDI input and output ports present now. This can block
/// if the system's MIDI service does, so call it without holding the core
/// lock where possible.
pub fn list_ports() -> (Vec<String>, Vec<String>) {
    let mut lister = LISTER.lock().unwrap_or_else(|e| e.into_inner());
    let (mi, mo) = match &mut *lister {
        Some(l) => l,
        empty => match (MidiInput::new(CLIENT), MidiOutput::new(CLIENT)) {
            (Ok(i), Ok(o)) => empty.insert((i, o)),
            _ => return Default::default(),
        },
    };
    let inputs = mi.ports().iter().filter_map(|p| mi.port_name(p).ok()).collect();
    let outputs = mo.ports().iter().filter_map(|p| mo.port_name(p).ok()).collect();
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

    /// The connected input ports.
    pub fn connected_inputs(&self) -> Vec<String> {
        self.conns.iter().map(|c| c.info.input.clone()).collect()
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
        r: &crate::hardware::Resolved,
        tx: Sender<MidiMessage>,
    ) -> Result<MidiConnection> {
        let profile = r.profile;
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

        let info = MidiConnection {
            input: input_name,
            output: output_name,
            device: r.name.clone(),
            profile,
            model: r.model.clone(),
            role: r.role.clone(),
        };
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

    /// Drop connections to ports that were connected when `inputs` was
    /// listed (`listed_with`) and are gone from it; a port connected since
    /// is kept, as the list may be older than the connection. Returns true
    /// if any were removed.
    pub fn prune(&mut self, inputs: &[String], listed_with: &[String]) -> bool {
        let before = self.conns.len();
        self.conns.retain(|c| inputs.contains(&c.info.input) || !listed_with.contains(&c.info.input));
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
