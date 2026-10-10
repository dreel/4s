//! Recording (RFC 0008): live notes into an instrument's clip.
//!
//! The engine reports each live note with the song tick the player heard
//! (`Feedback::Live`). A `Take` collects them and, once per loop pass (one
//! step before the loop's end, so the pass plays back in the next one) and
//! when recording ends, says what the clip becomes: quantized toward the
//! record grid, added to the clip (`overdub`) or replacing the part of the
//! loop the pass covered (`replace`). Each write goes through `edit_clip` as
//! one journal entry, so a pass is one undo step.
//!
//! `Take` is pure so an offline render (`render.offline` with `record`) runs
//! exactly the same code over the engine's feedback.

use super::*;
use fours_engine::offline::LiveNote;

/// Record settings; they persist between takes (not saved in the project).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordSettings {
    pub mode: RecordMode,
    pub quantize: Option<u32>,
    pub strength: f32,
    pub count_in: u32,
    pub offset_ms: f32,
}

impl Default for RecordSettings {
    fn default() -> Self {
        Self { mode: RecordMode::Overdub, quantize: None, strength: 1.0, count_in: 1, offset_ms: 0.0 }
    }
}

impl RecordSettings {
    /// Apply the settings a request names, checking all of them first.
    pub fn update(&mut self, p: &RecordParams) -> Result<(), RpcError> {
        if p.strength.is_some_and(|s| !(0.0..=1.0).contains(&s)) {
            return Err(RpcError::invalid("strength must be 0..1"));
        }
        if p.count_in.is_some_and(|c| c > 4) {
            return Err(RpcError::invalid("count_in must be 0..4 bars"));
        }
        if p.offset_ms.is_some_and(|o| !(0.0..=500.0).contains(&o)) {
            return Err(RpcError::invalid("offset_ms must be 0..500"));
        }
        if p.quantize.is_some_and(|q| q > MAX_CLIP_TICKS) {
            return Err(RpcError::invalid(format!("quantize must be 0 (off) or 1..{MAX_CLIP_TICKS} ticks")));
        }
        if let Some(m) = p.mode {
            self.mode = m;
        }
        if let Some(q) = p.quantize {
            self.quantize = (q > 0).then_some(q);
        }
        if let Some(s) = p.strength {
            self.strength = s;
        }
        if let Some(c) = p.count_in {
            self.count_in = c;
        }
        if let Some(o) = p.offset_ms {
            self.offset_ms = o;
        }
        Ok(())
    }
}

/// Notes played earlier than this before the song's tick 0 (during a
/// count-in) are not recorded; later ones are, just before the downbeat.
const EARLY: f64 = TICKS_PER_STEP as f64;

/// One recording, by song tick.
#[derive(Clone)]
pub struct Take {
    /// Held notes: (note, start, velocity).
    open: Vec<(u8, f64, u8)>,
    /// Released notes not written yet: (start, end, note, velocity).
    closed: Vec<(f64, f64, u8, u8)>,
    /// Start of the span not written yet.
    from: f64,
}

impl Take {
    /// A take starting at song tick `from`.
    pub fn new(from: f64) -> Self {
        Self { open: Vec::new(), closed: Vec::new(), from: from.max(0.0) }
    }

    /// A live note at song tick `tick` (already moved earlier by the
    /// settings' offset).
    pub fn note(&mut self, on: bool, gate: bool, note: u8, velocity: f32, tick: f64) {
        if on {
            if tick < -EARLY {
                return;
            }
            let velocity = (velocity * 127.0).round().clamp(1.0, 127.0) as u8;
            if gate {
                self.closed.push((tick, tick + NOTE_LEN as f64, note, velocity));
                return;
            }
            self.release(note, tick);
            self.open.push((note, tick, velocity));
        } else {
            self.release(note, tick);
        }
    }

    fn release(&mut self, note: u8, tick: f64) {
        if let Some(i) = self.open.iter().position(|(n, _, _)| *n == note) {
            let (_, start, velocity) = self.open.remove(i);
            self.closed.push((start, tick.max(start + 1.0), note, velocity));
        }
    }

    /// Whether a pass is due to be written at song tick `tick`, in a loop
    /// of `length` ticks (as long as it is now).
    pub fn due(&self, tick: u64, length: u32) -> bool {
        tick >= next_flush(self.from, length)
    }

