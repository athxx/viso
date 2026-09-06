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
use std::time::{Duration, Instant};

use viso_platform::backend::headless::HeadlessApp;
use viso_platform::{AcceptCell, RawEvent, WindowConfig, WindowId};
use viso_runtime::{FrameClock, FrameDriver, FramePhase, InputSample, RuntimeCx, Scheduler};

/// Shared, observable record of what the driver was asked to do.
#[derive(Default)]
struct Log {
    launches: u32,
    phase_calls: u32,
    frames: u32,
    inputs: u32,
    geometries: u32,
    /// The `frame_delta` each frame observed, in order (one entry per frame).
    deltas: Vec<Duration>,
    /// The windows the driver was told closed, in the order the hook fired.
    closed: Vec<WindowId>,
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

    fn on_input(&mut self, _sample: InputSample) {
        self.log.borrow_mut().inputs += 1;
    }

    fn run_phase(&mut self, phase: FramePhase, cx: &mut RuntimeCx<'_>) {
        let mut log = self.log.borrow_mut();
        log.phase_calls += 1;
        // Count a whole frame exactly once, on its first phase, and record the
        // delta the scheduler tagged this frame with.
        if phase == FramePhase::CollectInput {
            log.frames += 1;
            log.deltas.push(cx.frame_delta());
        }
    }

    fn wants_animation(&self) -> bool {
        self.animate
    }

    fn on_window_closed(&mut self, window: WindowId) {
        self.log.borrow_mut().closed.push(window);
    }
}

/// A clock that advances a fixed `dt` on every read. Because the scheduler
/// samples the clock once per frame (and never on launch/input callbacks), each
/// consecutive frame sees exactly `dt` of elapsed time after the first — a
/// deterministic stand-in for a display link ticking at a steady rate.
struct FixedStepClock {
    cursor: Instant,
    step: Duration,
}

impl FixedStepClock {
    fn new(step: Duration) -> Self {
        Self {
            cursor: Instant::now(),
            step,
        }
    }
}

