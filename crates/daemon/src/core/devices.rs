//! Devices (RFC 0010): audio effects in insert chains on mixer channels and
//! the master. An effect is a node like an instrument: an id from the same
//! namespace (its parameters are `<id>.*`), an engine slot of its own
//! (`effect_used`), and a place in one chain (`chains`: channel number, 0
//! for the master, -> effect ids in order). Effects are built here, sent to
//! the engine, and handed back by the audio thread to be dropped here.

use super::*;
use fours_engine::effect;

pub(super) struct DeviceState {
    pub id: String,
    pub kind: DeviceType,
    pub name: String,
    pub slot: u8,
}

impl Core {
    /// Whether an id is taken by an instrument or a device.
    pub(super) fn id_taken(&self, id: &str) -> bool {
        self.instruments.iter().any(|i| i.id == id) || self.devices.iter().any(|d| d.id == id)
    }

    fn free_device_id(&self, kind: DeviceType) -> String {
        let base = kind.default_id();
        if !self.id_taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}{n}")).find(|id| !self.id_taken(id)).unwrap()
    }

    pub(super) fn find_device(&self, id: &str) -> Result<&DeviceState, RpcError> {
        self.devices.iter().find(|d| d.id == id).ok_or_else(|| {
            let ids: Vec<&str> = self.devices.iter().map(|d| d.id.as_str()).collect();
            RpcError::invalid(format!("no device '{id}' (devices: {})", if ids.is_empty() { "none".into() } else { ids.join(", ") }))
        })
    }

    /// The channel whose chain holds a device, and its place there.
    pub(super) fn device_place(&self, id: &str) -> Option<(u32, u32)> {
        self.chains.iter().find_map(|(n, ids)| ids.iter().position(|d| d == id).map(|p| (*n, p as u32)))
    }

    /// Every effect, by chain (the master first) and place.
    pub(super) fn device_infos(&self) -> Vec<DeviceInfo> {
        self.chains
            .iter()
            .flat_map(|(n, ids)| {
                ids.iter().enumerate().filter_map(move |(p, id)| {
                    let d = self.devices.iter().find(|d| &d.id == id)?;
                    Some(DeviceInfo { id: d.id.clone(), kind: d.kind, name: d.name.clone(), channel: *n, position: p as u32 })
                })
            })
            .collect()
    }

    fn check_chain(&self, n: u32) -> Result<(), RpcError> {
        if n != MASTER {
            self.check_channel(n)?;
        }
        Ok(())
    }

    /// Build an effect in a free engine slot (the caller checked there is
    /// one, and queue room), not in any chain yet.
    pub(super) fn add_device_unchecked(&mut self, id: &str, kind: DeviceType, name: &str) {
        let slot = self.effect_used.iter().position(|u| !u).expect("checked by the caller") as u8;
        self.effect_used[slot as usize] = true;
        let effect = effect::make(kind, self.sample_rate as f32);
        self.send(Command::AddEffect { slot, effect });
        self.devices.push(DeviceState { id: id.to_string(), kind, name: name.to_string(), slot });
    }

    /// Send every parameter of a device to the engine.
    pub(super) fn push_device_params(&mut self, id: &str) {
        let Some(slot) = self.devices.iter().find(|d| d.id == id).map(|d| d.slot) else { return };
        let cmds: Vec<Command> = self
            .params
            .iter()
            .filter(|p| matches!(p.target, ParamTarget::Effect { slot: s, .. } if s == slot))
            .map(|p| Command::SetParam { target: p.target, value: p.value as f32 })
            .collect();
        for c in cmds {
            self.send(c);
        }
    }

    /// Replace a chain (the ids exist) and tell the engine.
    pub(super) fn set_chain_unchecked(&mut self, n: u32, ids: Vec<String>) {
        let mut slots = [0u8; MAX_INSERTS];
        let mut len = 0;
        for id in ids.iter().take(MAX_INSERTS) {
            if let Some(d) = self.devices.iter().find(|d| &d.id == id) {
                slots[len] = d.slot;
                len += 1;
            }
        }
        let ch = if n == MASTER { fours_engine::MASTER_CHAIN } else { (n - 1) as u8 };
        self.send(Command::SetChain { ch, slots, len: len as u8 });
        if ids.is_empty() {
            self.chains.remove(&n);
        } else {
            self.chains.insert(n, ids);
        }
    }

    /// Take a device out of the engine and the graph. The caller checked
    /// queue room and `in_flight`, and emits `graph`.
    pub(super) fn teardown_device(&mut self, id: &str) {
        if let Some((n, _)) = self.device_place(id) {
            let ids: Vec<String> = self.chains[&n].iter().filter(|d| *d != id).cloned().collect();
            self.set_chain_unchecked(n, ids);
        }
        if let Some(pos) = self.devices.iter().position(|d| d.id == id) {
            let d = self.devices.remove(pos);
            self.send(Command::RemoveEffect { slot: d.slot });
            self.effect_used[d.slot as usize] = false;
            self.in_flight += 1;
        }
    }

    /// Remove every effect on a channel (it is going away).
    pub(super) fn teardown_chain(&mut self, n: u32) {
        for id in self.chains.get(&n).cloned().unwrap_or_default() {
            self.teardown_device(&id);
        }
    }

    pub(super) fn device_types(&self) -> DeviceTypesResult {
        let mut types: Vec<DeviceTypeInfo> = InstrumentType::ALL
            .iter()
            .map(|k| {
                let id = k.default_id();
                let mut ports = k.ports();
                ports.extend(instrument::outputs(*k, id, k.label()).into_iter().map(|o| PortInfo {
                    id: o.source,
                    kind: PortKind::Audio,
                    direction: PortDirection::Out,
                    width: Some(o.width),
                }));
                DeviceTypeInfo {
                    id: k.id().into(),
                    kind: DeviceKind::Instrument,
                    label: k.label().into(),
                    default_id: id.into(),
                    ports,
                    params: instrument::params(*k, id),
                    knob_pages: instrument::knob_pages(*k, id),
                    layout: Some(k.layout()),
                }
            })
            .collect();
        types.extend(DeviceType::ALL.iter().map(|k| DeviceTypeInfo {
            id: k.id().into(),
            kind: k.kind(),
            label: k.label().into(),
            default_id: k.default_id().into(),
            ports: k.ports(),
            params: effect::params(*k, k.default_id()),
            knob_pages: effect::knob_pages(*k, k.default_id()),
            layout: None,
        }));
        DeviceTypesResult { types }
    }

    fn device_add(&mut self, p: DeviceAddParams, origin: &str) -> Result<DeviceInfo, RpcError> {
        let id = match p.id {
            Some(id) => id,
            None => self.free_device_id(p.kind),
        };
        validate_instrument_id(&id).map_err(RpcError::invalid)?;
        if self.id_taken(&id) {
            return Err(RpcError::invalid(format!("'{id}' is already an instrument or device id")));
        }
        self.check_chain(p.channel)?;
        let mut ids = self.chains.get(&p.channel).cloned().unwrap_or_default();
        if ids.len() >= MAX_INSERTS {
            return Err(RpcError::invalid(format!("at most {MAX_INSERTS} inserts on a channel")));
        }
        if !self.effect_used.iter().any(|u| !u) {
            return Err(RpcError::invalid(format!("at most {MAX_EFFECTS} effects")));
        }
        let name = match check_name(p.name)? {
            Some(n) => n,
            None => {
                let same = self.devices.iter().filter(|d| d.kind == p.kind).count();
                if same == 0 { p.kind.label().to_string() } else { format!("{} {}", p.kind.label(), same + 1) }
            }
        };
        self.ensure_room(ADD_COMMANDS)?;
        self.add_device_unchecked(&id, p.kind, &name);
        self.rebuild_params();
        self.push_device_params(&id);
        let at = p.position.map_or(ids.len(), |p| (p as usize).min(ids.len()));
        ids.insert(at, id.clone());
        self.set_chain_unchecked(p.channel, ids);
        self.graph_changed(origin);
        Ok(self.device_infos().into_iter().find(|d| d.id == id).expect("just added"))
    }

    fn device_remove(&mut self, id: &str, origin: &str) -> Result<Graph, RpcError> {
        self.find_device(id)?;
        if self.in_flight >= RETURN_CAPACITY {
            return Err(RpcError::failed(format!(
                "{} removed devices are still waiting to be returned by the audio engine",
                self.in_flight
            )));
        }
        self.ensure_room(2)?;
        self.teardown_device(id);
        self.graph_changed(origin);
        Ok(self.graph())
    }

    fn device_move(&mut self, p: DeviceMoveParams, origin: &str) -> Result<Graph, RpcError> {
        self.find_device(&p.id)?;
        self.check_chain(p.channel)?;
        let (from, _) = self.device_place(&p.id).expect("every device is in a chain");
        let mut target = self.chains.get(&p.channel).cloned().unwrap_or_default();
        target.retain(|d| d != &p.id);
        if target.len() >= MAX_INSERTS {
            return Err(RpcError::invalid(format!("at most {MAX_INSERTS} inserts on a channel")));
        }
        self.ensure_room(2)?;
        if from != p.channel {
            let rest: Vec<String> = self.chains[&from].iter().filter(|d| **d != p.id).cloned().collect();
            self.set_chain_unchecked(from, rest);
        }
        let at = p.position.map_or(target.len(), |q| (q as usize).min(target.len()));
        target.insert(at, p.id.clone());
        self.set_chain_unchecked(p.channel, target);
        self.graph_changed(origin);
        Ok(self.graph())
    }

    /// Device RPCs. Any other request is handed back.
    pub(super) fn handle_device(&mut self, req: Request, origin: &str) -> Result<RpcResult, Request> {
        Ok(match req {
            Request::DeviceTypes(_) => ok(self.device_types()),
            Request::DeviceList(_) => ok(DeviceListResult { devices: self.device_infos() }),
            Request::DeviceAdd(p) => self.device_add(p, origin).and_then(ok),
            Request::DeviceRemove(p) => self.device_remove(&p.id, origin).and_then(ok),
            Request::DeviceMove(p) => self.device_move(p, origin).and_then(ok),
            other => return Err(other),
        })
    }

    // ---- journal doc -----------------------------------------------------------

    /// Devices as undo keys: `device:<id>` -> `{type, name}` and `chain:<n>`
    /// -> the ids in order.
    pub(super) fn device_doc(&self, d: &mut Doc) {
        for dev in &self.devices {
            d.insert(format!("device:{}", dev.id), json!({ "type": dev.kind.id(), "name": dev.name }));
        }
        for (n, ids) in &self.chains {
            d.insert(format!("chain:{n}"), json!(ids));
        }
    }
}

