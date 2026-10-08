//! Livid Block controller logic: grid pads edit the target drum instrument's
//! pattern, knobs set its params, LEDs mirror the pattern and playhead. This
//! is pure state; the same logic serves the real device (via MIDI), the
//! virtual controller (via RPC), and a future bridge daemon.
//!
//! Layout: 8 rows = 8 tracks, 8 columns = 8 steps of the current page.
//! Knob N controls track N's parameter selected by the knob mode. With no
//! target (no `tr808`), the grid is dark and input does nothing.

use fours_protocol::{ControllerState, KnobMode, MAX_STEPS, NUM_TRACKS, STEP_OFF};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const GRID: usize = 8;

pub type Leds = [[u8; GRID]; GRID];

pub struct Controller {
    /// Id of the drum instrument the controller drives.
    pub target: Option<String>,
    pub knob_mode: KnobMode,
    pub page: u32,
    pub follow: bool,
    pub leds: Leds,
    pub device: Option<String>,
}

impl Default for Controller {
    fn default() -> Self {
        Self {
            target: None,
            knob_mode: KnobMode::Volume,
            page: 0,
            follow: true,
            leds: [[0; GRID]; GRID],
            device: None,
        }
    }
}

impl Controller {
    pub fn state(&self) -> ControllerState {
        ControllerState {
            target: self.target.clone(),
            knob_mode: self.knob_mode,
            knob_params: match &self.target {
                Some(t) => (0..GRID).map(|i| self.knob_mode.param_path(t, i)).collect(),
                None => Vec::new(),
            },
            page: self.page,
            follow: self.follow,
            leds: self.leds.iter().map(|r| r.to_vec()).collect(),
            device: self.device.clone(),
        }
    }

    pub fn num_pages(length: u32) -> u32 {
        length.div_ceil(GRID as u32).max(1)
    }

    /// Step index a pad maps to on the current page.
    pub fn pad_step(&self, col: u32) -> u32 {
        self.page * GRID as u32 + col
    }

    /// Recompute LEDs. Lit = step on; the playhead column is inverted so it
    /// is visible on both lit and unlit steps. Steps past `length` are dark,
    /// and everything is dark without a target pattern.
    pub fn compute_leds(
        &self,
        pattern: Option<&[[u8; MAX_STEPS]; NUM_TRACKS]>,
        length: u32,
        playhead: Option<u32>,
    ) -> Leds {
        let mut leds = [[0u8; GRID]; GRID];
        let Some(pattern) = pattern else { return leds };
        for (row, led_row) in leds.iter_mut().enumerate().take(NUM_TRACKS) {
            for (col, led) in led_row.iter_mut().enumerate() {
                let step = self.pad_step(col as u32);
                if step >= length {
                    continue;
                }
                let on = pattern[row][step as usize] != STEP_OFF;
                let at_playhead = playhead == Some(step);
                *led = (on != at_playhead) as u8;
            }
        }
        leds
    }

    /// Store new LEDs and return the cells that changed.
    pub fn set_leds(&mut self, leds: Leds) -> Vec<(usize, usize, u8)> {
        let mut diff = Vec::new();
        for r in 0..GRID {
            for c in 0..GRID {
                if self.leds[r][c] != leds[r][c] {
                    diff.push((r, c, leds[r][c]));
                }
            }
        }
        self.leds = leds;
        diff
    }
}

/// MIDI mapping for the Livid Block. Loaded from `<data-dir>/livid-block.json`
/// (written with these defaults on first run) so it can be corrected after
/// probing the device with `4s midi monitor`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlockMap {
    /// MIDI channel, 0-based.
    pub channel: u8,
    /// Note number for each pad, `grid_notes[row][col]`, row 0 at the top.
    /// The Block numbers its pads down each column: note `col * 8 + row`.
    pub grid_notes: [[u8; GRID]; GRID],
    /// CC number for each knob, left to right.
    pub knob_ccs: [u8; GRID],
    /// Note-on velocity for a lit / unlit LED.
    pub led_on_velocity: u8,
    pub led_off_velocity: u8,
}

impl Default for BlockMap {
    fn default() -> Self {
        Self::with_notes(|r, c| c * GRID + r)
    }
}

impl BlockMap {
    fn with_notes(note: impl Fn(usize, usize) -> usize) -> Self {
        let mut grid_notes = [[0u8; GRID]; GRID];
        for (r, row) in grid_notes.iter_mut().enumerate() {
            for (c, n) in row.iter_mut().enumerate() {
                *n = note(r, c) as u8;
            }
        }
        Self {
            channel: 0,
            grid_notes,
            knob_ccs: [1, 2, 3, 4, 5, 6, 7, 8],
            led_on_velocity: 127,
            led_off_velocity: 0,
        }
    }

