//! Note layouts (RFC 0009 phase 2): how an instrument's notes show in a step
//! view, declared by the instrument instead of hard-coded per type. A
//! drum machine names its notes (rows); a melodic instrument has a pitch
//! range, and a monophonic one a single note per step. Note expressions are
//! per-note properties the instrument reads from an event: the 303's accent
//! (a velocity at or above a threshold) and slide (the note held into the
//! next one).
//!
//! `step_cells` reads a clip through a layout at a resolution, and
//! `set_step` edits one step, so the daemon, the CLI, the UI, and the Block
//! agree on what a step is.

use crate::clip::*;
use crate::types::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// A named note: one row of a drum machine's grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct NoteRow {
    pub note: u8,
    /// Stable id (`kick`).
    pub id: String,
    pub name: String,
    /// Two letters (`BD`).
    pub short: String,
}

/// How an expression is stored in an event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpressionKind {
    /// On when the velocity is at least `min`; setting it writes `on`,
    /// clearing it `off`.
    Velocity { min: u8, on: u8, off: u8 },
    /// The note is held one tick into the next step, so the next note
    /// starts while it sounds (a 303's slide); clearing it gives the
    /// note its usual gate.
    Overlap,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct NoteExpression {
    /// Stable id (`accent`, `slide`).
    pub id: String,
    pub label: String,
    #[serde(flatten)]
    pub kind: ExpressionKind,
}

/// How an instrument's notes show in a step view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct NoteLayout {
    /// Named rows, top first (a drum machine's voices); empty for a pitched
    /// instrument.
    pub rows: Vec<NoteRow>,
    /// Lowest and highest pitch a pitched instrument plays.
    pub low: u8,
    pub high: u8,
    /// One note at a time: a step holds at most one note.
    pub mono: bool,
    /// A new step's note: its length as a fraction of a step, and velocity.
    pub gate: f32,
    pub velocity: u8,
    pub expressions: Vec<NoteExpression>,
}

impl InstrumentType {
    /// How this instrument's notes show in a step view.
    pub fn layout(self) -> NoteLayout {
        let accent = NoteExpression {
            id: "accent".into(),
            label: "Accent".into(),
            kind: ExpressionKind::Velocity { min: VEL_ACCENT_MIN, on: VEL_ACCENT, off: VEL_ON },
        };
        match self {
            InstrumentType::Tr808 => NoteLayout {
                rows: Voice::ALL
                    .iter()
                    .map(|v| NoteRow {
                        note: v.gm_note(),
                        id: v.id().into(),
                        name: v.label().into(),
                        short: v.short().into(),
                    })
                    .collect(),
                low: 36,
                high: 56,
                mono: false,
                gate: 1.0,
                velocity: VEL_ON,
                expressions: vec![accent],
            },
            // 16 pads on GM drum notes from 36 (as `sampler::FIRST_NOTE`).
            InstrumentType::Sampler => NoteLayout {
                rows: (1..=16u8)
                    .map(|k| NoteRow { note: 35 + k, id: format!("pad{k}"), name: format!("Pad {k}"), short: format!("P{k}") })
                    .collect(),
                low: 36,
                high: 51,
                mono: false,
                gate: 1.0,
                velocity: VEL_ON,
                expressions: vec![accent],
            },
            InstrumentType::Tb303 => NoteLayout {
                rows: Vec::new(),
                low: NOTE_MIN,
                high: NOTE_MAX,
                mono: true,
                gate: 0.5,
                velocity: VEL_ON,
                expressions: vec![
                    accent,
                    NoteExpression { id: "slide".into(), label: "Slide".into(), kind: ExpressionKind::Overlap },
                ],
            },
        }
    }
}

/// One note on a step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, JsonSchema)]
pub struct StepCell {
    /// 0-based, at the view's resolution.
    pub step: u32,
    pub note: u8,
    pub velocity: u8,
    pub len: u32,
    /// The ids of the expressions that are on.
    pub expressions: Vec<String>,
}

impl NoteLayout {
    fn expression(&self, id: &str) -> Option<&NoteExpression> {
        self.expressions.iter().find(|x| x.id == id)
    }

