//! Clips (RFC 0007, phase 2): each instrument's sequence as timed note
//! events. Every edit, including the step editors (`pattern.*`), the Block
//! grid, and undo, goes through `edit_clip`, which sends the engine only the
//! difference and emits `clip_changed` plus the step-view events the editors
//! already understand.

use super::*;

pub(super) struct ClipState {
    pub kind: InstrumentType,
    /// Ticks; `None` follows `sequencer.length`.
    pub length: Option<u32>,
    /// Sorted and unique by (tick, note).
    pub events: Vec<ClipEvent>,
}

impl ClipState {
    pub fn new(kind: InstrumentType) -> Self {
        Self { kind, length: None, events: Vec::new() }
    }
}

/// Events that draw a drum grid.
fn grid_events(grid: &[[u8; MAX_STEPS]; NUM_TRACKS]) -> Vec<ClipEvent> {
    let mut out: Vec<ClipEvent> = Voice::ALL
        .iter()
        .flat_map(|v| (0..MAX_STEPS).filter_map(move |s| drum_event(*v, s, grid[v.index()][s])))
        .collect();
    out.sort_by_key(ClipEvent::key);
    out
}

/// Events that play note steps.
fn step_events(steps: &[NoteStep]) -> Vec<ClipEvent> {
    steps.iter().enumerate().filter_map(|(i, s)| note_event(i, s)).collect()
}

/// A clip as a project file stores it: as a step pattern when the pattern
/// says exactly the same thing (and the clip follows `sequencer.length`),
/// else as events.
pub(super) fn project_entry(c: &ClipState, length: usize) -> (Option<ProjectPattern>, Option<ProjectClip>) {
    if c.events.is_empty() && c.length.is_none() {
        return (None, None);
    }
    if c.length.is_none() {
        match c.kind {
            InstrumentType::Tr808 => {
                let grid = drum_grid(&c.events);
                if grid_events(&grid) == c.events {
                    let tracks = Voice::ALL
                        .iter()
                        .filter(|v| grid[v.index()].iter().any(|s| *s != STEP_OFF))
                        .map(|v| (*v, steps_for_file(&grid[v.index()], length)))
                        .collect();
                    return (Some(ProjectPattern::Drums(tracks)), None);
                }
            }
            InstrumentType::Tb303 => {
                let steps = note_steps(&c.events);
                let in_range = c.events.iter().all(|e| (NOTE_MIN..=NOTE_MAX).contains(&e.note));
                if in_range && step_events(&steps) == c.events {
                    let last = steps.iter().rposition(|s| s.note.is_some()).unwrap_or(0);
                    let end = if last < length { length } else { (last + 1).div_ceil(16) * 16 };
                    return (Some(ProjectPattern::Notes(format_notes(&steps, end))), None);
                }
            }
        }
    }
    (None, Some(ProjectClip { length: c.length, events: format_events(&c.events) }))
}

/// The clip a project file describes for an instrument.
pub(super) fn from_project(
    kind: InstrumentType,
    pattern: Option<&ProjectPattern>,
    clip: Option<&ProjectClip>,
) -> Result<(Option<u32>, Vec<ClipEvent>), String> {
    if let Some(c) = clip {
        if c.length.is_some_and(|l| l == 0 || l > MAX_CLIP_TICKS) {
            return Err(format!("clip length must be 1..{MAX_CLIP_TICKS} ticks"));
        }
        return Ok((c.length, parse_events(&c.events)?));
    }
    let events = match (kind, pattern) {
        (_, None) => Vec::new(),
        (InstrumentType::Tr808, Some(ProjectPattern::Drums(tracks))) => {
            let mut grid = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
            for (voice, s) in tracks {
                let steps = parse_steps(s).map_err(|e| format!("{}: {e}", voice.id()))?;
                grid[voice.index()][..steps.len()].copy_from_slice(&steps);
            }
            grid_events(&grid)
        }
        (InstrumentType::Tb303, Some(ProjectPattern::Notes(s))) => step_events(&parse_notes(s)?),
        _ => return Err("pattern does not match the instrument type".into()),
    };
    Ok((None, events))
}

impl Core {
    // ---- queries -------------------------------------------------------------

    pub(super) fn clip(&self, id: &str) -> Clip {
        let c = &self.clips[id];
        Clip { instrument: id.to_string(), length: c.length, events: c.events.clone() }
    }

    pub(super) fn is_drums(&self, id: &str) -> bool {
        self.clips.get(id).is_some_and(|c| c.kind == InstrumentType::Tr808)
    }