    /// Write what was played up to song tick `upto` (with `end`, also the
    /// notes still held, released there). Returns the clip's new events, or
    /// `None` if nothing changes.
    pub fn flush(
        &mut self,
        upto: f64,
        end: bool,
        s: &RecordSettings,
        length: u32,
        existing: &[ClipEvent],
    ) -> Option<Vec<ClipEvent>> {
        if end {
            for (note, _, _) in self.open.clone() {
                self.release(note, upto);
            }
        }
        let length = length.max(1);
        let (a, b) = (self.from, upto.max(self.from));
        self.from = b;
        let mut events = existing.to_vec();
        if s.mode == RecordMode::Replace && b > a {
            let l = length as f64;
            let (la, lb) = (a.rem_euclid(l), b.rem_euclid(l));
            let covered = |t: u32| {
                let t = t as f64;
                b - a >= l || if la <= lb { t >= la && t < lb } else { t >= la || t < lb }
            };
            events.retain(|e| !covered(e.tick));
        }
        for (start, end, note, velocity) in self.closed.drain(..) {
            let local = (start.rem_euclid(length as f64).round() as u32) % length;
            let tick = match s.quantize {
                Some(g) => snap_tick(local, g, s.strength, length),
                None => local,
            };
            let len = ((end - start).round() as u32).clamp(1, length);
            events.retain(|e| e.key() != (tick, note));
            events.push(ClipEvent { tick, len, note, velocity });
        }
        events.sort_by_key(ClipEvent::key);
        (events != existing).then_some(events)
    }
}

/// The first pass end after `after`: one step before each loop end (for
/// loops longer than two steps), so a pass is written before its first
/// notes come round again. On a step's first tick, since writes happen as
/// steps start (a loop of any tick length still writes before it wraps).
fn next_flush(after: f64, length: u32) -> u64 {
    let (l, step) = (length.max(1) as u64, TICKS_PER_STEP as u64);
    let margin = if l > 2 * step { step } else { 0 };
    let after = after.max(0.0) as u64;
    let mut k = after / l + 1;
    loop {
        let at = (k * l - margin) / step * step;
        if at > after {
            return at;
        }
        k += 1;
    }
}

/// Ticks a delay of `ms` covers at `tempo` (swing aside).
pub fn ms_to_ticks(ms: f32, tempo: f64) -> f64 {
    ms as f64 / 1000.0 * tempo / 60.0 * PPQ as f64
}

/// Record the `Live` notes of `slot` in an offline render's feedback into
/// `existing`, as a live take would: the clip it leaves.
pub fn record_feedback(
    feedback: &[Feedback],
    slot: u8,
    s: &RecordSettings,
    length: u32,
    tempo: f64,
    existing: &[ClipEvent],
) -> Vec<ClipEvent> {
    let offset = ms_to_ticks(s.offset_ms, tempo);
    let mut take = Take::new(0.0);
    let mut events = existing.to_vec();
    let mut last = 0u64;
    for f in feedback {
        match *f {
            Feedback::Live { slot: sl, note, velocity, on, gate, tick: Some(t), .. } if sl == slot => {
                take.note(on, gate, note, velocity, t - offset);
            }
            Feedback::Step { tick, .. } => {
                last = tick;
                if take.due(tick, length)
                    && let Some(e) = take.flush(tick as f64, false, s, length, &events)
                {
                    events = e;
                }
            }
            _ => {}
        }
    }
    let upto = (last + TICKS_PER_STEP as u64) as f64;
    take.flush(upto, true, s, length, &events).unwrap_or(events)
}

/// What an offline render plays and records (RFC 0008).
pub struct RenderInput {
    pub notes: Vec<LiveNote>,
    /// With `record`: the instrument, settings, and its loop and clip.
    pub record: Option<RenderRecord>,
}

pub struct RenderRecord {
    pub instrument: String,
    /// Its index in the render spec (its slot in the render).
    pub slot: u8,
    pub settings: RecordSettings,
    pub length: u32,
    pub clip_length: Option<u32>,
    pub events: Vec<ClipEvent>,
    pub tempo: f64,
}

impl RenderRecord {
    /// Record a render's feedback: the clip the take would leave.
    pub fn clip(&self, feedback: &[Feedback]) -> Clip {
        let events = record_feedback(feedback, self.slot, &self.settings, self.length, self.tempo, &self.events);
        Clip { instrument: self.instrument.clone(), length: self.clip_length, events }
    }
}

