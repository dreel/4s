//! Clips (RFC 0007, phase 2): an instrument's sequence as timed note events
//! on a tick clock. Step patterns are views over clips: a drum step is an
//! event on the step's first tick at its voice's GM note, a note step an
//! event on the step's first tick. These conversions live here so the
//! daemon, the offline renderer, the CLI, and migrations agree.
//!
//! Tracks (RFC 0008 phase B): each instrument has a pool of clips, one of
//! them selected (what pattern mode plays and the step editors edit), and an
//! arrangement of placements on the song timeline.

use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Ticks per quarter note.
pub const PPQ: u32 = 96;
/// Ticks per sequencer step (a 16th).
pub const TICKS_PER_STEP: u32 = PPQ / 4;
/// Ticks per 4/4 bar.
pub const TICKS_PER_BAR: u32 = PPQ * 4;
/// Most events a clip holds.
pub const MAX_EVENTS: usize = 1024;
/// Most clips, across every track.
pub const MAX_CLIPS: usize = 256;
/// Most placements on one track's arrangement.
pub const MAX_PLACEMENTS: usize = 256;
/// The song's longest extent, in ticks (999 bars).
pub const MAX_SONG_TICKS: u32 = 999 * TICKS_PER_BAR;
/// Longest clip, in ticks (`MAX_STEPS` steps).
pub const MAX_CLIP_TICKS: u32 = MAX_STEPS as u32 * TICKS_PER_STEP;

/// Velocity of an "on" step (0.7, the level steps have always played at).
pub const VEL_ON: u8 = 89;
/// Velocity of an accented step.
pub const VEL_ACCENT: u8 = 127;
/// Velocities at or above this read as accented in step views: the same
/// 0.95 at which the 303 plays an accent (121/127).
pub const VEL_ACCENT_MIN: u8 = 121;
/// A 303 note step gates for half a step.
pub const NOTE_LEN: u32 = TICKS_PER_STEP / 2;
/// A sliding 303 step holds one tick past the next step's start, so the
/// next note starts while it is still held and glides (legato).
pub const SLIDE_LEN: u32 = TICKS_PER_STEP + 1;

/// One note in a clip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS, JsonSchema)]
pub struct ClipEvent {
    /// Start, in ticks from the clip's start (`PPQ` per quarter note).
    pub tick: u32,
    /// Length in ticks (at least 1).
    pub len: u32,
    /// MIDI note; a drum machine plays the voice on that GM note.
    pub note: u8,
    /// 1..127.
    pub velocity: u8,
}

impl ClipEvent {
    /// Events are ordered and unique by (tick, note).
    pub fn key(&self) -> (u32, u8) {
        (self.tick, self.note)
    }
}

/// A clip in an instrument's pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct Clip {
    pub instrument: String,
    /// Its id in the instrument's pool (1, 2, ...).
    pub id: u32,
    pub name: String,
    /// Length in ticks; `None` follows `sequencer.length`.
    pub length: Option<u32>,
    /// Sorted by (tick, note).
    pub events: Vec<ClipEvent>,
}

/// A note a take is recording, not written yet: where it will go (in
/// pattern mode, the selected clip; in song mode, the clip under it), at
/// the tick it was played (before quantize), held so far if still held.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct TakeNote {
    pub clip: u32,
    pub tick: u32,
    pub len: u32,
    pub note: u8,
    pub velocity: u8,
    /// Still held.
    pub held: bool,
}

/// A clip without its events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct ClipHeader {
    pub id: u32,
    pub name: String,
    pub length: Option<u32>,
}

/// A clip placed on the song timeline. It plays from `offset` ticks into
/// the clip, looping at the clip's length, for `length` ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct Placement {
    pub clip: u32,
    /// Song tick it starts at.
    pub start: u32,
    pub length: u32,
    #[serde(default)]
    pub offset: u32,
}

impl Placement {
    pub fn end(&self) -> u32 {
        self.start + self.length
    }
}