    /// The drum grid an instrument's clip shows.
    pub(super) fn drums(&self, id: &str) -> [[u8; MAX_STEPS]; NUM_TRACKS] {
        self.clips.get(id).map(|c| drum_grid(&c.events)).unwrap_or([[STEP_OFF; MAX_STEPS]; NUM_TRACKS])
    }

    /// The note steps an instrument's clip shows.
    pub(super) fn note_view(&self, id: &str) -> [NoteStep; MAX_STEPS] {
        self.clips.get(id).map(|c| note_steps(&c.events)).unwrap_or([NoteStep::default(); MAX_STEPS])
    }

    // ---- the one edit path ---------------------------------------------------

    /// Replace a clip's events (and, with `Some`, its length). The engine
    /// gets only the difference; clients get `clip_changed` and the step
    /// view's events.
    pub(super) fn edit_clip(
        &mut self,
        id: &str,
        events: Vec<ClipEvent>,
        length: Option<Option<u32>>,
        origin: &str,
    ) -> Result<Clip, RpcError> {
        let slot = self.slot(id)?;
        let events = normalize_events(&events).map_err(RpcError::invalid)?;
        if let Some(Some(l)) = length
            && (l == 0 || l > MAX_CLIP_TICKS)
        {
            return Err(RpcError::invalid(format!("clip length must be 1..{MAX_CLIP_TICKS} ticks")));
        }
        let old = &self.clips[id];
        let removed: Vec<ClipEvent> =
            old.events.iter().filter(|e| events.binary_search_by_key(&e.key(), ClipEvent::key).is_err()).copied().collect();
        let added: Vec<ClipEvent> = events.iter().filter(|e| !old.events.contains(e)).copied().collect();
        let new_length = length.unwrap_or(old.length);
        let length_changed = new_length != old.length;
        if removed.is_empty() && added.is_empty() && !length_changed {
            return Ok(self.clip(id));
        }
        self.ensure_room(removed.len() + added.len() + 1)?;
        let (old_grid, old_notes) = (self.drums(id), self.note_view(id));
        for e in &removed {
            self.send(Command::RemoveEvent { slot, tick: e.tick, note: e.note });
        }
        for e in &added {
            self.send(Command::AddEvent { slot, event: *e });
        }
        if length_changed {
            self.send(Command::SetClipLength { slot, length: new_length });
        }
        let c = self.clips.get_mut(id).expect("checked by slot");
        c.events = events;
        c.length = new_length;

        // The step views' events, so editors update without knowing clips.
        if self.is_drums(id) {
            let grid = self.drums(id);
            let changed: Vec<(Voice, usize)> = Voice::ALL
                .iter()
                .flat_map(|v| (0..MAX_STEPS).map(move |s| (*v, s)))
                .filter(|(v, s)| grid[v.index()][*s] != old_grid[v.index()][*s])
                .collect();
            if let [(voice, step)] = changed[..] {
                let level = grid[voice.index()][step];
                self.emit(origin, Event::StepChanged { instrument: id.to_string(), voice, step: step as u32, level });
            } else {
                for v in Voice::ALL.iter().filter(|v| changed.iter().any(|(x, _)| x == *v)) {
                    let steps = grid[v.index()].to_vec();
                    self.emit(origin, Event::PatternChanged { instrument: id.to_string(), voice: *v, steps });
                }
            }
        } else {
            let notes = self.note_view(id);
            if notes != old_notes {
                self.emit(origin, Event::NotesChanged { instrument: id.to_string(), steps: notes.to_vec() });
            }
        }
        let clip = self.clip(id);
        self.emit(origin, Event::ClipChanged { clip: clip.clone() });
        self.mark_dirty(origin);
        self.refresh_controller(origin, false);
        Ok(clip)
    }

    // ---- step views ----------------------------------------------------------

    pub(super) fn check_step(step: u32) -> Result<(), RpcError> {
        if step as usize >= MAX_STEPS {
            return Err(RpcError::invalid(format!("step must be 0..{}", MAX_STEPS - 1)));
        }
        Ok(())
    }

