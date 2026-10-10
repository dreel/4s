//! Clips (RFC 0007 phase 2, RFC 0008 phase B): each instrument's track holds
//! a pool of clips (timed note events), one of them selected: what pattern
//! mode plays and what the step editors (`pattern.*`) and the Block grid
//! edit. Every clip edit, undo included, goes through `edit_clip`, which
//! sends the engine only the difference and emits `clip_changed` plus, for
//! the selected clip, the step-view events the editors already understand.
//!
//! Clips live in the engine's shared clip table; `engine` is a clip's index
//! there, allocated here (`clip_used`).

use super::*;

pub(super) struct ClipState {
    pub name: String,
    /// Ticks; `None` follows `sequencer.length`.
    pub length: Option<u32>,
    /// Sorted and unique by (tick, note).
    pub events: Vec<ClipEvent>,
    /// Index in the engine's clip table.
    pub engine: u16,
}

pub(super) struct TrackState {
    pub kind: InstrumentType,
    pub clips: BTreeMap<u32, ClipState>,
    pub selected: u32,
    /// Sorted by start, not overlapping.
    pub placements: Vec<Placement>,
}

impl TrackState {
    pub fn selected(&self) -> &ClipState {
        &self.clips[&self.selected]
    }

    /// The id a new clip gets: one past the highest.
    pub fn next_id(&self) -> u32 {
        self.clips.keys().next_back().map_or(1, |n| n + 1)
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
/// else as events; the name when it is not the id.
pub(super) fn project_entry(kind: InstrumentType, n: u32, c: &ClipState, length: usize) -> ProjectClip {
    let mut out = ProjectClip {
        name: (c.name != n.to_string()).then(|| c.name.clone()),
        length: c.length,
        ..Default::default()
    };
    if c.events.is_empty() {
        return out;
    }
    if c.length.is_none() {
        match kind {
            InstrumentType::Tr808 => {
                let grid = drum_grid(&c.events);
                if grid_events(&grid) == c.events {
                    let tracks = Voice::ALL
                        .iter()
                        .filter(|v| grid[v.index()].iter().any(|s| *s != STEP_OFF))
                        .map(|v| (*v, steps_for_file(&grid[v.index()], length)))
                        .collect();
                    out.pattern = Some(ProjectPattern::Drums(tracks));
                    return out;
                }
            }
            InstrumentType::Tb303 => {
                let steps = note_steps(&c.events);
                let in_range = c.events.iter().all(|e| (NOTE_MIN..=NOTE_MAX).contains(&e.note));
                if in_range && step_events(&steps) == c.events {
                    let last = steps.iter().rposition(|s| s.note.is_some()).unwrap_or(0);
                    let end = if last < length { length } else { (last + 1).div_ceil(16) * 16 };
                    out.pattern = Some(ProjectPattern::Notes(format_notes(&steps, end)));
                    return out;
                }
            }
        }
    }
    out.events = Some(format_events(&c.events));
    out
}

/// The length and events a project file's clip describes.
pub(super) fn from_project(kind: InstrumentType, c: &ProjectClip) -> Result<(Option<u32>, Vec<ClipEvent>), String> {
    if c.length.is_some_and(|l| l == 0 || l > MAX_CLIP_TICKS) {
        return Err(format!("clip length must be 1..{MAX_CLIP_TICKS} ticks"));
    }
    let events = match (kind, &c.pattern, &c.events) {
        (_, Some(_), Some(_)) => return Err("a clip has a pattern or events, not both".into()),
        (_, None, Some(e)) => parse_events(e)?,
        (_, None, None) => Vec::new(),
        (InstrumentType::Tr808, Some(ProjectPattern::Drums(tracks)), None) => {
            let mut grid = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
            for (voice, s) in tracks {
                let steps = parse_steps(s).map_err(|e| format!("{}: {e}", voice.id()))?;
                grid[voice.index()][..steps.len()].copy_from_slice(&steps);
            }
            grid_events(&grid)
        }
        (InstrumentType::Tb303, Some(ProjectPattern::Notes(s)), None) => step_events(&parse_notes(s)?),
        _ => return Err("pattern does not match the instrument type".into()),
    };
    Ok((c.length, events))
}

/// A project file's track, parsed and checked.
pub(super) struct LoadedTrack {
    pub selected: u32,
    /// (id, name, length, events).
    pub clips: Vec<(u32, String, Option<u32>, Vec<ClipEvent>)>,
    pub placements: Vec<Placement>,
}

/// Parse and check a project file's track: at least one clip, the
/// selected clip exists, placements name existing clips and do not overlap.
pub(super) fn load_track(kind: InstrumentType, t: &ProjectTrack) -> Result<LoadedTrack, String> {
    if t.clips.is_empty() {
        return Err("a track has at least one clip".into());
    }
    if !t.clips.contains_key(&t.selected) {
        return Err(format!("the selected clip {} is not in the pool", t.selected));
    }
    let mut clips = Vec::new();
    for (n, c) in &t.clips {
        if *n == 0 {
            return Err("clip ids start at 1".into());
        }
        let (length, events) = from_project(kind, c).map_err(|e| format!("clip {n}: {e}"))?;
        let name = match &c.name {
            Some(name) => check_clip_name(name).map_err(|e| format!("clip {n}: {}", e.message))?,
            None => n.to_string(),
        };
        clips.push((*n, name, length, events));
    }
    let mut placements = t.arrangement.clone();
    placements.sort_by_key(|p| p.start);
    if placements.len() > MAX_PLACEMENTS {
        return Err(format!("at most {MAX_PLACEMENTS} placements on a track"));
    }
    for (i, p) in placements.iter().enumerate() {
        if !t.clips.contains_key(&p.clip) {
            return Err(format!("a placement at tick {} plays clip {}, which is not in the pool", p.start, p.clip));
        }
        if p.length == 0 || p.start.checked_add(p.length).is_none_or(|e| e > MAX_SONG_TICKS) || p.offset >= MAX_CLIP_TICKS {
            return Err(format!("the placement at tick {} is out of range", p.start));
        }
        if placements.get(i + 1).is_some_and(|q| q.start < p.end()) {
            return Err(format!("placements at ticks {} and {} overlap", p.start, placements[i + 1].start));
        }
    }
    Ok(LoadedTrack { selected: t.selected, clips, placements })
}

/// Check a clip name.
fn check_clip_name(name: &str) -> Result<String, RpcError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 32 {
        return Err(RpcError::invalid("clip names are 1..32 characters"));
    }
    Ok(name.to_string())
}

/// Undo sets of clip keys (see `Core::clip_doc`), gathered by `apply_sets`.
#[derive(Default)]
pub(super) struct ClipSets {
    /// `clip:<id>.<n>`: name and length, or `None` to delete it.
    pub clips: BTreeMap<(String, u32), Option<(String, Option<u32>)>>,
    /// `event:<id>.<n>.<tick>.<note>`: (tick, note, (len, velocity) or
    /// `None` to remove).
    pub events: BTreeMap<(String, u32), Vec<(u32, u8, Option<(u32, u8)>)>>,
    /// `selected:<id>`.
    pub selected: BTreeMap<String, u32>,
    /// `place:<id>.<start>`: (start, placement or `None` to remove).
    pub places: BTreeMap<String, Vec<(u32, Option<Placement>)>>,
}

impl ClipSets {
    /// Parse one key's value into the sets; `None` if it is not a clip key
    /// (or is malformed).
    pub fn parse(&mut self, kind: &str, rest: &str, v: &Value) -> Option<()> {
        match kind {
            "clip" => {
                let (id, n) = rest.split_once('.')?;
                let value = match v {
                    Value::Null => None,
                    v => Some((
                        v.get("name").and_then(Value::as_str)?.to_string(),
                        v.get("length").and_then(Value::as_u64).map(|l| l as u32),
                    )),
                };
                self.clips.insert((id.to_string(), n.parse().ok()?), value);
            }
            "event" => {
                // `<id>.<n>.<tick>.<note>`; ids have no dots.
                let mut parts = rest.splitn(4, '.');
                let id = parts.next()?;
                let n: u32 = parts.next()?.parse().ok()?;
                let tick = parts.next()?.parse().ok()?;
                let note = parts.next()?.parse().ok()?;
                let val = match v {
                    Value::Null => None,
                    v => Some((
                        v.get("len").and_then(Value::as_u64).unwrap_or(TICKS_PER_STEP as u64) as u32,
                        v.get("velocity").and_then(Value::as_u64).unwrap_or(VEL_ON as u64) as u8,
                    )),
                };
                self.events.entry((id.to_string(), n)).or_default().push((tick, note, val));
            }
            "selected" => {
                self.selected.insert(rest.to_string(), v.as_u64()? as u32);
            }
            "place" => {
                let (id, start) = rest.split_once('.')?;
                let start: u32 = start.parse().ok()?;
                let p = match v {
                    Value::Null => None,
                    v => Some(Placement {
                        clip: v.get("clip").and_then(Value::as_u64)? as u32,
                        start,
                        length: v.get("length").and_then(Value::as_u64)? as u32,
                        offset: v.get("offset").and_then(Value::as_u64).unwrap_or(0) as u32,
                    }),
                };
                self.places.entry(id.to_string()).or_default().push((start, p));
            }
            _ => return None,
        }
        Some(())
    }