    /// Whether an expression is on in an event (`res`: ticks per step).
    pub fn has(&self, x: &NoteExpression, e: &ClipEvent, res: u32) -> bool {
        match x.kind {
            ExpressionKind::Velocity { min, .. } => e.velocity >= min,
            ExpressionKind::Overlap => e.len > res,
        }
    }

    /// A new note's length at `res` ticks a step.
    fn gate_len(&self, res: u32) -> u32 {
        ((res as f32 * self.gate).round() as u32).max(1)
    }

    /// Whether a note shows on this layout's grid.
    pub fn shows(&self, note: u8) -> bool {
        if self.rows.is_empty() { (self.low..=self.high).contains(&note) } else { self.rows.iter().any(|r| r.note == note) }
    }
}

/// The cells of a clip at `res` ticks a step: events on a step's first tick
/// that the layout shows (a mono layout shows the lowest note of a step).
pub fn step_cells(layout: &NoteLayout, events: &[ClipEvent], res: u32) -> Vec<StepCell> {
    let res = res.max(1);
    let mut out: Vec<StepCell> = Vec::new();
    for e in events.iter().filter(|e| e.tick % res == 0 && layout.shows(e.note)) {
        let step = e.tick / res;
        if layout.mono && out.iter().any(|c| c.step == step) {
            continue;
        }
        let expressions = layout.expressions.iter().filter(|x| layout.has(x, e, res)).map(|x| x.id.clone()).collect();
        out.push(StepCell { step, note: e.note, velocity: e.velocity, len: e.len, expressions });
    }
    out
}

