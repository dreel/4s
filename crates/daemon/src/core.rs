//! The daemon core: authoritative state, request handling, and event fan-out.
//!
//! All mutations go through `Core` (behind one mutex). Each mutation updates
//! state, forwards `Command`s to the real-time engine, and emits an event so
//! every client (UI, CLI, controllers) stays in sync regardless of origin.
//!
//! State is a graph: instruments (each in an engine slot), mixer channels,
//! and routes from instrument outputs to channels. The parameter registry is
//! rebuilt from the graph whenever it changes.

use crate::controller::{BlockInput, BlockMap, Controller, decode_block};
use crate::midi::{Midi, MidiMessage, list_ports};
use fours_engine::instrument::{self, MAX_OUTPUTS};
use fours_engine::offline::{RenderInstrument, RenderPattern, RenderSpec};
use fours_engine::params::{self, CHANNEL_PARAMS, NUM_GLOBALS};
use fours_engine::{Command, Feedback, ParamTarget, RETURN_CAPACITY};
use fours_protocol::*;
use rtrb::Producer;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub type Shared = Arc<Mutex<Core>>;

#[derive(Debug)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn invalid(msg: impl Into<String>) -> Self {
        Self { code: -32602, message: msg.into() }
    }
    pub fn failed(msg: impl Into<String>) -> Self {
        Self { code: -32000, message: msg.into() }
    }
}

type RpcResult = Result<Value, RpcError>;

fn ok<T: serde::Serialize>(v: T) -> RpcResult {
    Ok(serde_json::to_value(v).expect("result serializes"))
}

/// Commands an instrument add may need (add, params, routes, pattern), with
/// headroom; checked before any structural change so pushes cannot fail
/// halfway.
const ADD_COMMANDS: usize = 96;

struct InstrumentState {
    id: String,
    kind: InstrumentType,
    name: String,
    slot: u8,
    outputs: Vec<OutputInfo>,
}

enum PatternState {
    Drums(Box<[[u8; MAX_STEPS]; NUM_TRACKS]>),
    Notes(Box<[NoteStep; MAX_STEPS]>),
}

impl PatternState {
    fn new(kind: InstrumentType) -> Self {
        match kind {
            InstrumentType::Tr808 => PatternState::Drums(Box::new([[STEP_OFF; MAX_STEPS]; NUM_TRACKS])),
            InstrumentType::Tb303 => PatternState::Notes(Box::new([NoteStep::default(); MAX_STEPS])),
        }
    }
}

struct ParamEntry {
    info: ParamInfo,
    target: ParamTarget,
    value: f64,
}

pub struct Core {
    params: Vec<ParamEntry>,
    index: HashMap<String, usize>,
    instruments: Vec<InstrumentState>,
    channels: Vec<ChannelInfo>,
    routes: BTreeMap<String, u32>,
    patterns: HashMap<String, PatternState>,
    slot_used: [bool; MAX_INSTRUMENTS],
    /// Removed instruments not yet handed back by the audio thread.
    in_flight: usize,
    sample_rate: u32,
    playing: bool,
    playhead: Option<u32>,
    controller: Controller,
    block_map: BlockMap,
    midi: Midi,
    midi_tx: Sender<MidiMessage>,
    /// The note each instrument slot is holding for a keyboard, and which
    /// keyboard (input port) started it: only that key's note-off releases
    /// it, and unplugging that keyboard releases it too.
    held_notes: HashMap<u8, (String, u8)>,
    project: ProjectInfo,
    pub audio: AudioStatus,
    seq: u64,
    events: broadcast::Sender<Arc<EventEnvelope>>,
    commands: Producer<Command>,
    data_dir: PathBuf,
    meters_silent: bool,
}

impl Core {
    pub fn new(
        commands: Producer<Command>,
        midi_tx: Sender<MidiMessage>,
        data_dir: PathBuf,
        audio: AudioStatus,
    ) -> Self {
        let block_map = BlockMap::load_or_create(&data_dir.join("livid-block.json"));
        let (events, _) = broadcast::channel(4096);
        let mut core = Self {
            params: Vec::new(),
            index: HashMap::new(),
            instruments: Vec::new(),
            channels: Vec::new(),
            routes: BTreeMap::new(),
            patterns: HashMap::new(),
            slot_used: [false; MAX_INSTRUMENTS],
            in_flight: 0,
            sample_rate: audio.sample_rate,
            playing: false,
            playhead: None,
            controller: Controller::default(),
            block_map,
            midi: Midi::default(),
            midi_tx,
            held_notes: HashMap::new(),
            project: ProjectInfo { path: None, dirty: false },
            audio,
            seq: 0,
            events,
            commands,
            data_dir,
            meters_silent: false,
        };
        core.rebuild_params();
        core.apply_project_file(&default_project(), "engine").expect("default project applies");
        core
    }

    // ---- infrastructure ----------------------------------------------------

    /// Subscribe to events. Returns the receiver and the current seq; nothing
    /// is missed because both are taken under the core lock.
    pub fn subscribe(&self) -> (broadcast::Receiver<Arc<EventEnvelope>>, u64) {
        (self.events.subscribe(), self.seq)
    }

    fn emit(&mut self, origin: &str, event: Event) {
        self.seq += 1;
        let _ = self.events.send(Arc::new(EventEnvelope { seq: self.seq, origin: origin.to_string(), event }));
    }

    fn send(&mut self, cmd: Command) {
        if let Err(rtrb::PushError::Full(cmd)) = self.commands.push(cmd) {
            tracing::warn!("engine command queue full; dropped {cmd:?}");
        }
    }

    /// Fail before a structural change if the command queue could fill up
    /// partway through it.
    fn ensure_room(&self, commands: usize) -> Result<(), RpcError> {
        if self.commands.slots() < commands {
            return Err(RpcError::failed("the audio engine is busy (command queue full); try again"));
        }
        Ok(())
    }

    fn mark_dirty(&mut self, origin: &str) {
        if !self.project.dirty {
            self.project.dirty = true;
            let info = self.project.clone();
            self.emit(origin, Event::Project { info });
        }
    }

    fn global(&self, index: usize) -> f64 {
        self.params[index].value
    }

