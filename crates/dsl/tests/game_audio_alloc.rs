//! The audio thread's call path allocates nothing once warm: each block
//! decodes its commands in place, runs the `AudioCommands` and
//! `AudioProcess` hooks on the VM and queues its events, all without a heap
//! allocation, counted by a global allocator on the rendering thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::rc::Rc;

use viso_behavior::game::AudioHost;
use viso_behavior::native::Natives;
use viso_behavior::{Module, Value};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;

/// Counts the allocations of a thread while it measures.
struct Counting;

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

fn count() {
    // `try_with`: the allocator also runs while thread locals are torn down.
    let _ = MEASURING.try_with(|measuring| {
        if measuring.get() {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
    });
}

// SAFETY: every call is forwarded to the system allocator unchanged; the
// counting touches only `const`-initialized thread locals, which allocate
// nothing.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract, forwarded.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        // SAFETY: the caller's contract, forwarded.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        // SAFETY: the caller's contract, forwarded.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        count();
        // SAFETY: the caller's contract, forwarded.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The allocations and frees `f` makes on this thread.
fn allocations(f: impl FnOnce()) -> u64 {
    ALLOCATIONS.with(|n| n.set(0));
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get)
}

const SYNTH: &str = r#"
import viso::game::{AudioProcess, AudioCommands, AudioBlock};

@derive(AudioCommand)
enum Cue {
    stop;
    play(F32, I64);
    shape { attack: F32; release: F32; };
}

@derive(AudioEvent)
enum Heard { tick; }

const VOICES: I64 = 4;

fn mix(x: F32, level: F32) -> F32 { x * level }

export system Synth implements AudioProcess + AudioCommands {
    @local state level: F32 = 0.0f32;
    @local state phase: F32 = 0.0f32;
    @local state every = 0;
    @local state count = 0;
    @local state attack: F32 = 0.0f32;

    action audio_command(command: Cue) {
        match command {
            Cue::stop => { level = 0.0f32; },
            Cue::play(l, e) => { level = l; every = e; },
            Cue::shape { attack: a, release: r } => { attack = a + r; },
        }
    }

    action audio_process(block: AudioBlock) {
        for f in 0..block.frames() {
            phase = phase + 0.01f32;
            if phase > 1.0f32 { phase = phase - 2.0f32; }
            let mut sample = 0.0f32;
            for v in 0..VOICES {
                sample = sample + mix(phase, level);
            }
            for c in 0..block.channels() {
                block.write(c, f, sample + block.input(c, f));
            }
            count += 1;
            if every > 0 && count >= every {
                count = 0;
                let sent = block.send(Heard::tick);
            }
        }
    }
}
"#;

fn module() -> Rc<Module> {
    let compiled = compile_file_for(
        SYNTH,
        &Origin {
            package: "app".to_owned(),
            module: vec!["synth".to_owned()],
            language: None,
        },
        Natives::standard(),
        TargetProfile::default(),
    );
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn play(level: f64) -> Value {
    Value::Agg(Rc::new(viso_behavior::Aggregate {
        tag: 1,
        fields: [Value::Float(level), Value::Int(64)].into(),
    }))
}

#[test]
fn a_warm_block_allocates_nothing() {
    let (mut host, link) =
        AudioHost::new(&module().encode(), &Natives::standard(), 2, 256, 48_000.0)
            .expect("builds")
            .expect("has audio systems");
    let commands = [play(0.25), play(0.5), Value::Int(0)];
    let mut block = vec![0.0f32; 2 * 256];
    // Warm: the VM's registers and frames grow to what the hooks need, and
    // the message slot to each command's shape.
    for command in &commands {
        assert!(link.send(command));
        host.render(&mut block);
    }
    while link.receive().is_some() {}

    let mut sent = 0;
    let made = allocations(|| {
        for round in 0..1_000 {
            let command = &commands[round % commands.len()];
            sent += u32::from(link.send(command));
            host.render(&mut block);
            while link.receive().is_some() {}
        }
    });
    assert_eq!(sent, 1_000);
    assert_eq!(made, 0, "a warm block allocates or frees nothing");
    assert_eq!(
        link.status().faults(),
        0,
        "{:?}",
        link.status().take_fault()
    );
    assert!(block.iter().any(|&s| s != 0.0), "it rendered");
    assert_eq!(link.status().blocks(), 1_003);
}
