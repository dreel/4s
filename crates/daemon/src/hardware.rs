//! This machine's MIDI hardware entries (RFC 0007): physical port -> logical
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    role: Option<String>,
}

/// How a port is connected: its logical name, profile, and model role.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub name: String,
    pub profile: DeviceProfile,
    pub model: Option<String>,
    pub role: Option<String>,
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

/// A port to connect automatically when it appears: a known model's port
/// that the model uses (`crate::models`).
pub fn known_model_port(port: &str) -> bool {
    crate::models::for_port(port).is_some_and(|(_, role)| role.is_some())
}

impl Hardware {
    pub fn load(path: &Path) -> Hardware {
        let file = match std::fs::read_to_string(path) {
            Ok(s) => serde_json::from_str(&s).unwrap_or_else(|e| {
                // Keep the user's file: it is moved aside, not overwritten.
                let bak = path.with_extension("json.bak");
                let _ = std::fs::rename(path, &bak);
                tracing::warn!("invalid {}: {e}; moved to {} and starting empty", path.display(), bak.display());
                File::default()
            }),
            Err(_) => File::default(),
        };
        let mut file = file;
        if let Some(s) = &file.seat
            && let Err(e) = validate_name("seat", s)
        {
            tracing::warn!("{}: ignoring the pinned seat: {e}", path.display());
            file.seat = None;
        }
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
                model: e.model.clone(),
                role: e.role.clone(),
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

    /// A name for a port seen for the first time: its model's name for it,
    /// else from its port name; numbered if another port already has it.
    fn fresh_name(&self, port: &str, model_name: Option<&str>) -> String {
        let mut base = model_name.map(str::to_string).unwrap_or_else(|| slug(port));
        if !self.name_taken(&base, port) {
            return base;
        }
        // Leave room for the number within the 32-character limit.
        base.truncate(28);
        (2..).map(|n| format!("{base}{n}")).find(|n| !self.name_taken(n, port)).unwrap()
    }

    /// The name and profile a connection of `port` gets: the given ones,
    /// else the saved ones, else defaults. A given name may be the one of
    /// port `replaces`, which the caller forgets once connected. Changes
    /// nothing.
    pub fn resolve(
        &self,
        port: &str,
        name: Option<&str>,
        profile: Option<DeviceProfile>,
        replaces: Option<&str>,
    ) -> Result<Resolved, String> {
        if let Some(n) = name {
            validate_name("device", n)?;
            if self.name_taken(n, port) && self.port_named(n).as_deref() != replaces {
                return Err(format!("another port is already named '{n}'"));
            }
        }
        let saved = self.file.devices.get(port);
        // A known model names the port and sets its profile; its ignored
        // ports connect as plain devices when asked for by hand.
        let model = crate::models::for_port(port);
        let role = model.and_then(|(_, r)| r);
        let name = name
            .map(str::to_string)
            .or(saved.map(|e| e.name.clone()))
            .unwrap_or_else(|| self.fresh_name(port, role));
        let profile = profile
            .or(saved.map(|e| e.profile))
            .unwrap_or(model.filter(|(_, r)| r.is_some()).map(|(m, _)| m.profile).unwrap_or_default());
        Ok(Resolved {
            name,
            profile,
            model: role.and(model.map(|(m, _)| m.id.clone())),
            role: role.map(str::to_string),
        })
    }

    /// Record a successful connection (as `resolve` named it), to be
    /// reconnected automatically when the port comes back.
    pub fn connected(&mut self, port: &str, r: &Resolved) {
        let e = Entry {
            name: r.name.clone(),
            profile: r.profile,
            auto_connect: true,
            model: r.model.clone(),
            role: r.role.clone(),
        };
        self.file.devices.insert(port.to_string(), e);
        self.save();
    }

    /// The user disconnected this port by hand.
    pub fn hand_disconnected(&self, port: &str) -> bool {
        self.file.devices.get(port).is_some_and(|e| !e.auto_connect)
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

    /// Drop a port's entry.
    pub fn forget(&mut self, port: &str) {
        if self.file.devices.remove(port).is_some() {
            self.save();
        }
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