    fn length(&self) -> u32 {
        self.global(params::LENGTH) as u32
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// An instrument handed back by the audio thread has been dropped.
    pub fn instrument_returned(&mut self) {
        self.in_flight = self.in_flight.saturating_sub(1);
    }

    // ---- graph -------------------------------------------------------------

    fn graph(&self) -> Graph {
        Graph {
            instruments: self.instruments.iter().map(|i| self.instrument_info(i)).collect(),
            channels: self.channels.clone(),
            routes: self.routes.clone(),
        }
    }

    fn instrument_info(&self, i: &InstrumentState) -> InstrumentInfo {
        InstrumentInfo { id: i.id.clone(), kind: i.kind, name: i.name.clone(), outputs: i.outputs.clone() }
    }

    fn graph_changed(&mut self, origin: &str) {
        self.rebuild_params();
        let graph = self.graph();
        self.emit(origin, Event::Graph { graph });
        self.mark_dirty(origin);
    }

    /// Rebuild the parameter registry from the graph, keeping values by path.
    /// New entries take their defaults.
    fn rebuild_params(&mut self) {
        let old: HashMap<String, f64> = self.params.drain(..).map(|p| (p.info.path, p.value)).collect();
        let mut entries: Vec<(ParamInfo, ParamTarget)> = Vec::new();
        for (i, info) in params::globals().into_iter().enumerate() {
            entries.push((info, ParamTarget::Global(i)));
        }
        for c in &self.channels {
            for (i, info) in params::channel(c.n, &c.name).into_iter().enumerate() {
                entries.push((info, ParamTarget::Channel { ch: (c.n - 1) as u8, index: i as u8 }));
            }
        }
        for inst in &self.instruments {
            for (i, info) in instrument::params(inst.kind, &inst.id).into_iter().enumerate() {
                entries.push((info, ParamTarget::Instrument { slot: inst.slot, index: i as u16 }));
            }
        }
        self.params = entries
            .into_iter()
            .map(|(info, target)| {
                let value = old.get(&info.path).copied().unwrap_or(info.default);
                ParamEntry { info, target, value }
            })
            .collect();
        self.index = self.params.iter().enumerate().map(|(i, p)| (p.info.path.clone(), i)).collect();
    }

    fn find_instrument(&self, id: &str) -> Result<&InstrumentState, RpcError> {
        self.instruments.iter().find(|i| i.id == id).ok_or_else(|| {
            RpcError::invalid(format!("no instrument '{id}' (instruments: {})", self.instrument_ids()))
        })
    }

    fn instrument_ids(&self) -> String {
        if self.instruments.is_empty() {
            return "none".into();
        }
        self.instruments.iter().map(|i| format!("{} ({})", i.id, i.kind.id())).collect::<Vec<_>>().join(", ")
    }

    /// Resolve an instrument of `kind`: the given id, or the first of that
    /// type in creation order.
    fn resolve(&self, id: Option<&str>, kind: InstrumentType) -> Result<String, RpcError> {
        match id {
            Some(id) => {
                let inst = self.find_instrument(id)?;
                if inst.kind != kind {
                    return Err(RpcError::invalid(format!(
                        "instrument '{id}' is a {}, not a {}",
                        inst.kind.id(),
                        kind.id()
                    )));
                }
                Ok(inst.id.clone())
            }
            None => self.instruments.iter().find(|i| i.kind == kind).map(|i| i.id.clone()).ok_or_else(|| {
                RpcError::invalid(format!(
                    "no {} instrument; pass `instrument` (instruments: {})",
                    kind.id(),
                    self.instrument_ids()
                ))
            }),
        }
    }

    fn slot_of(&self, id: &str) -> u8 {
        self.instruments.iter().find(|i| i.id == id).map(|i| i.slot).unwrap_or(0)
    }

    fn free_id(&self, kind: InstrumentType) -> String {
        let base = kind.default_id();
        let taken = |id: &str| self.instruments.iter().any(|i| i.id == id);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|n| format!("{base}{n}")).find(|id| !taken(id)).unwrap()
    }

    pub fn instrument_add(&mut self, p: InstrumentAddParams, origin: &str) -> Result<InstrumentInfo, RpcError> {
        let auto_id = p.id.is_none();
        let id = match p.id {
            Some(id) => id,
            None => self.free_id(p.kind),
        };
        validate_instrument_id(&id).map_err(RpcError::invalid)?;
        if self.instruments.iter().any(|i| i.id == id) {
            return Err(RpcError::invalid(format!("an instrument '{id}' already exists")));
        }
        let slot = self.slot_used.iter().position(|u| !u).ok_or_else(|| {
            RpcError::invalid(format!("at most {MAX_INSTRUMENTS} instruments"))
        })? as u8;
        let channel = if p.no_channel {
            None
        } else if let Some(n) = p.channel {
            if !self.channels.iter().any(|c| c.n == n) {
                return Err(RpcError::invalid(format!("no channel {n}")));
            }
            Some(n)
        } else {
            if self.channels.len() >= MAX_CHANNELS {
                return Err(RpcError::invalid(format!("at most {MAX_CHANNELS} channels")));
            }
            None
        };
        self.ensure_room(ADD_COMMANDS)?;

        let name = p.name.unwrap_or_else(|| match id.strip_prefix(p.kind.default_id()) {
            Some(n) if auto_id && !n.is_empty() => format!("{} {n}", p.kind.label()),
            _ => p.kind.label().to_string(),
        });
        self.add_instrument_unchecked(&id, p.kind, &name, slot);
        let route_to = match (p.no_channel, channel) {
            (true, _) => None,
            (false, Some(n)) => Some(n),
            (false, None) => Some(self.add_channel_unchecked(&name)),
        };
        if let Some(n) = route_to {
            self.set_route_unchecked(&id, 0, Some(n));
        }
        self.graph_changed(origin);
        self.push_instrument_params(&id);
        if self.controller.target.is_none() && p.kind == InstrumentType::Tr808 {
            self.controller.target = Some(id.clone());
        }
        self.refresh_controller(origin, true);
        let inst = self.find_instrument(&id)?;
        Ok(self.instrument_info(inst))
    }

    fn add_instrument_unchecked(&mut self, id: &str, kind: InstrumentType, name: &str, slot: u8) {
        let instrument = instrument::make(kind, self.sample_rate as f32);
        self.send(Command::AddInstrument { slot, instrument });
        self.slot_used[slot as usize] = true;
        self.instruments.push(InstrumentState {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            slot,
            outputs: instrument::outputs(kind, id, name),
        });
        self.patterns.insert(id.to_string(), PatternState::new(kind));
    }

    /// Send every parameter of an instrument to the engine.
    fn push_instrument_params(&mut self, id: &str) {
        let slot = self.slot_of(id);
        let cmds: Vec<Command> = self
            .params
            .iter()
            .filter(|p| matches!(p.target, ParamTarget::Instrument { slot: s, .. } if s == slot))
            .map(|p| Command::SetParam { target: p.target, value: p.value as f32 })
            .collect();
        for c in cmds {
            self.send(c);
        }
    }

