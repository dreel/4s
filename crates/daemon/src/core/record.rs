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

/// A played note: when (ticks from play, for spans and lengths), where (the
/// song position heard; in pattern mode the same), and what.
#[derive(Clone, Copy, Debug)]
struct Played {
    tick: f64,
    pos: f64,
    end: f64,
    note: u8,
    velocity: u8,
}

/// Where a take's notes go when it is written.
#[derive(Clone, Debug)]
pub enum TakeTarget {
    /// Pattern mode: the selected clip, looping at `length` ticks.
    Clip { clip: u32, length: u32 },
    /// Song mode: the track's arrangement (each placement with its clip's
    /// loop length), the song loop, and the id a new clip would get.
    Song { placements: Vec<(Placement, u32)>, looping: Option<(u32, u32)>, next_id: u32 },
}

/// What writing a take does to a track.
#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    /// A clip's new events.
    Clip { clip: u32, events: Vec<ClipEvent> },
    /// A new clip, placed at `start` for its `length`.
    New { clip: u32, start: u32, length: u32, events: Vec<ClipEvent> },
    /// A clip this take made, grown to `length` (and its placement with it).
    Grow { clip: u32, start: u32, length: u32, events: Vec<ClipEvent> },
}

/// One recording.
#[derive(Clone)]
pub struct Take {
    /// Held notes.
    open: Vec<Played>,
    /// Released notes not written yet.
    closed: Vec<Played>,
    /// Start of the span not written yet: tick from play and song position.
    from: (f64, f64),
    /// The clip this take made in song mode (id, start, length), which grows
    /// as notes come after it.
    made: Option<(u32, u32, u32)>,
}

impl Take {
    /// A take starting at tick `tick` from play, song position `pos`.
    pub fn new(tick: f64, pos: f64) -> Self {
        Self { open: Vec::new(), closed: Vec::new(), from: (tick.max(0.0), pos.max(0.0)), made: None }
    }

    /// A live note at tick `tick` from play and song position `pos` (both
    /// already moved earlier by the settings' offset).
    pub fn note(&mut self, on: bool, gate: bool, note: u8, velocity: f32, tick: f64, pos: f64) {
        if on {
            if tick < -EARLY {
                return;
            }
            let velocity = (velocity * 127.0).round().clamp(1.0, 127.0) as u8;
            let played = Played { tick, pos, end: tick + NOTE_LEN as f64, note, velocity };
            if gate {
                self.closed.push(played);
                return;
            }
            self.release(note, tick);
            self.open.push(played);
        } else {
            self.release(note, tick);
        }
    }

    fn release(&mut self, note: u8, tick: f64) {
        if let Some(i) = self.open.iter().position(|p| p.note == note) {
            let p = self.open.remove(i);
            self.closed.push(Played { end: tick.max(p.tick + 1.0), ..p });
        }
    }

    /// The notes not written yet, where they will go (unquantized; held
    /// notes as long as they are at tick `now`). In song mode, notes with no
    /// placement under them are left out (they make a clip when written).
    pub fn preview(&self, now: f64, target: &TakeTarget) -> Vec<TakeNote> {
        let held = self.open.iter().map(|p| (Played { end: now.max(p.tick + 1.0), ..*p }, true));
        self.closed
            .iter()
            .map(|p| (*p, false))
            .chain(held)
            .filter_map(|(p, held)| {
                let (clip, tick, clen) = match target {
                    TakeTarget::Clip { clip, length } => {
                        let l = (*length).max(1);
                        (*clip, (p.tick.rem_euclid(l as f64).round() as u32) % l, l)
                    }
                    TakeTarget::Song { placements, .. } => {
                        let at = p.pos.floor().max(0.0) as u32;
                        let (q, clen) = placements.iter().find(|(q, _)| q.start <= at && at < q.end())?;
                        let clen = (*clen).max(1);
                        (q.clip, (((p.pos - q.start as f64).max(0.0).round() as u32) + q.offset) % clen, clen)
                    }
                };
                let len = ((p.end - p.tick).round() as u32).clamp(1, clen);
                Some(TakeNote { clip, tick, len, note: p.note, velocity: p.velocity, held })
            })
            .collect()
    }