impl FrameClock for FixedStepClock {
    fn now(&mut self) -> Instant {
        let at = self.cursor;
        self.cursor += self.step;
        at
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
    assert_eq!(
        log.frames, 1,
        "opening a window on launch schedules the first frame"
    );
    assert_eq!(log.phase_calls, PHASES, "the first frame walks all phases");
}

#[test]
fn n_beats_drive_exactly_n_frames() {
    // With animation on, every beat has a pending reason and runs a full frame.
    // The window opened on launch also draws its first frame, so N scripted
    // beats yield N+1 frames: the launch frame plus one per beat.
    let n = 5u32;
    let script: Vec<RawEvent> = (0..n)
        .map(|_| RawEvent::RedrawRequested {
            window: WindowId(1),
        })
        .collect();
    let log = run(script, true);

    assert_eq!(log.frames, n + 1, "the first frame plus N beat frames");
    assert_eq!(
        log.phase_calls,
        (n + 1) * PHASES,
        "each frame walks all 12 phases in order"
    );
}

#[test]
fn idle_beats_do_no_work() {
    // Animation off and no dirtying event: a bare beat finds an idle reason set
    // and must run zero phases (idle does no work).
    let script = vec![
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ];
    let log = run(script, false);

    // The launch frame is the only one: after it drains, each bare beat finds an
    // idle reason set and runs nothing more.
    assert_eq!(
        log.frames, 1,
        "only the first frame runs; idle beats add none"
    );
    assert_eq!(log.phase_calls, PHASES, "just the first frame's phases");
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
    // The launch frame draws first; then the input dirties the tree and the
    // following beat drains it — two frames total.
    assert_eq!(log.frames, 2, "first frame plus the post-input frame");
    assert_eq!(log.phase_calls, 2 * PHASES);
}

#[test]
fn key_text_and_ime_preedit_each_reach_the_driver() {
    // Key, committed text, and an in-progress IME preedit each normalize to an
    // InputSample and hit on_input; each also dirties the frame so the following
    // beat drains them.
    use viso_platform::{KeyCode, Modifiers, RawImePreedit, RawKey, RawText};
    let script = vec![
        RawEvent::Key(RawKey {
            window: WindowId(1),
            code: KeyCode::Enter,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }),
        RawEvent::Text(RawText {
            window: WindowId(1),
            text: "a".to_string(),
        }),
        RawEvent::ImePreedit(RawImePreedit {
            window: WindowId(1),
            text: "に".to_string(),
            caret: "に".len(),
        }),
        RawEvent::RedrawRequested {
            window: WindowId(1),
        },
    ];
    let log = run(script, false);

    assert_eq!(log.inputs, 3, "key, text, and preedit each fired on_input");
    // The launch frame draws first; the three inputs dirty the tree and the
    // following beat drains them into one frame — two frames total.
    assert_eq!(log.frames, 2, "first frame plus the post-input frame");
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
    // The launch frame draws first; the resize then requests its own redraw —
    // two frames total.
    assert_eq!(log.frames, 2, "first frame plus the resize frame");
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

/// A driver that opens *two* windows on launch, so the loop tracks two open
/// windows and only exits after both close.
struct TwoWindowDriver {
    log: Rc<RefCell<Log>>,
}

impl FrameDriver for TwoWindowDriver {
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
        self.log.borrow_mut().launches += 1;
        cx.create_window(WindowConfig::default()).unwrap();
        cx.create_window(WindowConfig::default()).unwrap();
    }
    fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
    fn on_input(&mut self, _sample: InputSample) {}
    fn run_phase(&mut self, _phase: FramePhase, _cx: &mut RuntimeCx<'_>) {}
    fn on_window_closed(&mut self, window: WindowId) {
        self.log.borrow_mut().closed.push(window);
    }
}

#[test]
fn on_window_closed_fires_per_window_and_the_last_close_exits() {
    // Two windows open on launch. Each WindowClosed drives on_window_closed in
    // order; the loop stays alive after the first close and exits only when the
    // second brings open_windows to zero.
    let log = Rc::new(RefCell::new(Log::default()));
    let script = vec![
        RawEvent::WindowClosed {
            window: WindowId(1),
        },
        RawEvent::WindowClosed {
            window: WindowId(2),
        },
        // Must never be delivered: the loop exited when the second window closed.
        RawEvent::Resized {
            window: WindowId(2),
            width: 9,
            height: 9,
        },
    ];
    let app = Box::new(HeadlessApp::scripted(script));
    let driver = TwoWindowDriver {
        log: Rc::clone(&log),
    };
    Scheduler::new(app, driver).run();
    let log = Rc::try_unwrap(log).ok().unwrap().into_inner();

    assert_eq!(
        log.closed,
        vec![WindowId(1), WindowId(2)],
        "the hook fires once per window, in close order"
    );
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

/// A `FrameDriver` that opens no window on launch — an app with nothing to show
/// yet. Used to prove launch alone (without a window) drives no frame.
struct NoWindowDriver {
    log: Rc<RefCell<Log>>,
}

impl FrameDriver for NoWindowDriver {
    fn on_launch(&mut self, _cx: &mut RuntimeCx<'_>) {
        self.log.borrow_mut().launches += 1;
    }
    fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
    fn on_input(&mut self, _sample: InputSample) {}
    fn run_phase(&mut self, phase: FramePhase, _cx: &mut RuntimeCx<'_>) {
        let mut log = self.log.borrow_mut();
        log.phase_calls += 1;
        if phase == FramePhase::CollectInput {
            log.frames += 1;
        }
    }
}

#[test]
fn launch_without_a_window_drives_no_frame() {
    // No window opened on launch: nothing to draw, so the scheduler stays idle
    // and runs zero frames (idle zero-CPU is preserved).
    let log = Rc::new(RefCell::new(Log::default()));
    let app = Box::new(HeadlessApp::scripted(vec![]));
    let driver = NoWindowDriver {
        log: Rc::clone(&log),
    };
    Scheduler::new(app, driver).run();
    let log = Rc::try_unwrap(log).ok().unwrap().into_inner();

    assert_eq!(log.launches, 1, "on_launch still fires once");
    assert_eq!(log.frames, 0, "no window means no first frame");
    assert_eq!(log.phase_calls, 0);
}

#[test]
fn injected_clock_gives_each_frame_the_scripted_delta() {
    // A fixed-step clock advances a constant dt per frame. With animation on,
    // N scripted beats plus the launch frame run N+1 frames; the first frame
    // has no predecessor (delta ZERO), and every subsequent frame sees exactly
    // the injected dt.
    let n = 4u32;
    let step = Duration::from_millis(16);
    let script: Vec<RawEvent> = (0..n)
        .map(|_| RawEvent::RedrawRequested {
            window: WindowId(1),
        })
        .collect();

    let log = Rc::new(RefCell::new(Log::default()));
    let app = Box::new(HeadlessApp::scripted(script));
    let driver = CountingDriver::new(Rc::clone(&log), true);
    Scheduler::with_clock(app, driver, FixedStepClock::new(step)).run();
    let log = Rc::try_unwrap(log).ok().unwrap().into_inner();

    assert_eq!(log.frames, n + 1, "launch frame plus one per beat");
    assert_eq!(
        log.deltas.len() as u32,
        n + 1,
        "one recorded delta per frame"
    );
    assert_eq!(
        log.deltas[0],
        Duration::ZERO,
        "the first frame has no predecessor, so its delta is zero"
    );
    for (i, d) in log.deltas.iter().enumerate().skip(1) {
        assert_eq!(*d, step, "frame {i} observes exactly the injected step");
    }
}

#[test]
fn default_wall_clock_first_frame_delta_is_zero() {
    // The production path (Scheduler::new -> WallClock) still tags the first
    // frame with a ZERO delta: there is no earlier frame to diff against. This
    // is the smoke test that the default clock wiring is intact.
    let log = run(vec![], false);
    assert_eq!(log.frames, 1, "the launch frame runs");
    assert_eq!(
        log.deltas,
        vec![Duration::ZERO],
        "the sole (first) frame carries a zero delta under the wall clock"
    );
}
