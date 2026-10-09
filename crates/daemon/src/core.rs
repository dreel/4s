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
use crate::journal::{self, Doc, History, Journal};
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
    /// The note each instrument slot is holding (`voice.note_on`, or a MIDI
    /// keyboard), and who holds it: a client connection (`conn:<id>`) or a
    /// keyboard (`midi:<port>`). Only the holder's note-off for that note
    /// releases it; a holder that disconnects releases its notes.
    held_notes: HashMap<u8, (String, u8)>,
    project: ProjectInfo,
    pub audio: AudioStatus,
    seq: u64,
    events: broadcast::Sender<Arc<EventEnvelope>>,
    commands: Producer<Command>,
    data_dir: PathBuf,
    meters_silent: bool,
    journal: Journal,
    history: History,
    /// Where the current history segment starts (daemon start, or the last
    /// project new/load/import): the journal seq and the project then.
    segment_seq: u64,
    segment_base: ProjectFile,
    /// The user of clients that do not name one, and of MIDI input.
    host_user: String,
}

impl Core {
    pub fn new(
        commands: Producer<Command>,
        midi_tx: Sender<MidiMessage>,
        data_dir: PathBuf,
        audio: AudioStatus,
        journal_dir: Option<PathBuf>,
        host_user: String,
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
            journal: Journal::new(journal_dir),
            history: History::default(),
            segment_seq: 0,
            segment_base: default_project(),
            host_user,
        };
        core.rebuild_params();
        core.apply_project_file(&default_project(), "engine").expect("default project applies");
        core.start_segment();
        core
    }

    /// Start a history segment at the current state (see `journal.export`).
    fn start_segment(&mut self) {
        self.segment_seq = self.journal.seq();
        self.segment_base = self.to_project_file();
        self.journal.push_segment(&self.segment_base, unix_time());
    }

    fn journal_export(&self) -> Result<Recording, RpcError> {
        let entries = self.journal.since(self.segment_seq).ok_or_else(|| {
            RpcError::failed(
                "the start of this session is no longer in memory (over 10000 entries); \
                 replay the journal file under <data-dir>/journal instead",
            )
        })?;
        Ok(Recording {
            format_version: RECORDING_FORMAT_VERSION,
            base: self.segment_base.clone(),
            entries,
            digest: journal::digest(&self.doc()),
        })
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

    fn slot_of(&self, id: &str) -> Option<u8> {
        self.instruments.iter().find(|i| i.id == id).map(|i| i.slot)
    }

    /// Engine slot of an instrument id, or an error naming the instruments.
    fn slot(&self, id: &str) -> Result<u8, RpcError> {
        self.slot_of(id).ok_or_else(|| {
            RpcError::invalid(format!("no instrument '{id}' (instruments: {})", self.instrument_ids()))
        })
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
        if p.no_channel && p.channel.is_some() {
            return Err(RpcError::invalid("pass `channel` or `no_channel`, not both"));
        }
        let channel = if p.no_channel {
            None
        } else if let Some(n) = p.channel {
            if !self.channels.iter().any(|c| c.n == n) {
                return Err(RpcError::invalid(format!("no channel {n}")));
            }
            Some(n)
        } else if let Some(n) = self.first_empty_channel() {
            Some(n)
        } else {
            if self.channels.len() >= MAX_CHANNELS {
                return Err(RpcError::invalid(format!("at most {MAX_CHANNELS} channels")));
            }
            None
        };
        self.ensure_room(ADD_COMMANDS)?;

        let name = check_name(p.name)?.unwrap_or_else(|| match id.strip_prefix(p.kind.default_id()) {
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
        let Some(slot) = self.slot_of(id) else { return };
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
        // The removal, plus deactivating every channel it may leave empty.
        self.ensure_room(1 + self.instruments[pos].outputs.len())?;
        let fed = self.teardown_instrument(pos, origin);
        if !p.keep_channels {
            for n in fed {
                if !self.routes.values().any(|c| *c == n) && self.channels.iter().any(|c| c.n == n) {
                    self.remove_channel_unchecked(n);
                }
            }
        }
        self.graph_changed(origin);
        self.refresh_controller(origin, true);
        Ok(self.graph())
    }

    /// Remove an instrument from the engine and the graph (its routes
    /// included), retargeting the controller and keyboards that used it.
    /// Returns the channels its outputs fed. The caller checks queue room and
    /// `in_flight` first, and emits `graph`.
    fn teardown_instrument(&mut self, pos: usize, origin: &str) -> Vec<u32> {
        let inst = self.instruments.remove(pos);
        self.send(Command::RemoveInstrument { slot: inst.slot });
        self.slot_used[inst.slot as usize] = false;
        self.in_flight += 1;
        self.patterns.remove(&inst.id);
        let fed: Vec<u32> = inst.outputs.iter().filter_map(|o| self.routes.remove(&o.source)).collect();
        if self.controller.target.as_deref() == Some(inst.id.as_str()) {
            self.controller.target =
                self.instruments.iter().find(|i| i.kind == InstrumentType::Tr808).map(|i| i.id.clone());
        }
        self.held_notes.remove(&inst.slot);
        if self.midi.clear_instrument(&inst.id) {
            let connections = self.midi.connections();
            self.emit(origin, Event::Midi { connections });
        }
        fed
    }

    /// The first channel, in display order, that nothing is routed to.
    fn first_empty_channel(&self) -> Option<u32> {
        self.channels.iter().map(|c| c.n).find(|n| !self.routes.values().any(|r| r == n))
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
        let name = check_name(name)?.unwrap_or_else(|| format!("Ch {n}"));
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
        let name = check_name(Some(name))?.unwrap_or_default();
        let c = self.channels.iter_mut().find(|c| c.n == n).unwrap();
        c.name = name;
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
        // With `swap`, whatever else feeds the target channel takes this
        // source's old place. If it had none, they stay and share the channel,
        // so nothing goes silent.
        let old = self.routes.get(&p.source).copied();
        let displaced: Vec<(String, usize)> = match p.channel {
            Some(n) if p.swap && old.is_some() => self
                .routes
                .iter()
                .filter(|(s, c)| **c == n && **s != p.source)
                .filter_map(|(s, _)| self.find_source(s))
                .collect(),
            _ => Vec::new(),
        };
        self.ensure_room(2 * (1 + displaced.len()))?;
        for (did, doutput) in &displaced {
            self.set_route_unchecked(did, *doutput, old);
        }
        self.set_route_unchecked(&id, output, p.channel);
        self.graph_changed(origin);
        Ok(self.graph())
    }

    pub fn channel_move(&mut self, p: ChannelMoveParams, origin: &str) -> Result<Graph, RpcError> {
        self.check_channel(p.n)?;
        let len = self.channels.len() as u32;
        if p.position < 1 || p.position > len {
            return Err(RpcError::invalid(format!("position must be 1..={len}")));
        }
        let from = self.channels.iter().position(|c| c.n == p.n).unwrap();
        let c = self.channels.remove(from);
        self.channels.insert(p.position as usize - 1, c);
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
            let slot = self.slot(id)?;
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
            let slot = self.slot(id)?;
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
        let slot = self.slot(id)?;
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

    // ---- held notes ----------------------------------------------------------

    /// Start a held note on a note instrument (default: the first `tb303`).
    /// A newer note takes over the voice (gliding, as legato on a 303).
    fn note_on(&mut self, id: Option<&str>, note: u8, velocity: f32, holder: &str) -> Result<(), RpcError> {
        if !(NOTE_MIN..=NOTE_MAX).contains(&note) {
            return Err(RpcError::invalid(format!("note must be {NOTE_MIN}..{NOTE_MAX}")));
        }
        let id = self.resolve(id, InstrumentType::Tb303)?;
        let slot = self.slot(&id)?;
        self.ensure_room(1)?;
        self.held_notes.insert(slot, (holder.to_string(), note));
        self.send(Command::NoteOn { slot, note, velocity: velocity.clamp(0.0, 1.0), gate: false });
        Ok(())
    }

    /// Release a held note, if `holder` is holding exactly that note. With
    /// no instrument given, it goes to whichever instrument the holder
    /// started that note on (not re-resolved, so adding or removing a `tb303`
    /// in between cannot leave it stuck).
    fn note_off(&mut self, id: Option<&str>, note: u8, holder: &str) -> Result<(), RpcError> {
        let held = (holder.to_string(), note);
        let slot = match id {
            Some(id) => {
                let id = self.resolve(Some(id), InstrumentType::Tb303)?;
                Some(self.slot(&id)?).filter(|s| self.held_notes.get(s) == Some(&held))
            }
            None => self.held_notes.iter().find(|(_, h)| **h == held).map(|(s, _)| *s),
        };
        if let Some(slot) = slot {
            // Never record a release the engine did not get.
            self.ensure_room(1)?;
            self.held_notes.remove(&slot);
            self.send(Command::NoteOff { slot });
        }
        Ok(())
    }

    /// Release every note a client connection holds (it disconnected). The
    /// release is sent even when the queue looks full: there is no caller to
    /// report an error to, and a dropped note-off is logged by `send`.
    pub fn release_held_by(&mut self, holder: &str) {
        let slots: Vec<u8> = self.held_notes.iter().filter(|(_, (h, _))| h == holder).map(|(s, _)| *s).collect();
        for slot in slots {
            self.held_notes.remove(&slot);
            self.send(Command::NoteOff { slot });
        }
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

    /// Handle a raw message from a connected MIDI device. Input that does
    /// something is turned into its equivalent RPC and handled (and
    /// journaled) as the host user, with the device (`midi:<port>`) as origin
    /// and as the holder of the notes it plays.
    pub fn handle_midi(&mut self, msg: MidiMessage) {
        let origin = format!("midi:{}", msg.port);
        self.emit(&origin, Event::MidiIn { port: msg.port.clone(), data: msg.data.clone() });
        let d = &msg.data;
        let req = match msg.kind {
            DeviceKind::LividBlock => match decode_block(&self.block_map, d) {
                // A release does nothing (pads toggle on press).
                Some(BlockInput::Pad { row, col, pressed: true }) => {
                    Some(Request::ControllerPress(PadParams { row: row as u32, col: col as u32, pressed: Some(true) }))
                }
                Some(BlockInput::Pad { pressed: false, .. }) => None,
                Some(BlockInput::Knob { index, value }) => {
                    Some(Request::ControllerKnob(KnobParams { index: index as u32, value }))
                }
                None => None,
            },
            DeviceKind::GenericDrums => match (d.len() >= 3 && d[0] & 0xf0 == 0x90 && d[2] > 0, &self.controller.target) {
                (true, Some(target)) => Voice::ALL.iter().find(|v| v.gm_note() == d[1]).map(|v| {
                    Request::VoiceTrigger(TriggerParams {
                        instrument: Some(target.clone()),
                        voice: Some(*v),
                        note: None,
                        velocity: Some(d[2] as f32 / 127.0),
                    })
                }),
                _ => None,
            },
            DeviceKind::Keyboard if d.len() >= 3 => {
                let instrument =
                    self.midi.connections().into_iter().find(|c| c.input == msg.port).and_then(|c| c.instrument);
                let (status, note, vel) = (d[0] & 0xf0, d[1], d[2]);
                if status == 0x90 && vel > 0 {
                    Some(Request::VoiceNoteOn(NoteParams { instrument, note, velocity: Some(vel as f32 / 127.0) }))
                } else if status == 0x80 || status == 0x90 {
                    Some(Request::VoiceNoteOff(NoteParams { instrument, note, velocity: None }))
                } else {
                    None
                }
            }
            DeviceKind::Keyboard => None,
        };
        if let Some(req) = req {
            let user = self.host_user.clone();
            let _ = self.handle(req, &origin, &origin, Some(&user));
        }
    }

    // ---- midi --------------------------------------------------------------

    fn midi_ports(&self) -> MidiPortsResult {
        let (inputs, outputs) = list_ports();
        MidiPortsResult { inputs, outputs, connections: self.midi.connections() }
    }

    fn midi_changed(&mut self, origin: &str) {
        // Release notes held by keyboards that went away.
        let holders: Vec<String> = self.midi.connections().into_iter().map(|c| format!("midi:{}", c.input)).collect();
        let orphaned: Vec<u8> = self
            .held_notes
            .iter()
            .filter(|(_, (h, _))| h.starts_with("midi:") && !holders.contains(h))
            .map(|(slot, _)| *slot)
            .collect();
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
            if p.kind != DeviceKind::Keyboard {
                return Err(RpcError::invalid("`instrument` only applies to --kind keyboard"));
            }
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
        for i in &file.instruments {
            check_name(Some(i.name.clone()))?;
        }
        let mut ns = std::collections::HashSet::new();
        for c in &file.channels {
            check_name(Some(c.name.clone()))?;
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
            self.channels.push(ChannelInfo { n: c.n, name: c.name.trim().to_string() });
        }
        for (slot, i) in file.instruments.iter().enumerate() {
            self.add_instrument_unchecked(&i.id, i.kind, i.name.trim(), slot as u8);
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
            let slot = self.slot_of(id).unwrap_or_default();
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
        // Keyboards set to play an instrument this project lacks fall back
        // to the first tb303, as after removing it.
        let mut midi_changed = false;
        for c in self.midi.connections() {
            if let Some(id) = c.instrument
                && !self.instruments.iter().any(|i| i.id == id)
            {
                midi_changed |= self.midi.clear_instrument(&id);
            }
        }
        if midi_changed {
            let connections = self.midi.connections();
            self.emit(origin, Event::Midi { connections });
        }
        self.emit(origin, Event::Reset);
        self.refresh_controller(origin, true);
        Ok(warnings)
    }

    pub fn project_new(&mut self, origin: &str) -> Result<ProjectInfo, RpcError> {
        self.apply_project_file(&default_project(), origin)?;
        self.start_segment();
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
        self.start_segment();
        self.project = ProjectInfo { path: Some(bundle.to_string_lossy().into_owned()), dirty: false };
        let info = self.project.clone();
        self.emit(origin, Event::Project { info: info.clone() });
        Ok(info)
    }

    /// Load a project sent inline. It is unsaved: no bundle path, dirty.
    pub fn project_import(&mut self, file: &ProjectFile, origin: &str) -> Result<ProjectInfo, RpcError> {
        for w in self.apply_project_file(file, origin)? {
            tracing::warn!("project.import: {w}");
        }
        self.start_segment();
        self.project = ProjectInfo { path: None, dirty: true };
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

    // ---- journal and undo ----------------------------------------------------

    /// The undoable state as flat keys (see `journal`): params and steps that
    /// differ from their defaults, instruments, channels and their order, and
    /// routes. Transport, MIDI connections, and the controller view are not
    /// in it.
    fn doc(&self) -> Doc {
        let mut d = Doc::new();
        for p in &self.params {
            if p.value != p.info.default {
                d.insert(format!("param:{}", p.info.path), json!(p.value));
            }
        }
        for i in &self.instruments {
            d.insert(format!("instrument:{}", i.id), json!({ "type": i.kind.id(), "name": i.name }));
        }
        for (id, pattern) in &self.patterns {
            match pattern {
                PatternState::Drums(tracks) => {
                    for (t, steps) in tracks.iter().enumerate() {
                        let voice = Voice::from_index(t).map(|v| v.id()).unwrap_or_default();
                        for (i, level) in steps.iter().enumerate().filter(|(_, l)| **l != STEP_OFF) {
                            d.insert(format!("step:{id}.{voice}.{i}"), json!(level));
                        }
                    }
                }
                PatternState::Notes(steps) => {
                    for (i, step) in steps.iter().enumerate().filter(|(_, s)| **s != NoteStep::default()) {
                        d.insert(format!("note:{id}.{i}"), json!(step));
                    }
                }
            }
        }
        for c in &self.channels {
            d.insert(format!("channel:{}", c.n), json!(c.name));
        }
        d.insert("channels:order".into(), json!(self.channels.iter().map(|c| c.n).collect::<Vec<_>>()));
        for (source, n) in &self.routes {
            d.insert(format!("route:{source}"), json!(n));
        }
        d
    }

    fn journal_context(&self) -> JournalContext {
        JournalContext { playing: self.playing, step: self.playhead, page: self.controller.page }
    }

    fn journal_push(&mut self, entry: JournalEntry, origin: &str) {
        self.emit(origin, Event::Journal { entry: entry.clone() });
        self.journal.push(entry);
    }

    fn emit_history(&mut self, user: &str, origin: &str) {
        let s = self.history.summary(user);
        self.emit(
            origin,
            Event::History {
                user: user.to_string(),
                undo_label: s.undo_label,
                redo_label: s.redo_label,
                undo_count: s.undo_count,
                redo_count: s.redo_count,
            },
        );
    }

    /// Undo (or redo) the user's last step, leaving keys another user
    /// changed since. Journaled as its own entry that `reverts` the step.
    fn history_step(&mut self, user: &str, origin: &str, redo: bool) -> Result<HistoryStepResult, RpcError> {
        let Some(plan) = self.history.plan(user, redo) else {
            return Ok(HistoryStepResult {
                label: None,
                changed: Vec::new(),
                skipped: Vec::new(),
                history: self.history.info(user),
            });
        };
        let context = self.journal_context();
        let before = self.doc();
        let mut skipped = plan.skipped;
        let applied = self.apply_sets(&plan.sets, origin);
        let changes = journal::diff(&before, &self.doc());
        let method = if redo { "history.redo" } else { "history.undo" };
        let entry = |seq: u64, changes: Vec<Change>, error: Option<String>| JournalEntry {
            seq,
            time: unix_time(),
            user: user.to_string(),
            origin: origin.to_string(),
            method: method.to_string(),
            params: json!({}),
            context: context.clone(),
            changes,
            reverts: Some(plan.seq),
            error,
        };
        let more = match applied {
            Ok(more) => more,
            Err(e) => {
                // A limit was hit before anything changed: journal the failure
                // and leave the stacks as they were.
                let seq = self.journal.next_seq();
                let failed = entry(seq, changes, Some(e.message.clone()));
                self.journal_push(failed, origin);
                return Err(e);
            }
        };
        skipped.extend(more);
        let changed = changes.iter().map(|c| c.key.clone()).collect();
        let seq = self.journal.next_seq();
        self.history.finish(user, redo, seq, changes.clone());
        self.emit_history(user, origin);
        let done = entry(seq, changes, None);
        self.journal_push(done, origin);
        Ok(HistoryStepResult { label: Some(plan.label), changed, skipped, history: self.history.info(user) })
    }

    /// Set doc keys to the given values (`null`: absent, or the default for
    /// params and steps), in an order that keeps the graph valid: channels
    /// and instruments are created first, then order, routes, params, and
    /// steps are restored, then instruments and channels are removed. Every
    /// limit is checked before anything changes, so an error means nothing
    /// changed. Returns keys that could not be set (e.g. a channel that still
    /// has other sources is not removed).
    fn apply_sets(&mut self, sets: &[(String, Value)], origin: &str) -> Result<Vec<String>, RpcError> {
        let mut channels: Vec<(u32, Option<String>)> = Vec::new();
        let mut instruments: Vec<(String, Option<(InstrumentType, String)>)> = Vec::new();
        let mut order: Option<Vec<u32>> = None;
        let mut routes: Vec<(String, Option<u32>)> = Vec::new();
        let mut params: Vec<(String, Option<f64>)> = Vec::new();
        let mut tracks: BTreeMap<(String, Voice), Vec<(usize, u8)>> = BTreeMap::new();
        let mut notes: BTreeMap<String, Vec<(usize, NoteStep)>> = BTreeMap::new();
        let mut skipped = Vec::new();
        for (key, v) in sets {
            let Some((kind, rest)) = key.split_once(':') else { continue };
            let parsed = match kind {
                "channel" => rest.parse().ok().map(|n| channels.push((n, v.as_str().map(String::from)))),
                "instrument" => {
                    let inst = v.get("type").and_then(Value::as_str).and_then(InstrumentType::parse);
                    let name = v.get("name").and_then(Value::as_str).unwrap_or(rest).to_string();
                    match (v.is_null(), inst) {
                        (true, _) => Some(instruments.push((rest.to_string(), None))),
                        (false, Some(k)) => Some(instruments.push((rest.to_string(), Some((k, name))))),
                        (false, None) => None,
                    }
                }
                "channels" => serde_json::from_value(v.clone()).ok().map(|o| order = Some(o)),
                "route" => Some(routes.push((rest.to_string(), v.as_u64().map(|n| n as u32)))),
                "param" => Some(params.push((rest.to_string(), v.as_f64()))),
                "step" => {
                    let mut parts = rest.splitn(3, '.');
                    match (parts.next(), parts.next().and_then(Voice::parse), parts.next().and_then(|i| i.parse().ok())) {
                        (Some(id), Some(voice), Some(i)) if i < MAX_STEPS => {
                            let level = v.as_u64().unwrap_or(STEP_OFF as u64) as u8;
                            Some(tracks.entry((id.to_string(), voice)).or_default().push((i, level)))
                        }
                        _ => None,
                    }
                }
                "note" => match rest.split_once('.').map(|(id, i)| (id, i.parse::<usize>())) {
                    Some((id, Ok(i))) if i < MAX_STEPS => {
                        let step = serde_json::from_value(v.clone()).unwrap_or_default();
                        Some(notes.entry(id.to_string()).or_default().push((i, step)))
                    }
                    _ => None,
                },
                _ => None,
            };
            if parsed.is_none() {
                skipped.push(key.clone());
            }
        }

        // Nothing that belongs to an instrument being removed needs restoring.
        let removing: Vec<String> = instruments
            .iter()
            .filter(|(id, v)| v.is_none() && self.instruments.iter().any(|i| &i.id == id))
            .map(|(id, _)| id.clone())
            .collect();
        let owned = |key: &str| removing.iter().any(|id| key == id || key.starts_with(&format!("{id}.")));
        routes.retain(|(s, _)| !owned(s));
        params.retain(|(p, _)| !owned(p));
        tracks.retain(|(id, _), _| !owned(id));
        notes.retain(|id, _| !owned(id));
        let adding: Vec<&(String, Option<(InstrumentType, String)>)> = instruments
            .iter()
            .filter(|(id, v)| v.is_some() && !self.instruments.iter().any(|i| &i.id == id))
            .collect();
        let free = self.slot_used.iter().filter(|u| !**u).count();
        if adding.len() > free {
            return Err(RpcError::failed(format!("at most {MAX_INSTRUMENTS} instruments")));
        }
        if self.in_flight + removing.len() > RETURN_CAPACITY {
            return Err(RpcError::failed(format!(
                "{} removed instruments are still waiting to be returned by the audio engine",
                self.in_flight
            )));
        }
        let new_channels = channels.iter().filter(|(n, v)| v.is_some() && !self.channels.iter().any(|c| c.n == *n));
        if self.channels.len() + new_channels.count() > MAX_CHANNELS {
            return Err(RpcError::failed(format!("at most {MAX_CHANNELS} channels")));
        }
        self.ensure_room(
            adding.len() * ADD_COMMANDS
                + removing.len() * (1 + MAX_OUTPUTS)
                + channels.len()
                + routes.len()
                + params.len()
                + tracks.len()
                + notes.len(),
        )?;

        let mut graph = false;
        for (n, name) in &channels {
            let Some(name) = name else { continue };
            match self.channels.iter_mut().find(|c| c.n == *n) {
                Some(c) => c.name = name.clone(),
                None => {
                    self.send(Command::SetChannelActive { ch: (*n - 1) as u8, active: true });
                    self.channels.push(ChannelInfo { n: *n, name: name.clone() });
                }
            }
            graph = true;
        }
        let mut added = Vec::new();
        for (id, v) in &instruments {
            let Some((kind, name)) = v else { continue };
            if let Some(i) = self.instruments.iter_mut().find(|i| &i.id == id) {
                i.name = name.clone();
                i.outputs = instrument::outputs(i.kind, id, name);
            } else {
                let slot = self.slot_used.iter().position(|u| !u).unwrap() as u8;
                self.add_instrument_unchecked(id, *kind, name, slot);
                // As `instrument.add`: a drum machine is the Block's target if it has none.
                if self.controller.target.is_none() && *kind == InstrumentType::Tr808 {
                    self.controller.target = Some(id.clone());
                }
                added.push(id.clone());
            }
            graph = true;
        }
        if let Some(order) = order {
            let mut rest = std::mem::take(&mut self.channels);
            for n in order {
                if let Some(pos) = rest.iter().position(|c| c.n == n) {
                    self.channels.push(rest.remove(pos));
                }
            }
            self.channels.extend(rest);
            graph = true;
        }
        if graph {
            self.rebuild_params();
            for id in &added {
                self.push_instrument_params(id);
            }
        }
        for (source, n) in routes {
            match (self.find_source(&source), n) {
                (Some((id, o)), Some(n)) if self.channels.iter().any(|c| c.n == n) => {
                    self.set_route_unchecked(&id, o, Some(n))
                }
                (Some((id, o)), None) => self.set_route_unchecked(&id, o, None),
                _ => {
                    skipped.push(format!("route:{source}"));
                    continue;
                }
            }
            graph = true;
        }
        for (path, v) in params {
            match self.index.get(&path) {
                Some(&i) => {
                    let value = v.unwrap_or(self.params[i].info.default);
                    if self.set_param(&path, value, origin).is_err() {
                        skipped.push(format!("param:{path}"));
                    }
                }
                None => skipped.push(format!("param:{path}")),
            }
        }
        for ((id, voice), steps) in tracks {
            if !matches!(self.patterns.get(&id), Some(PatternState::Drums(_))) {
                skipped.extend(steps.iter().map(|(i, _)| format!("step:{id}.{}.{i}", voice.id())));
                continue;
            }
            let mut full = self.drums(&id)[voice.index()];
            for (i, level) in steps {
                full[i] = level;
            }
            if self.set_track(&id, voice, &full, origin).is_err() {
                skipped.push(format!("step:{id}.{}", voice.id()));
            }
        }
        for (id, steps) in notes {
            let Some(PatternState::Notes(current)) = self.patterns.get(&id) else {
                skipped.extend(steps.iter().map(|(i, _)| format!("note:{id}.{i}")));
                continue;
            };
            let mut full = **current;
            for (i, step) in steps {
                full[i] = step;
            }
            if self.set_notes(&id, &full, origin).is_err() {
                skipped.push(format!("note:{id}"));
            }
        }
        for id in &removing {
            if let Some(pos) = self.instruments.iter().position(|i| &i.id == id) {
                self.teardown_instrument(pos, origin);
                graph = true;
            }
        }
        for (n, name) in &channels {
            if name.is_some() || !self.channels.iter().any(|c| c.n == *n) {
                continue;
            }
            if self.routes.values().any(|c| c == n) {
                skipped.push(format!("channel:{n}"));
            } else {
                self.remove_channel_unchecked(*n);
                graph = true;
            }
        }
        if graph {
            self.graph_changed(origin);
            self.refresh_controller(origin, true);
        }
        Ok(skipped)
    }

    // ---- dispatch ----------------------------------------------------------

    /// Handle a request and journal it. Connection-level methods (hello,
    /// subscribe, render) are handled by the server before reaching here.
    /// `origin` names the client in events; `client` identifies this
    /// connection (`conn:<id>`) and holds the notes it starts; `user` owns
    /// the change in the undo history (None: the host user).
    pub fn handle(&mut self, req: Request, origin: &str, client: &str, user: Option<&str>) -> RpcResult {
        let user = user.unwrap_or(&self.host_user).to_string();
        match &req {
            Request::HistoryUndo(_) => return ok(self.history_step(&user, origin, false)?),
            Request::HistoryRedo(_) => return ok(self.history_step(&user, origin, true)?),
            Request::HistoryGet(_) => return ok(self.history.info(&user)),
            Request::JournalGet(p) => return ok(JournalGetResult { entries: self.journal.get(p) }),
            Request::JournalExport(_) => return ok(self.journal_export()?),
            _ => {}
        }
        if read_only(&req) {
            return self.dispatch(req, origin, client);
        }
        let method = req.method();
        let params = serde_json::to_value(&req).ok().and_then(|mut v| v.get_mut("params").map(Value::take));
        let params = params.unwrap_or(Value::Null);
        // A project load or new replaces everything and starts a fresh history.
        let fresh = matches!(req, Request::ProjectNew(_) | Request::ProjectLoad(_) | Request::ProjectImport(_));
        let context = self.journal_context();
        let before = (!fresh && changes_doc(&req)).then(|| self.doc());
        // Taken first, so a segment this request starts begins after it.
        let seq = self.journal.next_seq();
        let result = self.dispatch(req, origin, client);
        let changes = before.map(|b| journal::diff(&b, &self.doc())).unwrap_or_default();
        if fresh && result.is_ok() {
            for u in self.history.clear() {
                self.emit_history(&u, origin);
            }
        } else if !changes.is_empty() {
            let was = self.history.summary(&user);
            self.history.record(&user, seq, journal::label(method, &params, &changes), &changes);
            if self.history.summary(&user) != was {
                self.emit_history(&user, origin);
            }
        }
        let error = result.as_ref().err().map(|e| e.message.clone());
        let entry = JournalEntry {
            seq,
            time: unix_time(),
            user,
            origin: origin.to_string(),
            method: method.to_string(),
            params,
            context,
            changes,
            reverts: None,
            error,
        };
        self.journal_push(entry, origin);
        result
    }

    fn dispatch(&mut self, req: Request, origin: &str, client: &str) -> RpcResult {
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
            Request::ChannelMove(p) => ok(self.channel_move(p, origin)?),
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
                        let slot = self.slot(&id)?;
                        self.send(Command::Trigger { slot, voice: voice.index() as u8, velocity });
                    }
                    (None, Some(note)) => {
                        if !(NOTE_MIN..=NOTE_MAX).contains(&note) {
                            return Err(RpcError::invalid(format!("note must be {NOTE_MIN}..{NOTE_MAX}")));
                        }
                        let id = self.resolve(p.instrument.as_deref(), InstrumentType::Tb303)?;
                        let slot = self.slot(&id)?;
                        self.send(Command::NoteOn { slot, note, velocity, gate: true });
                    }
                    _ => return Err(RpcError::invalid("pass exactly one of `voice` (drums) or `note` (notes)")),
                }
                ok(Empty {})
            }
            Request::VoiceNoteOn(p) => {
                self.note_on(p.instrument.as_deref(), p.note, p.velocity.unwrap_or(1.0), client)?;
                ok(Empty {})
            }
            Request::VoiceNoteOff(p) => {
                self.note_off(p.instrument.as_deref(), p.note, client)?;
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
            Request::ProjectImport(p) => ok(self.project_import(&p.file, origin)?),
            Request::ProjectList(_) => ok(self.project_list()),
            Request::EngineStatus(_) => ok(self.audio.clone()),
            Request::Hello(_)
            | Request::EventsSubscribe(_)
            | Request::EventsUnsubscribe(_)
            | Request::RenderOffline(_)
            | Request::DaemonInfo(_)
            | Request::DaemonShutdown(_) => Err(RpcError::failed("handled by connection")),
            Request::HistoryUndo(_)
            | Request::HistoryRedo(_)
            | Request::HistoryGet(_)
            | Request::JournalGet(_)
            | Request::JournalExport(_) => {
                Err(RpcError::failed("handled by Core::handle"))
            }
        }
    }
}

/// Requests that cannot change state, so are not journaled. An exhaustive
/// match: a new method must be classified here.
fn read_only(req: &Request) -> bool {
    use Request::*;
    match req {
        Hello(_) | StateGet(_) | EventsSubscribe(_) | EventsUnsubscribe(_) | ParamList(_) | ParamGet(_)
        | InstrumentTypes(_) | InstrumentList(_) | PatternGet(_) | PatternGetNotes(_) | HistoryGet(_)
        | JournalGet(_) | JournalExport(_) | ControllerGet(_) | MidiPorts(_) | ProjectList(_) | RenderOffline(_) | EngineStatus(_)
        | DaemonInfo(_) | DaemonShutdown(_) => true,
        ParamSet(_) | TransportPlay(_) | TransportStop(_) | InstrumentAdd(_) | InstrumentRemove(_) | ChannelAdd(_)
        | ChannelRemove(_) | ChannelRename(_) | ChannelMove(_) | RouteSet(_) | HistoryUndo(_) | HistoryRedo(_)
        | PatternSet(_) | PatternSetStep(_) | PatternToggleStep(_) | PatternClear(_) | PatternSetNotes(_)
        | PatternSetNote(_) | VoiceTrigger(_) | VoiceNoteOn(_) | VoiceNoteOff(_) | ControllerPress(_)
        | ControllerKnob(_) | ControllerSetMode(_) | MidiConnect(_) | MidiDisconnect(_) | ProjectNew(_)
        | ProjectSave(_) | ProjectLoad(_) | ProjectImport(_) => false,
    }
}

/// Whether a journaled request can change the doc (`Core::doc`). Those that
/// cannot (performance, transport, connections) skip building it twice,
/// which matters for a stream of keyboard notes.
fn changes_doc(req: &Request) -> bool {
    !matches!(
        req,
        Request::TransportPlay(_)
            | Request::TransportStop(_)
            | Request::VoiceTrigger(_)
            | Request::VoiceNoteOn(_)
            | Request::VoiceNoteOff(_)
            | Request::ControllerSetMode(_)
            | Request::MidiConnect(_)
            | Request::MidiDisconnect(_)
            | Request::ProjectSave(_)
    )
}

fn unix_time() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// Trim a display name; an empty one is an error.
fn check_name(name: Option<String>) -> Result<Option<String>, RpcError> {
    match name {
        Some(n) if n.trim().is_empty() => Err(RpcError::invalid("name must not be empty")),
        Some(n) => Ok(Some(n.trim().to_string())),
        None => Ok(None),
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