    pub fn instrument_remove(&mut self, p: InstrumentRemoveParams, origin: &str) -> Result<Graph, RpcError> {
        let pos = self.instruments.iter().position(|i| i.id == p.id).ok_or_else(|| {
            RpcError::invalid(format!("no instrument '{}' (instruments: {})", p.id, self.instrument_ids()))
        })?;
        if self.in_flight >= RETURN_CAPACITY {
            return Err(RpcError::failed(format!(
                "{} removed instruments are still waiting to be returned by the audio engine",
                self.in_flight
            )));
        }
        self.ensure_room(4)?;
        let inst = self.instruments.remove(pos);
        self.send(Command::RemoveInstrument { slot: inst.slot });
        self.slot_used[inst.slot as usize] = false;
        self.in_flight += 1;
        self.patterns.remove(&inst.id);
        let fed: Vec<u32> = inst.outputs.iter().filter_map(|o| self.routes.remove(&o.source)).collect();
        if !p.keep_channels {
            for n in fed {
                if !self.routes.values().any(|c| *c == n) && self.channels.iter().any(|c| c.n == n) {
                    self.remove_channel_unchecked(n);
                }
            }
        }
        if self.controller.target.as_deref() == Some(inst.id.as_str()) {
            self.controller.target =
                self.instruments.iter().find(|i| i.kind == InstrumentType::Tr808).map(|i| i.id.clone());
        }
        self.held_notes.remove(&inst.slot);
        self.graph_changed(origin);
        self.refresh_controller(origin, true);
        if self.midi.clear_instrument(&inst.id) {
            let connections = self.midi.connections();
            self.emit(origin, Event::Midi { connections });
        }
        Ok(self.graph())
    }

    /// Lowest free channel number, activated in the engine at defaults.
    fn add_channel_unchecked(&mut self, name: &str) -> u32 {
        let n = (1..=MAX_CHANNELS as u32).find(|n| !self.channels.iter().any(|c| c.n == *n)).unwrap();
        self.send(Command::SetChannelActive { ch: (n - 1) as u8, active: true });
        self.channels.push(ChannelInfo { n, name: name.to_string() });
        n
    }

    fn remove_channel_unchecked(&mut self, n: u32) {
        let sources: Vec<String> = self.routes.iter().filter(|(_, c)| **c == n).map(|(s, _)| s.clone()).collect();
        for s in sources {
            if let Some((id, o)) = self.find_source(&s) {
                self.set_route_unchecked(&id, o, None);
            }
        }
        self.send(Command::SetChannelActive { ch: (n - 1) as u8, active: false });
        self.channels.retain(|c| c.n != n);
    }

    pub fn channel_add(&mut self, name: Option<String>, origin: &str) -> Result<ChannelInfo, RpcError> {
        if self.channels.len() >= MAX_CHANNELS {
            return Err(RpcError::invalid(format!("at most {MAX_CHANNELS} channels")));
        }
        self.ensure_room(2)?;
        let n = (1..=MAX_CHANNELS as u32).find(|n| !self.channels.iter().any(|c| c.n == *n)).unwrap();
        let name = name.unwrap_or_else(|| format!("Ch {n}"));
        self.add_channel_unchecked(&name);
        self.graph_changed(origin);
        Ok(ChannelInfo { n, name })
    }

    fn check_channel(&self, n: u32) -> Result<(), RpcError> {
        if !self.channels.iter().any(|c| c.n == n) {
            let have: Vec<String> = self.channels.iter().map(|c| c.n.to_string()).collect();
            return Err(RpcError::invalid(format!("no channel {n} (channels: {})", have.join(", "))));
        }
        Ok(())
    }

    pub fn channel_remove(&mut self, n: u32, origin: &str) -> Result<Graph, RpcError> {
        self.check_channel(n)?;
        self.ensure_room(MAX_OUTPUTS * MAX_INSTRUMENTS + 2)?;
        self.remove_channel_unchecked(n);
        self.graph_changed(origin);
        Ok(self.graph())
    }

    pub fn channel_rename(&mut self, n: u32, name: String, origin: &str) -> Result<ChannelInfo, RpcError> {
        self.check_channel(n)?;
        if name.trim().is_empty() {
            return Err(RpcError::invalid("name must not be empty"));
        }
        let c = self.channels.iter_mut().find(|c| c.n == n).unwrap();
        c.name = name.trim().to_string();
        let info = c.clone();
        self.graph_changed(origin);
        Ok(info)
    }

    /// (instrument id, output index) for a source name.
    fn find_source(&self, source: &str) -> Option<(String, usize)> {
        self.instruments.iter().find_map(|i| {
            i.outputs.iter().position(|o| o.source == source).map(|o| (i.id.clone(), o))
        })
    }

    fn set_route_unchecked(&mut self, id: &str, output: usize, channel: Option<u32>) {
        let Some(inst) = self.instruments.iter().find(|i| i.id == id) else { return };
        let (slot, source) = (inst.slot, inst.outputs[output].source.clone());
        self.send(Command::SetRoute {
            slot,
            output: output as u8,
            channel: channel.map(|n| (n - 1) as u8),
        });
        match channel {
            Some(n) => self.routes.insert(source, n),
            None => self.routes.remove(&source),
        };
    }

    pub fn route_set(&mut self, p: RouteSetParams, origin: &str) -> Result<Graph, RpcError> {
        let (id, output) = self.find_source(&p.source).ok_or_else(|| {
            let all: Vec<String> =
                self.instruments.iter().flat_map(|i| i.outputs.iter().map(|o| o.source.clone())).collect();
            RpcError::invalid(format!("no output '{}' (sources: {})", p.source, all.join(", ")))
        })?;
        if let Some(n) = p.channel {
            self.check_channel(n)?;
        }
        self.ensure_room(2)?;
        self.set_route_unchecked(&id, output, p.channel);
        self.graph_changed(origin);
        Ok(self.graph())
    }

    fn instrument_types(&self) -> InstrumentTypesResult {
        InstrumentTypesResult {
            types: InstrumentType::ALL
                .iter()
                .map(|k| InstrumentTypeInfo {
                    kind: *k,
                    label: k.label().to_string(),
                    default_id: k.default_id().to_string(),
                    outputs: instrument::outputs(*k, k.default_id(), k.label()),
                    params: instrument::params(*k, k.default_id()),
                })
                .collect(),
        }
    }

    // ---- snapshot ----------------------------------------------------------

    /// Recompute controller LEDs, push changes to hardware, and emit an event
    /// if anything visible changed.
    fn refresh_controller(&mut self, origin: &str, force_event: bool) {
        let pages = Controller::num_pages(self.length());
        if self.controller.page >= pages {
            self.controller.page = pages - 1;
        }
        let pattern = self.target_pattern();
        let leds = self.controller.compute_leds(pattern, self.length(), self.playhead);
        let diff = self.controller.set_leds(leds);
        for (r, c, v) in &diff {
            let msg = self.block_map.led_message(*r, *c, *v);
            self.midi.send_block(&msg);
        }
        if force_event || !diff.is_empty() {
            let state = self.controller.state();
            self.emit(origin, Event::Controller { state });
        }
    }

    fn target_pattern(&self) -> Option<&[[u8; MAX_STEPS]; NUM_TRACKS]> {
        match self.patterns.get(self.controller.target.as_deref()?)? {
            PatternState::Drums(d) => Some(d),
            PatternState::Notes(_) => None,
        }
    }

