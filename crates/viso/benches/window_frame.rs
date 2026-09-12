//! Microbenchmarks for the multi-window control — the Tier 4 finale.
//!
//! A window is not a node control: it has no build/layout/paint of its own, so
//! there is nothing to bench in a `BuildCx` the way the widget packs do. Its real
//! cost lives in three facade paths, and each is only reachable through the *real*
//! `AppDriver` frame loop, so every bench here drives `viso`'s public headless
//! seam (`__test_support::drive_scripted` + a scripted `HeadlessApp`) exactly as
//! the facade tests do — benches are an external crate and cannot touch the
//! facade internals (`AppDriver`, `WindowState`, `RuntimeCx`) directly:
//!
//! 1. `window/open` — the cost of servicing one deferred `window()` open: a
//!    `create_window`, an entire `WindowState` brought up (its node store, the
//!    reactive stores, animations/timers, GPU bring-up under a headless surface),
//!    and the deferred content closure run against that fresh store.
//! 2. `window/close` — the cost of the single teardown path: a `WindowHandle`
//!    close routed through the platform seam → `WindowClosed` → `on_window_closed`
//!    → `windows.retain(…)`, dropping exactly that window's `WindowState`.
//! 3. `window/fan_out/N` — the per-frame cost of `run_phase` iterating N live
//!    windows (N ∈ {1, 4, 16}). This is the load-bearing measurement for the
//!    `Vec<WindowState>` linear-scan choice (ADR 0020 / section 45): the per-frame
//!    access pattern is *iterate all windows*, so the cost must scale linearly in
//!    N with no hidden per-window overhead. Reported per group so a superlinear
//!    regression (an accidental map rebuild, an O(N²) fold) shows up as a
//!    faster-than-linear climb across the three sizes.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;

use viso::__test_support::drive_scripted;
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{PointerButtons, PointerPhase, Size};

/// The launch window's logical surface (`WindowConfig::default`, scale 1.0), so a
/// center-of-surface pointer sample lands on the launch window's root.
const SURFACE_W: f64 = 800.0;
const SURFACE_H: f64 = 600.0;
/// The step each scripted frame advances the fixed-step clock.
const STEP: Duration = Duration::from_millis(16);

/// A primary pointer-down at the launch window's center — drives one handler
/// action per press (the app sequences open/close by a press counter).
fn press() -> RawEvent {
    RawEvent::Pointer(RawPointer {
        window: WindowId(1),
        x: SURFACE_W / 2.0,
        y: SURFACE_H / 2.0,
        buttons: RawButtons::PRIMARY,
        modifiers: RawModifiers::default(),
        phase: RawPhase::Down,
    })
}

/// A redraw beat on the launch window — primes the pump and advances one frame so
/// a deferred open/close request recorded on the previous frame is serviced.
fn redraw() -> RawEvent {
    RawEvent::RedrawRequested {
        window: WindowId(1),
    }
}

/// Author a minimal hosted tree for an opened window — a full-surface focusable
/// host, the same shape the facade tests open. Returns its root.
fn build_child(build: &mut viso::ui::BuildCx<'_>) -> Option<viso::ui::NodeId> {
    let r = build.flex(
        FlexStyle {
            size: Size::fill(),
            ..FlexStyle::default()
        },
        |_cx| {},
    );
    build.focusable(r, true);
    Some(r.id())
}

/// A launch app whose press handler opens one window on the first press and
/// closes it on the second — used by the `open` and `close` benches. The handle
/// is kept across presses so the second press can close programmatically.
struct OpenCloseApp {
    presses: Rc<Cell<u32>>,
    child: Rc<Cell<Option<WindowHandle>>>,
}

impl Application for OpenCloseApp {
    fn new(_cx: &mut AppCx) -> Self {
        OpenCloseApp {
            presses: Rc::new(Cell::new(0)),
            child: Rc::new(Cell::new(None)),
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let presses = self.presses.clone();
        let child = self.child.clone();
        let root = cx.flex(
            FlexStyle {
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |_cx| {},
        );
        cx.focusable(root, true);
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase != PointerPhase::Down || !p.buttons.contains(PointerButtons::PRIMARY) {
                return;
            }
            let n = presses.get();
            presses.set(n + 1);
            match n {
                // Press 1: open one window through the public seam and keep it.
                0 => {
                    let handle = window(WindowConfig {
                        title: "child".to_string(),
                        size: (320.0, 240.0),
                        ..Default::default()
                    })
                    .content(build_child)
                    .open(ev);
                    child.set(Some(handle));
                }
                // Press 2: close it through the handle (the single teardown path).
                _ => {
                    if let Some(handle) = child.take() {
                        handle.close(ev);
                    }
                }
            }
        });
    }
}