/// An instrument's track: its clip pool, the selected clip, and its
/// arrangement (placements sorted by start, not overlapping).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct TrackInfo {
    pub instrument: String,
    pub selected: u32,
    pub clips: Vec<ClipHeader>,
    pub arrangement: Vec<Placement>,
}

/// Lay `p` onto an arrangement: placements it covers go, ones it overlaps
/// are cut to what is outside it (the right part keeps playing from where
/// it was, by its offset). Returns the new arrangement, sorted.
pub fn place(arrangement: &[Placement], p: Placement) -> Vec<Placement> {
    let mut out = Vec::with_capacity(arrangement.len() + 2);
    for q in arrangement {
        if q.end() <= p.start || q.start >= p.end() {
            out.push(*q);
            continue;
        }
        if q.start < p.start {
            out.push(Placement { length: p.start - q.start, ..*q });
        }
        if q.end() > p.end() {
            let cut = p.end() - q.start;
            out.push(Placement { start: p.end(), length: q.end() - p.end(), offset: q.offset + cut, ..*q });
        }
    }
    out.push(p);
    out.sort_by_key(|q| q.start);
    out
}

/// Check and normalize events: in range, sorted, unique by (tick, note)
/// (the later of two duplicates wins).
pub fn normalize_events(events: &[ClipEvent]) -> Result<Vec<ClipEvent>, String> {
    let mut out: Vec<ClipEvent> = Vec::with_capacity(events.len());
    for e in events {
        if e.tick >= MAX_CLIP_TICKS {
            return Err(format!("tick {} is past the longest clip ({MAX_CLIP_TICKS} ticks)", e.tick));
        }
        if e.note > 127 || e.velocity == 0 || e.velocity > 127 || e.len == 0 {
            return Err(format!("event at tick {}: note 0..127, velocity 1..127, len >= 1", e.tick));
        }
        out.retain(|x| x.key() != e.key());
        out.push(*e);
    }
    out.sort_by_key(ClipEvent::key);
    if out.len() > MAX_EVENTS {
        return Err(format!("at most {MAX_EVENTS} events in a clip"));
    }
    Ok(out)
}

// ---- quantize ---------------------------------------------------------------

/// Move `tick` toward the nearest multiple of `grid` by `strength` (0..1; 1
/// lands exactly on the grid). A tick that ends up at or past the loop's end
/// wraps to its start, where it would play; one past the longest clip takes
/// the last grid line before it.
pub fn snap_tick(tick: u32, grid: u32, strength: f32, loop_len: u32) -> u32 {
    let g = grid.max(1);
    let nearest = (tick + g / 2) / g * g;
    let moved = tick as f64 + (nearest as f64 - tick as f64) * strength.clamp(0.0, 1.0) as f64;
    let t = moved.round() as u32;
    let t = if tick < loop_len && t >= loop_len { t - loop_len } else { t };
    if t >= MAX_CLIP_TICKS { (MAX_CLIP_TICKS - 1) / g * g } else { t }
}

/// Grid names the CLI and UI offer, with their size in ticks: `1/4` .. `1/32`,
/// and triplets (`1/8t` = three in the time of two 1/8s).
pub const GRIDS: &[(&str, u32)] = &[
    ("1/4", PPQ),
    ("1/8", PPQ / 2),
    ("1/8t", PPQ / 3),
    ("1/16", PPQ / 4),
    ("1/16t", PPQ / 6),
    ("1/32", PPQ / 8),
];

/// Parse a grid: a name from `GRIDS`, `1` (a bar), `1/2`, or `off`
/// (`None`).
pub fn parse_grid(s: &str) -> Result<Option<u32>, String> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "off" | "none" => return Ok(None),
        "1" | "1/1" => return Ok(Some(TICKS_PER_BAR)),
        "1/2" => return Ok(Some(PPQ * 2)),
        _ => {}
    }
    GRIDS
        .iter()
        .find(|(n, _)| *n == s)
        .map(|(_, t)| Some(*t))
        .ok_or_else(|| format!("invalid grid '{s}' (1/4, 1/8, 1/8t, 1/16, 1/16t, 1/32, or off)"))
}