/// A take in progress on the live engine.
pub(super) struct LiveTake {
    pub take: Take,
    pub instrument: String,
    pub slot: u8,
    pub user: String,
    /// The connection that started it (`conn:<id>`), whose seat the
    /// journal records.
    pub client: String,
    pub origin: String,
}

impl Core {
    /// The notes an offline render plays, and what it records: into
    /// `record.instrument`, else the caller's seat focus.
    pub fn render_input(&self, p: &RenderParams, client: &str) -> Result<RenderInput, RpcError> {
        let input = p.input.as_deref().unwrap_or_default();
        if input.is_empty() && p.record.is_none() {
            return Ok(RenderInput { notes: Vec::new(), record: None });
        }
        let id = self.clip_target(p.record.as_ref().and_then(|r| r.instrument.as_deref()), client)?;
        let slot = self.instruments.iter().position(|i| i.id == id).expect("resolved") as u8;
        let mut notes = Vec::new();
        for n in input {
            if n.note > 127 || n.velocity.is_some_and(|v| v == 0 || v > 127) || !n.time.is_finite() || n.time < 0.0 {
                return Err(RpcError::invalid("input notes: time >= 0, note 0..127, velocity 1..127"));
            }
            notes.push(LiveNote {
                time: n.time,
                slot,
                note: n.note,
                velocity: n.velocity.unwrap_or(100) as f32 / 127.0,
                duration: n.duration.unwrap_or(0.125).max(0.0),
            });
        }
        let record = match &p.record {
            None => None,
            Some(r) => {
                let mut settings = self.record.clone();
                settings.update(r)?;
                let c = &self.clips[&id];
                Some(RenderRecord {
                    instrument: id.clone(),
                    slot,
                    settings,
                    length: self.loop_len(&id),
                    clip_length: c.length,
                    events: c.events.clone(),
                    tempo: self.global(params::TEMPO),
                })
            }
        };
        Ok(RenderInput { notes, record })
    }

    /// An instrument's loop length in ticks.
    pub(super) fn loop_len(&self, id: &str) -> u32 {
        self.clips.get(id).and_then(|c| c.length).unwrap_or(self.length() * TICKS_PER_STEP)
    }

    pub(super) fn record_state(&self) -> RecordState {
        let s = &self.record;
        RecordState {
            recording: self.take.is_some(),
            instrument: self.take.as_ref().map(|t| t.instrument.clone()),
            user: self.take.as_ref().map(|t| t.user.clone()),
            mode: s.mode,
            quantize: s.quantize,
            strength: s.strength,
            count_in: s.count_in,
            offset_ms: s.offset_ms,
        }
    }

    pub(super) fn record_changed(&mut self, origin: &str) {
        let state = self.record_state();
        self.emit(origin, Event::Record { state });
    }

    /// `transport.record`: settings, then start or end a take. Starting
    /// from a stop plays after the count-in.
    pub(super) fn transport_record(
        &mut self,
        p: RecordParams,
        origin: &str,
        client: &str,
        user: &str,
    ) -> Result<RecordState, RpcError> {
        let mut settings = self.record.clone();
        settings.update(&p)?;
        let target = match (p.arm, &p.instrument) {
            (Some(true), id) => Some(self.clip_target(id.as_deref(), client)?),
            (_, Some(id)) => Some(self.find_instrument(id)?.id.clone()),
            _ => None,
        };
        self.record = settings;
        match p.arm {
            Some(true) => {
                let id = target.expect("resolved above");
                // (`settle_take` already ended a take into another one.)
                if self.take.as_ref().is_some_and(|t| t.instrument != id) {
                    self.end_take();
                }
                if self.take.is_none() {
                    let slot = self.slot(&id)?;
                    let from = if self.playing { self.play_tick as f64 } else { 0.0 };
                    if !self.playing {
                        self.start(origin, self.record.count_in * TICKS_PER_BAR);
                    }
                    let take = Take::new(from);
                    self.take =
                        Some(LiveTake {
                        take,
                        instrument: id,
                        slot,
                        user: user.to_string(),
                        client: client.to_string(),
                        origin: origin.to_string(),
                    });
                }
            }
            Some(false) => self.end_take(),
            None => {}
        }
        self.record_changed(origin);
        Ok(self.record_state())
    }