    /// Re-send every LED (after a device connects).
    fn push_all_leds(&mut self) {
        for r in 0..crate::controller::GRID {
            for c in 0..crate::controller::GRID {
                let msg = self.block_map.led_message(r, c, self.controller.leds[r][c]);
                self.midi.send_block(&msg);
            }
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            seq: self.seq,
            transport: TransportState { playing: self.playing, step: self.playhead },
            params: self.params.iter().map(|p| (p.info.path.clone(), p.value)).collect(),
            graph: self.graph(),
            patterns: self
                .instruments
                .iter()
                .map(|i| InstrumentPattern { instrument: i.id.clone(), pattern: self.pattern_data(&i.id) })
                .collect(),
            controller: self.controller.state(),
            midi: self.midi.connections(),
            project: self.project.clone(),
            audio: self.audio.clone(),
        }
    }

    fn pattern_data(&self, id: &str) -> PatternData {
        match self.patterns.get(id) {
            Some(PatternState::Drums(_)) => PatternData::Drums { tracks: self.track_patterns(id, None) },
            Some(PatternState::Notes(n)) => PatternData::Notes { steps: n.to_vec() },
            None => PatternData::Notes { steps: Vec::new() },
        }
    }

    fn drums(&self, id: &str) -> &[[u8; MAX_STEPS]; NUM_TRACKS] {
        match self.patterns.get(id) {
            Some(PatternState::Drums(d)) => d,
            _ => panic!("drum pattern for '{id}'"),
        }
    }

    fn drums_mut(&mut self, id: &str) -> &mut [[u8; MAX_STEPS]; NUM_TRACKS] {
        match self.patterns.get_mut(id) {
            Some(PatternState::Drums(d)) => d,
            _ => panic!("drum pattern for '{id}'"),
        }
    }

    fn track_patterns(&self, id: &str, voice: Option<Voice>) -> Vec<TrackPattern> {
        let d = self.drums(id);
        Voice::ALL
            .iter()
            .filter(|v| voice.is_none_or(|x| x == **v))
            .map(|v| TrackPattern { voice: *v, steps: d[v.index()].to_vec() })
            .collect()
    }

    fn pattern_result(&self, id: &str, voice: Option<Voice>) -> PatternResult {
        PatternResult { instrument: id.to_string(), length: self.length(), tracks: self.track_patterns(id, voice) }
    }

    /// Everything an offline render needs, copied out of the core.
    pub fn render_spec(&self) -> RenderSpec {
        let mut globals = [0.0f32; NUM_GLOBALS];
        for p in &self.params {
            if let ParamTarget::Global(i) = p.target {
                globals[i] = p.value as f32;
            }
        }
        let channels = self
            .channels
            .iter()
            .map(|c| {
                let mut values = [0.0f32; CHANNEL_PARAMS];
                for p in &self.params {
                    if let ParamTarget::Channel { ch, index } = p.target
                        && ch as u32 == c.n - 1
                    {
                        values[index as usize] = p.value as f32;
                    }
                }
                ((c.n - 1) as u8, values)
            })
            .collect();
        let instruments = self
            .instruments
            .iter()
            .map(|i| {
                let params = self
                    .params
                    .iter()
                    .filter(|p| matches!(p.target, ParamTarget::Instrument { slot, .. } if slot == i.slot))
                    .map(|p| p.value as f32)
                    .collect();
                let routes = i.outputs.iter().map(|o| self.routes.get(&o.source).map(|n| (n - 1) as u8)).collect();
                let pattern = match &self.patterns[&i.id] {
                    PatternState::Drums(d) => RenderPattern::Drums(**d),
                    PatternState::Notes(n) => RenderPattern::Notes(**n),
                };
                RenderInstrument { id: i.id.clone(), kind: i.kind, params, routes, pattern }
            })
            .collect();
        RenderSpec { globals, channels, instruments }
    }

    // ---- parameters --------------------------------------------------------

    fn param_id(&self, path: &str) -> Result<usize, RpcError> {
        self.index.get(path).copied().ok_or_else(|| {
            RpcError::invalid(format!("unknown parameter '{path}' (see param.list)"))
        })
    }

    pub fn set_param(&mut self, path: &str, value: f64, origin: &str) -> Result<ParamValue, RpcError> {
        let id = self.param_id(path)?;
        if !value.is_finite() {
            return Err(RpcError::invalid("value must be a finite number"));
        }
        let value = self.params[id].info.clamp(value);
        if self.params[id].value != value {
            self.params[id].value = value;
            let target = self.params[id].target;
            self.send(Command::SetParam { target, value: value as f32 });
            self.emit(origin, Event::ParamChanged { path: path.to_string(), value });
            self.mark_dirty(origin);
            if target == ParamTarget::Global(params::LENGTH) {
                self.refresh_controller(origin, false);
            }
        }
        Ok(ParamValue { path: path.to_string(), value })
    }

    // ---- pattern -----------------------------------------------------------

    fn check_step(step: u32) -> Result<(), RpcError> {
        if step as usize >= MAX_STEPS {
            return Err(RpcError::invalid(format!("step must be 0..{}", MAX_STEPS - 1)));
        }
        Ok(())
    }

    pub fn set_step(
        &mut self,
        id: &str,
        voice: Voice,
        step: u32,
        level: u8,
        origin: &str,
    ) -> Result<StepResult, RpcError> {
        Self::check_step(step)?;
        if level > STEP_ACCENT {
            return Err(RpcError::invalid("level must be 0 (off), 1 (on), or 2 (accent)"));
        }
        let t = voice.index();
        if self.drums(id)[t][step as usize] != level {
            self.drums_mut(id)[t][step as usize] = level;
            let slot = self.slot_of(id);
            self.send(Command::SetDrumStep { slot, track: t as u8, step: step as u8, level });
            self.emit(origin, Event::StepChanged { instrument: id.to_string(), voice, step, level });
            self.mark_dirty(origin);
            self.refresh_controller(origin, false);
        }
        Ok(StepResult { instrument: id.to_string(), voice, step, level })
    }

    fn set_track(&mut self, id: &str, voice: Voice, steps: &[u8], origin: &str) -> Result<TrackPattern, RpcError> {
        if steps.len() > MAX_STEPS {
            return Err(RpcError::invalid(format!("at most {MAX_STEPS} steps")));
        }
        if steps.iter().any(|s| *s > STEP_ACCENT) {
            return Err(RpcError::invalid("step levels must be 0, 1, or 2"));
        }
        let mut full = [STEP_OFF; MAX_STEPS];
        full[..steps.len()].copy_from_slice(steps);
        let t = voice.index();
        if self.drums(id)[t] != full {
            self.drums_mut(id)[t] = full;
            let slot = self.slot_of(id);
            self.send(Command::SetDrumTrack { slot, track: t as u8, steps: full });
            self.emit(origin, Event::PatternChanged { instrument: id.to_string(), voice, steps: full.to_vec() });
            self.mark_dirty(origin);
            self.refresh_controller(origin, false);
        }
        Ok(TrackPattern { voice, steps: full.to_vec() })
    }

    fn notes_result(&self, id: &str) -> NotesResult {
        let steps = match self.patterns.get(id) {
            Some(PatternState::Notes(n)) => n.to_vec(),
            _ => Vec::new(),
        };
        NotesResult { instrument: id.to_string(), length: self.length(), steps }
    }

