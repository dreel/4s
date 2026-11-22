//! Devices (RFC 0010): audio effects in insert chains on mixer channels and
//! the master, and modulators routed to parameters. A device is a node like
//! an instrument: an id from the same namespace (its parameters are
//! `<id>.*`) and an engine slot of its own (`effect_used`, `mod_used`). An
//! effect has a place in one chain (`chains`: channel number, 0 for the
//! master, -> effect ids in order); a modulator sits in the rack and its
//! outputs (`<id>.<k>`) move parameters through modulation routes
//! (`mod_routes`). Devices are built here, sent to the engine, and handed
//! back by the audio thread to be dropped here.

use super::*;
use fours_engine::{effect, modulator};

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

    /// Every effect, by chain (the master first) and place, then the
    /// modulators in the rack.
    pub(super) fn device_infos(&self) -> Vec<DeviceInfo> {
        let chained = self.chains.iter().flat_map(|(n, ids)| {
            ids.iter().enumerate().filter_map(move |(p, id)| {
                let d = self.devices.iter().find(|d| &d.id == id)?;
                Some(DeviceInfo { id: d.id.clone(), kind: d.kind, name: d.name.clone(), channel: Some(*n), position: p as u32 })
            })
        });
        let rack = self.devices.iter().filter(|d| d.kind.kind() == DeviceKind::Modulator).enumerate().map(|(p, d)| {
            DeviceInfo { id: d.id.clone(), kind: d.kind, name: d.name.clone(), channel: None, position: p as u32 }
        });
        chained.chain(rack).collect()
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
        let slot = if kind.kind() == DeviceKind::Modulator {
            let slot = self.mod_used.iter().position(|u| !u).expect("checked by the caller") as u8;
            self.mod_used[slot as usize] = true;
            self.send(Command::AddModulator { slot, modulator: modulator::make(kind) });
            slot
        } else {
            let slot = self.effect_used.iter().position(|u| !u).expect("checked by the caller") as u8;
            self.effect_used[slot as usize] = true;
            let effect = effect::make(kind, self.sample_rate as f32);
            self.send(Command::AddEffect { slot, effect });
            slot
        };
        self.devices.push(DeviceState { id: id.to_string(), kind, name: name.to_string(), slot });
    }

    /// Whether the engine has a free slot for a device of this kind.
    pub(super) fn has_room_for(&self, kind: DeviceType) -> bool {
        if kind.kind() == DeviceKind::Modulator {
            self.mod_used.iter().any(|u| !u)
        } else {
            self.effect_used.iter().any(|u| !u)
        }
    }

    /// The engine target of a device's parameter `index`.
    pub(super) fn device_target(d: &DeviceState, index: usize) -> ParamTarget {
        if d.kind.kind() == DeviceKind::Modulator {
            ParamTarget::Modulator { slot: d.slot, index: index as u16 }
        } else {
            ParamTarget::Effect { slot: d.slot, index: index as u16 }
        }
    }

    /// Send every parameter of a device to the engine.
    pub(super) fn push_device_params(&mut self, id: &str) {
        let Some(d) = self.devices.iter().find(|d| d.id == id) else { return };
        let own = Self::device_target(d, 0);
        let cmds: Vec<Command> = self
            .params
            .iter()
            .filter(|p| match (p.target, own) {
                (ParamTarget::Effect { slot: a, .. }, ParamTarget::Effect { slot: b, .. }) => a == b,
                (ParamTarget::Modulator { slot: a, .. }, ParamTarget::Modulator { slot: b, .. }) => a == b,
                _ => false,
            })
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
            if d.kind.kind() == DeviceKind::Modulator {
                // The engine drops its routes with it.
                self.send(Command::RemoveModulator { slot: d.slot });
                self.mod_used[d.slot as usize] = false;
            } else {
                self.send(Command::RemoveEffect { slot: d.slot });
                self.effect_used[d.slot as usize] = false;
            }
            self.in_flight += 1;
        }
        // Routes from a removed modulator: the engine dropped them; the
        // daemon's list follows in `graph_changed` (one prune for the
        // whole change).
    }

    /// Effects on a channel: what removing it takes with it.
    pub(super) fn inserts_on(&self, n: u32) -> usize {
        self.chains.get(&n).map_or(0, Vec::len)
    }

    /// Fail before removing channels if their effects would overflow the
    /// return ring, or they and `commands` more the command queue.
    pub(super) fn check_teardown(&self, channels: &[u32], commands: usize) -> Result<(), RpcError> {
        let n: usize = channels.iter().map(|c| self.inserts_on(*c)).sum();
        if self.in_flight + n > RETURN_CAPACITY {
            return Err(RpcError::failed(format!(
                "{} removed instruments or devices are still waiting to be returned by the audio engine",
                self.in_flight
            )));
        }
        // Plus a sidechain cleared per compressor at most.
        let keyed = self.devices.iter().filter(|d| d.kind == DeviceType::Compressor).count();
        self.ensure_room(2 * n + 2 + commands + keyed + self.route_commands())
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
            params: fours_engine::device_params(*k, k.default_id()),
            knob_pages: fours_engine::device_knob_pages(*k, k.default_id()),
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
        let modulator = p.kind.kind() == DeviceKind::Modulator;
        let channel = match (modulator, p.channel) {
            (true, None) => None,
            (true, Some(_)) => return Err(RpcError::invalid("a modulator sits in the rack, not on a channel: leave out `channel`")),
            (false, Some(n)) => Some(n),
            (false, None) => return Err(RpcError::invalid("an audio effect needs a `channel` (0 for the master)")),
        };
        if modulator && p.position.is_some() {
            return Err(RpcError::invalid("a modulator has no place in a chain: leave out `position`"));
        }
        if let Some(n) = channel {
            self.check_chain(n)?;
            if self.inserts_on(n) >= MAX_INSERTS {
                return Err(RpcError::invalid(format!("at most {MAX_INSERTS} inserts on a channel")));
            }
        }
        if !self.has_room_for(p.kind) {
            return Err(RpcError::invalid(if modulator {
                format!("at most {MAX_MODULATORS} modulators")
            } else {
                format!("at most {MAX_EFFECTS} effects")
            }));
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
        if let Some(n) = channel {
            let mut ids = self.chains.get(&n).cloned().unwrap_or_default();
            let at = p.position.map_or(ids.len(), |p| (p as usize).min(ids.len()));
            ids.insert(at, id.clone());
            self.set_chain_unchecked(n, ids);
        }
        self.graph_changed(origin);
        Ok(self.device_infos().into_iter().find(|d| d.id == id).expect("just added"))
    }

    fn device_remove(&mut self, id: &str, origin: &str) -> Result<Graph, RpcError> {
        self.find_device(id)?;
        if self.in_flight >= RETURN_CAPACITY {
            return Err(RpcError::failed(format!(
                "{} removed instruments or devices are still waiting to be returned by the audio engine",
                self.in_flight
            )));
        }
        self.ensure_room(2 + self.route_commands())?;
        self.teardown_device(id);
        self.graph_changed(origin);
        Ok(self.graph())
    }

    fn device_move(&mut self, p: DeviceMoveParams, origin: &str) -> Result<Graph, RpcError> {
        if self.find_device(&p.id)?.kind.kind() == DeviceKind::Modulator {
            return Err(RpcError::invalid(format!("'{}' is a modulator: it has no place in a chain", p.id)));
        }
        let (from, _) = self.device_place(&p.id).ok_or_else(|| RpcError::failed(format!("'{}' is in no chain", p.id)))?;
        let channel = p.channel.unwrap_or(from);
        self.check_chain(channel)?;
        let mut target = self.chains.get(&channel).cloned().unwrap_or_default();
        target.retain(|d| d != &p.id);
        if target.len() >= MAX_INSERTS {
            return Err(RpcError::invalid(format!("at most {MAX_INSERTS} inserts on a channel")));
        }
        self.ensure_room(2)?;
        if from != channel {
            let rest: Vec<String> = self.chains[&from].iter().filter(|d| **d != p.id).cloned().collect();
            self.set_chain_unchecked(from, rest);
        }
        let at = p.position.map_or(target.len(), |q| (q as usize).min(target.len()));
        target.insert(at, p.id.clone());
        self.set_chain_unchecked(channel, target);
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
            Request::ModRoute(p) => self.mod_route(p, origin).and_then(ok),
            Request::ModUnroute(p) => self.mod_unroute(p, origin).and_then(ok),
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
        for r in &self.mod_routes {
            d.insert(format!("mod:{}>{}", r.source, r.target), json!(r.depth));
        }
    }

    // ---- modulation routes -----------------------------------------------------

    /// A modulator output (`lfo.2`): its engine slot and output index.
    fn mod_source(&self, source: &str) -> Option<(u8, u8)> {
        let (id, k) = source.rsplit_once('.')?;
        let d = self.devices.iter().find(|d| d.id == id && d.kind.kind() == DeviceKind::Modulator)?;
        let k: usize = k.parse().ok()?;
        (1..=modulator::outputs(d.kind, id).len()).contains(&k).then_some((d.slot, (k - 1) as u8))
    }

    /// A route's target: a continuous parameter, and its range. Not the
    /// transport's (the daemon counts time by them).
    fn mod_target(&self, path: &str) -> Result<(ParamTarget, f32, f32), RpcError> {
        if transport_param(path) {
            return Err(RpcError::invalid(format!("'{path}' is the transport's: it takes no modulation")));
        }
        let p = &self.params[self.param_id(path)?];
        match p.info.kind {
            ParamKind::Continuous { min, max } => Ok((p.target, min as f32, max as f32)),
            _ => Err(RpcError::invalid(format!("'{path}' is not continuous: only continuous parameters take modulation"))),
        }
    }

    /// The engine's route table: each route with its target's value now as
    /// its base.
    pub(super) fn engine_routes(&self) -> Vec<fours_engine::ModRoute> {
        self.mod_routes
            .iter()
            .filter_map(|r| {
                let (source, output) = self.mod_source(&r.source)?;
                let (target, min, max) = self.mod_target(&r.target).ok()?;
                let base = self.params[self.param_id(&r.target).ok()?].value as f32;
                Some(fours_engine::ModRoute { source, output, target, depth: r.depth, min, max, base })
            })
            .collect()
    }

    /// Send the route table entries that changed to the engine (at most
    /// one command per route; a route's base is not compared, since the
    /// engine keeps it up to date itself).
    pub(super) fn sync_routes(&mut self) {
        let routes = self.engine_routes();
        let same = |a: &Option<fours_engine::ModRoute>, b: &Option<fours_engine::ModRoute>| match (a, b) {
            (Some(a), Some(b)) => fours_engine::ModRoute { base: 0.0, ..*a } == fours_engine::ModRoute { base: 0.0, ..*b },
            (None, None) => true,
            _ => false,
        };
        for i in 0..MAX_MOD_ROUTES {
            let route = routes.get(i).copied();
            if !same(&route, &self.routes_sent[i]) {
                self.send(Command::SetModRoute { index: i as u8, route });
                self.routes_sent[i] = route;
            }
        }
    }

    /// Commands a route table change can take at most.
    pub(super) fn route_commands(&self) -> usize {
        self.mod_routes.len() + 1
    }

    /// Drop routes whose source or target went away, telling the engine.
    pub(super) fn prune_routes(&mut self) {
        let before = self.mod_routes.len();
        let keep: Vec<ModRoute> = self
            .mod_routes
            .iter()
            .filter(|r| self.mod_source(&r.source).is_some() && self.mod_target(&r.target).is_ok())
            .cloned()
            .collect();
        if keep.len() != before {
            self.mod_routes = keep;
            self.sync_routes();
        }
    }

    pub(super) fn set_mod_route_unchecked(&mut self, source: &str, target: &str, depth: Option<f32>) {
        let at = self.mod_routes.iter().position(|r| r.source == source && r.target == target);
        match (at, depth) {
            (Some(i), Some(d)) => self.mod_routes[i].depth = d,
            (None, Some(d)) => self.mod_routes.push(ModRoute { source: source.into(), target: target.into(), depth: d }),
            (Some(i), None) => {
                self.mod_routes.remove(i);
            }
            (None, None) => {}
        }
    }

    fn mod_route(&mut self, p: ModRouteParams, origin: &str) -> Result<ModRoutesResult, RpcError> {
        if self.mod_source(&p.source).is_none() {
            let outs: Vec<String> = self
                .devices
                .iter()
                .filter(|d| d.kind.kind() == DeviceKind::Modulator)
                .flat_map(|d| modulator::outputs(d.kind, &d.id))
                .collect();
            return Err(RpcError::invalid(format!(
                "no modulator output '{}' (outputs: {})",
                p.source,
                if outs.is_empty() { "none; add an lfo".into() } else { outs.join(", ") }
            )));
        }
        self.mod_target(&p.target)?;
        if !p.depth.is_finite() || !(-1.0..=1.0).contains(&p.depth) {
            return Err(RpcError::invalid("depth must be -1..1"));
        }
        let exists = self.mod_routes.iter().any(|r| r.source == p.source && r.target == p.target);
        if !exists && self.mod_routes.len() >= MAX_MOD_ROUTES {
            return Err(RpcError::invalid(format!("at most {MAX_MOD_ROUTES} modulation routes")));
        }
        self.ensure_room(self.route_commands() + 1)?;
        self.set_mod_route_unchecked(&p.source, &p.target, Some(p.depth));
        self.sync_routes();
        self.graph_changed(origin);
        Ok(ModRoutesResult { routes: self.mod_routes.clone() })
    }

    fn mod_unroute(&mut self, p: ModUnrouteParams, origin: &str) -> Result<ModRoutesResult, RpcError> {
        if !self.mod_routes.iter().any(|r| r.source == p.source && r.target == p.target) {
            return Err(RpcError::invalid(format!("no route from '{}' to '{}'", p.source, p.target)));
        }
        self.ensure_room(self.route_commands())?;
        self.set_mod_route_unchecked(&p.source, &p.target, None);
        self.sync_routes();
        self.graph_changed(origin);
        Ok(ModRoutesResult { routes: self.mod_routes.clone() })
    }
}

