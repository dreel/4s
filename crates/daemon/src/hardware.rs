//! This machine's MIDI hardware entries (RFC 0006): physical port -> logical
//! device name, profile, and whether to connect it automatically. Kept in
//! `<data-dir>/midi-devices.json` because port names belong to a machine,
//! while the bindings that use the logical names travel with the project.

use fours_protocol::{DeviceProfile, MidiDevice, slug, validate_name};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FILE: &str = "midi-devices.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct Entry {
    name: String,
    #[serde(default)]
    profile: DeviceProfile,
    #[serde(default)]
    auto_connect: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct File {
    /// Seat this machine's devices use; unset = follow the local user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seat: Option<String>,
    #[serde(default)]
    devices: BTreeMap<String, Entry>,
}

pub struct Hardware {
    path: PathBuf,
    file: File,
}

/// A port named like a Livid Block gets its profile by default.
pub fn looks_like_block(port: &str) -> bool {
    port.to_lowercase().contains("block")
}

impl Hardware {
    pub fn load(path: &Path) -> Hardware {
        let file = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                tracing::warn!("invalid {}: {e}; starting empty", path.display());
                File::default()
            }),
            Err(_) => File::default(),
        };
        Hardware { path: path.to_path_buf(), file }
    }

    fn save(&self) {
        let json = serde_json::to_string_pretty(&self.file).expect("serializes") + "\n";
        if let Err(e) = std::fs::write(&self.path, json) {
            tracing::warn!("could not save {}: {e}", self.path.display());
        }
    }

    pub fn devices(&self) -> Vec<MidiDevice> {
        self.file
            .devices
            .iter()
            .map(|(port, e)| MidiDevice {
                port: port.clone(),
                name: e.name.clone(),
                profile: e.profile,
                auto_connect: e.auto_connect,
            })
            .collect()
    }

    pub fn pinned_seat(&self) -> Option<&str> {
        self.file.seat.as_deref()
    }

    pub fn set_seat(&mut self, seat: Option<String>) {
        self.file.seat = seat;
        self.save();
    }

    pub fn get(&self, port: &str) -> Option<(String, DeviceProfile)> {
        self.file.devices.get(port).map(|e| (e.name.clone(), e.profile))
    }

    fn name_taken(&self, name: &str, except_port: &str) -> bool {
        self.file.devices.iter().any(|(p, e)| p != except_port && e.name == name)
    }

    /// A name for a port seen for the first time: from its port name,
    /// numbered if another port already has it.
    fn fresh_name(&self, port: &str) -> String {
        let base = slug(port);
        if !self.name_taken(&base, port) {
            return base;
        }
        (2..).map(|n| format!("{base}{n}")).find(|n| !self.name_taken(n, port)).unwrap()
    }

    /// Record a connection: the given name/profile, else the saved ones,
    /// else defaults. Returns the entry's name and profile.
    pub fn connected(
        &mut self,
        port: &str,
        name: Option<&str>,
        profile: Option<DeviceProfile>,
    ) -> Result<(String, DeviceProfile), String> {
        if let Some(n) = name {
            validate_name("device", n)?;
            if self.name_taken(n, port) {
                return Err(format!("another port is already named '{n}'"));
            }
        }
        let default_profile =
            if looks_like_block(port) { DeviceProfile::LividBlock } else { DeviceProfile::Generic };
        let fresh = self.fresh_name(port);
        let e = self.file.devices.entry(port.to_string()).or_insert(Entry {
            name: fresh,
            profile: default_profile,
            auto_connect: true,
        });
        if let Some(n) = name {
            e.name = n.to_string();
        }
        if let Some(p) = profile {
            e.profile = p;
        }
        e.auto_connect = true;
        let r = (e.name.clone(), e.profile);
        self.save();
        Ok(r)
    }

    /// The user disconnected this port: do not connect it automatically.
    pub fn disconnected(&mut self, port: &str) {
        if let Some(e) = self.file.devices.get_mut(port) {
            e.auto_connect = false;
            self.save();
        }
    }

    /// Rename the entry of `port`.
    pub fn rename(&mut self, port: &str, name: &str) -> Result<(), String> {
        validate_name("device", name)?;
        if self.name_taken(name, port) {
            return Err(format!("another port is already named '{name}'"));
        }
        let e = self.file.devices.get_mut(port).ok_or_else(|| format!("no device entry for port '{port}'"))?;
        e.name = name.to_string();
        self.save();
        Ok(())
    }

    /// The port with this logical name, if any.
    pub fn port_named(&self, name: &str) -> Option<String> {
        self.file.devices.iter().find(|(_, e)| e.name == name).map(|(p, _)| p.clone())
    }

    /// Ports to connect automatically.
    pub fn auto_ports(&self) -> Vec<String> {
        self.file.devices.iter().filter(|(_, e)| e.auto_connect).map(|(p, _)| p.clone()).collect()
    }
}
