//! Step views and the Block on clips (RFC 0009 phase 2). `step.get` and
//! `step.set` read and edit any clip through its instrument's note layout
//! (`fours_protocol::layout`). The Block's grid edits the clip a seat's
//! focused (armed) track is playing (its launched clip, or the placement's
//! clip under the playhead), else the clip it launched, else its selected
//! clip: drum rows for a drum machine, eight pitch rows for a pitched
//! instrument. In launch mode each row is a track: its first seven clips to
//! launch, and the last column back to the arrangement.

use super::*;
use crate::controller::GRID;

/// Ticks a step in a step view by default: a 16th.
const RESOLUTION: u32 = TICKS_PER_STEP;

impl Core {
    // ---- step views -----------------------------------------------------------

    pub(super) fn step_grid(&self, id: &str, n: u32, res: u32) -> StepGrid {
        let kind = self.find_instrument(id).map(|i| i.kind).unwrap_or(InstrumentType::Tr808);
        let layout = kind.layout();
        let c = &self.track(id).clips[&n];
        StepGrid {
            instrument: id.to_string(),
            clip: n,
            resolution: res,
            steps: c.length.div_ceil(res),
            cells: step_cells(&layout, &c.events, res),
            layout,
        }
    }

    /// Edit one step of a clip (see `fours_protocol::set_step`).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn edit_step(
        &mut self,
        id: &str,
        n: u32,
        res: u32,
        step: u32,
        note: Option<u8>,
        on: bool,
        expressions: &BTreeMap<String, bool>,
        origin: &str,
    ) -> Result<(), RpcError> {
        let layout = self.find_instrument(id)?.kind.layout();
        let events = set_step(&layout, &self.track(id).clips[&n].events, res, step, note, on, expressions)
            .map_err(RpcError::invalid)?;
        self.edit_clip(id, n, events, None, origin)?;
        Ok(())
    }

    /// `step.get` and `step.set`. Any other request is handed back.
    pub(super) fn handle_step(&mut self, req: Request, origin: &str, client: &str) -> Result<RpcResult, Request> {
        let resolution = |r: Option<u32>| match r.unwrap_or(RESOLUTION) {
            0 => Err(RpcError::invalid("resolution is 1.. ticks a step (24 = a 16th)")),
            r => Ok(r.min(MAX_CLIP_TICKS)),
        };
        Ok(match req {
            Request::StepGet(p) => (|| {
                let id = self.clip_target(p.instrument.as_deref(), client)?;
                let n = self.clip_id(&id, p.clip)?;
                Ok(self.step_grid(&id, n, resolution(p.resolution)?))
            })()
            .and_then(ok),
            Request::StepSet(p) => (|| {
                let id = self.clip_target(p.instrument.as_deref(), client)?;
                let n = self.clip_id(&id, p.clip)?;
                let res = resolution(p.resolution)?;
                self.edit_step(&id, n, res, p.step, p.note, p.on.unwrap_or(true), &p.expressions, origin)?;
                Ok(self.step_grid(&id, n, res))
            })()
            .and_then(ok),
            other => return Err(other),
        })
    }

    // ---- the Block's grid -----------------------------------------------------

    /// The instrument and clip a seat's step grid edits: what its focus
    /// plays now, else the clip it launched, else its selected clip.
    pub(super) fn grid_target(&self, seat: &str) -> Option<(String, u32)> {
        let id = self.seat_focus(seat)?;
        let t = self.track(&id);
        let n = self.track_playhead(&id).map(|p| p.clip).or(t.launched).unwrap_or(t.selected);
        Some((id, n))
    }

    /// The note of each step-grid row of a seat's focus, top first: its
    /// named rows, or eight pitches up from the base note.
    fn grid_notes(&self, seat: &str) -> Vec<u8> {
        let Some(i) = self.seat_focus(seat).and_then(|id| self.find_instrument(&id).ok()) else { return Vec::new() };
        let layout = i.kind.layout();
        if layout.rows.is_empty() {
            let base = self.controller.base_note;
            (0..GRID as u8).rev().map(|r| base.saturating_add(r).min(127)).collect()
        } else {
            layout.rows.iter().take(GRID).map(|r| r.note).collect()
        }
    }

    /// What each row shows, for `ControllerState::rows`.
    pub(super) fn grid_rows(&self, seat: &str) -> Vec<String> {
        match self.controller.grid {
            GridMode::Launch => self.instruments.iter().take(GRID).map(|i| i.id.clone()).collect(),
            GridMode::Steps => {
                let Some(i) = self.seat_focus(seat).and_then(|id| self.find_instrument(&id).ok()) else { return Vec::new() };
                let layout = i.kind.layout();
                self.grid_notes(seat)
                    .iter()
                    .map(|n| layout.rows.iter().find(|r| r.note == *n).map_or_else(|| note_name(*n), |r| r.short.clone()))
                    .collect()
            }
        }
    }

    /// Steps the host seat's grid pages over: its target clip's length in
    /// 16ths (rounded up), or one page in launch mode.
    pub(super) fn grid_length(&self) -> u32 {
        match (self.controller.grid, self.grid_target(&self.host_seat)) {
            (GridMode::Steps, Some((id, n))) => self.clip_len(&id, n).div_ceil(RESOLUTION).min(MAX_STEPS as u32),
            _ => GRID as u32,
        }
    }

    /// The step the host seat's target clip is playing, if it is.
    pub(super) fn grid_playhead(&self) -> Option<u32> {
        if self.controller.grid != GridMode::Steps {
            return None;
        }
        let (id, n) = self.grid_target(&self.host_seat)?;
        let at = self.track_playhead(&id).filter(|p| p.clip == n)?;
        Some(at.tick / RESOLUTION)
    }

    /// The host seat's grid as LEDs.
    pub(super) fn grid_leds(&self) -> crate::controller::Leds {
        let c = &self.controller;
        match c.grid {
            GridMode::Launch => {
                // A track's launched clip is lit, and its last pad while it
                // has one (press to go back).
                let mut leds = [[0u8; GRID]; GRID];
                for (r, i) in self.instruments.iter().take(GRID).enumerate() {
                    if let Some(n) = self.track(&i.id).launched {
                        if (1..GRID as u32).contains(&n) {
                            leds[r][n as usize - 1] = 1;
                        }
                        leds[r][GRID - 1] = 1;
                    }
                }
                leds
            }
            GridMode::Steps => {
                let Some((id, n)) = self.grid_target(&self.host_seat) else { return [[0; GRID]; GRID] };
                let Ok(i) = self.find_instrument(&id) else { return [[0; GRID]; GRID] };
                let cells = step_cells(&i.kind.layout(), &self.track(&id).clips[&n].events, RESOLUTION);
                let notes = self.grid_notes(&self.host_seat);
                let on = |row: usize, step: u32| cells.iter().any(|x| x.step == step && Some(&x.note) == notes.get(row));
                c.compute_leds(notes.len(), on, self.grid_length(), self.grid_playhead())
            }
        }
    }

    /// A grid pad. Steps: toggles the row's note on the pad's step of the
    /// seat's target clip (a mono instrument's step takes the pressed
    /// pitch). Launch: launches the row's track's clip, or goes back.
    pub(super) fn controller_pad(&mut self, seat: &str, row: u32, col: u32, pressed: bool, origin: &str) -> Result<(), RpcError> {
        if row as usize >= GRID || col as usize >= GRID {
            return Err(RpcError::invalid("row and col must be 0..7"));
        }
        if !pressed {
            return Ok(());
        }
        if self.controller.grid == GridMode::Launch {
            let Some(id) = self.instruments.get(row as usize).map(|i| i.id.clone()) else { return Ok(()) };
            let clip = if col as usize == GRID - 1 { None } else { Some(col + 1) };
            if clip.is_some_and(|n| !self.track(&id).clips.contains_key(&n)) {
                return Ok(());
            }
            self.launch(&id, clip, false)?;
            self.launch_changed(&id, origin);
            return Ok(());
        }
        let Some((id, n)) = self.grid_target(seat) else { return Ok(()) };
        let Some(note) = self.grid_notes(seat).get(row as usize).copied() else { return Ok(()) };
        let step = self.controller.pad_step(col);
        if step >= self.clip_len(&id, n).div_ceil(RESOLUTION) {
            return Ok(());
        }
        let layout = self.find_instrument(&id)?.kind.layout();
        let lit = step_cells(&layout, &self.track(&id).clips[&n].events, RESOLUTION)
            .iter()
            .any(|x| x.step == step && x.note == note);
        let none = BTreeMap::new();
        if lit {
            let note = if layout.mono { None } else { Some(note) };
            self.edit_step(&id, n, RESOLUTION, step, note, false, &none, origin)
        } else {
            self.edit_step(&id, n, RESOLUTION, step, Some(note), true, &none, origin)
        }
    }
}