    fn set_notes(&mut self, id: &str, steps: &[NoteStep], origin: &str) -> Result<NotesResult, RpcError> {
        if steps.len() > MAX_STEPS {
            return Err(RpcError::invalid(format!("at most {MAX_STEPS} steps")));
        }
        if let Some(bad) = steps.iter().filter_map(|s| s.note).find(|n| !(NOTE_MIN..=NOTE_MAX).contains(n)) {
            return Err(RpcError::invalid(format!("note {bad} out of range ({NOTE_MIN}..{NOTE_MAX})")));
        }
        let mut full = [NoteStep::default(); MAX_STEPS];
        full[..steps.len()].copy_from_slice(steps);
        let slot = self.slot_of(id);
        let changed = match self.patterns.get_mut(id) {
            Some(PatternState::Notes(n)) if **n != full => {
                **n = full;
                true
            }
            _ => false,
        };
        if changed {
            self.send(Command::SetNotes { slot, steps: full });
            self.emit(origin, Event::NotesChanged { instrument: id.to_string(), steps: full.to_vec() });
            self.mark_dirty(origin);
        }
        Ok(self.notes_result(id))
    }

    // ---- transport ---------------------------------------------------------

    fn play(&mut self, origin: &str) -> TransportState {
        self.send(Command::Play);
        if !self.playing {
            self.playing = true;
            self.emit(origin, Event::Transport { playing: true });
        }
        TransportState { playing: true, step: self.playhead }
    }

    fn stop(&mut self, origin: &str) -> TransportState {
        self.send(Command::Stop);
        if self.playing {
            self.playing = false;
            self.playhead = None;
            self.emit(origin, Event::Transport { playing: false });
            self.refresh_controller(origin, false);
        }
        TransportState { playing: false, step: None }
    }

    // ---- controller --------------------------------------------------------

    fn controller_pad(&mut self, row: u32, col: u32, pressed: bool, origin: &str) -> Result<(), RpcError> {
        if row as usize >= NUM_TRACKS || col as usize >= crate::controller::GRID {
            return Err(RpcError::invalid("row and col must be 0..7"));
        }
        let Some(target) = self.controller.target.clone() else { return Ok(()) };
        if !pressed {
            return Ok(());
        }
        let step = self.controller.pad_step(col);
        if step >= self.length() {
            return Ok(());
        }
        let voice = Voice::from_index(row as usize).unwrap();
        let cur = self.drums(&target)[row as usize][step as usize];
        let next = if cur == STEP_OFF { STEP_ON } else { STEP_OFF };
        self.set_step(&target, voice, step, next, origin)?;
        Ok(())
    }

    fn controller_knob(&mut self, index: u32, value: f64, origin: &str) -> Result<(), RpcError> {
        if index as usize >= NUM_TRACKS {
            return Err(RpcError::invalid("knob index must be 0..7"));
        }
        let Some(target) = self.controller.target.clone() else { return Ok(()) };
        let path = self.controller.knob_mode.param_path(&target, index as usize);
        let id = self.param_id(&path)?;
        let v = value.clamp(0.0, 1.0);
        let scaled = match self.params[id].info.kind {
            ParamKind::Continuous { min, max } => min + v * (max - min),
            ParamKind::Integer { min, max } => min as f64 + v * (max - min) as f64,
            ParamKind::Toggle => v,
        };
        self.set_param(&path, scaled, origin)?;
        Ok(())
    }

    fn controller_mode(&mut self, p: ControllerModeParams, origin: &str) -> Result<ControllerState, RpcError> {
        if let Some(t) = &p.target {
            let id = self.resolve(Some(t), InstrumentType::Tr808)?;
            self.controller.target = Some(id);
        }
        if let Some(m) = p.knob_mode {
            self.controller.knob_mode = m;
        }
        if let Some(f) = p.follow {
            self.controller.follow = f;
        }
        if let Some(page) = p.page {
            self.controller.page = page.min(Controller::num_pages(self.length()) - 1);
            // Manually choosing a page while playing implies not following.
            if p.follow.is_none() && self.playing {
                self.controller.follow = false;
            }
        }
        self.mark_dirty(origin);
        self.refresh_controller(origin, true);
        Ok(self.controller.state())
    }

    /// Handle a raw message from a connected MIDI device.
    pub fn handle_midi(&mut self, msg: MidiMessage) {
        let origin = format!("midi:{}", msg.port);
        self.emit(&origin, Event::MidiIn { port: msg.port.clone(), data: msg.data.clone() });
        let d = &msg.data;
        match msg.kind {
            DeviceKind::LividBlock => match decode_block(&self.block_map, d) {
                Some(BlockInput::Pad { row, col, pressed }) => {
                    let _ = self.controller_pad(row as u32, col as u32, pressed, &origin);
                }
                Some(BlockInput::Knob { index, value }) => {
                    let _ = self.controller_knob(index as u32, value, &origin);
                }
                None => {}
            },
            DeviceKind::GenericDrums => {
                if d.len() >= 3
                    && d[0] & 0xf0 == 0x90
                    && d[2] > 0
                    && let Some(v) = Voice::ALL.iter().find(|v| v.gm_note() == d[1])
                    && let Some(target) = self.controller.target.clone()
                {
                    let slot = self.slot_of(&target);
                    self.send(Command::Trigger { slot, voice: v.index() as u8, velocity: d[2] as f32 / 127.0 });
                }
            }
            DeviceKind::Keyboard => {
                if d.len() < 3 {
                    return;
                }
                let configured =
                    self.midi.connections().into_iter().find(|c| c.input == msg.port).and_then(|c| c.instrument);
                let Ok(id) = self.resolve(configured.as_deref(), InstrumentType::Tb303) else { return };
                let slot = self.slot_of(&id);
                let (status, note, vel) = (d[0] & 0xf0, d[1], d[2]);
                if status == 0x90 && vel > 0 {
                    self.held_notes.insert(slot, (msg.port.clone(), note));
                    let velocity = vel as f32 / 127.0;
                    self.send(Command::NoteOn { slot, note, velocity, gate: false });
                } else if (status == 0x80 || status == 0x90)
                    && self.held_notes.get(&slot) == Some(&(msg.port.clone(), note))
                {
                    self.held_notes.remove(&slot);
                    self.send(Command::NoteOff { slot });
                }
            }
        }
    }

    // ---- midi --------------------------------------------------------------

    fn midi_ports(&self) -> MidiPortsResult {
        let (inputs, outputs) = list_ports();
        MidiPortsResult { inputs, outputs, connections: self.midi.connections() }
    }