    /// Drop everything that belongs to the instruments in `ids`.
    pub fn retain_not(&mut self, ids: &[String]) {
        self.clips.retain(|(id, _), _| !ids.contains(id));
        self.events.retain(|(id, _), _| !ids.contains(id));
        self.selected.retain(|id, _| !ids.contains(id));
        self.places.retain(|id, _| !ids.contains(id));
    }

    /// Engine commands they may take, at most.
    pub fn commands(&self) -> usize {
        self.clips.len() * 2
            + self.events.values().map(Vec::len).sum::<usize>()
            + self.selected.len()
            + self.places.values().map(Vec::len).sum::<usize>() * 2
    }
}

impl Core {
    // ---- queries -------------------------------------------------------------

    pub(super) fn track(&self, id: &str) -> &TrackState {
        &self.tracks[id]
    }

    /// A clip by instrument and id (default: the selected clip).
    pub(super) fn clip_id(&self, id: &str, n: Option<u32>) -> Result<u32, RpcError> {
        let t = self.track(id);
        let n = n.unwrap_or(t.selected);
        if !t.clips.contains_key(&n) {
            let ids: Vec<String> = t.clips.keys().map(|k| k.to_string()).collect();
            return Err(RpcError::invalid(format!("no clip {n} on '{id}' (clips: {})", ids.join(", "))));
        }
        Ok(n)
    }

