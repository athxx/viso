//! Frame-loop microbenchmarks and the idle-cost invariant (§12.1).
//!
//! Two things are measured here, both through the public API (benches are an
//! external crate and cannot touch `pub(crate)` internals like `RuntimeCx::new`,
//! so we drive whole frames via `Scheduler` + the headless backend):
//!
//! 1. An assertion that an idle reason set decides on `NoFrame` — the runtime
//!    must do no work when nothing is dirty. This runs once at startup so a
//!    regression fails the bench binary immediately.
//! 2. The per-frame cost of walking all twelve phases for a blank frame, as a
//!    baseline to catch dispatch-overhead regressions before pixels land.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_platform::backend::headless::HeadlessApp;
use viso_platform::{RawEvent, WindowConfig, WindowId};
use viso_runtime::{
    FrameDecision, FrameDriver, FramePhase, RedrawReason, RedrawReasons, RuntimeCx, Scheduler,
};

/// The Phase 1 blank-frame driver: opens one window, every phase a no-op.
struct BlankDriver;

impl FrameDriver for BlankDriver {
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
        let _ = cx.create_window(WindowConfig::default());
    }
    fn on_geometry(&mut self, _w: WindowId, _s: f64, _width: u32, _height: u32) {}
    fn on_input(&mut self) {}
    fn run_phase(&mut self, phase: FramePhase, _cx: &mut RuntimeCx<'_>) {
        // Keep the phase from being optimized away entirely.
        black_box(phase);
    }
    fn wants_animation(&self) -> bool {
        // Keep beats flowing so each scripted RedrawRequested runs a full frame.
        true
    }
}

/// Run `n` blank frames end-to-end through the scheduler and headless pump.
fn drive_frames(n: u32) {
    let script: Vec<RawEvent> = (0..n)
        .map(|_| RawEvent::RedrawRequested {
            window: WindowId(1),
        })
        .collect();
    let app = Box::new(HeadlessApp::scripted(script));
    Scheduler::new(app, BlankDriver).run();
}

/// The §12.1 invariant, checked before benchmarking: idle ⇒ no frame.
fn assert_idle_does_no_work() {
    let idle = RedrawReasons::new();
    assert!(idle.is_idle());
    assert_eq!(
        idle.decide(),
        FrameDecision::NoFrame,
        "an idle reason set must decide NoFrame (§12.1)"
    );
    // A single ordinary reason must escalate off idle.
    let mut dirty = RedrawReasons::new();
    dirty.add(RedrawReason::StateDirty);
    assert_ne!(dirty.decide(), FrameDecision::NoFrame);
}

fn bench_blank_frames(c: &mut Criterion) {
    assert_idle_does_no_work();

    let mut group = c.benchmark_group("blank_frame_loop");
    for n in [1u32, 60, 600] {
        group.bench_function(format!("{n}_frames"), |b| {
            b.iter(|| drive_frames(black_box(n)));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_blank_frames);
criterion_main!(benches);