    fn midi_changed(&mut self, origin: &str) {
        // Release notes held by keyboards that went away.
        let ports: Vec<String> = self.midi.connections().into_iter().map(|c| c.input).collect();
        let orphaned: Vec<u8> =
            self.held_notes.iter().filter(|(_, (p, _))| !ports.contains(p)).map(|(slot, _)| *slot).collect();
        for slot in orphaned {
            self.held_notes.remove(&slot);
            self.send(Command::NoteOff { slot });
        }
        self.controller.device = self.midi.block_name();
        let connections = self.midi.connections();
        self.emit(origin, Event::Midi { connections });
        self.push_all_leds();
        self.refresh_controller(origin, true);
    }

    pub fn midi_connect(&mut self, p: MidiConnectParams, origin: &str) -> Result<MidiPortsResult, RpcError> {
        if let Some(id) = &p.instrument {
            self.resolve(Some(id), InstrumentType::Tb303)?;
        }
        self.midi
            .connect(&p.input, p.output.as_deref(), p.kind, p.instrument.clone(), self.midi_tx.clone())
            .map_err(|e| RpcError::failed(e.to_string()))?;
        self.midi_changed(origin);
        Ok(self.midi_ports())
    }

    /// Hotplug: drop vanished ports and, if `auto`, connect any Livid Block
    /// that appeared.
    pub fn midi_autoconnect(&mut self, auto: bool) {
        let (inputs, _) = list_ports();
        let mut changed = self.midi.prune(&inputs);
        if changed {
            tracing::info!("MIDI device disconnected");
        }
        if auto
            && self.midi.block_name().is_none()
            && let Some(name) = inputs.iter().find(|n| n.to_lowercase().contains("block"))
        {
            match self.midi.connect(name, None, DeviceKind::LividBlock, None, self.midi_tx.clone()) {
                Ok(c) => {
                    tracing::info!("auto-connected Livid Block: {} (output: {:?})", c.input, c.output);
                    changed = true;
                }
                Err(e) => tracing::warn!("auto-connect {name} failed: {e}"),
            }
        }
        if changed {
            self.midi_changed("engine");
        }
    }

    // ---- engine feedback ---------------------------------------------------

    pub fn handle_feedback(&mut self, fb: Feedback) {
        match fb {
            Feedback::Step { step, time } => {
                if !self.playing {
                    return;
                }
                self.playhead = Some(step);
                if self.controller.follow {
                    self.controller.page = step / crate::controller::GRID as u32;
                }
                self.emit("engine", Event::Playhead { step, time });
                self.refresh_controller("engine", false);
            }
            Feedback::Trigger { slot, voice, note, velocity, time, .. } => {
                if let Some(inst) = self.instruments.iter().find(|i| i.slot == slot) {
                    let instrument = inst.id.clone();
                    let voice = voice.and_then(|v| Voice::from_index(v as usize));
                    self.emit("engine", Event::Trigger { instrument, voice, note, velocity, time });
                }
            }
            Feedback::Stopped { .. } => {}
            Feedback::Meters { channels, master } => {
                let levels: Vec<ChannelLevel> = self
                    .channels
                    .iter()
                    .map(|c| {
                        let [left, right] = channels[(c.n - 1) as usize];
                        ChannelLevel { channel: c.n, left, right }
                    })
                    .collect();
                let silent = levels.iter().all(|l| l.left < 1e-5 && l.right < 1e-5) && master.iter().all(|x| *x < 1e-5);
                if silent && self.meters_silent {
                    return;
                }
                self.meters_silent = silent;
                self.emit("engine", Event::Meters { channels: levels, master: master.to_vec() });
            }
        }
    }

    // ---- projects ----------------------------------------------------------

    fn projects_dir(&self) -> PathBuf {
        self.data_dir.join("projects")
    }

    /// Resolve a user-supplied project path to an engine-side bundle dir.
    pub fn resolve_project_path(&self, p: &str) -> PathBuf {
        let mut path = PathBuf::from(p);
        if path.extension().and_then(|e| e.to_str()) != Some(PROJECT_EXTENSION) {
            let mut s = path.into_os_string();
            s.push(format!(".{PROJECT_EXTENSION}"));
            path = PathBuf::from(s);
        }
        if path.is_relative() { self.projects_dir().join(path) } else { path }
    }

    pub fn resolve_data_path(&self, p: &str) -> PathBuf {
        let path = PathBuf::from(p);
        if path.is_relative() { self.data_dir.join(path) } else { path }
    }

    fn to_project_file(&self) -> ProjectFile {
        let length = self.length() as usize;
        let mut patterns = BTreeMap::new();
        for i in &self.instruments {
            match &self.patterns[&i.id] {
                PatternState::Drums(d) => {
                    let tracks: BTreeMap<Voice, String> = Voice::ALL
                        .iter()
                        .filter(|v| d[v.index()].iter().any(|s| *s != STEP_OFF))
                        .map(|v| (*v, steps_for_file(&d[v.index()], length)))
                        .collect();
                    if !tracks.is_empty() {
                        patterns.insert(i.id.clone(), ProjectPattern::Drums(tracks));
                    }
                }
                PatternState::Notes(n) => {
                    if let Some(last) = n.iter().rposition(|s| s.note.is_some()) {
                        let end = if last < length { length } else { (last + 1).div_ceil(16) * 16 };
                        patterns.insert(i.id.clone(), ProjectPattern::Notes(format_notes(&n[..], end)));
                    }
                }
            }
        }
        ProjectFile {
            format_version: PROJECT_FORMAT_VERSION,
            instruments: self
                .instruments
                .iter()
                .map(|i| ProjectInstrument { id: i.id.clone(), kind: i.kind, name: i.name.clone() })
                .collect(),
            channels: self.channels.clone(),
            routes: self.routes.clone(),
            params: self.params.iter().map(|p| (p.info.path.clone(), p.value)).collect(),
            patterns,
            controller: ProjectController {
                target: self.controller.target.clone(),
                knob_mode: self.controller.knob_mode,
                follow: self.controller.follow,
            },
        }
    }

    /// Check a project file can be applied in full; nothing changes on error.
    fn validate_project(&self, file: &ProjectFile) -> Result<(), RpcError> {
        if file.instruments.len() > MAX_INSTRUMENTS {
            return Err(RpcError::invalid(format!("project has more than {MAX_INSTRUMENTS} instruments")));
        }
        if file.channels.len() > MAX_CHANNELS {
            return Err(RpcError::invalid(format!("project has more than {MAX_CHANNELS} channels")));
        }
        let mut ids = std::collections::HashSet::new();
        for i in &file.instruments {
            validate_instrument_id(&i.id).map_err(RpcError::invalid)?;
            if !ids.insert(&i.id) {
                return Err(RpcError::invalid(format!("duplicate instrument id '{}'", i.id)));
            }
        }
        let mut ns = std::collections::HashSet::new();
        for c in &file.channels {
            if c.n == 0 || c.n as usize > MAX_CHANNELS || !ns.insert(c.n) {
                return Err(RpcError::invalid(format!("invalid or duplicate channel number {}", c.n)));
            }
        }
        if self.in_flight + self.instruments.len() > RETURN_CAPACITY {
            return Err(RpcError::failed(format!(
                "{} removed instruments are still waiting to be returned by the audio engine",
                self.in_flight
            )));
        }
        // Exactly what `apply_project_file` sends: teardown, channels and
        // their parameters, instruments with their parameters and patterns,
        // routes, and globals.
        let instrument_params: usize =
            file.instruments.iter().map(|i| instrument::params(i.kind, &i.id).len()).sum();
        let needed = self.instruments.len()
            + self.channels.len()
            + file.channels.len() * (1 + CHANNEL_PARAMS)
            + file.instruments.len() * (1 + NUM_TRACKS)
            + instrument_params
            + file.routes.len()
            + NUM_GLOBALS;
        self.ensure_room(needed)
    }