    /// The first default (`row * 8 + col`), which had the grid transposed on
    /// the device. A saved map still equal to it was never edited by hand.
    fn transposed_default() -> Self {
        Self::with_notes(|r, c| r * GRID + c)
    }

    pub fn load_or_create(path: &Path) -> BlockMap {
        match std::fs::read_to_string(path) {
            Ok(s) => match serde_json::from_str::<BlockMap>(&s) {
                Ok(m) if m == Self::transposed_default() => {
                    tracing::info!("{}: updating the transposed default grid map", path.display());
                    let m = BlockMap::default();
                    if let Err(e) = std::fs::write(path, serde_json::to_string_pretty(&m).unwrap() + "\n") {
                        tracing::warn!("{}: could not save the updated map: {e}", path.display());
                    }
                    m
                }
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("invalid {}: {e}; using defaults", path.display());
                    BlockMap::default()
                }
            },
            Err(_) => {
                let m = BlockMap::default();
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(path, serde_json::to_string_pretty(&m).unwrap() + "\n");
                m
            }
        }
    }

    pub fn pad_for_note(&self, note: u8) -> Option<(usize, usize)> {
        for r in 0..GRID {
            for c in 0..GRID {
                if self.grid_notes[r][c] == note {
                    return Some((r, c));
                }
            }
        }
        None
    }

    pub fn knob_for_cc(&self, cc: u8) -> Option<usize> {
        self.knob_ccs.iter().position(|k| *k == cc)
    }

    pub fn led_message(&self, row: usize, col: usize, on: u8) -> [u8; 3] {
        let vel = if on != 0 { self.led_on_velocity } else { self.led_off_velocity };
        [0x90 | (self.channel & 0x0f), self.grid_notes[row][col], vel]
    }
}

/// Decoded input from a Livid Block.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BlockInput {
    Pad { row: usize, col: usize, pressed: bool },
    Knob { index: usize, value: f64 },
}

pub fn decode_block(map: &BlockMap, msg: &[u8]) -> Option<BlockInput> {
    if msg.len() < 3 || (msg[0] & 0x0f) != map.channel {
        return None;
    }
    match msg[0] & 0xf0 {
        0x90 | 0x80 => {
            let (row, col) = map.pad_for_note(msg[1])?;
            let pressed = msg[0] & 0xf0 == 0x90 && msg[2] > 0;
            Some(BlockInput::Pad { row, col, pressed })
        }
        0xb0 => {
            let index = map.knob_for_cc(msg[1])?;
            Some(BlockInput::Knob { index, value: msg[2] as f64 / 127.0 })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fours_protocol::STEP_ON;

    #[test]
    fn leds_show_steps_and_inverted_playhead() {
        let mut c = Controller::default();
        let mut pattern = [[STEP_OFF; MAX_STEPS]; NUM_TRACKS];
        pattern[0][0] = STEP_ON;
        pattern[1][9] = STEP_ON;
        let leds = c.compute_leds(Some(&pattern), 16, Some(0));
        assert_eq!(leds[0][0], 0, "lit step under playhead is inverted");
        assert_eq!(leds[1][0], 1, "unlit step under playhead is lit");
        assert_eq!(leds[1][1], 0);
        c.page = 1;
        let leds = c.compute_leds(Some(&pattern), 16, None);
        assert_eq!(leds[1][1], 1, "page 2 shows step 10 in column 2");
        let leds = c.compute_leds(Some(&pattern), 12, None);
        assert_eq!(leds[0][5], 0, "steps past length are dark");
    }

    #[test]
    fn the_old_transposed_default_is_migrated_and_edited_maps_kept() {
        let m = BlockMap::default();

        let dir = std::env::temp_dir().join(format!("4s-blockmap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("livid-block.json");
        std::fs::write(&path, serde_json::to_string(&BlockMap::transposed_default()).unwrap()).unwrap();
        assert_eq!(BlockMap::load_or_create(&path), m);
        assert_eq!(serde_json::from_str::<BlockMap>(&std::fs::read_to_string(&path).unwrap()).unwrap(), m);

        let mut custom = BlockMap::transposed_default();
        custom.channel = 3;
        std::fs::write(&path, serde_json::to_string(&custom).unwrap()).unwrap();
        assert_eq!(BlockMap::load_or_create(&path), custom, "a hand-edited map is kept");
        let _ = std::fs::remove_dir_all(dir);
    }
}