    pub(super) fn clip(&self, id: &str, n: u32) -> Clip {
        let c = &self.track(id).clips[&n];
        Clip { instrument: id.to_string(), id: n, name: c.name.clone(), length: c.length, events: c.events.clone() }
    }

    pub(super) fn track_info(&self, id: &str) -> TrackInfo {
        let t = self.track(id);
        TrackInfo {
            instrument: id.to_string(),
            selected: t.selected,
            clips: t.clips.iter().map(|(n, c)| ClipHeader { id: *n, name: c.name.clone(), length: c.length }).collect(),
            arrangement: t.placements.clone(),
        }
    }

    pub(super) fn track_changed(&mut self, id: &str, origin: &str) {
        let track = self.track_info(id);
        self.emit(origin, Event::Track { track });
        self.mark_dirty(origin);
    }

    pub(super) fn is_drums(&self, id: &str) -> bool {
        self.tracks.get(id).is_some_and(|t| t.kind == InstrumentType::Tr808)
    }

    /// The drum grid an instrument's selected clip shows.
    pub(super) fn drums(&self, id: &str) -> [[u8; MAX_STEPS]; NUM_TRACKS] {
        self.tracks.get(id).map(|t| drum_grid(&t.selected().events)).unwrap_or([[STEP_OFF; MAX_STEPS]; NUM_TRACKS])
    }

    /// The note steps an instrument's selected clip shows.
    pub(super) fn note_view(&self, id: &str) -> [NoteStep; MAX_STEPS] {
        self.tracks.get(id).map(|t| note_steps(&t.selected().events)).unwrap_or([NoteStep::default(); MAX_STEPS])
    }

