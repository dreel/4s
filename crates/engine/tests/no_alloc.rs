//! The audio thread must never allocate or free. Count heap operations made
//! on this thread while `RtEngine::process` runs, with instruments being
//! added, routed, played, and removed through the command ring.

use fours_engine::instrument::{self, MAX_BLOCK};
use fours_engine::{Command, Engine, ParamTarget, Placement, RtEngine, params};
use fours_protocol::{ClipEvent, InstrumentType, MAX_EVENTS, NoteStep, STEP_ON, Voice, drum_event, note_event};
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
            Command::ClearClip { clip: 0 },
            Command::AddEvent { clip: 0, event: kicks[0] },
            Command::AddEvent { clip: 0, event: kicks[1] },
            Command::AddEvent { clip: 1, event: bass[0] },
            Command::AddEvent { clip: 1, event: bass[1] },
            Command::RemoveEvent { clip: 0, tick: kicks[1].tick, note: kicks[1].note },
            Command::AddEvent { clip: 0, event: kicks[1] },
            Command::SetClipLength { clip: 1, length: Some(48) },
            Command::SelectClip { slot: 0, clip: Some(0) },
            Command::SelectClip { slot: 1, clip: Some(1) },
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

        // Song mode: the arrangement, a range loop, and locating while
        // playing.
        for c in [
            Command::SetParam { target: ParamTarget::Global(params::SONG_MODE), value: 1.0 },
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
        assert_eq!(n, 0, "round {round}: {n} heap operations in song mode");
        assert!(link.commands.push(Command::SetParam { target: ParamTarget::Global(params::SONG_MODE), value: 0.0 }).is_ok());

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
            Command::SetChannelActive { ch: 1, active: false },
        ] {
            assert!(link.commands.push(c).is_ok());
        }
        let n = counted(|| rt.process(&mut out, 2));
        assert_eq!(n, 0, "round {round}: {n} heap operations while removing");
        // The removed instruments came back to be dropped here, off the
        // audio thread.
        assert_eq!(link.returns.slots(), 2);
        while let Ok(b) = link.returns.pop() {
            drop(b);
        }
    }
}
