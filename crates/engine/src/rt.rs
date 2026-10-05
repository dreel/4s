//! Real-time wrapper: the audio thread owns an `RtEngine` and talks to the
//! control side only through lock-free SPSC ring buffers.

use crate::engine::{Command, Engine, Feedback};
use crate::instrument::Instrument;
use rtrb::{Consumer, Producer, RingBuffer};

/// Capacity of the ring that hands removed instruments back to the control
/// side. The control side never has more than this many in flight (see
/// `Core`), so the audio thread's push always has room.
pub const RETURN_CAPACITY: usize = 4 * fours_protocol::MAX_INSTRUMENTS;

pub struct RtEngine {
    engine: Engine,
    commands: Consumer<Command>,
    feedback: Producer<Feedback>,
    returns: Producer<Box<dyn Instrument>>,
}

/// Control-side ends of the queues.
pub struct EngineLink {
    pub commands: Producer<Command>,
    pub feedback: Consumer<Feedback>,
    /// Removed instruments, to be dropped off the audio thread.
    pub returns: Consumer<Box<dyn Instrument>>,
}

impl RtEngine {
    pub fn new(engine: Engine) -> (RtEngine, EngineLink) {
        let (cp, cc) = RingBuffer::new(4096);
        let (fp, fc) = RingBuffer::new(8192);
        let (rp, rc) = RingBuffer::new(RETURN_CAPACITY);
        (
            RtEngine { engine, commands: cc, feedback: fp, returns: rp },
            EngineLink { commands: cp, feedback: fc, returns: rc },
        )
    }

    pub fn sample_rate(&self) -> u32 {
        self.engine.sample_rate()
    }

    /// Apply pending commands, then render. Call from the audio callback.
    pub fn process(&mut self, out: &mut [f32], channels: usize) {
        let fb = &mut self.feedback;
        let mut emit = |f: Feedback| {
            // Dropping feedback when the control side is behind is fine;
            // audio must never wait.
            let _ = fb.push(f);
        };
        while let Ok(cmd) = self.commands.pop() {
            if let Some(old) = self.engine.apply(cmd, &mut emit)
                && let Err(rtrb::PushError::Full(old)) = self.returns.push(old)
            {
                // Cannot happen while the control side keeps its in-flight
                // budget. Never free on the audio thread: leak instead.
                std::mem::forget(old);
            }
        }
        self.engine.render(out, channels, &mut emit);
    }
}