    /// A clip's loop length in ticks (its own, else `sequencer.length`).
    pub(super) fn clip_len(&self, id: &str, n: u32) -> u32 {
        self.track(id).clips.get(&n).and_then(|c| c.length).unwrap_or(self.length() * TICKS_PER_STEP)
    }

    // ---- the clip table ------------------------------------------------------

    /// Clips that can still be made (the engine's clip table).
    pub(super) fn free_clips(&self) -> usize {
        self.clip_used.iter().filter(|u| !**u).count()
    }

    /// Make a clip in a pool, with its contents, in a free slot of the
    /// engine's clip table. The caller checks there is one and queue room.
    pub(super) fn add_clip_unchecked(&mut self, id: &str, n: u32, name: String, length: Option<u32>, events: Vec<ClipEvent>) {
        let engine = self.clip_used.iter().position(|u| !u).expect("checked by the caller") as u16;
        self.clip_used[engine as usize] = true;
        // A reused table entry may hold an old clip.
        self.send(Command::ClearClip { clip: engine });
        if length.is_some() {
            self.send(Command::SetClipLength { clip: engine, length });
        }
        for e in &events {
            self.send(Command::AddEvent { clip: engine, event: *e });
        }
        let t = self.tracks.get_mut(id).expect("track exists");
        t.clips.insert(n, ClipState { name, length, events, engine });
    }

    /// A new track with one empty clip, selected (on an instrument just
    /// added to the engine).
    pub(super) fn add_track_unchecked(&mut self, id: &str, kind: InstrumentType, slot: u8) {
        self.tracks.insert(id.to_string(), TrackState { kind, clips: BTreeMap::new(), selected: 1, placements: Vec::new() });
        self.add_clip_unchecked(id, 1, "1".into(), None, Vec::new());
        let engine = self.track(id).clips[&1].engine;
        self.send(Command::SelectClip { slot, clip: Some(engine) });
    }

    /// Replace a new instrument's track with a loaded one. The caller checked
    /// clip table and queue room (`validate_project`).
    pub(super) fn load_track(&mut self, id: &str, t: LoadedTrack) -> Result<(), RpcError> {
        let kind = self.track(id).kind;
        self.remove_track(id);
        self.tracks.insert(id.to_string(), TrackState { kind, clips: BTreeMap::new(), selected: t.selected, placements: Vec::new() });
        for (n, name, length, events) in t.clips {
            self.add_clip_unchecked(id, n, name, length, events);
        }
        let (slot, engine) = (self.slot(id)?, self.track(id).selected().engine);
        self.send(Command::SelectClip { slot, clip: Some(engine) });
        self.set_arrangement(id, t.placements)
    }

    /// Forget a track (its instrument left the engine, which resets the
    /// slot), freeing its clips' table entries.
    pub(super) fn remove_track(&mut self, id: &str) {
        if let Some(t) = self.tracks.remove(id) {
            for c in t.clips.values() {
                self.clip_used[c.engine as usize] = false;
            }
        }
    }

    // ---- the one edit path ---------------------------------------------------

    /// Replace a clip's events (and, with `Some`, its length). The engine
    /// gets only the difference; clients get `clip_changed` and, for the
    /// selected clip, the step view's events.
    pub(super) fn edit_clip(
        &mut self,
        id: &str,
        n: u32,
        events: Vec<ClipEvent>,
        length: Option<Option<u32>>,
        origin: &str,
    ) -> Result<Clip, RpcError> {
        self.slot(id)?;
        let n = self.clip_id(id, Some(n))?;
        let events = normalize_events(&events).map_err(RpcError::invalid)?;
        if let Some(Some(l)) = length
            && (l == 0 || l > MAX_CLIP_TICKS)
        {
            return Err(RpcError::invalid(format!("clip length must be 1..{MAX_CLIP_TICKS} ticks")));
        }
        let old = &self.track(id).clips[&n];
        let engine = old.engine;
        let removed: Vec<ClipEvent> =
            old.events.iter().filter(|e| events.binary_search_by_key(&e.key(), ClipEvent::key).is_err()).copied().collect();
        let added: Vec<ClipEvent> = events.iter().filter(|e| !old.events.contains(e)).copied().collect();
        let new_length = length.unwrap_or(old.length);
        let length_changed = new_length != old.length;
        if removed.is_empty() && added.is_empty() && !length_changed {
            return Ok(self.clip(id, n));
        }
        self.ensure_room(removed.len() + added.len() + 1)?;
        let selected = self.track(id).selected == n;
        let (old_grid, old_notes) = (self.drums(id), self.note_view(id));
        for e in &removed {
            self.send(Command::RemoveEvent { clip: engine, tick: e.tick, note: e.note });
        }
        for e in &added {
            self.send(Command::AddEvent { clip: engine, event: *e });
        }
        if length_changed {
            self.send(Command::SetClipLength { clip: engine, length: new_length });
        }
        let c = self.tracks.get_mut(id).and_then(|t| t.clips.get_mut(&n)).expect("checked");
        c.events = events;
        c.length = new_length;
        if selected {
            self.emit_step_views(id, &old_grid, &old_notes, origin);
        }
        let clip = self.clip(id, n);
        self.emit(origin, Event::ClipChanged { clip: clip.clone() });
        self.mark_dirty(origin);
        self.refresh_controller(origin, false);
        Ok(clip)
    }

