//! A rollback session writing its packets into one reused encoder allocates
//! nothing per packet once the encoder has held the largest.

use std::alloc::{GlobalAlloc, Layout, System};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use viso_behavior::game::{RollbackSession, Scheduler, SessionConfig};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};
use viso_ende::Encoder;

/// Counts every allocation and reallocation of the test binary.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every call forwards to `System` with the caller's own arguments,
// so `System`'s guarantees are this allocator's.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the caller upholds `alloc`'s contract for `layout`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator, which is `System`'s.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: as in `dealloc`, and the caller upholds `realloc`'s contract.
        unsafe { System.realloc(ptr, layout, size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

const IDLE: &str = r#"
import viso::game::{FixedUpdate, FixedFrame};

export system Idle implements FixedUpdate {
    state ticks = 0;

    action fixed_update(frame: FixedFrame) {
        ticks += 1;
    }
}
"#;

fn peer(local: u32) -> RollbackSession {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    };
    let profile = TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(IDLE, &origin, Natives::standard(), profile);
    assert!(compiled.errors().next().is_none());
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let game = Scheduler::with_seed(vm, 7).expect("a started game");
    RollbackSession::new(game, SessionConfig::new(2, local)).expect("a session")
}

#[test]
fn a_steady_packet_into_a_reused_encoder_allocates_nothing() {
    let (mut a, mut b) = (peer(0), peer(1));
    let mut enc = Encoder::new();
    let mut packets = 0;
    let mut allocated = 0;
    for frame in 0..240 {
        a.advance(1);
        b.advance(1);
        let before = ALLOCATIONS.load(Ordering::Relaxed);
        enc.clear();
        a.write_packet_for(1, &mut enc);
        let after = ALLOCATIONS.load(Ordering::Relaxed);
        if frame >= 60 {
            allocated += after - before;
            packets += 1;
        }
        assert_eq!(
            enc.as_bytes(),
            a.packet_for(1),
            "the same bytes as a fresh one"
        );
        b.receive(enc.as_bytes()).expect("a packet");
        a.receive(&b.packet_for(0)).expect("a packet");
    }
    assert_eq!(
        allocated, 0,
        "{allocated} allocations over {packets} packets"
    );
}