    /// Replace all state from a project file and resync the engine. All or
    /// nothing: the file is validated before anything changes.
    fn apply_project_file(&mut self, file: &ProjectFile, origin: &str) -> Result<Vec<String>, RpcError> {
        self.validate_project(file)?;
        let mut warnings = Vec::new();

        // Tear down.
        for inst in std::mem::take(&mut self.instruments) {
            self.send(Command::RemoveInstrument { slot: inst.slot });
            self.in_flight += 1;
        }
        for c in std::mem::take(&mut self.channels) {
            self.send(Command::SetChannelActive { ch: (c.n - 1) as u8, active: false });
        }
        self.slot_used = [false; MAX_INSTRUMENTS];
        self.held_notes.clear();
        self.routes.clear();
        self.patterns.clear();
        self.params.clear();

        // Build.
        for c in &file.channels {
            self.send(Command::SetChannelActive { ch: (c.n - 1) as u8, active: true });
            self.channels.push(c.clone());
        }
        for (slot, i) in file.instruments.iter().enumerate() {
            self.add_instrument_unchecked(&i.id, i.kind, &i.name, slot as u8);
        }
        for (source, n) in &file.routes {
            match (self.find_source(source), self.channels.iter().any(|c| c.n == *n)) {
                (Some((id, o)), true) => self.set_route_unchecked(&id, o, Some(*n)),
                _ => warnings.push(format!("ignored route {source} -> {n}")),
            }
        }
        self.rebuild_params();
        for (path, v) in &file.params {
            match self.index.get(path) {
                Some(id) => self.params[*id].value = self.params[*id].info.clamp(*v),
                None => warnings.push(format!("ignored unknown parameter '{path}'")),
            }
        }
        let cmds: Vec<Command> =
            self.params.iter().map(|p| Command::SetParam { target: p.target, value: p.value as f32 }).collect();
        for c in cmds {
            self.send(c);
        }
        for (id, pattern) in &file.patterns {
            let slot = self.slot_of(id);
            match (self.patterns.get_mut(id), pattern) {
                (Some(PatternState::Drums(d)), ProjectPattern::Drums(tracks)) => {
                    let mut cmds = Vec::new();
                    for (voice, s) in tracks {
                        match parse_steps(s) {
                            Ok(steps) => {
                                d[voice.index()][..steps.len()].copy_from_slice(&steps);
                                cmds.push(Command::SetDrumTrack { slot, track: voice.index() as u8, steps: d[voice.index()] });
                            }
                            Err(e) => warnings.push(format!("{id}.{}: {e}", voice.id())),
                        }
                    }
                    for c in cmds {
                        self.send(c);
                    }
                }
                (Some(PatternState::Notes(n)), ProjectPattern::Notes(s)) => match parse_notes(s) {
                    Ok(steps) => {
                        n.copy_from_slice(&steps);
                        let steps = **n;
                        self.send(Command::SetNotes { slot, steps });
                    }
                    Err(e) => warnings.push(format!("{id}: {e}")),
                },
                _ => warnings.push(format!("ignored pattern for '{id}' (no matching instrument)")),
            }
        }

        let target = file.controller.target.clone().filter(|t| {
            self.instruments.iter().any(|i| &i.id == t && i.kind == InstrumentType::Tr808)
        });
        self.controller.target = target.or_else(|| {
            self.instruments.iter().find(|i| i.kind == InstrumentType::Tr808).map(|i| i.id.clone())
        });
        self.controller.knob_mode = file.controller.knob_mode;
        self.controller.follow = file.controller.follow;
        self.controller.page = 0;
        self.emit(origin, Event::Reset);
        self.refresh_controller(origin, true);
        Ok(warnings)
    }

    pub fn project_new(&mut self, origin: &str) -> Result<ProjectInfo, RpcError> {
        self.apply_project_file(&default_project(), origin)?;
        self.project = ProjectInfo { path: None, dirty: false };
        let info = self.project.clone();
        self.emit(origin, Event::Project { info: info.clone() });
        Ok(info)
    }

    pub fn project_save(&mut self, path: Option<String>, origin: &str) -> Result<ProjectInfo, RpcError> {
        let bundle = match path {
            Some(p) => self.resolve_project_path(&p),
            None => PathBuf::from(
                self.project.path.clone().ok_or_else(|| RpcError::invalid("no current project path; pass a path"))?,
            ),
        };
        std::fs::create_dir_all(&bundle).map_err(|e| RpcError::failed(format!("create {}: {e}", bundle.display())))?;
        let json = project_to_json(&self.to_project_file());
        let file = bundle.join(PROJECT_FILE_NAME);
        std::fs::write(&file, json).map_err(|e| RpcError::failed(format!("write {}: {e}", file.display())))?;
        self.project = ProjectInfo { path: Some(bundle.to_string_lossy().into_owned()), dirty: false };
        let info = self.project.clone();
        self.emit(origin, Event::Project { info: info.clone() });
        Ok(info)
    }

    pub fn project_load(&mut self, path: &str, origin: &str) -> Result<ProjectInfo, RpcError> {
        let bundle = self.resolve_project_path(path);
        let file = bundle.join(PROJECT_FILE_NAME);
        let json = std::fs::read_to_string(&file)
            .map_err(|e| RpcError::failed(format!("read {}: {e}", file.display())))?;
        let project = parse_project(&json).map_err(|e| RpcError::failed(format!("{}: {e}", file.display())))?;
        for w in self.apply_project_file(&project, origin)? {
            tracing::warn!("{}: {w}", file.display());
        }
        self.project = ProjectInfo { path: Some(bundle.to_string_lossy().into_owned()), dirty: false };
        let info = self.project.clone();
        self.emit(origin, Event::Project { info: info.clone() });
        Ok(info)
    }