    /// The step views' events, so editors update without knowing clips.
    fn emit_step_views(
        &mut self,
        id: &str,
        old_grid: &[[u8; MAX_STEPS]; NUM_TRACKS],
        old_notes: &[NoteStep; MAX_STEPS],
        origin: &str,
    ) {
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
            if notes != *old_notes {
                self.emit(origin, Event::NotesChanged { instrument: id.to_string(), steps: notes.to_vec() });
            }
        }
    }

    /// Select the clip pattern mode plays and the step editors edit.
    pub(super) fn select_clip(&mut self, id: &str, n: u32, origin: &str) -> Result<TrackInfo, RpcError> {
        let n = self.clip_id(id, Some(n))?;
        if self.track(id).selected != n {
            self.ensure_room(1)?;
            let (old_grid, old_notes) = (self.drums(id), self.note_view(id));
            let (slot, engine) = (self.slot(id)?, self.track(id).clips[&n].engine);
            self.send(Command::SelectClip { slot, clip: Some(engine) });
            self.tracks.get_mut(id).expect("checked").selected = n;
            self.emit_step_views(id, &old_grid, &old_notes, origin);
            self.track_changed(id, origin);
            self.refresh_controller(origin, false);
        }
        Ok(self.track_info(id))
    }

    /// A new clip in a pool with the given contents (`n`: its id, default
    /// the next).
    pub(super) fn new_clip(
        &mut self,
        id: &str,
        n: Option<u32>,
        name: Option<String>,
        length: Option<u32>,
        events: Vec<ClipEvent>,
        select: bool,
        origin: &str,
    ) -> Result<Clip, RpcError> {
        if self.free_clips() == 0 {
            return Err(RpcError::failed(format!("at most {MAX_CLIPS} clips in a project")));
        }
        if length.is_some_and(|l| l == 0 || l > MAX_CLIP_TICKS) {
            return Err(RpcError::invalid(format!("clip length must be 1..{MAX_CLIP_TICKS} ticks")));
        }
        let events = normalize_events(&events).map_err(RpcError::invalid)?;
        let n = n.unwrap_or_else(|| self.track(id).next_id());
        if self.track(id).clips.contains_key(&n) {
            return Err(RpcError::invalid(format!("'{id}' already has a clip {n}")));
        }
        let name = match name {
            Some(name) => check_clip_name(&name)?,
            None => n.to_string(),
        };
        self.ensure_room(events.len() + 4)?;
        self.add_clip_unchecked(id, n, name, length, events);
        let clip = self.clip(id, n);
        self.emit(origin, Event::ClipChanged { clip: clip.clone() });
        if select {
            self.select_clip(id, n, origin)?;
        }
        self.track_changed(id, origin);
        Ok(clip)
    }

