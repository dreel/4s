//! The instrument abstraction. An instrument is built on the control side
//! (`make`), handed to the audio thread through the command ring, and
//! dropped back on the control side after removal. Everything it does on the
//! audio thread (`render`, `on_step`, the setters) must be allocation-free.

use crate::tb303::Tb303;
use crate::tr808::Tr808;
use fours_protocol::{InstrumentType, KnobPage, MAX_STEPS, NoteStep, OutputInfo, OutputWidth, ParamInfo};

/// Most frames rendered per call. The engine splits larger blocks.
pub const MAX_BLOCK: usize = 256;

/// Most outputs any instrument has (the 808: main + 8 direct outs).
pub const MAX_OUTPUTS: usize = 9;

/// Something an instrument played on a step: a drum voice or a note.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub voice: Option<u8>,
    pub note: Option<u8>,
    pub velocity: f32,
}

pub trait Instrument: Send {
    fn num_outputs(&self) -> usize;
    fn output_width(&self, output: usize) -> OutputWidth;

    /// Set parameter `index` (in `params()` order).
    fn set_param(&mut self, index: usize, value: f32);

    /// A routed direct out leaves the instrument's main mix.
    fn set_routed(&mut self, _output: usize, _routed: bool) {}

    fn set_drum_step(&mut self, _track: usize, _step: usize, _level: u8) {}
    fn set_drum_track(&mut self, _track: usize, _steps: &[u8; MAX_STEPS]) {}
    fn set_notes(&mut self, _steps: &[NoteStep; MAX_STEPS]) {}

    /// A sequencer step started. `step_samples` is its length (with swing).
    fn on_step(&mut self, step: usize, step_samples: f64, emit: &mut dyn FnMut(Hit));

    /// The transport stopped.
    fn on_stop(&mut self) {}

    /// Start a note: the one input every instrument takes (RFC 0007). With
    /// `gate_samples`, it releases by itself after that long; otherwise it
    /// holds until `note_off` for the same note. Returns what played, or
    /// `None` if the note means nothing to this instrument (a drum machine
    /// maps notes to voices).
    fn note_on(&mut self, note: u8, velocity: f32, gate_samples: Option<f64>) -> Option<Hit>;

    /// Release a held note.
    fn note_off(&mut self, _note: u8) {}

    /// Render `frames` (<= MAX_BLOCK) frames into the output buffers.
    fn render(&mut self, frames: usize);

    /// Interleaved stereo buffer of the last `render` (mono outputs use the
    /// left lane only).
    fn output(&self, output: usize) -> &[f32];

    /// Jump parameter smoothers to their targets (before an offline render).
    fn snap(&mut self) {}
}

pub fn make(kind: InstrumentType, sample_rate: f32) -> Box<dyn Instrument> {
    match kind {
        InstrumentType::Tr808 => Box::new(Tr808::new(sample_rate)),
        InstrumentType::Tb303 => Box::new(Tb303::new(sample_rate)),
    }
}

/// Parameters of an instance with id `id`, in index order.
pub fn params(kind: InstrumentType, id: &str) -> Vec<ParamInfo> {
    match kind {
        InstrumentType::Tr808 => Tr808::params(id),
        InstrumentType::Tb303 => Tb303::params(id),
    }
}

/// Knob pages of an instance with id `id` (RFC 0007).
pub fn knob_pages(kind: InstrumentType, id: &str) -> Vec<KnobPage> {
    match kind {
        InstrumentType::Tr808 => Tr808::knob_pages(id),
        InstrumentType::Tb303 => Tb303::knob_pages(id),
    }
}

/// Outputs of an instance with id `id`, main first.
pub fn outputs(kind: InstrumentType, id: &str, name: &str) -> Vec<OutputInfo> {
    match kind {
        InstrumentType::Tr808 => Tr808::outputs(id, name),
        InstrumentType::Tb303 => vec![OutputInfo { source: id.to_string(), label: name.to_string(), width: OutputWidth::Mono }],
    }
}