    /// Whether a pass is due to be written as the step at tick `tick` from
    /// play, song position `pos`, starts.
    pub fn due(&self, tick: u64, pos: u64, target: &TakeTarget) -> bool {
        match target {
            TakeTarget::Clip { length, .. } => tick >= next_flush(self.from.0, *length),
            // A song loop writes a step before its end; without one, the
            // take is written when it ends.
            TakeTarget::Song { looping: Some((ls, le)), .. } => {
                let (l, step) = ((le - ls) as u64, TICKS_PER_STEP as u64);
                let margin = if l > 2 * step { step } else { 0 };
                pos == (*le as u64 - margin) / step * step
            }
            TakeTarget::Song { looping: None, .. } => false,
        }
    }

    /// Write what was played up to tick `upto` from play, song position
    /// `upto_pos` (with `end`, also the notes still held, released there).
    /// `existing` gives a clip's events. Returns the writes (none if nothing
    /// changes).
    pub fn flush(
        &mut self,
        upto: f64,
        upto_pos: f64,
        end: bool,
        s: &RecordSettings,
        target: &TakeTarget,
        existing: &dyn Fn(u32) -> Vec<ClipEvent>,
    ) -> Vec<Write> {
        if end {
            for p in self.open.clone() {
                self.release(p.note, upto);
            }
        }
        let (a, a_pos) = self.from;
        let (b, b_pos) = (upto.max(a), upto_pos);
        self.from = (b, b_pos);
        let quantize = |t: u32, len: u32| match s.quantize {
            Some(g) => snap_tick(t, g, s.strength, len),
            None => t,
        };
        let notes: Vec<Played> = self.closed.drain(..).collect();
        // Each touched clip's events, starting from what it has.
        let mut edits: BTreeMap<u32, Vec<ClipEvent>> = BTreeMap::new();
        let put = |edits: &mut BTreeMap<u32, Vec<ClipEvent>>, clip: u32, e: ClipEvent| {
            let list = edits.entry(clip).or_insert_with(|| existing(clip));
            list.retain(|x| x.key() != e.key());
            list.push(e);
        };
        let mut writes = Vec::new();
        match target {
            TakeTarget::Clip { clip, length } => {
                let length = (*length).max(1);
                if s.mode == RecordMode::Replace && b > a {
                    let l = length as f64;
                    let (la, lb) = (a.rem_euclid(l), b.rem_euclid(l));
                    let covered = |t: u32| {
                        let t = t as f64;
                        b - a >= l || if la <= lb { t >= la && t < lb } else { t >= la || t < lb }
                    };
                    edits.entry(*clip).or_insert_with(|| existing(*clip)).retain(|e| !covered(e.tick));
                }
                for p in notes {
                    let local = (p.tick.rem_euclid(length as f64).round() as u32) % length;
                    let len = ((p.end - p.tick).round() as u32).clamp(1, length);
                    put(&mut edits, *clip, ClipEvent { tick: quantize(local, length), len, note: p.note, velocity: p.velocity });
                }
            }
            TakeTarget::Song { placements, looping, next_id } => {
                // The song spans the pass covered (split where a loop wraps).
                let spans: Vec<(f64, f64)> = match looping {
                    Some((ls, le)) if b - a >= (le - ls) as f64 => vec![(*ls as f64, *le as f64)],
                    Some((ls, le)) if b_pos < a_pos => vec![(a_pos, *le as f64), (*ls as f64, b_pos)],
                    _ if b_pos > a_pos => vec![(a_pos, b_pos)],
                    _ => vec![],
                };
                if s.mode == RecordMode::Replace {
                    for (p, clen) in placements {
                        for (x, y) in &spans {
                            let (ox, oy) = (x.max(p.start as f64), y.min(p.end() as f64));
                            if ox >= oy {
                                continue;
                            }
                            let clen = (*clen).max(1);
                            let from = (((ox - p.start as f64) as u32) + p.offset) % clen;
                            let len = (oy - ox).ceil() as u32;
                            edits
                                .entry(p.clip)
                                .or_insert_with(|| existing(p.clip))
                                .retain(|e| len < clen && (e.tick + clen - from) % clen >= len);
                        }
                    }
                }
                let mut outside = Vec::new();
                for n in notes {
                    let at = n.pos.floor().max(0.0) as u32;
                    match placements.iter().find(|(p, _)| p.start <= at && at < p.end()) {
                        Some((p, clen)) => {
                            let clen = (*clen).max(1);
                            let local = (((n.pos - p.start as f64).max(0.0).round() as u32) + p.offset) % clen;
                            let len = ((n.end - n.tick).round() as u32).clamp(1, clen);
                            put(&mut edits, p.clip, ClipEvent { tick: quantize(local, clen), len, note: n.note, velocity: n.velocity });
                        }
                        None => outside.push(n),
                    }
                }
                if !outside.is_empty() {
                    writes.extend(self.make_clip(&outside, placements, *next_id, existing, &quantize));
                }
            }
        }
        let mut out: Vec<Write> = edits
            .into_iter()
            .filter_map(|(clip, mut events)| {
                events.sort_by_key(ClipEvent::key);
                (events != existing(clip)).then_some(Write::Clip { clip, events })
            })
            .collect();
        out.extend(writes);
        out
    }

