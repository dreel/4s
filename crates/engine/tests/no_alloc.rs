//! The audio thread must never allocate or free. Count heap operations made
//! on this thread while `RtEngine::process` runs, with instruments being
//! added, routed, played, and removed through the command ring.

use fours_engine::{effect, modulator, ModRoute};
use fours_engine::instrument::{self, MAX_BLOCK};
use fours_engine::{Command, Engine, ParamTarget, Placement, RtEngine, params};
use fours_protocol::{ClipEvent, DeviceType, InstrumentType, MAX_EVENTS, NoteStep, STEP_ON, Voice, drum_event, note_event};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    // `try_with`: the allocator may run during thread teardown.
    let _ = COUNTING.try_with(|c| {
        if c.get() {
            let _ = COUNT.try_with(|n| n.set(n.get() + 1));
        }
    });
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        note();
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn counted(f: impl FnOnce()) -> usize {
    COUNT.with(|n| n.set(0));
    COUNTING.with(|c| c.set(true));
    f();
    COUNTING.with(|c| c.set(false));
    COUNT.with(|n| n.get())
}

#[test]
fn process_never_allocates() {
    let sr = 48000;
    let (mut rt, mut link) = RtEngine::new(Engine::new(sr));
    let mut out = vec![0.0f32; MAX_BLOCK * 4 * 2];
    let kicks: Vec<ClipEvent> = [0, 2].iter().filter_map(|s| drum_event(Voice::Kick, *s, STEP_ON)).collect();
    // A slide: the first note overlaps the second.
    let bass: Vec<ClipEvent> = [
        NoteStep { note: Some(36), accent: true, slide: true },
        NoteStep { note: Some(43), accent: false, slide: false },
    ]
    .iter()
    .enumerate()
    .filter_map(|(i, s)| note_event(i, s))
    .collect();

    // A short sample, built here (allocations here are fine).
    let sample: std::sync::Arc<[f32]> = (0..4800).map(|i| ((i as f32) * 0.05).sin() * 0.5).collect::<Vec<f32>>().into();

    for round in 0..3u8 {
        // Built on the control side (allocations here are fine).
        let cmds = vec![
            Command::SetChannelActive { ch: 0, active: true },
            Command::SetChannelActive { ch: 1, active: true },
            Command::AddInstrument { slot: 0, instrument: instrument::make(InstrumentType::Tr808, sr as f32) },
            Command::AddInstrument { slot: 1, instrument: instrument::make(InstrumentType::Tb303, sr as f32) },
            Command::SetRoute { slot: 0, output: 0, channel: Some(0) },
            Command::SetRoute { slot: 0, output: 1, channel: Some(1) },
            Command::SetRoute { slot: 1, output: 0, channel: Some(1) },
            // Inserts (RFC 0010): a filter and a delay on channel 1, a
            // reverb on the master, one parameter of each kind.
            Command::AddEffect { slot: 0, effect: effect::make(DeviceType::Filter, sr as f32) },
            Command::AddEffect { slot: 1, effect: effect::make(DeviceType::Delay, sr as f32) },
            Command::AddEffect { slot: 2, effect: effect::make(DeviceType::Reverb, sr as f32) },
            Command::SetParam { target: ParamTarget::Effect { slot: 0, index: 1 }, value: 1.0 },
            Command::SetParam { target: ParamTarget::Effect { slot: 1, index: 1 }, value: 0.4 },
            Command::SetParam { target: ParamTarget::Effect { slot: 2, index: 1 }, value: 0.3 },
            Command::SetParam { target: ParamTarget::Effect { slot: 0, index: 3 }, value: 0.3 },
            Command::SetChain { ch: 0, slots: [0, 1, 0, 0, 0, 0, 0, 0], len: 2 },
            Command::SetChain { ch: fours_engine::MASTER_CHAIN, slots: [2, 0, 0, 0, 0, 0, 0, 0], len: 1 },
            // A sampler (RFC 0010) with a sample on pad 1, played below.
            Command::AddInstrument { slot: 2, instrument: instrument::make(InstrumentType::Sampler, sr as f32) },
            Command::SetRoute { slot: 2, output: 0, channel: Some(1) },
            Command::SetSample { slot: 2, pad: 0, sample: Some(sample.clone()) },
            Command::NoteOn { slot: 2, note: 36, velocity: 1.0, gate: false },
            // An LFO (RFC 0010) moving the filter's cutoff and channel 1's pan.
            Command::AddModulator { slot: 0, modulator: modulator::make(DeviceType::Lfo) },
            Command::SetParam { target: ParamTarget::Modulator { slot: 0, index: 2 }, value: 0.8 },
            Command::SetModRoute { index: 0, route: Some(ModRoute { source: 0, output: 0, target: ParamTarget::Effect { slot: 0, index: 3 }, depth: 0.3, min: 0.0, max: 1.0, base: 0.3 }) },
            Command::SetModRoute { index: 1, route: Some(ModRoute { source: 0, output: 1, target: ParamTarget::Channel { ch: 0, index: 1 }, depth: -0.5, min: -1.0, max: 1.0, base: 0.0 }) },
            Command::ClearClip { clip: 0 },
            Command::AddEvent { clip: 0, event: kicks[0] },
            Command::AddEvent { clip: 0, event: kicks[1] },
            Command::AddEvent { clip: 1, event: bass[0] },
            Command::AddEvent { clip: 1, event: bass[1] },
            Command::RemoveEvent { clip: 0, tick: kicks[1].tick, note: kicks[1].note },
            Command::AddEvent { clip: 0, event: kicks[1] },
            Command::SetClipLength { clip: 1, length: 48 },
            Command::Launch { slot: 0, clip: Some(0), now: false },
            Command::Launch { slot: 1, clip: Some(1), now: false },
            Command::AddPlacement { slot: 0, placement: Placement { start: 0, length: 384, offset: 0, clip: 0 } },
            Command::AddPlacement { slot: 1, placement: Placement { start: 96, length: 200, offset: 24, clip: 1 } },
            Command::RemovePlacement { slot: 1, start: 96 },
            Command::AddPlacement { slot: 1, placement: Placement { start: 96, length: 200, offset: 24, clip: 1 } },
            Command::Locate { tick: 48 },
            Command::SetParam { target: ParamTarget::Channel { ch: 1, index: 1 }, value: -0.5 },
            Command::SetParam { target: ParamTarget::Instrument { slot: 1, index: 2 }, value: 0.7 },
            Command::NoteOn { slot: 0, note: 42, velocity: 1.0, gate: false },
            Command::NoteOn { slot: 1, note: 40, velocity: 1.0, gate: true },
            Command::NoteOn { slot: 1, note: 41, velocity: 0.7, gate: false },
            Command::NoteOn { slot: 1, note: 43, velocity: 0.7, gate: false },
            Command::NoteOff { slot: 1, note: 43 },
            Command::NoteOff { slot: 1, note: 41 },
            Command::SetParam { target: ParamTarget::Global(params::METRONOME), value: 1.0 },
            Command::Play { count_in: fours_protocol::TICKS_PER_BAR },
        ];
        for c in cmds {
            assert!(link.commands.push(c).is_ok());
        }
        let n = counted(|| {
            for _ in 0..200 {
                rt.process(&mut out, 2);
            }
        });
        assert_eq!(n, 0, "round {round}: {n} heap operations while playing");

        // Live notes while playing report their song position.
        for c in [
            Command::NoteOn { slot: 1, note: 45, velocity: 0.7, gate: false },
            Command::NoteOff { slot: 1, note: 45 },
            Command::NoteOn { slot: 0, note: 36, velocity: 1.0, gate: true },
        ] {
            assert!(link.commands.push(c).is_ok());
        }
        let n = counted(|| rt.process(&mut out, 2));
        assert_eq!(n, 0, "round {round}: {n} heap operations playing live notes");

        // Back to the arrangement (one track at once, one at the next bar), a
        // launch while playing, a range loop, and locating while playing.
        for c in [
            Command::Launch { slot: 0, clip: None, now: true },
            Command::Launch { slot: 1, clip: None, now: false },
            Command::SetParam { target: ParamTarget::Global(params::LOOP), value: 2.0 },
            Command::SetParam { target: ParamTarget::Global(params::LOOP_END), value: 1.0 },
            Command::Locate { tick: 0 },
        ] {
            assert!(link.commands.push(c).is_ok());
        }
        let n = counted(|| {
            for _ in 0..200 {
                rt.process(&mut out, 2);
            }
        });
        assert_eq!(n, 0, "round {round}: {n} heap operations playing the arrangement");
        assert!(link.commands.push(Command::Launch { slot: 1, clip: Some(1), now: false }).is_ok());
        let n = counted(|| {
            for _ in 0..200 {
                rt.process(&mut out, 2);
            }
        });
        assert_eq!(n, 0, "round {round}: {n} heap operations launching while playing");

        // Filling a clip to capacity stays in its preallocated storage.
        for tick in 0..MAX_EVENTS as u32 {
            let event = ClipEvent { tick, len: 1, note: 38, velocity: 100 };
            assert!(link.commands.push(Command::AddEvent { clip: 0, event }).is_ok());
        }
        let n = counted(|| rt.process(&mut out, 2));
        assert_eq!(n, 0, "round {round}: {n} heap operations filling a clip");

        for c in [
            Command::Stop,
            Command::SetRoute { slot: 0, output: 1, channel: None },
            Command::RemoveInstrument { slot: 0 },
            Command::RemoveInstrument { slot: 1 },
            Command::RemoveInstrument { slot: 2 },
            Command::SetModRoute { index: 1, route: None },
            Command::RemoveModulator { slot: 0 },
            Command::RemoveEffect { slot: 0 },
            Command::RemoveEffect { slot: 1 },
            Command::RemoveEffect { slot: 2 },
            Command::SetChannelActive { ch: 1, active: false },
        ] {
            assert!(link.commands.push(c).is_ok());
        }
        let n = counted(|| rt.process(&mut out, 2));
        assert_eq!(n, 0, "round {round}: {n} heap operations while removing");
        // The removed instruments and effects came back to be dropped here,
        // off the audio thread.
        assert_eq!(link.returns.slots(), 7);
        while let Ok(b) = link.returns.pop() {
            drop(b);
        }
    }
}
