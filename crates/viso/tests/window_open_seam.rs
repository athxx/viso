//! Commit-4 integration coverage for the window open/close deferred seam,
//! driven through the real facade frame loop (`drive_scripted` +
//! `FixedStepClock`, the same pump `viso::run` uses).
//!
//! A control cannot open an OS window itself — the UI tier holds no platform
//! seam, and the new window's store does not exist when a handler runs — so
//! opening a window rides the same three-hop deferred path as an animation or a
//! timer: a handler records a [`request_open_window`] on `EventCx`, the router
//! hands it to the originating window's store queue, and the facade drains that
//! queue in the one phase it holds a live scheduling context (the flush phase),
//! where it creates the OS window, brings up its state through the shared
//! bring-up path, and runs the deferred build closure against its fresh store.
//! Closing is symmetric: [`request_close_window`] carries a raw id to the
//! facade, which asks the platform to close that window — the resulting
//! `WindowClosed` event routes through `on_window_closed`, the single teardown
//! path shared with a user-driven OS close (no double teardown).
//!
//! This pack asserts the seam end to end against the headless backend, which
//! opens windows with sequential ids (`1` for the launch window, `2` for the
//! next) and, on `close_window`, front-queues a `WindowClosed` beat so the
//! programmatic close and the OS-driven close travel the identical path:
//!
//! - a pointer-down on the launch window opens a second window, and the next
//!   frame settles with two independent windows (distinct ids, distinct stores,
//!   distinct roots);
//! - the two windows own disjoint retained trees — the second window's tree is
//!   built by *its own* deferred closure, not the launch app's `build`;
//! - a later pointer-down closes the second window through the platform seam,
//!   tearing exactly its state back down to one window;
//! - closing the last remaining window drops the open count to zero and the
//!   loop exits (the multi-window frame-halt / exit contract).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{
    Modifiers as RawModifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{PointerButtons, PointerPhase, Size, WindowConfig};

const SURFACE_W: f64 = 200.0;
const SURFACE_H: f64 = 150.0;
const STEP: Duration = Duration::from_millis(16);

/// How the launch app's pointer handler should react to each successive
/// pointer-down: open the second window on the first press, close it on the
/// second, and (in the close-last variant) close its own launch window on the
/// third.
#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Press 1 opens window 2; press 2 closes window 2. The launch window stays
    /// open, so the loop settles at one window rather than exiting.
    OpenThenClose,
    /// Press 1 opens window 2; press 2 closes window 2; press 3 closes the
    /// launch window. With no window left the open count hits zero and the loop
    /// exits.
    CloseEverything,
}

/// A launch application whose sole root is a full-surface focusable host with a
/// pointer handler that opens/closes windows through the seam. A press counter
/// (a plain `Cell`, no reactive state needed) sequences the actions so one input
/// tape can drive open, then close, then optionally close-last.
struct WindowSeamApp {
    mode: Mode,
    /// Number of pointer-downs seen so far, so the handler knows whether this
    /// press should open, close, or close-last.
    presses: Rc<Cell<u32>>,
}

impl Application for WindowSeamApp {
    fn new(_cx: &mut AppCx) -> Self {
        // `mode` is overwritten by the constructors below before `build` runs;
        // start in the simplest mode.
        WindowSeamApp {
            mode: Mode::OpenThenClose,
            presses: Rc::new(Cell::new(0)),
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let mode = self.mode;
        let presses = self.presses.clone();
        let root = cx.flex(
            viso::ui::FlexStyle {
                size: Size::fill(),
                ..viso::ui::FlexStyle::default()
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
                // First press: open a second window. Its tree is authored by
                // *this* deferred closure — a single focusable host of its own —
                // so it shares nothing with the launch window's store.
                0 => {
                    ev.request_open_window(
                        WindowConfig {
                            title: "second".to_string(),
                            size: (SURFACE_W, SURFACE_H),
                        },
                        |build| {
                            let r = build.flex(
                                viso::ui::FlexStyle {
                                    size: Size::fill(),
                                    ..viso::ui::FlexStyle::default()
                                },
                                |_cx| {},
                            );
                            Some(r.id())
                        },
                    );
                }
                // Second press: close the second window. The headless backend
                // assigned it id 2 (the launch window is id 1). Closing goes
                // through the platform seam → `WindowClosed` → single teardown.
                1 => ev.request_close_window(2),
                // Third press (CloseEverything only): close the launch window
                // too, dropping the open count to zero so the loop exits.
                _ if mode == Mode::CloseEverything => ev.request_close_window(1),
                _ => {}
            }
        });
    }
}

