//! The frame loop of an app that mounts a `view!`, for the hot reload parity
//! check (`Viso_Hot_Reload.md` §1.2): run once as is and once with
//! `--features hot-reload`, the two differ only by the dev path — the session
//! adopting the mount and starting its watcher, and its per-frame check for a
//! staged edit. A release artifact compiles neither.
//!
//! `frame_loop/view/startup` is the app brought up with one frame;
//! `frame_loop/view/240` adds 240 frames that each toggle the `if show` branch
//! (a press, then a redraw beat: a structural update, relayout and repaint), so
//! the per-frame cost is their difference over 240. An idle beat would be
//! skipped and measure nothing.

use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use viso::__test_support::drive_scripted;
use viso::platform::{Modifiers, PointerButtons, PointerPhase, RawEvent, RawPointer, WindowId};
use viso::prelude::*;

const STEP: Duration = Duration::from_millis(16);

struct TodoApp;

impl Application for TodoApp {
    fn new(_cx: &mut AppCx) -> Self {
        TodoApp
    }

    /// No caption band, so the view's cells sit at the surface origin.
    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("../tests/fixtures/todo_app.vs")(cx);
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested {
        window: WindowId(1),
    }
}

/// A press and release on the `show = !show` cell, the fourth in the first row.
fn toggle(script: &mut Vec<RawEvent>) {
    for (buttons, phase) in [
        (PointerButtons::PRIMARY, PointerPhase::Down),
        (PointerButtons::default(), PointerPhase::Up),
    ] {
        script.push(RawEvent::Pointer(RawPointer::mouse(
            WindowId(1),
            70.0,
            10.0,
            buttons,
            Modifiers::default(),
            phase,
        )));
    }
}

fn drive(frames: u32) -> viso::ui::FrameRecompute {
    let mut script = vec![redraw()];
    for _ in 0..frames {
        toggle(&mut script);
        script.push(redraw());
    }
    let app = drive_scripted::<TodoApp>(script, STEP);
    black_box(app.recompute())
}

fn bench_view(c: &mut Criterion) {
    // The press lands: each frame relays the branch out.
    assert!(drive(1).laid_out > 0, "a toggle frame does work");
    let mut group = c.benchmark_group("frame_loop/view");
    group.bench_function("startup", |b| b.iter(|| drive(black_box(0))));
    group.bench_function("240", |b| b.iter(|| drive(black_box(240))));
    group.finish();
}

criterion_group!(benches, bench_view);
criterion_main!(benches);