/// A grid's name (`1/16`), or its ticks when it has none.
pub fn format_grid(grid: Option<u32>) -> String {
    match grid {
        None => "off".into(),
        Some(t) => GRIDS.iter().find(|(_, g)| *g == t).map(|(n, _)| n.to_string()).unwrap_or_else(|| format!("{t} ticks")),
    }
}

// ---- step views ---------------------------------------------------------------

/// The drum event for `voice` at `step` with `level` (`None` for off).
pub fn drum_event(voice: Voice, step: usize, level: u8) -> Option<ClipEvent> {
    (level != STEP_OFF).then(|| ClipEvent {
        tick: step as u32 * TICKS_PER_STEP,
        len: TICKS_PER_STEP,
        note: voice.gm_note(),
        velocity: if level == STEP_ACCENT { VEL_ACCENT } else { VEL_ON },
    })
}

/// The drum grid a clip shows: events on a step's first tick at a voice's
/// GM note. Other events are not on the grid.
pub fn drum_grid(events: &[ClipEvent]) -> [[u8; MAX_STEPS]; NUM_TRACKS] {
    let mut grid = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
    for e in events.iter().filter(|e| e.tick % TICKS_PER_STEP == 0) {
        let step = (e.tick / TICKS_PER_STEP) as usize;
        if let (Some(v), true) = (Voice::ALL.iter().find(|v| v.gm_note() == e.note), step < MAX_STEPS) {
            grid[v.index()][step] = if e.velocity >= VEL_ACCENT_MIN { STEP_ACCENT } else { STEP_ON };
        }
    }
    grid
}

/// Whether an event is one cell of the drum grid.
pub fn on_drum_grid(e: &ClipEvent) -> bool {
    e.tick % TICKS_PER_STEP == 0 && Voice::ALL.iter().any(|v| v.gm_note() == e.note)
}

/// The note event for `step` (`None` for a rest).
pub fn note_event(step: usize, s: &NoteStep) -> Option<ClipEvent> {
    s.note.map(|note| ClipEvent {
        tick: step as u32 * TICKS_PER_STEP,
        len: if s.slide { SLIDE_LEN } else { NOTE_LEN },
        note,
        velocity: if s.accent { VEL_ACCENT } else { VEL_ON },
    })
}

/// The note steps a clip shows: the lowest event on each step's first tick.
/// A note held past the next step's start slides.
pub fn note_steps(events: &[ClipEvent]) -> [NoteStep; MAX_STEPS] {
    let mut steps = [NoteStep::default(); MAX_STEPS];
    for e in events.iter().filter(|e| e.tick % TICKS_PER_STEP == 0) {
        let i = (e.tick / TICKS_PER_STEP) as usize;
        if i < MAX_STEPS && steps[i].note.is_none() {
            steps[i] =
                NoteStep { note: Some(e.note), accent: e.velocity >= VEL_ACCENT_MIN, slide: e.len > TICKS_PER_STEP };
        }
    }
    steps
}

/// Whether an event is one cell of the note-step view (on a step's first
/// tick).
pub fn on_note_grid(e: &ClipEvent) -> bool {
    e.tick % TICKS_PER_STEP == 0
}

// ---- text form ----------------------------------------------------------------

