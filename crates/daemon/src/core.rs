//! The daemon core: authoritative state, request handling, and event fan-out.
//!
//! All mutations go through `Core` (behind one mutex). Each mutation updates
//! state, forwards a `Command` to the real-time engine, and emits an event so
//! every client (UI, CLI, controllers) stays in sync regardless of origin.

use crate::controller::{BlockInput, BlockMap, Controller, decode_block};
use crate::midi::{Midi, MidiMessage, list_ports};
use fours_engine::params::{self, ParamId};
use fours_engine::{Command, Feedback};
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

pub struct Core {
    registry: Vec<ParamInfo>,
    index: HashMap<String, ParamId>,
    values: Vec<f64>,
    pattern: [[u8; MAX_STEPS]; NUM_TRACKS],
    playing: bool,
    playhead: Option<u32>,
    controller: Controller,
    block_map: BlockMap,
    midi: Midi,
    midi_tx: Sender<MidiMessage>,
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
        let registry = params::registry();
        let index = registry.iter().enumerate().map(|(i, p)| (p.path.clone(), i)).collect();
        let values = registry.iter().map(|p| p.default).collect();
        let block_map = BlockMap::load_or_create(&data_dir.join("livid-block.json"));
        let (events, _) = broadcast::channel(4096);
        Self {
            registry,
            index,
            values,
            pattern: [[STEP_OFF; MAX_STEPS]; NUM_TRACKS],
            playing: false,
            playhead: None,
            controller: Controller::default(),
            block_map,
            midi: Midi::default(),
            midi_tx,
            project: ProjectInfo { path: None, dirty: false },
            audio,
            seq: 0,
            events,
            commands,
            data_dir,
            meters_silent: false,
        }
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
        if self.commands.push(cmd).is_err() {
            tracing::warn!("engine command queue full; dropped {cmd:?}");
        }
    }

    fn mark_dirty(&mut self, origin: &str) {
        if !self.project.dirty {
            self.project.dirty = true;
            let info = self.project.clone();
            self.emit(origin, Event::Project { info });
        }
    }

    fn length(&self) -> u32 {
        self.values[params::LENGTH] as u32
    }

    /// Values as f32 in registry order (engine format).
    pub fn engine_values(&self) -> Vec<f32> {
        self.values.iter().map(|v| *v as f32).collect()
    }

    pub fn pattern(&self) -> [[u8; MAX_STEPS]; NUM_TRACKS] {
        self.pattern
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Recompute controller LEDs, push changes to hardware, and emit an event
    /// if anything visible changed.
    fn refresh_controller(&mut self, origin: &str, force_event: bool) {
        let pages = Controller::num_pages(self.length());
        if self.controller.page >= pages {
            self.controller.page = pages - 1;
        }
        let leds = self.controller.compute_leds(&self.pattern, self.length(), self.playhead);
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
            params: self.registry.iter().zip(&self.values).map(|(p, v)| (p.path.clone(), *v)).collect(),
            pattern: self.track_patterns(None),
            controller: self.controller.state(),
            midi: self.midi.connections(),
            project: self.project.clone(),
            audio: self.audio.clone(),
        }
    }

    fn track_patterns(&self, voice: Option<Voice>) -> Vec<TrackPattern> {
        Voice::ALL
            .iter()
            .filter(|v| voice.is_none_or(|x| x == **v))
            .map(|v| TrackPattern { voice: *v, steps: self.pattern[v.index()].to_vec() })
            .collect()
    }

    fn pattern_result(&self, voice: Option<Voice>) -> PatternResult {
        PatternResult { length: self.length(), tracks: self.track_patterns(voice) }
    }

    // ---- parameters --------------------------------------------------------

    fn param_id(&self, path: &str) -> Result<ParamId, RpcError> {
        self.index.get(path).copied().ok_or_else(|| {
            RpcError::invalid(format!("unknown parameter '{path}' (see param.list)"))
        })
    }

    pub fn set_param(&mut self, path: &str, value: f64, origin: &str) -> Result<ParamValue, RpcError> {
        let id = self.param_id(path)?;
        if !value.is_finite() {
            return Err(RpcError::invalid("value must be a finite number"));
        }
        let value = self.registry[id].clamp(value);
        if self.values[id] != value {
            self.values[id] = value;
            self.send(Command::SetParam { id, value: value as f32 });
            self.emit(origin, Event::ParamChanged { path: path.to_string(), value });
            self.mark_dirty(origin);
            if id == params::LENGTH {
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

    pub fn set_step(&mut self, voice: Voice, step: u32, level: u8, origin: &str) -> Result<StepResult, RpcError> {
        Self::check_step(step)?;
        if level > STEP_ACCENT {
            return Err(RpcError::invalid("level must be 0 (off), 1 (on), or 2 (accent)"));
        }
        let t = voice.index();
        if self.pattern[t][step as usize] != level {
            self.pattern[t][step as usize] = level;
            self.send(Command::SetStep { track: t as u8, step: step as u8, level });
            self.emit(origin, Event::StepChanged { voice, step, level });
            self.mark_dirty(origin);
            self.refresh_controller(origin, false);
        }
        Ok(StepResult { voice, step, level })
    }

    fn set_track(&mut self, voice: Voice, steps: &[u8], origin: &str) -> Result<TrackPattern, RpcError> {
        if steps.len() > MAX_STEPS {
            return Err(RpcError::invalid(format!("at most {MAX_STEPS} steps")));
        }
        if steps.iter().any(|s| *s > STEP_ACCENT) {
            return Err(RpcError::invalid("step levels must be 0, 1, or 2"));
        }
        let mut full = [STEP_OFF; MAX_STEPS];
        full[..steps.len()].copy_from_slice(steps);
        let t = voice.index();
        if self.pattern[t] != full {
            self.pattern[t] = full;
            self.send(Command::SetTrack { track: t as u8, steps: full });
            self.emit(origin, Event::PatternChanged { voice, steps: full.to_vec() });
            self.mark_dirty(origin);
            self.refresh_controller(origin, false);
        }
        Ok(TrackPattern { voice, steps: full.to_vec() })
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
        if !pressed {
            return Ok(());
        }
        let step = self.controller.pad_step(col);
        if step >= self.length() {
            return Ok(());
        }
        let voice = Voice::from_index(row as usize).unwrap();
        let cur = self.pattern[row as usize][step as usize];
        let next = if cur == STEP_OFF { STEP_ON } else { STEP_OFF };
        self.set_step(voice, step, next, origin)?;
        Ok(())
    }

    fn controller_knob(&mut self, index: u32, value: f64, origin: &str) -> Result<(), RpcError> {
        if index as usize >= NUM_TRACKS {
            return Err(RpcError::invalid("knob index must be 0..7"));
        }
        let path = self.controller.knob_mode.param_path(index as usize);
        let id = self.param_id(&path)?;
        let v = value.clamp(0.0, 1.0);
        let scaled = match self.registry[id].kind {
            ParamKind::Continuous { min, max } => min + v * (max - min),
            ParamKind::Integer { min, max } => min as f64 + v * (max - min) as f64,
            ParamKind::Toggle => v,
        };
        self.set_param(&path, scaled, origin)?;
        Ok(())
    }

    fn controller_mode(&mut self, p: ControllerModeParams, origin: &str) -> ControllerState {
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
        self.controller.state()
    }

    /// Handle a raw message from a connected MIDI device.
    pub fn handle_midi(&mut self, msg: MidiMessage) {
        let origin = format!("midi:{}", msg.port);
        self.emit(&origin, Event::MidiIn { port: msg.port.clone(), data: msg.data.clone() });
        match msg.kind {
            DeviceKind::LividBlock => match decode_block(&self.block_map, &msg.data) {
                Some(BlockInput::Pad { row, col, pressed }) => {
                    let _ = self.controller_pad(row as u32, col as u32, pressed, &origin);
                }
                Some(BlockInput::Knob { index, value }) => {
                    let _ = self.controller_knob(index as u32, value, &origin);
                }
                None => {}
            },
            DeviceKind::GenericDrums => {
                let d = &msg.data;
                if d.len() >= 3 && d[0] & 0xf0 == 0x90 && d[2] > 0
                    && let Some(v) = Voice::ALL.iter().find(|v| v.gm_note() == d[1])
                {
                    self.send(Command::Trigger { track: v.index() as u8, velocity: d[2] as f32 / 127.0 });
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
        self.controller.device = self.midi.block_name();
        let connections = self.midi.connections();
        self.emit(origin, Event::Midi { connections });
        self.push_all_leds();
        self.refresh_controller(origin, true);
    }

    pub fn midi_connect(&mut self, p: MidiConnectParams, origin: &str) -> Result<MidiPortsResult, RpcError> {
        self.midi
            .connect(&p.input, p.output.as_deref(), p.kind, self.midi_tx.clone())
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
            match self.midi.connect(name, None, DeviceKind::LividBlock, self.midi_tx.clone()) {
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
            Feedback::Trigger { track, velocity, time, .. } => {
                if let Some(voice) = Voice::from_index(track as usize) {
                    self.emit("engine", Event::Trigger { voice, velocity, time });
                }
            }
            Feedback::Stopped { .. } => {}
            Feedback::Meters { tracks, master } => {
                let silent = tracks.iter().chain(master.iter()).all(|x| *x < 1e-5);
                if silent && self.meters_silent {
                    return;
                }
                self.meters_silent = silent;
                self.emit("engine", Event::Meters { tracks: tracks.to_vec(), master: master.to_vec() });
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
        let patterns: BTreeMap<Voice, String> = Voice::ALL
            .iter()
            .filter(|v| self.pattern[v.index()].iter().any(|s| *s != STEP_OFF))
            .map(|v| (*v, steps_for_file(&self.pattern[v.index()], length)))
            .collect();
        ProjectFile {
            format_version: PROJECT_FORMAT_VERSION,
            params: self.registry.iter().zip(&self.values).map(|(p, v)| (p.path.clone(), *v)).collect(),
            patterns,
            controller: ProjectController { knob_mode: self.controller.knob_mode, follow: self.controller.follow },
        }
    }

    /// Replace all state from a project file and resync the engine.
    fn apply_project_file(&mut self, file: &ProjectFile, origin: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        self.values = self.registry.iter().map(|p| p.default).collect();
        for (path, v) in &file.params {
            match self.index.get(path) {
                Some(id) => self.values[*id] = self.registry[*id].clamp(*v),
                None => warnings.push(format!("ignored unknown parameter '{path}'")),
            }
        }
        self.pattern = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
        for (voice, s) in &file.patterns {
            match parse_steps(s) {
                Ok(steps) => self.pattern[voice.index()][..steps.len()].copy_from_slice(&steps),
                Err(e) => warnings.push(format!("{}: {e}", voice.id())),
            }
        }
        self.controller.knob_mode = file.controller.knob_mode;
        self.controller.follow = file.controller.follow;
        self.controller.page = 0;

        for id in 0..self.values.len() {
            let value = self.values[id] as f32;
            self.send(Command::SetParam { id, value });
        }
        for t in 0..NUM_TRACKS {
            let steps = self.pattern[t];
            self.send(Command::SetTrack { track: t as u8, steps });
        }
        self.emit(origin, Event::Reset);
        self.refresh_controller(origin, true);
        warnings
    }

    pub fn project_new(&mut self, origin: &str) -> ProjectInfo {
        let file = ProjectFile {
            format_version: PROJECT_FORMAT_VERSION,
            params: BTreeMap::new(),
            patterns: BTreeMap::new(),
            controller: ProjectController { knob_mode: KnobMode::Volume, follow: true },
        };
        self.apply_project_file(&file, origin);
        self.project = ProjectInfo { path: None, dirty: false };
        let info = self.project.clone();
        self.emit(origin, Event::Project { info: info.clone() });
        info
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
        for w in self.apply_project_file(&project, origin) {
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
        match req {
            Request::StateGet(_) => ok(self.snapshot()),
            Request::ParamList(p) => {
                let params = self
                    .registry
                    .iter()
                    .filter(|i| p.prefix.as_ref().is_none_or(|pre| i.path.starts_with(pre.as_str())))
                    .cloned()
                    .collect();
                ok(ParamListResult { params })
            }
            Request::ParamGet(p) => {
                let id = self.param_id(&p.path)?;
                ok(ParamValue { path: p.path, value: self.values[id] })
            }
            Request::ParamSet(p) => ok(self.set_param(&p.path, p.value, origin)?),
            Request::TransportPlay(_) => ok(self.play(origin)),
            Request::TransportStop(_) => ok(self.stop(origin)),
            Request::PatternGet(p) => ok(self.pattern_result(p.voice)),
            Request::PatternSet(p) => ok(self.set_track(p.voice, &p.steps, origin)?),
            Request::PatternSetStep(p) => ok(self.set_step(p.voice, p.step, p.level, origin)?),
            Request::PatternToggleStep(p) => {
                Self::check_step(p.step)?;
                let cur = self.pattern[p.voice.index()][p.step as usize];
                let next = if cur == STEP_OFF { STEP_ON } else { STEP_OFF };
                ok(self.set_step(p.voice, p.step, next, origin)?)
            }
            Request::PatternClear(p) => {
                for v in Voice::ALL {
                    if p.voice.is_none_or(|x| x == v) {
                        self.set_track(v, &[], origin)?;
                    }
                }
                ok(self.pattern_result(p.voice))
            }
            Request::VoiceTrigger(p) => {
                let velocity = p.velocity.unwrap_or(1.0).clamp(0.0, 1.0);
                self.send(Command::Trigger { track: p.voice.index() as u8, velocity });
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
            Request::ControllerSetMode(p) => ok(self.controller_mode(p, origin)),
            Request::MidiPorts(_) => ok(self.midi_ports()),
            Request::MidiConnect(p) => ok(self.midi_connect(p, origin)?),
            Request::MidiDisconnect(p) => {
                self.midi.disconnect(&p.input).map_err(|e| RpcError::failed(e.to_string()))?;
                self.midi_changed(origin);
                ok(self.midi_ports())
            }
            Request::ProjectNew(_) => ok(self.project_new(origin)),
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

/// Used by tests and the server for a JSON error body.
pub fn error_json(code: i32, message: &str) -> Value {
    json!({ "code": code, "message": message })
}
