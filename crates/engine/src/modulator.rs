//! Modulators (RFC 0010): devices whose outputs move parameters through
//! modulation routes. A modulator is built on the control side, handed to
//! the audio thread, and dropped back on the control side, as instruments
//! and effects are. `advance` must not allocate.

use crate::params::{cont, toggle};
use fours_protocol::{DeviceType, KnobPage, ParamInfo, ParamKind};
use std::f32::consts::TAU;

/// What a modulator may follow: the tempo and a beat clock that runs from
/// play (synced LFOs follow it, so renders are deterministic).
#[derive(Clone, Copy, Debug)]
pub struct ModContext {
    pub sr: f32,
    pub tempo: f32,
    /// Beats since the song's tick 0 at the start of this block.
    pub beat: f64,
}

pub trait Modulator: Send {
    fn set_param(&mut self, index: usize, value: f32);
    /// Advance by `frames` frames; outputs then hold this block's values.
    fn advance(&mut self, frames: usize, ctx: &ModContext);
    /// Output `k` (0-based), -1..1.
    fn output(&self, k: usize) -> f32;
}

/// LFOs in an `lfo` device.
pub const LFOS: usize = 4;
/// Parameters per LFO: shape, sync, rate, division, phase.
const LFO_PARAMS: usize = 5;

/// LFO shapes: sine, triangle, saw up, saw down, square, sample and hold.
pub const SHAPES: usize = 6;

/// Synced LFO periods, in beats: 1/16, 1/8, 1/4, 1/2, a bar, two bars,
/// four bars.
pub const PERIODS: [f64; 7] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

/// Parameters of a modulator with id `id`, in index order.
pub fn params(kind: DeviceType, id: &str) -> Vec<ParamInfo> {
    debug_assert_eq!(kind, DeviceType::Lfo);
    let int = |path: String, label: String, max: i32, default: f64| ParamInfo {
        path,
        label,
        kind: ParamKind::Integer { min: 0, max },
        default,
        unit: None,
    };
    (1..=LFOS)
        .flat_map(|k| {
            let mut sync = toggle(format!("{id}.{k}.sync"), format!("LFO {k} Sync"));
            sync.default = 1.0;
            [
                int(format!("{id}.{k}.shape"), format!("LFO {k} Shape"), SHAPES as i32 - 1, 0.0),
                sync,
                cont(format!("{id}.{k}.rate"), format!("LFO {k} Rate"), 0.0, 1.0, 0.5, None),
                int(format!("{id}.{k}.division"), format!("LFO {k} Division"), PERIODS.len() as i32 - 1, 4.0),
                cont(format!("{id}.{k}.phase"), format!("LFO {k} Phase"), 0.0, 1.0, 0.0, None),
            ]
        })
        .collect()
}

/// A knob page per LFO.
pub fn knob_pages(kind: DeviceType, id: &str) -> Vec<KnobPage> {
    let p = params(kind, id);
    (0..LFOS)
        .map(|k| KnobPage {
            id: format!("lfo{}", k + 1),
            label: format!("LFO {}", k + 1),
            params: p[k * LFO_PARAMS..(k + 1) * LFO_PARAMS].iter().map(|x| x.path.clone()).collect(),
        })
        .collect()
}

/// Its outputs' names, `<id>.1` ...
pub fn outputs(kind: DeviceType, id: &str) -> Vec<String> {
    debug_assert_eq!(kind, DeviceType::Lfo);
    (1..=LFOS).map(|k| format!("{id}.{k}")).collect()
}

pub fn make(kind: DeviceType) -> Box<dyn Modulator> {
    debug_assert_eq!(kind, DeviceType::Lfo);
    let mut m = Box::new(LfoBank::default());
    for (i, p) in params(kind, "x").iter().enumerate() {
        m.set_param(i, p.default as f32);
    }
    m
}

#[derive(Clone, Copy, Default)]
struct Lfo {
    shape: u8,
    sync: bool,
    rate: f32,
    division: usize,
    phase: f32,
    /// Free-running phase, 0..1.
    free: f64,
    /// The cycle a sample-and-hold value belongs to, and the value.
    held: (i64, f32),
    out: f32,
}

/// Four LFOs.
#[derive(Default)]
pub struct LfoBank {
    lfos: [Lfo; LFOS],
    noise: u32,
}

fn wave(shape: u8, x: f32) -> f32 {
    match shape {
        1 => 1.0 - 4.0 * (x - 0.5).abs(),
        2 => 2.0 * x - 1.0,
        3 => 1.0 - 2.0 * x,
        4 => {
            if x < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        _ => (TAU * x).sin(),
    }
}

impl Modulator for LfoBank {
    fn set_param(&mut self, index: usize, value: f32) {
        let Some(l) = self.lfos.get_mut(index / LFO_PARAMS) else { return };
        match index % LFO_PARAMS {
            0 => l.shape = value.round().clamp(0.0, (SHAPES - 1) as f32) as u8,
            1 => l.sync = value >= 0.5,
            2 => l.rate = value.clamp(0.0, 1.0),
            3 => l.division = (value.round().max(0.0) as usize).min(PERIODS.len() - 1),
            _ => l.phase = value.clamp(0.0, 1.0),
        }
    }

    fn advance(&mut self, frames: usize, ctx: &ModContext) {
        for i in 0..LFOS {
            let l = &mut self.lfos[i];
            // Cycles so far: from the beat clock when synced, else free.
            let cycles = if l.sync {
                ctx.beat / PERIODS[l.division]
            } else {
                // 0.01 Hz .. 20 Hz.
                let hz = 0.01 * 2000f64.powf(l.rate as f64);
                l.free = (l.free + hz * frames as f64 / ctx.sr as f64).fract();
                l.free
            };
            let t = cycles + l.phase as f64;
            let x = t.rem_euclid(1.0) as f32;
            l.out = if l.shape == 5 {
                let cycle = t.floor() as i64;
                if cycle != l.held.0 {
                    let mut n = self.noise.max(1);
                    n ^= n << 13;
                    n ^= n >> 17;
                    n ^= n << 5;
                    self.noise = n;
                    l.held = (cycle, (n as f32 / u32::MAX as f32) * 2.0 - 1.0);
                }
                l.held.1
            } else {
                wave(l.shape, x)
            };
        }
    }

    fn output(&self, k: usize) -> f32 {
        self.lfos.get(k).map_or(0.0, |l| l.out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synced sine LFO at one bar starts at 0, peaks a beat in, and is
    /// back at 0 half a bar in.
    #[test]
    fn synced_lfo_follows_the_beat_clock() {
        let mut m = make(DeviceType::Lfo);
        let at = |m: &mut Box<dyn Modulator>, beat: f64| {
            m.advance(1, &ModContext { sr: 48000.0, tempo: 120.0, beat });
            m.output(0)
        };
        assert!(at(&mut m, 0.0).abs() < 1e-5);
        assert!((at(&mut m, 1.0) - 1.0).abs() < 1e-5);
        assert!(at(&mut m, 2.0).abs() < 1e-4);
        assert!((at(&mut m, 3.0) + 1.0).abs() < 1e-5);
    }

    /// Every shape stays in -1..1.
    #[test]
    fn shapes_stay_in_range() {
        for shape in 0..SHAPES {
            let mut m = make(DeviceType::Lfo);
            m.set_param(0, shape as f32);
            for i in 0..200 {
                m.advance(64, &ModContext { sr: 48000.0, tempo: 120.0, beat: i as f64 * 0.07 });
                assert!(m.output(0).abs() <= 1.0, "shape {shape}");
            }
        }
    }
}