/// Undo sets of device keys, gathered by `apply_sets`.
#[derive(Default)]
pub(super) struct DeviceSets {
    /// `device:<id>`: (type, name), or `None` to remove it.
    pub devices: Vec<(String, Option<(DeviceType, String)>)>,
    /// `chain:<n>`: the ids in order (`[]` when it went away).
    pub chains: Vec<(u32, Vec<String>)>,
}

impl DeviceSets {
    pub fn parse(&mut self, kind: &str, rest: &str, v: &Value) -> Option<()> {
        match kind {
            "device" => {
                let value = match v {
                    Value::Null => None,
                    v => Some((DeviceType::parse(v.get("type")?.as_str()?)?, v.get("name")?.as_str()?.to_string())),
                };
                self.devices.push((rest.to_string(), value));
            }
            "chain" => {
                let ids = match v {
                    Value::Null => Vec::new(),
                    v => serde_json::from_value(v.clone()).ok()?,
                };
                self.chains.push((rest.parse().ok()?, ids));
            }
            _ => return None,
        }
        Some(())
    }
}

impl Core {
    /// Apply undo sets of device keys: make devices first (with their
    /// parameters), then restore chains, then remove devices. Returns keys
    /// that could not be set.
    pub(super) fn apply_device_sets(&mut self, sets: DeviceSets) -> Vec<String> {
        let mut skipped = Vec::new();
        let mut added = Vec::new();
        for (id, v) in &sets.devices {
            let Some((kind, name)) = v else { continue };
            if let Some(d) = self.devices.iter_mut().find(|d| &d.id == id) {
                d.name = name.clone();
            } else if self.instruments.iter().any(|i| &i.id == id) || !self.effect_used.iter().any(|u| !u) {
                skipped.push(format!("device:{id}"));
            } else {
                self.add_device_unchecked(id, *kind, name);
                added.push(id.clone());
            }
        }
        if !added.is_empty() {
            self.rebuild_params();
            for id in &added {
                self.push_device_params(id);
            }
        }
        for (n, ids) in sets.chains {
            let exists = n == MASTER || self.channels.iter().any(|c| c.n == n);
            let known = ids.iter().all(|id| self.devices.iter().any(|d| &d.id == id));
            if !exists || !known || ids.len() > MAX_INSERTS {
                skipped.push(format!("chain:{n}"));
                continue;
            }
            // A device moving here leaves the chain it was in.
            for id in &ids {
                if let Some((from, _)) = self.device_place(id).filter(|(from, _)| *from != n) {
                    let rest: Vec<String> = self.chains[&from].iter().filter(|d| *d != id).cloned().collect();
                    self.set_chain_unchecked(from, rest);
                }
            }
            self.set_chain_unchecked(n, ids);
        }
        for (id, v) in &sets.devices {
            if v.is_none() && self.devices.iter().any(|d| &d.id == id) {
                self.teardown_device(id);
            }
        }
        skipped
    }
}