    /// One drum step: the event on the step's first tick at the voice's GM
    /// note.
    pub fn set_step(&mut self, id: &str, voice: Voice, step: u32, level: u8, origin: &str) -> Result<StepResult, RpcError> {
        Self::check_step(step)?;
        if level > STEP_ACCENT {
            return Err(RpcError::invalid("level must be 0 (off), 1 (on), or 2 (accent)"));
        }
        let tick = step * TICKS_PER_STEP;
        let mut events: Vec<ClipEvent> =
            self.clips[id].events.iter().filter(|e| e.key() != (tick, voice.gm_note())).copied().collect();
        events.extend(drum_event(voice, step as usize, level));
        self.edit_clip(id, events, None, origin)?;
        Ok(StepResult { instrument: id.to_string(), voice, step, level })
    }

    /// A voice's row: only steps whose level changes are rewritten, so
    /// other velocities, lengths, and anything off the grid stay.
    pub(super) fn set_track(&mut self, id: &str, voice: Voice, steps: &[u8], origin: &str) -> Result<TrackPattern, RpcError> {
        if steps.len() > MAX_STEPS {
            return Err(RpcError::invalid(format!("at most {MAX_STEPS} steps")));
        }
        if steps.iter().any(|s| *s > STEP_ACCENT) {
            return Err(RpcError::invalid("step levels must be 0, 1, or 2"));
        }
        let mut full = [STEP_OFF; MAX_STEPS];
        full[..steps.len()].copy_from_slice(steps);
        let old = self.drums(id)[voice.index()];
        let changed: Vec<usize> = (0..MAX_STEPS).filter(|s| old[*s] != full[*s]).collect();
        let mut events: Vec<ClipEvent> = self.clips[id]
            .events
            .iter()
            .filter(|e| {
                let step = (e.tick / TICKS_PER_STEP) as usize;
                !(e.note == voice.gm_note() && on_drum_grid(e) && changed.contains(&step))
            })
            .copied()
            .collect();
        events.extend(changed.iter().filter_map(|s| drum_event(voice, *s, full[*s])));
        self.edit_clip(id, events, None, origin)?;
        Ok(TrackPattern { voice, steps: full.to_vec() })
    }

    pub(super) fn notes_result(&self, id: &str) -> NotesResult {
        NotesResult { instrument: id.to_string(), length: self.length(), steps: self.note_view(id).to_vec() }
    }

    /// Note steps: only steps whose view changes are rewritten (their
    /// events on the step's first tick), so chords, velocities, lengths, and
    /// events between steps stay elsewhere.
    pub(super) fn set_notes(&mut self, id: &str, steps: &[NoteStep], origin: &str) -> Result<NotesResult, RpcError> {
        if steps.len() > MAX_STEPS {
            return Err(RpcError::invalid(format!("at most {MAX_STEPS} steps")));
        }
        if let Some(bad) = steps.iter().filter_map(|s| s.note).find(|n| !(NOTE_MIN..=NOTE_MAX).contains(n)) {
            return Err(RpcError::invalid(format!("note {bad} out of range ({NOTE_MIN}..{NOTE_MAX})")));
        }
        let mut full = [NoteStep::default(); MAX_STEPS];
        full[..steps.len()].copy_from_slice(steps);
        let old = self.note_view(id);
        let changed: Vec<usize> = (0..MAX_STEPS).filter(|i| old[*i] != full[*i]).collect();
        let mut events: Vec<ClipEvent> = self.clips[id]
            .events
            .iter()
            .filter(|e| !(on_note_grid(e) && changed.contains(&((e.tick / TICKS_PER_STEP) as usize))))
            .copied()
            .collect();
        events.extend(changed.iter().filter_map(|i| note_event(*i, &full[*i])));
        self.edit_clip(id, events, None, origin)?;
        Ok(self.notes_result(id))
    }

    // ---- clip RPCs -----------------------------------------------------------

    pub(super) fn clip_target(&self, id: Option<&str>, client: &str) -> Result<String, RpcError> {
        match id {
            Some(id) => Ok(self.find_instrument(id)?.id.clone()),
            None => self.default_instrument(client),
        }
    }