    /// Delete a clip and its placements. A track keeps at least one clip;
    /// deleting the selected one selects the lowest other.
    pub(super) fn delete_clip(&mut self, id: &str, n: u32, origin: &str) -> Result<TrackInfo, RpcError> {
        let n = self.clip_id(id, Some(n))?;
        let t = self.track(id);
        if t.clips.len() == 1 {
            return Err(RpcError::invalid(format!("'{id}' keeps at least one clip")));
        }
        let placements: Vec<Placement> = t.placements.iter().filter(|p| p.clip != n).copied().collect();
        self.ensure_room(t.placements.len() + 2)?;
        if t.selected == n {
            let other = *t.clips.keys().find(|k| **k != n).expect("more than one");
            self.select_clip(id, other, origin)?;
        }
        self.set_arrangement(id, placements)?;
        let c = self.tracks.get_mut(id).and_then(|t| t.clips.remove(&n)).expect("checked");
        self.clip_used[c.engine as usize] = false;
        self.emit(origin, Event::ClipDeleted { instrument: id.to_string(), id: n });
        self.track_changed(id, origin);
        Ok(self.track_info(id))
    }

    // ---- step views ----------------------------------------------------------

    pub(super) fn check_step(step: u32) -> Result<(), RpcError> {
        if step as usize >= MAX_STEPS {
            return Err(RpcError::invalid(format!("step must be 0..{}", MAX_STEPS - 1)));
        }
        Ok(())
    }

    /// One drum step of the selected clip: the event on the step's first
    /// tick at the voice's GM note.
    pub fn set_step(&mut self, id: &str, voice: Voice, step: u32, level: u8, origin: &str) -> Result<StepResult, RpcError> {
        Self::check_step(step)?;
        if level > STEP_ACCENT {
            return Err(RpcError::invalid("level must be 0 (off), 1 (on), or 2 (accent)"));
        }
        let tick = step * TICKS_PER_STEP;
        let t = self.track(id);
        let mut events: Vec<ClipEvent> =
            t.selected().events.iter().filter(|e| e.key() != (tick, voice.gm_note())).copied().collect();
        events.extend(drum_event(voice, step as usize, level));
        self.edit_clip(id, t.selected, events, None, origin)?;
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
        let t = self.track(id);
        let mut events: Vec<ClipEvent> = t
            .selected()
            .events
            .iter()
            .filter(|e| {
                let step = (e.tick / TICKS_PER_STEP) as usize;
                !(e.note == voice.gm_note() && on_drum_grid(e) && changed.contains(&step))
            })
            .copied()
            .collect();
        events.extend(changed.iter().filter_map(|s| drum_event(voice, *s, full[*s])));
        self.edit_clip(id, t.selected, events, None, origin)?;
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
        let t = self.track(id);
        let mut events: Vec<ClipEvent> = t
            .selected()
            .events
            .iter()
            .filter(|e| !(on_note_grid(e) && changed.contains(&((e.tick / TICKS_PER_STEP) as usize))))
            .copied()
            .collect();
        events.extend(changed.iter().filter_map(|i| note_event(*i, &full[*i])));
        self.edit_clip(id, t.selected, events, None, origin)?;
        Ok(self.notes_result(id))
    }

    // ---- clip RPCs -----------------------------------------------------------

    pub(super) fn clip_target(&self, id: Option<&str>, client: &str) -> Result<String, RpcError> {
        match id {
            Some(id) => Ok(self.find_instrument(id)?.id.clone()),
            None => self.default_instrument(client),
        }
    }

    /// The instrument and clip a request names (defaults: the caller's
    /// focus and its selected clip).
    fn clip_ref(&self, id: Option<&str>, n: Option<u32>, client: &str) -> Result<(String, u32), RpcError> {
        let id = self.clip_target(id, client)?;
        let n = self.clip_id(&id, n)?;
        Ok((id, n))
    }