/// Undo sets of device keys, gathered by `apply_sets`.
#[derive(Default)]
pub(super) struct DeviceSets {
    /// `device:<id>`: (type, name), or `None` to remove it.
    pub devices: Vec<(String, Option<(DeviceType, String)>)>,
    /// `chain:<n>`: the ids in order (`[]` when it went away).
    pub chains: Vec<(u32, Vec<String>)>,
    /// `mod:<source>><target>`: the depth, or `None` to remove the route.
    pub routes: Vec<(String, String, Option<f32>)>,
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
            "mod" => {
                let (source, target) = rest.split_once('>')?;
                self.routes.push((source.to_string(), target.to_string(), v.as_f64().map(|d| d as f32)));
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
            } else if self.instruments.iter().any(|i| &i.id == id) || !self.has_room_for(*kind) {
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
        // A device made here whose chain could not be restored (its channel
        // went away since) does not stay behind, unplaced.
        for id in added {
            let effect = self.devices.iter().any(|d| d.id == id && d.kind.kind() == DeviceKind::AudioEffect);
            if effect && self.device_place(&id).is_none() {
                self.teardown_device(&id);
                skipped.push(format!("device:{id}"));
            }
        }
        skipped
    }

    /// Apply undo sets of `mod:` keys (after parameters, so sources and
    /// targets exist). Returns keys that could not be set.
    pub(super) fn apply_route_sets(&mut self, routes: Vec<(String, String, Option<f32>)>) -> Vec<String> {
        let mut skipped = Vec::new();
        for (source, target, depth) in routes {
            let new = !self.mod_routes.iter().any(|r| r.source == source && r.target == target);
            let room = depth.is_none() || !new || self.mod_routes.len() < MAX_MOD_ROUTES;
            let ok = room && (depth.is_none() || (self.mod_source(&source).is_some() && self.mod_target(&target).is_ok()));
            if ok {
                self.set_mod_route_unchecked(&source, &target, depth);
            } else {
                skipped.push(format!("mod:{source}>{target}"));
            }
        }
        self.sync_routes();
        skipped
    }
}