    /// Clip RPCs. Any other request is handed back.
    pub(super) fn handle_clip(&mut self, req: Request, origin: &str, client: &str) -> Result<RpcResult, Request> {
        let r = |c: Result<Clip, RpcError>| c.and_then(ok);
        Ok(match req {
            Request::ClipGet(p) => self.clip_target(p.instrument.as_deref(), client).map(|id| self.clip(&id)).and_then(ok),
            Request::ClipSet(p) => {
                r(self.clip_target(p.instrument.as_deref(), client).and_then(|id| self.edit_clip(&id, p.events, None, origin)))
            }
            Request::ClipAdd(p) => r(self.clip_target(p.instrument.as_deref(), client).and_then(|id| {
                let mut events = self.clips[&id].events.clone();
                events.extend(p.events);
                self.edit_clip(&id, events, None, origin)
            })),
            Request::ClipRemove(p) => r(self.clip_target(p.instrument.as_deref(), client).and_then(|id| {
                let events = self.clips[&id]
                    .events
                    .iter()
                    .filter(|e| !p.events.iter().any(|k| (k.tick, k.note) == e.key()))
                    .copied()
                    .collect();
                self.edit_clip(&id, events, None, origin)
            })),
            Request::ClipUpdate(p) => r(self.clip_target(p.instrument.as_deref(), client).and_then(|id| {
                let mut events: Vec<ClipEvent> = self.clips[&id]
                    .events
                    .iter()
                    .filter(|e| !p.remove.iter().any(|k| (k.tick, k.note) == e.key()))
                    .copied()
                    .collect();
                events.extend(p.add);
                self.edit_clip(&id, events, None, origin)
            })),
            Request::ClipLength(p) => r(self.clip_target(p.instrument.as_deref(), client).and_then(|id| {
                let events = self.clips[&id].events.clone();
                self.edit_clip(&id, events, Some(p.length), origin)
            })),
            Request::ClipClear(p) => {
                r(self.clip_target(p.instrument.as_deref(), client).and_then(|id| self.edit_clip(&id, Vec::new(), None, origin)))
            }
            Request::ClipQuantize(p) => r(self.clip_target(p.instrument.as_deref(), client).and_then(|id| {
                if p.grid == 0 || p.grid > MAX_CLIP_TICKS {
                    return Err(RpcError::invalid("grid must be 1.. ticks (24 = a 16th)"));
                }
                if p.strength.is_some_and(|s| !(0.0..=1.0).contains(&s)) {
                    return Err(RpcError::invalid("strength must be 0..1"));
                }
                // Toward the nearest grid line; one that lands on the loop's
                // end wraps to its start (where it would play), and one past
                // the longest clip takes the last line before it. Events
                // that land together keep the later.
                let (strength, loop_len) = (p.strength.unwrap_or(1.0), self.loop_len(&id));
                let events: Vec<ClipEvent> = self.clips[&id]
                    .events
                    .iter()
                    .map(|e| ClipEvent { tick: snap_tick(e.tick, p.grid, strength, loop_len), ..*e })
                    .collect();
                self.edit_clip(&id, events, None, origin)
            })),
            other => return Err(other),
        })
    }

    // ---- journal doc ---------------------------------------------------------

    /// Clip contents as undo keys: one per event (`event:<id>.<tick>.<note>`
    /// -> `{len, velocity}`), so two people editing different notes never
    /// conflict, and `clip:<id>` for a clip's own length.
    pub(super) fn clip_doc(&self, d: &mut Doc) {
        for (id, c) in &self.clips {
            for e in &c.events {
                d.insert(format!("event:{id}.{}.{}", e.tick, e.note), json!({ "len": e.len, "velocity": e.velocity }));
            }
            if let Some(l) = c.length {
                d.insert(format!("clip:{id}"), json!(l));
            }
        }
    }

    /// Apply undo sets of clip keys. Returns keys that could not be set.
    pub(super) fn apply_clip_sets(
        &mut self,
        events: BTreeMap<String, Vec<(u32, u8, Option<(u32, u8)>)>>,
        lengths: Vec<(String, Option<u32>)>,
        origin: &str,
    ) -> Vec<String> {
        let mut skipped = Vec::new();
        let mut ids: Vec<String> = events.keys().cloned().collect();
        ids.extend(lengths.iter().map(|(id, _)| id.clone()));
        ids.sort();
        ids.dedup();
        for id in ids {
            let Some(c) = self.clips.get(&id) else {
                skipped.push(format!("clip:{id}"));
                continue;
            };
            let mut list = c.events.clone();
            for (tick, note, v) in events.get(&id).into_iter().flatten() {
                list.retain(|e| e.key() != (*tick, *note));
                if let Some((len, velocity)) = v {
                    list.push(ClipEvent { tick: *tick, note: *note, len: *len, velocity: *velocity });
                }
            }
            let length = lengths.iter().find(|(i, _)| *i == id).map(|(_, l)| *l);
            if self.edit_clip(&id, list, length, origin).is_err() {
                skipped.push(format!("clip:{id}"));
            }
        }
        skipped
    }
}
