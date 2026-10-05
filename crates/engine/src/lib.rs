//! 4S audio engine: instruments (TR-808-style drums, a TB-303-style bass), a
//! shared step sequencer, and a multi-channel stereo mixer.
//!
//! The engine has no I/O of its own. The daemon drives it from an audio
//! callback via `RtEngine`, or offline via `offline::render_graph`.

pub mod dsp;
pub mod engine;
pub mod instrument;
pub mod offline;
pub mod params;
pub mod rt;
pub mod tb303;
pub mod tr808;
pub mod voices;

pub use engine::{Command, Engine, Feedback, ParamTarget};
pub use instrument::Instrument;
pub use rt::{EngineLink, RETURN_CAPACITY, RtEngine};