    /// Clip RPCs. Any other request is handed back.
    pub(super) fn handle_clip(&mut self, req: Request, origin: &str, client: &str) -> Result<RpcResult, Request> {
        let r = |c: Result<Clip, RpcError>| c.and_then(ok);
        let events = |c: &Self, id: &str, n: u32| c.track(id).clips[&n].events.clone();
        Ok(match req {
            Request::ClipGet(p) => self.clip_ref(p.instrument.as_deref(), p.clip, client).map(|(id, n)| self.clip(&id, n)).and_then(ok),
            Request::ClipSet(p) => r(self
                .clip_ref(p.instrument.as_deref(), p.clip, client)
                .and_then(|(id, n)| self.edit_clip(&id, n, p.events, None, origin))),
            Request::ClipAdd(p) => r(self.clip_ref(p.instrument.as_deref(), p.clip, client).and_then(|(id, n)| {
                let mut list = events(self, &id, n);
                list.extend(p.events);
                self.edit_clip(&id, n, list, None, origin)
            })),
            Request::ClipRemove(p) => r(self.clip_ref(p.instrument.as_deref(), p.clip, client).and_then(|(id, n)| {
                let mut list = events(self, &id, n);
                list.retain(|e| !p.events.iter().any(|k| (k.tick, k.note) == e.key()));
                self.edit_clip(&id, n, list, None, origin)
            })),
            Request::ClipUpdate(p) => r(self.clip_ref(p.instrument.as_deref(), p.clip, client).and_then(|(id, n)| {
                let mut list = events(self, &id, n);
                list.retain(|e| !p.remove.iter().any(|k| (k.tick, k.note) == e.key()));
                list.extend(p.add);
                self.edit_clip(&id, n, list, None, origin)
            })),
            Request::ClipLength(p) => r(self.clip_ref(p.instrument.as_deref(), p.clip, client).and_then(|(id, n)| {
                let list = events(self, &id, n);
                self.edit_clip(&id, n, list, Some(p.length), origin)
            })),
            Request::ClipClear(p) => r(self
                .clip_ref(p.instrument.as_deref(), p.clip, client)
                .and_then(|(id, n)| self.edit_clip(&id, n, Vec::new(), None, origin))),
            Request::ClipQuantize(p) => r(self.clip_ref(p.instrument.as_deref(), p.clip, client).and_then(|(id, n)| {
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
                let (strength, loop_len) = (p.strength.unwrap_or(1.0), self.clip_len(&id, n));
                let list: Vec<ClipEvent> = events(self, &id, n)
                    .iter()
                    .map(|e| ClipEvent { tick: snap_tick(e.tick, p.grid, strength, loop_len), ..*e })
                    .collect();
                self.edit_clip(&id, n, list, None, origin)
            })),
            Request::ClipNew(p) => r(self
                .clip_target(p.instrument.as_deref(), client)
                .and_then(|id| self.new_clip(&id, None, p.name, p.length, Vec::new(), p.select.unwrap_or(true), origin))),
            Request::ClipDuplicate(p) => r(self.clip_ref(p.instrument.as_deref(), p.clip, client).and_then(|(id, n)| {
                let c = &self.track(&id).clips[&n];
                let (name, length, list) = (format!("{} copy", c.name), c.length, c.events.clone());
                let name: String = name.chars().take(32).collect();
                self.new_clip(&id, None, Some(name), length, list, true, origin)
            })),
            Request::ClipRename(p) => r(self.clip_ref(p.instrument.as_deref(), p.clip, client).and_then(|(id, n)| {
                let name = check_clip_name(&p.name)?;
                self.tracks.get_mut(&id).and_then(|t| t.clips.get_mut(&n)).expect("checked").name = name;
                let clip = self.clip(&id, n);
                self.emit(origin, Event::ClipChanged { clip: clip.clone() });
                self.track_changed(&id, origin);
                Ok(clip)
            })),
            Request::ClipDelete(p) => self
                .clip_ref(p.instrument.as_deref(), p.clip, client)
                .and_then(|(id, n)| self.delete_clip(&id, n, origin))
                .and_then(ok),
            Request::ClipSelect(p) => self
                .clip_ref(p.instrument.as_deref(), p.clip, client)
                .and_then(|(id, n)| self.select_clip(&id, n, origin))
                .and_then(ok),
            other => return Err(other),
        })
    }

    // ---- journal doc ---------------------------------------------------------