/// Edit one step of a clip at `res` ticks a step: turn `note` on (with the
/// layout's gate and velocity if it was off; a mono step's other notes go)
/// or off (`note` `None` on a mono layout clears the step), then set or
/// clear the named expressions on it. Events off the grid, and other
/// notes' lengths and velocities, are left alone. Returns the new events.
pub fn set_step(
    layout: &NoteLayout,
    events: &[ClipEvent],
    res: u32,
    step: u32,
    note: Option<u8>,
    on: bool,
    expressions: &BTreeMap<String, bool>,
) -> Result<Vec<ClipEvent>, String> {
    let res = res.max(1);
    let tick = step.checked_mul(res).filter(|t| *t < MAX_CLIP_TICKS).ok_or("step is past the longest clip")?;
    if let Some(n) = note.filter(|n| !layout.shows(*n)) {
        return Err(format!("note {n} is not on this instrument's grid"));
    }
    if !on && !expressions.is_empty() {
        return Err("expressions are set on a note being turned on, not off".into());
    }
    if let Some(id) = expressions.keys().find(|id| layout.expression(id).is_none()) {
        let ids: Vec<&str> = layout.expressions.iter().map(|x| x.id.as_str()).collect();
        return Err(format!("no expression '{id}' (this instrument has: {})", if ids.is_empty() { "none".into() } else { ids.join(", ") }));
    }
    let mut out: Vec<ClipEvent> = events.to_vec();
    let on_step = |e: &ClipEvent| e.tick == tick && layout.shows(e.note);
    match (note, on) {
        (None, true) => return Err("turning a step on needs a note".into()),
        (None, false) if layout.mono => out.retain(|e| !on_step(e)),
        (None, false) => return Err("turning a step off needs a note".into()),
        (Some(n), false) => out.retain(|e| e.key() != (tick, n)),
        (Some(n), true) => {
            // A mono step keeps one note: the old one's expressions carry
            // over to the new pitch.
            let old = out.iter().find(|e| on_step(e) && (layout.mono || e.note == n)).copied();
            if layout.mono {
                out.retain(|e| !on_step(e));
            } else {
                out.retain(|e| e.key() != (tick, n));
            }
            let base = ClipEvent { tick, note: n, len: layout.gate_len(res), velocity: layout.velocity };
            out.push(old.map_or(base, |o| ClipEvent { note: n, ..o }));
        }
    }
    if !expressions.is_empty() {
        let notes: Vec<u8> = match note {
            Some(n) if on => vec![n],
            _ => Vec::new(),
        };
        for e in out.iter_mut().filter(|e| e.tick == tick && notes.contains(&e.note)) {
            for (id, set) in expressions {
                let x = layout.expression(id).expect("checked");
                match x.kind {
                    ExpressionKind::Velocity { on, off, .. } => {
                        if *set != layout.has(x, e, res) {
                            e.velocity = if *set { on } else { off };
                        }
                    }
                    ExpressionKind::Overlap => {
                        if *set != layout.has(x, e, res) {
                            e.len = if *set { res + 1 } else { layout.gate_len(res) };
                        }
                    }
                }
            }
        }
    }
    out.sort_by_key(ClipEvent::key);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(layout: &NoteLayout, events: &[ClipEvent], step: u32, note: Option<u8>, on: bool, x: &[(&str, bool)]) -> Vec<ClipEvent> {
        let x = x.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        set_step(layout, events, TICKS_PER_STEP, step, note, on, &x).unwrap()
    }

    /// The declared layouts write exactly the events the old step views
    /// did (`drum_event`, `note_event`), so projects and the old `pattern.*`
    /// methods agree with the generic step view.
    #[test]
    fn layouts_write_what_the_step_views_wrote() {
        let drums = InstrumentType::Tr808.layout();
        let kick = set(&drums, &[], 2, Some(36), true, &[]);
        assert_eq!(kick, drum_event(Voice::Kick, 2, STEP_ON).into_iter().collect::<Vec<_>>());
        let accented = set(&drums, &kick, 2, Some(36), true, &[("accent", true)]);
        assert_eq!(accented, drum_event(Voice::Kick, 2, STEP_ACCENT).into_iter().collect::<Vec<_>>());
        assert!(set(&drums, &accented, 2, Some(36), false, &[]).is_empty());

        let bass = InstrumentType::Tb303.layout();
        let c2 = set(&bass, &[], 1, Some(36), true, &[("slide", true), ("accent", true)]);
        let want = NoteStep { note: Some(36), accent: true, slide: true };
        assert_eq!(c2, note_event(1, &want).into_iter().collect::<Vec<_>>());
        // A mono step takes a new pitch, keeping its expressions.
        let g2 = set(&bass, &c2, 1, Some(43), true, &[]);
        assert_eq!(g2, note_event(1, &NoteStep { note: Some(43), ..want }).into_iter().collect::<Vec<_>>());
        let plain = set(&bass, &g2, 1, Some(43), true, &[("slide", false), ("accent", false)]);
        assert_eq!(plain, note_event(1, &NoteStep { note: Some(43), accent: false, slide: false }).into_iter().collect::<Vec<_>>());
        assert!(set(&bass, &plain, 1, None, false, &[]).is_empty());
    }

    /// Cells read back what was written, at other resolutions too; notes
    /// off the grid are left alone by edits.
    #[test]
    fn cells_read_the_grid_and_edits_keep_the_rest() {
        let bass = InstrumentType::Tb303.layout();
        let off_grid = ClipEvent { tick: 30, len: 6, note: 40, velocity: 100 };
        let ev = set(&bass, &[off_grid], 2, Some(36), true, &[("slide", true)]);
        let cells = step_cells(&bass, &ev, TICKS_PER_STEP);
        assert_eq!(cells.len(), 1);
        assert_eq!((cells[0].step, cells[0].note, cells[0].expressions.clone()), (2, 36, vec!["slide".to_string()]));
        assert!(ev.contains(&off_grid));
        // At 1/8 (48 ticks) the same note is step 1, and its 25-tick length
        // is not a slide there.
        let eighth = step_cells(&bass, &ev, 48);
        assert_eq!((eighth[0].step, eighth[0].expressions.len()), (1, 0));
        // Checked: notes off the instrument's grid, unknown expressions.
        let drums = InstrumentType::Tr808.layout();
        assert!(set_step(&drums, &[], 24, 0, Some(60), true, &BTreeMap::new()).is_err());
        let slide = BTreeMap::from([("slide".to_string(), true)]);
        assert!(set_step(&drums, &[], 24, 0, Some(36), true, &slide).unwrap_err().contains("accent"));
    }
}