    fn project_list(&self) -> ProjectListResult {
        let mut projects: Vec<String> = std::fs::read_dir(self.projects_dir())
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(PROJECT_EXTENSION))
                    .filter(|p| p.join(PROJECT_FILE_NAME).exists())
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        projects.sort();
        ProjectListResult { projects }
    }

    // ---- dispatch ----------------------------------------------------------

    /// Handle a request. Connection-level methods (hello, subscribe, render)
    /// are handled by the server before reaching here.
    pub fn handle(&mut self, req: Request, origin: &str) -> RpcResult {
        let drum = |c: &Self, id: &Option<String>| c.resolve(id.as_deref(), InstrumentType::Tr808);
        match req {
            Request::StateGet(_) => ok(self.snapshot()),
            Request::ParamList(p) => {
                let params = self
                    .params
                    .iter()
                    .map(|e| &e.info)
                    .filter(|i| p.prefix.as_ref().is_none_or(|pre| i.path.starts_with(pre.as_str())))
                    .cloned()
                    .collect();
                ok(ParamListResult { params })
            }
            Request::ParamGet(p) => {
                let id = self.param_id(&p.path)?;
                ok(ParamValue { path: p.path, value: self.params[id].value })
            }
            Request::ParamSet(p) => ok(self.set_param(&p.path, p.value, origin)?),
            Request::TransportPlay(_) => ok(self.play(origin)),
            Request::TransportStop(_) => ok(self.stop(origin)),
            Request::InstrumentTypes(_) => ok(self.instrument_types()),
            Request::InstrumentList(_) => ok(InstrumentListResult { instruments: self.graph().instruments }),
            Request::InstrumentAdd(p) => ok(self.instrument_add(p, origin)?),
            Request::InstrumentRemove(p) => ok(self.instrument_remove(p, origin)?),
            Request::ChannelAdd(p) => ok(self.channel_add(p.name, origin)?),
            Request::ChannelRemove(p) => ok(self.channel_remove(p.n, origin)?),
            Request::ChannelRename(p) => ok(self.channel_rename(p.n, p.name, origin)?),
            Request::RouteSet(p) => ok(self.route_set(p, origin)?),
            Request::PatternGet(p) => {
                let id = drum(self, &p.instrument)?;
                ok(self.pattern_result(&id, p.voice))
            }
            Request::PatternSet(p) => {
                let id = drum(self, &p.instrument)?;
                ok(self.set_track(&id, p.voice, &p.steps, origin)?)
            }
            Request::PatternSetStep(p) => {
                let id = drum(self, &p.instrument)?;
                ok(self.set_step(&id, p.voice, p.step, p.level, origin)?)
            }
            Request::PatternToggleStep(p) => {
                let id = drum(self, &p.instrument)?;
                Self::check_step(p.step)?;
                let cur = self.drums(&id)[p.voice.index()][p.step as usize];
                let next = if cur == STEP_OFF { STEP_ON } else { STEP_OFF };
                ok(self.set_step(&id, p.voice, p.step, next, origin)?)
            }
            Request::PatternClear(p) => {
                let id = drum(self, &p.instrument)?;
                for v in Voice::ALL {
                    if p.voice.is_none_or(|x| x == v) {
                        self.set_track(&id, v, &[], origin)?;
                    }
                }
                ok(self.pattern_result(&id, p.voice))
            }
            Request::PatternGetNotes(p) => {
                let id = self.resolve(p.instrument.as_deref(), InstrumentType::Tb303)?;
                ok(self.notes_result(&id))
            }
            Request::PatternSetNotes(p) => {
                let id = self.resolve(p.instrument.as_deref(), InstrumentType::Tb303)?;
                ok(self.set_notes(&id, &p.steps, origin)?)
            }
            Request::PatternSetNote(p) => {
                let id = self.resolve(p.instrument.as_deref(), InstrumentType::Tb303)?;
                Self::check_step(p.step)?;
                let mut steps = self.notes_result(&id).steps;
                steps[p.step as usize] = p.note;
                ok(self.set_notes(&id, &steps, origin)?)
            }
            Request::VoiceTrigger(p) => {
                let velocity = p.velocity.unwrap_or(1.0).clamp(0.0, 1.0);
                match (p.voice, p.note) {
                    (Some(voice), None) => {
                        let id = drum(self, &p.instrument)?;
                        let slot = self.slot_of(&id);
                        self.send(Command::Trigger { slot, voice: voice.index() as u8, velocity });
                    }
                    (None, Some(note)) => {
                        if !(NOTE_MIN..=NOTE_MAX).contains(&note) {
                            return Err(RpcError::invalid(format!("note must be {NOTE_MIN}..{NOTE_MAX}")));
                        }
                        let id = self.resolve(p.instrument.as_deref(), InstrumentType::Tb303)?;
                        let slot = self.slot_of(&id);
                        self.send(Command::NoteOn { slot, note, velocity, gate: true });
                    }
                    _ => return Err(RpcError::invalid("pass exactly one of `voice` (drums) or `note` (notes)")),
                }
                ok(Empty {})
            }
            Request::ControllerGet(_) => ok(self.controller.state()),
            Request::ControllerPress(p) => {
                match p.pressed {
                    Some(pressed) => self.controller_pad(p.row, p.col, pressed, origin)?,
                    None => {
                        self.controller_pad(p.row, p.col, true, origin)?;
                        self.controller_pad(p.row, p.col, false, origin)?;
                    }
                }
                ok(self.controller.state())
            }
            Request::ControllerKnob(p) => {
                self.controller_knob(p.index, p.value, origin)?;
                ok(self.controller.state())
            }
            Request::ControllerSetMode(p) => ok(self.controller_mode(p, origin)?),
            Request::MidiPorts(_) => ok(self.midi_ports()),
            Request::MidiConnect(p) => ok(self.midi_connect(p, origin)?),
            Request::MidiDisconnect(p) => {
                self.midi.disconnect(&p.input).map_err(|e| RpcError::failed(e.to_string()))?;
                self.midi_changed(origin);
                ok(self.midi_ports())
            }
            Request::ProjectNew(_) => ok(self.project_new(origin)?),
            Request::ProjectSave(p) => ok(self.project_save(p.path, origin)?),
            Request::ProjectLoad(p) => ok(self.project_load(&p.path, origin)?),
            Request::ProjectList(_) => ok(self.project_list()),
            Request::EngineStatus(_) => ok(self.audio.clone()),
            Request::Hello(_)
            | Request::EventsSubscribe(_)
            | Request::EventsUnsubscribe(_)
            | Request::RenderOffline(_)
            | Request::DaemonInfo(_)
            | Request::DaemonShutdown(_) => Err(RpcError::failed("handled by connection")),
        }
    }
}

/// A new project: one `tr808` (`drums`) on channel 1, "Drums".
fn default_project() -> ProjectFile {
    ProjectFile {
        format_version: PROJECT_FORMAT_VERSION,
        instruments: vec![ProjectInstrument { id: "drums".into(), kind: InstrumentType::Tr808, name: "Drums".into() }],
        channels: vec![ChannelInfo { n: 1, name: "Drums".into() }],
        routes: BTreeMap::from([("drums".to_string(), 1)]),
        params: BTreeMap::new(),
        patterns: BTreeMap::new(),
        controller: ProjectController { target: Some("drums".into()), knob_mode: KnobMode::Volume, follow: true },
    }
}

/// Used by tests and the server for a JSON error body.
pub fn error_json(code: i32, message: &str) -> Value {
    json!({ "code": code, "message": message })
}
