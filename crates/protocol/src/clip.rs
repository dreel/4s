//! Clips (RFC 0007, phase 2): an instrument's sequence as timed note events
//! on a tick clock. Step patterns are views over clips: a drum step is an
//! event on the step's first tick at its voice's GM note, a note step an
//! event on the step's first tick. These conversions live here so the
//! daemon, the offline renderer, the CLI, and migrations agree.

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

/// An instrument's clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct Clip {
    pub instrument: String,
    /// Length in ticks; `None` follows `sequencer.length`.
    pub length: Option<u32>,
    /// Sorted by (tick, note).
    pub events: Vec<ClipEvent>,
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
/// wraps to its start, where it would play.
pub fn snap_tick(tick: u32, grid: u32, strength: f32, loop_len: u32) -> u32 {
    let g = grid.max(1);
    let nearest = (tick + g / 2) / g * g;
    let moved = tick as f64 + (nearest as f64 - tick as f64) * strength.clamp(0.0, 1.0) as f64;
    let t = moved.round() as u32;
    let t = if tick < loop_len && t >= loop_len { t - loop_len } else { t };
    t.min(MAX_CLIP_TICKS - 1)
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

/// Parse a grid: a name from `GRIDS`, `1` (a bar), `1/2`, a number of
/// ticks, or `off` (`None`).
pub fn parse_grid(s: &str) -> Result<Option<u32>, String> {
    let s = s.trim().to_ascii_lowercase();
    match s.as_str() {
        "off" | "none" | "0" => return Ok(None),
        "1" | "1/1" => return Ok(Some(TICKS_PER_BAR)),
        "1/2" => return Ok(Some(PPQ * 2)),
        _ => {}
    }
    if let Some((_, t)) = GRIDS.iter().find(|(n, _)| *n == s) {
        return Ok(Some(*t));
    }
    match s.parse::<u32>() {
        Ok(t) if t <= MAX_CLIP_TICKS => Ok(Some(t)),
        _ => Err(format!("invalid grid '{s}' (1/4, 1/8, 1/8t, 1/16, 1/16t, 1/32, ticks, or off)")),
    }
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
    fn text_form_round_trips() {
        let e = parse_events("24:D#2:25:127 0:C2:12 3:60").unwrap();
        assert_eq!(e[0], ClipEvent { tick: 0, len: 12, note: 36, velocity: VEL_ON });
        assert_eq!(e[1], ClipEvent { tick: 3, len: TICKS_PER_STEP, note: 60, velocity: VEL_ON });
        assert_eq!(parse_events(&format_events(&e)).unwrap(), e);
        assert!(parse_events("0:C2:12:300").is_err(), "velocity out of range");
    }
}