/// Format events as space-separated `tick:note:len:vel` tokens, notes by
/// name (`0:C2:12:89 24:D#2:25:127`).
pub fn format_events(events: &[ClipEvent]) -> String {
    events
        .iter()
        .map(|e| {
            let name = if (NOTE_MIN..=NOTE_MAX).contains(&e.note) { note_name(e.note) } else { e.note.to_string() };
            format!("{}:{name}:{}:{}", e.tick, e.len, e.velocity)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse `tick:note[:len[:vel]]` tokens (inverse of `format_events`). A note
/// is a name (`C2`) or a number; len defaults to one step, velocity to
/// `VEL_ON`.
pub fn parse_events(s: &str) -> Result<Vec<ClipEvent>, String> {
    let mut out = Vec::new();
    for tok in s.split_whitespace() {
        let parts: Vec<&str> = tok.split(':').collect();
        if !(2..=4).contains(&parts.len()) {
            return Err(format!("invalid event '{tok}' (expected tick:note[:len[:vel]], e.g. 0:C2:12:89)"));
        }
        let num = |p: &str, what: &str| p.parse::<u32>().map_err(|_| format!("invalid {what} in '{tok}'"));
        let note = match parts[1].parse::<u8>() {
            Ok(n) => n,
            Err(_) => parse_note(parts[1])?,
        };
        out.push(ClipEvent {
            tick: num(parts[0], "tick")?,
            note,
            len: parts.get(2).map(|p| num(p, "len")).transpose()?.unwrap_or(TICKS_PER_STEP),
            velocity: match parts.get(3).map(|p| num(p, "velocity")).transpose()? {
                None => VEL_ON,
                Some(v) if (1..=127).contains(&v) => v as u8,
                Some(v) => return Err(format!("velocity {v} in '{tok}' is not 1..127")),
            },
        });
    }
    normalize_events(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_views_round_trip_through_events() {
        let steps = parse_notes("C2 C2! D#2~ - G1").unwrap();
        let events: Vec<ClipEvent> = steps.iter().enumerate().filter_map(|(i, s)| note_event(i, s)).collect();
        assert_eq!(note_steps(&events).to_vec(), steps);

        let mut grid = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
        grid[0][0] = STEP_ACCENT;
        grid[1][4] = STEP_ON;
        let events: Vec<ClipEvent> = Voice::ALL
            .iter()
            .flat_map(|v| (0..MAX_STEPS).filter_map(move |s| drum_event(*v, s, grid[v.index()][s])))
            .collect();
        assert_eq!(drum_grid(&normalize_events(&events).unwrap()), grid);
    }

    #[test]
    fn snap_moves_by_strength_and_wraps_at_the_loop_end() {
        assert_eq!(snap_tick(30, 24, 1.0, 384), 24);
        assert_eq!(snap_tick(30, 24, 0.5, 384), 27);
        assert_eq!(snap_tick(30, 24, 0.0, 384), 30);
        assert_eq!(snap_tick(380, 24, 1.0, 384), 0, "the loop's end is its start");
        assert_eq!(snap_tick(380, 24, 0.5, 384), 382);
        assert_eq!(parse_grid("1/16").unwrap(), Some(24));
        assert_eq!(parse_grid("1/8T").unwrap(), Some(32));
        assert_eq!(parse_grid("off").unwrap(), None);
        assert!(parse_grid("1/7").is_err());
    }

    #[test]
    fn placing_over_placements_cuts_them() {
        let a = |clip, start, length, offset| Placement { clip, start, length, offset };
        let song = vec![a(1, 0, 100, 0), a(2, 100, 100, 0)];
        // Over the end of the first and the start of the second.
        assert_eq!(place(&song, a(3, 50, 100, 0)), vec![a(1, 0, 50, 0), a(3, 50, 100, 0), a(2, 150, 50, 50)]);
        // Inside one: it splits around it.
        assert_eq!(place(&song, a(3, 10, 20, 0))[..3], [a(1, 0, 10, 0), a(3, 10, 20, 0), a(1, 30, 70, 30)]);
        // Over everything.
        assert_eq!(place(&song, a(3, 0, 300, 0)), vec![a(3, 0, 300, 0)]);
    }

    #[test]
    fn text_form_round_trips() {
        let e = parse_events("24:D#2:25:127 0:C2:12 3:60").unwrap();
        assert_eq!(e[0], ClipEvent { tick: 0, len: 12, note: 36, velocity: VEL_ON });
        assert_eq!(e[1], ClipEvent { tick: 3, len: TICKS_PER_STEP, note: 60, velocity: VEL_ON });
        assert_eq!(parse_events(&format_events(&e)).unwrap(), e);
        assert!(parse_events("0:C2:12:300").is_err(), "velocity out of range");
    }
}