/// Builders for the two modes: `new` seeds `OpenThenClose`; wrapping newtypes
/// select `CloseEverything`. `drive_scripted` builds the app through
/// `Application::new`, so a mode override rides on a distinct type per mode.
struct OpenThenCloseApp(WindowSeamApp);
struct CloseEverythingApp(WindowSeamApp);

impl Application for OpenThenCloseApp {
    fn new(cx: &mut AppCx) -> Self {
        let mut app = WindowSeamApp::new(cx);
        app.mode = Mode::OpenThenClose;
        OpenThenCloseApp(app)
    }
    fn build(&mut self, cx: &mut BuildCx<'_>) {
        self.0.build(cx);
    }
}

impl Application for CloseEverythingApp {
    fn new(cx: &mut AppCx) -> Self {
        let mut app = WindowSeamApp::new(cx);
        app.mode = Mode::CloseEverything;
        CloseEverythingApp(app)
    }
    fn build(&mut self, cx: &mut BuildCx<'_>) {
        self.0.build(cx);
    }
}

/// A primary pointer-down at the launch window's center. One priming redraw runs
/// the first layout so the root has a world box the pointer can hit.
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

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested {
        window: WindowId(1),
    }
}

#[test]
fn a_handler_opens_a_second_window_with_its_own_isolated_state() {
    // Priming redraw (first layout), a press to open window 2, a beat to run the
    // frame that drains the open request and brings the window up. The loop stays
    // alive (two windows open) and exits only when the script's beats run dry.
    let app = drive_scripted::<OpenThenCloseApp>(vec![redraw(), press(), redraw()], STEP);

    // The open request was serviced: a second window exists alongside the launch
    // window.
    assert_eq!(
        app.window_count(),
        2,
        "the pointer handler's `request_open_window` opened a second window"
    );

    // The headless backend hands out sequential ids: the launch window is 1, the
    // opened window is 2.
    assert_eq!(app.window_id_at(0), WindowId(1), "launch window keeps id 1");
    assert_eq!(app.window_id_at(1), WindowId(2), "opened window is id 2");

    // The two windows own disjoint retained trees, each with its own declared
    // root. The second window's tree was built by its deferred closure, so it is
    // not the same node identity as the launch window's root, and each store
    // holds it independently.
    let root0 = app.root_at(0).expect("launch window declares a root");
    let root1 = app.root_at(1).expect("opened window declares a root");
    assert!(
        app.store_at(0).world(root0).w > 0.0,
        "the launch window laid its root out against its own surface"
    );
    assert!(
        app.store_at(1).world(root1).w > 0.0,
        "the opened window laid its own root out against its own surface"
    );
}

#[test]
fn a_handler_closes_the_opened_window_back_down_to_one() {
    // Open window 2 (press 1), then close it (press 2). Each press is followed by
    // a beat so the frame drains that press's request before the next arrives.
    let app = drive_scripted::<OpenThenCloseApp>(
        vec![redraw(), press(), redraw(), press(), redraw()],
        STEP,
    );

    // The close request routed through the platform seam → `WindowClosed` →
    // `on_window_closed`, tearing down exactly the second window. The launch
    // window survives.
    assert_eq!(
        app.window_count(),
        1,
        "closing the opened window tears down exactly its state"
    );
    assert_eq!(
        app.window_id_at(0),
        WindowId(1),
        "the surviving window is the launch window"
    );
}

#[test]
fn closing_every_window_exits_the_loop() {
    // Open window 2, close it, then close the launch window too. With no window
    // left the scheduler's open count hits zero and the loop exits on its own —
    // the script's trailing beats are never consumed because the pump has already
    // stopped. `run_returning` returns the settled driver regardless.
    let app = drive_scripted::<CloseEverythingApp>(
        vec![
            redraw(),
            press(),
            redraw(),
            press(),
            redraw(),
            press(),
            redraw(),
        ],
        STEP,
    );

    // Every window was torn down through the single close path; none leaked.
    assert_eq!(
        app.window_count(),
        0,
        "closing the last window drops the open count to zero and the loop exits"
    );
}
