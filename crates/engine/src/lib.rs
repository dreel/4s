//! 4S audio engine: TR-808-style drum voices, a step sequencer, and a mixer.
//!
//! The engine has no I/O of its own. The daemon drives it from an audio
//! callback via `RtEngine`, or offline via `offline::render_pattern`.

pub mod dsp;
pub mod engine;
pub mod offline;
pub mod params;
pub mod rt;
pub mod voices;

pub use engine::{Command, Engine, Feedback};
pub use rt::{EngineLink, RtEngine};