    /// A live note from the engine: part of the take if it is on the
    /// instrument being recorded.
    pub(super) fn record_note(&mut self, slot: u8, note: u8, velocity: f32, on: bool, gate: bool, tick: f64) {
        let offset = ms_to_ticks(self.record.offset_ms, self.global(params::TEMPO));
        if let Some(t) = self.take.as_mut().filter(|t| t.slot == slot) {
            t.take.note(on, gate, note, velocity, tick - offset);
        }
    }

    /// A step started at song tick `tick`: write a pass when one is due.
    pub(super) fn record_step(&mut self, tick: u64) {
        let due = self.take.as_ref().is_some_and(|t| t.take.due(tick, self.loop_len(&t.instrument)));
        if due {
            self.write_take(tick as f64, false);
        }
    }

    /// End the take, writing the notes played (those still held end now).
    pub(super) fn end_take(&mut self) {
        if self.take.is_some() {
            self.write_take((self.play_tick + TICKS_PER_STEP as u64) as f64, true);
            self.take = None;
        }
    }

    fn write_take(&mut self, upto: f64, end: bool) {
        let Some(mut t) = self.take.take() else { return };
        let (length, settings) = (self.loop_len(&t.instrument), self.record.clone());
        let existing = self.clips.get(&t.instrument).map(|c| c.events.clone()).unwrap_or_default();
        // Kept so a write that fails (a full command queue) is tried again
        // with the next pass instead of losing these notes.
        let unwritten = t.take.clone();
        if let Some(events) = t.take.flush(upto, end, &settings, length, &existing) {
            let (id, origin) = (t.instrument.clone(), t.origin.clone());
            // Journaled as the `clip.update` that makes the same change, so
            // a replay writes the same notes.
            let remove = existing
                .iter()
                .filter(|e| events.binary_search_by_key(&e.key(), ClipEvent::key).is_err())
                .map(|e| EventKey { tick: e.tick, note: e.note })
                .collect();
            let add = events.iter().filter(|e| !existing.contains(e)).copied().collect();
            let update = ClipUpdateParams { instrument: Some(id.clone()), remove, add, recorded: true };
            let params = serde_json::to_value(&update).expect("params serialize");
            let result =
                self.journaled(&t.user, &t.client, &origin, "clip.update", params, |c| c.edit_clip(&id, events, None, &origin));
            if let Err(e) = result {
                tracing::warn!("recording into {id}: {}", e.message);
                // A busy engine may take it next pass; an invalid clip (too
                // many events) never will.
                if e.code != RpcError::invalid("").code {
                    t.take = unwritten;
                }
            }
        }
        self.take = Some(t);
    }

    /// Before a transport request: stopping (or `record --off`) ends a
    /// take, and playing again restarts it from the new start, writing what
    /// was played. Done before the request is journaled, so the take's entry
    /// comes first and a replay sees the same entries.
    pub(super) fn settle_take(&mut self, req: &Request, origin: &str, client: &str) {
        let Some(current) = self.take.as_ref().map(|t| t.instrument.clone()) else { return };
        let ends = match req {
            Request::TransportStop(_) | Request::TransportRecord(RecordParams { arm: Some(false), .. }) => true,
            // Recording into another instrument ends this take.
            Request::TransportRecord(p @ RecordParams { arm: Some(true), .. }) => {
                self.clip_target(p.instrument.as_deref(), client).is_ok_and(|id| id != current)
            }
            _ => false,
        };
        if ends {
            self.end_take();
            self.record_changed(origin);
            return;
        }
        match req {
            Request::TransportPlay(_) => self.restart_take(),
            _ => {}
        }
    }

    /// The transport restarts from tick 0: write the take so far, then
    /// keep recording from the start.
    pub(super) fn restart_take(&mut self) {
        if self.take.is_some() {
            self.write_take((self.play_tick + TICKS_PER_STEP as u64) as f64, true);
            if let Some(t) = self.take.as_mut() {
                t.take = Take::new(0.0);
            }
        }
    }

    /// Drop a take without writing it (its instrument or project went away).
    pub(super) fn drop_take(&mut self, origin: &str) {
        if self.take.take().is_some() {
            self.record_changed(origin);
        }
    }
}
