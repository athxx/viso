//! End-to-end frame-loop tests driven by the headless platform backend.
//!
//! These exercise the whole `Scheduler` ↔ `PlatformApp` funnel without an OS:
//! a scripted [`RawEvent`] queue is played through [`Scheduler`] (which is the
//! `AppHandler`), and a counting [`FrameDriver`] records how many times each
//! phase ran. The invariants under test are the Phase 1 exit criteria: the loop
//! drives blank frames on beats, does no work when idle, and stops when the last
//! window closes or the platform returns `Exit`.

use std::cell::RefCell;
use std::rc::Rc;

use viso_platform::backend::headless::HeadlessApp;
use viso_platform::{AcceptCell, RawEvent, WindowConfig, WindowId};
use viso_runtime::{FrameDriver, FramePhase, RuntimeCx, Scheduler};

/// Shared, observable record of what the driver was asked to do.
#[derive(Default)]
struct Log {
    launches: u32,
    phase_calls: u32,
    frames: u32,
    inputs: u32,
    geometries: u32,
}

/// A `FrameDriver` that opens one window on launch and counts every callback.
///
/// `animate` controls `wants_animation()`: when true, the scheduler keeps
/// scheduling beats, so each scripted `RedrawRequested` finds a non-idle reason
/// set and actually runs a frame — the "continuous animation" path.
struct CountingDriver {
    log: Rc<RefCell<Log>>,
    animate: bool,
}

impl CountingDriver {
    fn new(log: Rc<RefCell<Log>>, animate: bool) -> Self {
        Self { log, animate }
    }
}

impl FrameDriver for CountingDriver {
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
        self.log.borrow_mut().launches += 1;
        cx.create_window(WindowConfig::default())
            .expect("headless window creation never fails");
    }

    fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {
        self.log.borrow_mut().geometries += 1;
    }

    fn on_input(&mut self) {
        self.log.borrow_mut().inputs += 1;
    }

    fn run_phase(&mut self, phase: FramePhase, _cx: &mut RuntimeCx<'_>) {
        let mut log = self.log.borrow_mut();
        log.phase_calls += 1;
        // Count a whole frame exactly once, on its first phase.
        if phase == FramePhase::CollectInput {
            log.frames += 1;
        }
    }

    fn wants_animation(&self) -> bool {
        self.animate
    }
}

const PHASES: u32 = FramePhase::ORDER.len() as u32;

fn run(script: Vec<RawEvent>, animate: bool) -> Log {
    let log = Rc::new(RefCell::new(Log::default()));
    let app = Box::new(HeadlessApp::scripted(script));
    let driver = CountingDriver::new(Rc::clone(&log), animate);
    Scheduler::new(app, driver).run();
    Rc::try_unwrap(log).ok().unwrap().into_inner()
}

#[test]
fn launch_opens_a_window_exactly_once() {
    // No scripted events: the headless pump fires AppLaunched, then drains empty.
    let log = run(vec![], false);
    assert_eq!(log.launches, 1, "on_launch fires exactly once");
    assert_eq!(log.frames, 0, "launch alone drives no frame");
    assert_eq!(log.phase_calls, 0);
}

#[test]
fn n_beats_drive_exactly_n_frames() {
    // With animation on, every beat has a pending reason and runs a full frame.
    let n = 5u32;
    let script: Vec<RawEvent> = (0..n)
        .map(|_| RawEvent::RedrawRequested {
            window: WindowId(1),
        })
        .collect();
    let log = run(script, true);

    assert_eq!(log.frames, n, "N beats run exactly N frames");
    assert_eq!(
        log.phase_calls,
        n * PHASES,
        "each frame walks all 12 phases in order"
    );
}

#[test]
fn idle_beats_do_no_work() {
    // Animation off and no dirtying event: a bare beat finds an idle reason set
    // and must run zero phases (§12.1 "idle does no work").
    let script = vec![
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ];
    let log = run(script, false);

    assert_eq!(log.frames, 0, "idle beats run no frames");
    assert_eq!(log.phase_calls, 0);
}

#[test]
fn input_dirties_then_next_beat_runs_a_frame() {
    // An input event adds InputDirty; the following beat drains it into a frame.
    use viso_platform::{Modifiers, PointerButtons, PointerPhase, RawPointer};
    let pointer = RawEvent::Pointer(RawPointer {
        window: WindowId(1),
        x: 10.0,
        y: 20.0,
        buttons: PointerButtons::PRIMARY,
        modifiers: Modifiers::default(),
        phase: PointerPhase::Down,
    });
    let script = vec![
        pointer,
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ];
    let log = run(script, false);

    assert_eq!(log.inputs, 1, "the input callback fired once");
    assert_eq!(log.frames, 1, "the beat after input runs one frame");
    assert_eq!(log.phase_calls, PHASES);
}

#[test]
fn geometry_change_is_observed_and_drives_a_frame() {
    // A resize calls on_geometry, marks WindowResize, requests a redraw; the
    // headless backend turns that request into a beat that runs the frame.
    let script = vec![RawEvent::Resized {
        window: WindowId(1),
        width: 1024,
        height: 768,
    }];
    let log = run(script, false);

    assert_eq!(log.geometries, 1, "on_geometry observed the resize");
    assert_eq!(log.frames, 1, "the requested redraw ran one frame");
}

#[test]
fn window_closed_stops_the_loop() {
    // After the only window closes, open_windows hits zero and the scheduler
    // returns Exit — even though more events remain scripted behind it.
    let script = vec![
        RawEvent::WindowClosed {
            window: WindowId(1),
        },
        // These must never be delivered: the loop has already exited.
        RawEvent::Resized {
            window: WindowId(1),
            width: 1,
            height: 1,
        },
        RawEvent::Resized {
            window: WindowId(1),
            width: 2,
            height: 2,
        },
    ];
    let log = run(script, false);

    assert_eq!(
        log.geometries, 0,
        "no event is processed after the last window closes"
    );
}

#[test]
fn close_requested_is_accepted_in_phase_1() {
    // Phase 1 accepts every close: the accept cell stays accepted after handling.
    let accept = AcceptCell::new();
    let script = vec![RawEvent::CloseRequested {
        window: WindowId(1),
        accept: accept.clone(),
    }];
    let _ = run(script, false);
    assert!(accept.is_accepted(), "Phase 1 never vetoes a close request");
}