    /// Tracks as undo keys: `clip:<id>.<n>` -> `{name, length}` for each
    /// clip in a pool, one key per event (`event:<id>.<n>.<tick>.<note>` ->
    /// `{len, velocity}`), so two people editing different notes never
    /// conflict, `selected:<id>`, and `place:<id>.<start>` -> `{clip,
    /// length, offset}` per placement.
    pub(super) fn clip_doc(&self, d: &mut Doc) {
        for (id, t) in &self.tracks {
            for (n, c) in &t.clips {
                d.insert(format!("clip:{id}.{n}"), json!({ "name": c.name, "length": c.length }));
                for e in &c.events {
                    d.insert(format!("event:{id}.{n}.{}.{}", e.tick, e.note), json!({ "len": e.len, "velocity": e.velocity }));
                }
            }
            d.insert(format!("selected:{id}"), json!(t.selected));
            for p in &t.placements {
                d.insert(format!("place:{id}.{}", p.start), json!({ "clip": p.clip, "length": p.length, "offset": p.offset }));
            }
        }
    }

    /// Apply undo sets of track keys: clips are made first (with their
    /// ids), then contents, selection, and arrangements are restored, then
    /// clips are deleted. `fresh` instruments were just made by the undo:
    /// their default clip goes unless the sets keep it. Returns keys that
    /// could not be set.
    pub(super) fn apply_clip_sets(&mut self, mut sets: ClipSets, fresh: &[String], origin: &str) -> Vec<String> {
        let mut skipped = Vec::new();
        // A fresh instrument's track is exactly what the sets say.
        for id in fresh {
            let restores_clips = sets.clips.keys().any(|(i, _)| i == id);
            if restores_clips && !sets.clips.contains_key(&(id.clone(), 1)) {
                sets.clips.insert((id.clone(), 1), None);
            }
        }
        // Make clips.
        for ((id, n), v) in &sets.clips {
            let Some((name, length)) = v else { continue };
            if !self.tracks.contains_key(id) {
                skipped.push(format!("clip:{id}.{n}"));
                continue;
            }
            if self.track(id).clips.contains_key(n) {
                let c = self.tracks.get_mut(id).and_then(|t| t.clips.get_mut(n)).expect("checked");
                let renamed = c.name != *name;
                c.name = name.clone();
                let events = c.events.clone();
                if self.edit_clip(id, *n, events, Some(*length), origin).is_err() {
                    skipped.push(format!("clip:{id}.{n}"));
                } else if renamed {
                    let clip = self.clip(id, *n);
                    self.emit(origin, Event::ClipChanged { clip });
                    self.track_changed(id, origin);
                }
            } else if self.new_clip(id, Some(*n), Some(name.clone()), *length, Vec::new(), false, origin).is_err() {
                skipped.push(format!("clip:{id}.{n}"));
            }
        }
        // Contents.
        for ((id, n), list) in &sets.events {
            let Some(c) = self.tracks.get(id).and_then(|t| t.clips.get(n)) else {
                skipped.push(format!("clip:{id}.{n}"));
                continue;
            };
            let mut events = c.events.clone();
            for (tick, note, v) in list {
                events.retain(|e| e.key() != (*tick, *note));
                if let Some((len, velocity)) = v {
                    events.push(ClipEvent { tick: *tick, note: *note, len: *len, velocity: *velocity });
                }
            }
            if self.edit_clip(id, *n, events, None, origin).is_err() {
                skipped.push(format!("clip:{id}.{n}"));
            }
        }
        for (id, n) in &sets.selected {
            if !self.tracks.contains_key(id) || self.select_clip(id, *n, origin).is_err() {
                skipped.push(format!("selected:{id}"));
            }
        }
        for (id, list) in &sets.places {
            if !self.tracks.contains_key(id) {
                skipped.push(format!("place:{id}"));
                continue;
            }
            let mut placements = self.track(id).placements.clone();
            for (start, p) in list {
                placements.retain(|q| q.start != *start);
                placements.extend(p);
            }
            placements.sort_by_key(|p| p.start);
            if self.set_arrangement(id, placements).is_err() {
                skipped.push(format!("place:{id}"));
            } else {
                self.track_changed(id, origin);
            }
        }
        // Delete clips last (their placements are gone by now).
        for ((id, n), v) in &sets.clips {
            if v.is_some() || !self.tracks.get(id).is_some_and(|t| t.clips.contains_key(n)) {
                continue;
            }
            if self.delete_clip(id, *n, origin).is_err() {
                skipped.push(format!("clip:{id}.{n}"));
            }
        }
        skipped
    }
}