// A launch app that opens `N` windows on its first press, so `run_phase` then
// iterates N live windows every redraw beat — the fan-out load. `drive_scripted`
// constructs the app through `Application::new`, which cannot carry a runtime
// count, so the three sizes are three concrete apps built by a macro, each
// hard-coding its N.
macro_rules! fan_out_app {
    ($name:ident, $n:expr) => {
        struct $name {
            opened: Rc<Cell<bool>>,
        }
        impl Application for $name {
            fn new(_cx: &mut AppCx) -> Self {
                $name {
                    opened: Rc::new(Cell::new(false)),
                }
            }
            fn build(&mut self, cx: &mut BuildCx<'_>) {
                let opened = self.opened.clone();
                let root = cx.flex(
                    FlexStyle {
                        size: Size::fill(),
                        ..FlexStyle::default()
                    },
                    |_cx| {},
                );
                cx.focusable(root, true);
                cx.on_pointer(root, move |ev| {
                    let Some(p) = ev.pointer() else { return };
                    if p.phase != PointerPhase::Down || !p.buttons.contains(PointerButtons::PRIMARY)
                    {
                        return;
                    }
                    if opened.replace(true) {
                        return;
                    }
                    // Open N-1 more windows (the launch window is the Nth), each a
                    // minimal hosted tree, so run_phase fans out over N windows.
                    let extra: u32 = $n - 1;
                    for _ in 0..extra {
                        window(WindowConfig {
                            title: "fan".to_string(),
                            size: (320.0, 240.0),
                            ..Default::default()
                        })
                        .content(build_child)
                        .open(ev);
                    }
                });
            }
        }
    };
}

fan_out_app!(FanOut1, 1u32);
fan_out_app!(FanOut4, 4u32);
fan_out_app!(FanOut16, 16u32);

/// The `open` script: prime, one press to record the deferred open, one beat to
/// service it (create the window, bring its state up, run its build). The whole
/// loop settles at two windows.
fn drive_open() {
    let app = drive_scripted::<OpenCloseApp>(vec![redraw(), press(), redraw()], STEP);
    black_box(app.window_count());
}

/// The `close` script: open (as above), then a second press records the deferred
/// close, and a beat services it — the platform seam delivers `WindowClosed`, the
/// driver tears the window's `WindowState` down, and the loop settles back to one
/// window (then exits once the last window would close — here the launch window
/// stays open, so the fold reports idle and the loop halts).
fn drive_open_close() {
    let app =
        drive_scripted::<OpenCloseApp>(vec![redraw(), press(), redraw(), press(), redraw()], STEP);
    black_box(app.window_count());
}

/// Drive `App` (which opens its N windows on the first press) and then run
/// `beats` redraw frames with all N windows live, so the timed work is
/// `run_phase` fanning out over N windows per frame.
fn drive_fan_out<A: Application>(beats: u32) {
    let mut script = vec![redraw(), press(), redraw()];
    for _ in 0..beats {
        script.push(redraw());
    }
    let app = drive_scripted::<A>(script, STEP);
    black_box(app.window_count());
}

fn bench_open(c: &mut Criterion) {
    let mut group = c.benchmark_group("window");
    group.bench_function("open", |b| b.iter(drive_open));
    group.bench_function("close", |b| b.iter(drive_open_close));
    group.finish();
}

fn bench_fan_out(c: &mut Criterion) {
    // A fixed beat budget across the three window counts, so the only variable is
    // N: linear fan-out means each step up in N adds a proportional per-frame cost
    // and nothing more.
    const BEATS: u32 = 60;
    let mut group = c.benchmark_group("window/fan_out");
    group.bench_function("1", |b| {
        b.iter(|| drive_fan_out::<FanOut1>(black_box(BEATS)))
    });
    group.bench_function("4", |b| {
        b.iter(|| drive_fan_out::<FanOut4>(black_box(BEATS)))
    });
    group.bench_function("16", |b| {
        b.iter(|| drive_fan_out::<FanOut16>(black_box(BEATS)))
    });
    group.finish();
}

criterion_group!(benches, bench_open, bench_fan_out);
criterion_main!(benches);