    /// Song mode: notes with no placement under them go in clips this take
    /// makes, in order: whole bars around them, each inside the gap between
    /// placements its notes start in (it does not cut other placements), at
    /// most the longest clip; the clip the take made last grows to fit
    /// notes that go on past it.
    fn make_clip(
        &mut self,
        notes: &[Played],
        placements: &[(Placement, u32)],
        next_id: u32,
        existing: &dyn Fn(u32) -> Vec<ClipEvent>,
        quantize: &dyn Fn(u32, u32) -> u32,
    ) -> Vec<Write> {
        let bar = TICKS_PER_BAR as f64;
        let mut notes = notes.to_vec();
        notes.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        let mut placed: Vec<Placement> = placements.iter().map(|(p, _)| *p).collect();
        let mut out: Vec<Write> = Vec::new();
        let mut next_id = next_id;
        let mut i = 0;
        while i < notes.len() {
            let at = notes[i].pos.max(0.0);
            let prev_end = placed.iter().map(Placement::end).filter(|e| *e as f64 <= at).max().unwrap_or(0);
            let next_start = placed.iter().map(|p| p.start).filter(|s| *s as f64 > at).min().unwrap_or(MAX_SONG_TICKS);
            // Grow the clip made last when this gap starts where it ends.
            let (clip, start, grow) = match self.made {
                Some((clip, start, length))
                    if start + length == prev_end && prev_end > 0 && at < (start + MAX_CLIP_TICKS) as f64 =>
                {
                    (clip, start, true)
                }
                _ => {
                    let id = next_id;
                    next_id += 1;
                    (id, (((at / bar).floor() * bar) as u32).max(prev_end), false)
                }
            };
            let limit = next_start.min(start + MAX_CLIP_TICKS).min(MAX_SONG_TICKS);
            // The notes that start before the limit go in this clip.
            let j = i + notes[i..].iter().take_while(|n| (n.pos.max(0.0) as u32) < limit).count().max(1);
            let group = &notes[i..j];
            i = j;
            let last = group.iter().map(|n| n.pos + (n.end - n.tick)).fold(0.0, f64::max);
            let end = (((last / bar).ceil() * bar) as u32).clamp(start + 1, limit.max(start + 1));
            let old_length = self.made.filter(|m| grow && m.0 == clip).map_or(0, |m| m.2);
            let length = (end - start).max(old_length);
            let mut events = match out.iter().find_map(|w| match w {
                Write::New { clip: c, events, .. } | Write::Grow { clip: c, events, .. } if *c == clip => Some(events.clone()),
                _ => None,
            }) {
                Some(e) => e,
                None if grow => existing(clip),
                None => Vec::new(),
            };
            for n in group {
                let local = ((n.pos - start as f64).max(0.0).round() as u32).min(length - 1);
                let len = ((n.end - n.tick).round() as u32).clamp(1, length);
                let e = ClipEvent { tick: quantize(local, length), len, note: n.note, velocity: n.velocity };
                events.retain(|x| x.key() != e.key());
                events.push(e);
            }
            events.sort_by_key(ClipEvent::key);
            // One write per clip: a clip made earlier in this write stays new.
            let made_now = out.iter().any(|w| matches!(w, Write::New { clip: c, .. } if *c == clip));
            out.retain(|w| !matches!(w, Write::New { clip: c, .. } | Write::Grow { clip: c, .. } if *c == clip));
            out.push(if grow && !made_now {
                Write::Grow { clip, start, length, events }
            } else {
                Write::New { clip, start, length, events }
            });
            placed.retain(|p| p.start != start);
            placed.push(Placement { clip, start, length, offset: 0 });
            self.made = Some((clip, start, length));
        }
        out
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

/// The requests that make a take's writes, so the journal (and a replay)
/// says exactly what it did.
pub fn write_requests(instrument: &str, writes: &[Write], existing: &dyn Fn(u32) -> Vec<ClipEvent>) -> Vec<Request> {
    let update = |clip: u32, events: &[ClipEvent]| {
        let old = existing(clip);
        let remove = old
            .iter()
            .filter(|e| events.binary_search_by_key(&e.key(), ClipEvent::key).is_err())
            .map(|e| EventKey { tick: e.tick, note: e.note })
            .collect();
        let add = events.iter().filter(|e| !old.contains(e)).copied().collect();
        Request::ClipUpdate(ClipUpdateParams {
            instrument: Some(instrument.to_string()),
            clip: Some(clip),
            remove,
            add,
            recorded: true,
        })
    };
    let place = |clip: u32, start: u32, length: u32| {
        Request::SongPlace(SongPlaceParams {
            instrument: Some(instrument.to_string()),
            clip: Some(clip),
            start,
            length: Some(length),
            offset: Some(0),
        })
    };
    let mut out = Vec::new();
    for w in writes {
        match w {
            Write::Clip { clip, events } => out.push(update(*clip, events)),
            Write::New { clip, start, length, events } => {
                out.push(Request::ClipNew(ClipNewParams {
                    instrument: Some(instrument.to_string()),
                    name: None,
                    length: Some(*length),
                    select: Some(false),
                }));
                out.push(Request::ClipUpdate(ClipUpdateParams {
                    instrument: Some(instrument.to_string()),
                    clip: Some(*clip),
                    remove: Vec::new(),
                    add: events.clone(),
                    recorded: true,
                }));
                out.push(place(*clip, *start, *length));
            }
            Write::Grow { clip, start, length, events } => {
                out.push(Request::ClipLength(ClipLengthParams {
                    instrument: Some(instrument.to_string()),
                    clip: Some(*clip),
                    length: Some(*length),
                }));
                out.push(update(*clip, events));
                out.push(place(*clip, *start, *length));
            }
        }
    }
    out
}

/// Record the `Live` notes of `slot` in an offline render's feedback, as a
/// live take would: the writes it makes, applied to `clips` (the track's
/// clips by id; new ones are added).
pub fn record_feedback(
    feedback: &[Feedback],
    slot: u8,
    s: &RecordSettings,
    target: &TakeTarget,
    tempo: f64,
    clips: &mut BTreeMap<u32, (Option<u32>, Vec<ClipEvent>)>,
) {
    let offset = ms_to_ticks(s.offset_ms, tempo);
    let start = match feedback.iter().find_map(|f| if let Feedback::Step { pos, .. } = f { Some(*pos) } else { None }) {
        Some(pos) => pos as f64,
        None => 0.0,
    };
    let mut take = Take::new(0.0, start);
    let mut target = target.clone();
    let (mut last, mut last_pos) = (0u64, start as u64);
    let apply = |writes: Vec<Write>, clips: &mut BTreeMap<u32, (Option<u32>, Vec<ClipEvent>)>, target: &mut TakeTarget| {
        for w in writes {
            match w {
                Write::Clip { clip, events } => clips.entry(clip).or_default().1 = events,
                Write::New { clip, start, length, events } | Write::Grow { clip, start, length, events } => {
                    clips.insert(clip, (Some(length), events));
                    if let TakeTarget::Song { placements, next_id, .. } = target {
                        let p = Placement { clip, start, length, offset: 0 };
                        let laid = place(&placements.iter().map(|(p, _)| *p).collect::<Vec<_>>(), p);
                        let len = |c: u32, ps: &[(Placement, u32)]| ps.iter().find(|(q, _)| q.clip == c).map(|(_, l)| *l);
                        *placements = laid.iter().map(|q| (*q, if q.clip == clip { length } else { len(q.clip, placements).unwrap_or(length) })).collect();
                        *next_id = (*next_id).max(clip + 1);
                    }
                }
            }
        }
    };
    for f in feedback {
        match *f {
            Feedback::Live { slot: sl, note, velocity, on, gate, tick: Some(t), pos: Some(p), .. } if sl == slot => {
                take.note(on, gate, note, velocity, t - offset, p - offset);
            }
            Feedback::Step { tick, pos, .. } => {
                (last, last_pos) = (tick, pos);
                if take.due(tick, pos, &target) {
                    let existing = |c: u32| clips.get(&c).map(|x| x.1.clone()).unwrap_or_default();
                    let writes = take.flush(tick as f64, pos as f64, false, s, &target, &existing);
                    apply(writes, clips, &mut target);
                }
            }
            _ => {}
        }
    }
    let step = TICKS_PER_STEP as u64;
    let existing = |c: u32| clips.get(&c).map(|x| x.1.clone()).unwrap_or_default();
    let writes = take.flush((last + step) as f64, (last_pos + step) as f64, true, s, &target, &existing);
    apply(writes, clips, &mut target);
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
    pub target: TakeTarget,
    /// The track's clips by id: (name, length, events).
    pub clips: BTreeMap<u32, (String, Option<u32>, Vec<ClipEvent>)>,
    pub tempo: f64,
}

impl RenderRecord {
    /// Record a render's feedback: the clips the take would change or make.
    pub fn clips(&self, feedback: &[Feedback]) -> Vec<Clip> {
        let mut clips: BTreeMap<u32, (Option<u32>, Vec<ClipEvent>)> =
            self.clips.iter().map(|(n, (_, l, e))| (*n, (*l, e.clone()))).collect();
        record_feedback(feedback, self.slot, &self.settings, &self.target, self.tempo, &mut clips);
        clips
            .into_iter()
            .filter(|(n, (l, e))| self.clips.get(n).is_none_or(|(_, l0, e0)| l0 != l || e0 != e))
            .map(|(n, (length, events))| Clip {
                instrument: self.instrument.clone(),
                id: n,
                name: self.clips.get(&n).map_or_else(|| n.to_string(), |c| c.0.clone()),
                length,
                events,
            })
            .collect()
    }
}

/// A take in progress on the live engine.
pub(super) struct LiveTake {
    pub take: Take,
    pub instrument: String,
    pub slot: u8,
    pub user: String,
    /// Recording into the arrangement (song mode when it started), else the
    /// selected clip.
    pub song: bool,
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
                let clips = self.track(&id).clips.iter().map(|(n, c)| (*n, (c.name.clone(), c.length, c.events.clone()))).collect();
                Some(RenderRecord {
                    instrument: id.clone(),
                    slot,
                    settings,
                    target: self.take_target(&id, self.song_mode()),
                    clips,
                    tempo: self.global(params::TEMPO),
                })
            }
        };
        Ok(RenderInput { notes, record })
    }

    /// Where a take into an instrument goes now: its selected clip in
    /// pattern mode, its arrangement in song mode (`song`: which, fixed when
    /// the take started).
    pub(super) fn take_target(&self, id: &str, song: bool) -> TakeTarget {
        let t = self.track(id);
        if song {
            TakeTarget::Song {
                placements: t.placements.iter().map(|p| (*p, self.clip_len(id, p.clip))).collect(),
                looping: self.song_loop(),
                next_id: t.next_id(),
            }
        } else {
            TakeTarget::Clip { clip: t.selected, length: self.clip_len(id, t.selected) }
        }
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
                // `settle_take` ends a take into another instrument before
                // the request; this covers a caller that skipped it.
                if self.take.as_ref().is_some_and(|t| t.instrument != id) {
                    self.end_take();
                }
                if self.take.is_none() {
                    let slot = self.slot(&id)?;
                    let from = if self.playing {
                        (self.play_tick as f64, self.song_pos.map_or(self.play_tick as f64, |p| p as f64))
                    } else {
                        (0.0, if self.song_mode() { self.locate as f64 } else { 0.0 })
                    };
                    if !self.playing {
                        self.start(origin, self.record.count_in * TICKS_PER_BAR);
                    }
                    let take = Take::new(from.0, from.1);
                    self.take = Some(LiveTake {
                        take,
                        instrument: id,
                        slot,
                        song: self.song_mode(),
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
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_note(&mut self, slot: u8, note: u8, velocity: f32, on: bool, gate: bool, tick: f64, pos: f64) {
        let offset = ms_to_ticks(self.record.offset_ms, self.global(params::TEMPO));
        let looping = self.song_loop();
        if let Some(t) = self.take.as_mut().filter(|t| t.slot == slot) {
            // Moved earlier past a song loop's start, it was played before
            // the wrap: at the loop's end.
            let mut p = pos - offset;
            if let (true, Some((ls, le))) = (t.song, looping)
                && p < ls as f64
                && pos >= ls as f64
                && tick - offset >= 0.0
            {
                p += (le - ls) as f64;
            }
            t.take.note(on, gate, note, velocity, tick - offset, p);
            self.emit_take_notes(tick - offset);
        }
    }

    /// Tell clients the notes the take has not written yet.
    fn emit_take_notes(&mut self, now: f64) {
        let Some(t) = &self.take else { return };
        let notes = t.take.preview(now, &self.take_target(&t.instrument, t.song));
        let instrument = t.instrument.clone();
        self.emit(&t.origin.clone(), Event::TakeNotes { instrument, notes });
    }

    /// A step started at tick `tick` from play, song position `pos`: write
    /// a pass when one is due.
    pub(super) fn record_step(&mut self, tick: u64, pos: u64) {
        let due = self.take.as_ref().is_some_and(|t| t.take.due(tick, pos, &self.take_target(&t.instrument, t.song)));
        if due {
            self.write_take(tick as f64, pos as f64, false);
        }
    }

    /// Where the take has got to: the step in progress, from play and in
    /// the song.
    fn take_upto(&self) -> (f64, f64) {
        let step = TICKS_PER_STEP as u64;
        let tick = self.play_tick + step;
        (tick as f64, self.song_pos.map_or(tick as f64, |p| (p as u64 + step) as f64))
    }

    /// End the take, writing the notes played (those still held end now).
    pub(super) fn end_take(&mut self) {
        if let Some(t) = &self.take {
            let (instrument, origin) = (t.instrument.clone(), t.origin.clone());
            let (tick, pos) = self.take_upto();
            self.write_take(tick, pos, true);
            self.take = None;
            self.emit(&origin, Event::TakeNotes { instrument, notes: Vec::new() });
        }
    }

    fn write_take(&mut self, upto: f64, upto_pos: f64, end: bool) {
        let Some(mut t) = self.take.take() else { return };
        let (target, settings) = (self.take_target(&t.instrument, t.song), self.record.clone());
        let pool: BTreeMap<u32, Vec<ClipEvent>> =
            self.track(&t.instrument).clips.iter().map(|(n, c)| (*n, c.events.clone())).collect();
        let existing = |n: u32| pool.get(&n).cloned().unwrap_or_default();
        // Kept so a write that fails (a full command queue) is tried again
        // with the next pass instead of losing these notes.
        let unwritten = t.take.clone();
        let writes = t.take.flush(upto, upto_pos, end, &settings, &target, &existing);
        if !writes.is_empty() {
            let (id, origin, client, user) = (t.instrument.clone(), t.origin.clone(), t.client.clone(), t.user.clone());
            // Journaled as the requests that make the same change (one
            // `clip.update`, or a `batch`), so a replay writes the same
            // notes, and a pass is one undo step.
            let mut requests = write_requests(&id, &writes, &existing);
            let (method, params) = if requests.len() == 1 {
                let r = requests.remove(0);
                let method = r.method();
                let mut v = serde_json::to_value(&r).expect("request serializes");
                let params = v.get_mut("params").map(Value::take).unwrap_or(Value::Null);
                requests.push(r);
                (method, params)
            } else {
                let list: Vec<Value> = requests.iter().map(|r| serde_json::to_value(r).expect("request serializes")).collect();
                ("batch", json!({ "requests": list }))
            };
            let result = self.journaled(&user, &client, &origin, method, params, |c| {
                for r in requests {
                    c.dispatch(r, &origin, &client, &user)?;
                }
                Ok(())
            });
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
        self.emit_take_notes(upto);
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
            // Jumping while recording in song mode: what was played is
            // written, and the take goes on from the new position.
            Request::TransportLocate(p) if self.playing && self.take.as_ref().is_some_and(|t| t.song) => {
                let (tick, pos) = self.take_upto();
                self.write_take(tick, pos, true);
                let from = self.play_tick as f64;
                if let Some(t) = self.take.as_mut() {
                    t.take = Take::new(from, p.tick as f64);
                }
            }
            _ => {}
        }
    }

    /// The transport restarts from tick 0 (in song mode, the locate
    /// point): write the take so far, then keep recording from the start.
    pub(super) fn restart_take(&mut self) {
        if self.take.is_some() {
            let (tick, pos) = self.take_upto();
            self.write_take(tick, pos, true);
            let locate = self.locate as f64;
            if let Some(t) = self.take.as_mut() {
                t.take = Take::new(0.0, if t.song { locate } else { 0.0 });
            }
        }
    }

    /// Drop a take without writing it (its instrument or project went away).
    pub(super) fn drop_take(&mut self, origin: &str) {
        if let Some(t) = self.take.take() {
            self.emit(origin, Event::TakeNotes { instrument: t.instrument, notes: Vec::new() });
            self.record_changed(origin);
        }
    }
}
