//! 4S audio engine: instruments (TR-808-style drums, a TB-303-style bass), a
//! shared step sequencer, and a multi-channel stereo mixer.
//!
//! The engine has no I/O of its own. The daemon drives it from an audio
//! callback via `RtEngine`, or offline via `offline::render_graph`.

pub mod dsp;
pub mod effect;
pub mod engine;
pub mod instrument;
pub mod modulator;
pub mod offline;
pub mod params;
pub mod rt;
pub mod tb303;
pub mod tr808;
pub mod voices;

pub use engine::{Command, Engine, Feedback, MASTER_CHAIN, ModRoute, ParamTarget, Placement, Returned};
pub use instrument::Instrument;
pub use rt::{EngineLink, RETURN_CAPACITY, RtEngine};

/// Parameters of a device that is not an instrument, by kind.
pub fn device_params(kind: fours_protocol::DeviceType, id: &str) -> Vec<fours_protocol::ParamInfo> {
    match kind.kind() {
        fours_protocol::DeviceKind::Modulator => modulator::params(kind, id),
        _ => effect::params(kind, id),
    }
}

/// Knob pages of a device that is not an instrument, by kind.
pub fn device_knob_pages(kind: fours_protocol::DeviceType, id: &str) -> Vec<fours_protocol::KnobPage> {
    match kind.kind() {
        fours_protocol::DeviceKind::Modulator => modulator::knob_pages(kind, id),
        _ => effect::knob_pages(kind, id),
    }
}
