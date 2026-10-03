//! Real-time wrapper: the audio thread owns an `RtEngine` and talks to the
//! control side only through lock-free SPSC ring buffers.

use crate::engine::{Command, Engine, Feedback};
use rtrb::{Consumer, Producer, RingBuffer};

pub struct RtEngine {
    engine: Engine,
    commands: Consumer<Command>,
    feedback: Producer<Feedback>,
}

/// Control-side ends of the queues.
pub struct EngineLink {
    pub commands: Producer<Command>,
    pub feedback: Consumer<Feedback>,
}

impl RtEngine {
    pub fn new(engine: Engine) -> (RtEngine, EngineLink) {
        let (cp, cc) = RingBuffer::new(4096);
        let (fp, fc) = RingBuffer::new(8192);
        (RtEngine { engine, commands: cc, feedback: fp }, EngineLink { commands: cp, feedback: fc })
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
            self.engine.apply(cmd, &mut emit);
        }
        self.engine.render(out, channels, &mut emit);
    }
}
