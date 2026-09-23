//! The frame scheduler: the runtime's implementation of the platform funnel.
//!
//! `Scheduler` owns the live platform app, the driver, and the aggregated
//! redraw reasons. It implements [`AppHandler`] — so it *is* the callback the
//! platform pump calls for every raw event. Its job each event:
//!
//! 1. classify the event into a [`RedrawReason`] (or run a frame, or exit),
//! 2. when a redraw beat arrives, run one frame through all phases,
//! 3. return a [`ControlFlow`] derived from the pending reasons — `Wait` when
//!    idle so the app spends zero CPU (the zero-CPU-when-idle contract), `Poll`
//!    when a frame is pending,
//!    `Exit` when the last window closed.

use std::time::{Duration, Instant};

use viso_platform::{
    AppHandler, ControlFlow, Modifiers as RawModifiers, PlatformApp, RawEvent, RawPointer,
    RawScroll,
};

use crate::clock::{FrameClock, WallClock};
use crate::context::RuntimeCx;
use crate::driver::{FrameDriver, Lifecycle};
use crate::frame::run_frame;
use crate::input::{
    CopySample, ImePreeditSample, InputSample, KeySample, Modifiers, PointerPhase, PointerSample,
    ScrollSample, TextSample,
};
use crate::schedule::{RedrawReason, RedrawReasons};

/// Owns the run loop's mutable state and routes every platform event.
///
/// Generic over the frame [`FrameClock`] so headless tests can inject a
/// deterministic clock; production uses [`WallClock`] via [`Scheduler::new`].
pub struct Scheduler<D: FrameDriver, C: FrameClock = WallClock> {
    app: Box<dyn PlatformApp>,
    driver: D,
    reasons: RedrawReasons,
    /// Windows currently open. When this empties after a close, we exit.
    open_windows: u32,
    launched: bool,
    /// The app is in the background: no frames, no animation beats. Pending
    /// reasons are kept and drained by the first frame after resume.
    suspended: bool,
    /// The time source sampled once at the head of each frame.
    clock: C,
    /// When the previous frame ran, for the delta the next frame observes.
    /// `None` before the first frame, so that frame's delta is `ZERO`.
    last_frame: Option<Instant>,
}

impl<D: FrameDriver> Scheduler<D, WallClock> {
    /// Build a scheduler over a platform app and a frame driver, using the
    /// production [`WallClock`] as its time source.
    pub fn new(app: Box<dyn PlatformApp>, driver: D) -> Self {
        Self::with_clock(app, driver, WallClock)
    }
}

impl<D: FrameDriver, C: FrameClock> Scheduler<D, C> {
    /// Build a scheduler with an explicit clock. Headless tests inject a
    /// [`crate::clock::ManualClock`] here to step frame time deterministically.
    pub fn with_clock(app: Box<dyn PlatformApp>, driver: D, clock: C) -> Self {
        Self {
            app,
            driver,
            reasons: RedrawReasons::new(),
            open_windows: 0,
            launched: false,
            suspended: false,
            clock,
            last_frame: None,
        }
    }

    /// Run to completion: hands `self` to the platform pump as the handler.
    ///
    /// Uses a raw pointer to satisfy the borrow checker — the pump borrows the
    /// app for the duration of `run`, and the handler *is* the same object that
    /// owns the app. This is sound: `run` blocks until the pump returns, and
    /// the two borrows never alias at a yield point (the pump only re-enters
    /// the handler between its own `&mut self.app` uses).
    pub fn run(mut self) {
        // SAFETY: `app` and the handler (`self`) live on the same stack frame
        // for the whole of `run`. `PlatformApp::run` takes `&mut *app` and,
        // between event deliveries, calls back into `handler` (also `self`).
        // These accesses are strictly interleaved, never simultaneous, so no
        // aliasing `&mut` is ever live at once.
        let app: *mut dyn PlatformApp = &mut *self.app;
        let handler: &mut dyn AppHandler = &mut self;
        unsafe { (*app).run(handler) };
    }

    /// Run to completion and hand the driver back, for headless inspection.
    ///
    /// Identical to [`run`](Self::run) but returns the owned driver once the
    /// pump exits (a scripted headless app drains its queue and returns, or the
    /// last window closes). The point is section 66: a headless test drives the
    /// *real* frame loop end to end and then reads the driver's final state —
    /// the animation registry emptied, the store's world rects re-derived — with
    /// no manually-driven `NodeStore` shim standing in for the loop.
    ///
    /// A native pump blocks forever on an empty queue, so this is only useful
    /// with the headless backend; there is no way to observe the return under a
    /// real display link.
    pub fn run_returning(mut self) -> D {
        // SAFETY: identical aliasing argument to `run` — `app` and the handler
        // (`self`) share this stack frame for the whole call, and the pump's
        // accesses to each are strictly interleaved, never simultaneous.
        let app: *mut dyn PlatformApp = &mut *self.app;
        let handler: &mut dyn AppHandler = &mut self;
        unsafe { (*app).run(handler) };
        self.driver
    }

    /// Run under a pump that returns before the app ends — the browser's event
    /// loop cannot block, so its [`PlatformApp::run`] installs callbacks and
    /// returns at once, and the handler must outlive that call. The scheduler
    /// is moved to the heap and leaked, so it lives for the rest of the
    /// process; when the pump does block, this behaves like [`run`](Self::run)
    /// except that the scheduler is never dropped.
    pub fn run_detached(self)
    where
        D: 'static,
        C: 'static,
    {
        let this: &'static mut Self = Box::leak(Box::new(self));
        // SAFETY: `this` is leaked, so it and the `app` it owns stay valid for
        // the rest of the process, including after `run` returns and whenever
        // the pump later re-enters the handler. As in `run`, the pump's uses of
        // `app` and of the handler are strictly interleaved, never simultaneous.
        let app: *mut dyn PlatformApp = &mut *this.app;
        unsafe { (*app).run(this) };
    }

    /// Run one frame if the pending reasons call for it, then reset them.
    fn maybe_run_frame(&mut self) {
        // Only spend a frame when something is actually pending — the
        // zero-CPU-when-idle contract. A backgrounded app draws nothing.
        if self.suspended || self.reasons.is_idle() {
            return;
        }
        // Sample the clock at the frame head and diff against the previous
        // frame; the first frame has no predecessor, so its delta is ZERO.
        let now = self.clock.now();
        let delta = self
            .last_frame
            .map(|prev| now.saturating_duration_since(prev))
            .unwrap_or(Duration::ZERO);
        self.last_frame = Some(now);
        let (created, state_dirty) = {
            let mut cx = RuntimeCx::new(self.app.as_mut(), delta, now);
            run_frame(&mut self.driver, &mut cx);
            (cx.windows_created(), cx.state_dirty_requested())
        };
        self.open_windows += created;
        // A write made during this frame (e.g. an input handler that ran in an
        // earlier phase) leaves state pending for the next frame's flush; carry
        // the reason forward so that frame actually runs.
        if state_dirty {
            self.reasons.add(RedrawReason::StateDirty);
        }
    }

    /// Hand a lifecycle transition to the driver with a live context, so it can
    /// request redraws for its windows on resume.
    fn lifecycle(&mut self, event: Lifecycle) {
        let state_dirty = {
            let mut cx = RuntimeCx::new(self.app.as_mut(), Duration::ZERO, self.clock.now());
            self.driver.on_lifecycle(&mut cx, event);
            cx.state_dirty_requested()
        };
        if state_dirty {
            self.reasons.add(RedrawReason::StateDirty);
        }
    }

    /// After handling an event, decide how the pump should proceed.
    fn resolve_control_flow(&mut self) -> ControlFlow {
        if self.launched && self.open_windows == 0 {
            return ControlFlow::Exit;
        }
        // In the background only OS events wake the loop; timers and animation
        // resume with the first frame after `Resumed`.
        if self.suspended {
            return ControlFlow::Wait;
        }
        // If the driver wants continuous animation, keep beats coming.
        if self.driver.wants_animation() {
            self.reasons.add(RedrawReason::AnimationActive);
        }
        let flow = self.reasons.decide().to_control_flow();
        // An otherwise-idle loop with a live one-shot timer must not simply block
        // forever ([`ControlFlow::Wait`]) — it has to wake when the timer is due.
        // Turn the driver's earliest deadline into a bounded
        // [`ControlFlow::WaitUntil`] so the pump sleeps until exactly that instant
        // and then wakes to fire it. Only when the decision is `Wait` (nothing
        // else is pending, no animation): a frame is already coming under `Poll`,
        // and the driver will fire the timer inside it, so no deadline is needed.
        if matches!(flow, ControlFlow::Wait)
            && let Some(deadline) = self.driver.next_timer_deadline()
        {
            return ControlFlow::WaitUntil(deadline);
        }
        flow
    }
}

impl<D: FrameDriver, C: FrameClock> AppHandler for Scheduler<D, C> {
    fn handle(&mut self, event: RawEvent) -> ControlFlow {
        match event {
            RawEvent::AppLaunched => {
                self.launched = true;
                let (created, first, state_dirty) = {
                    // Launch runs no frame body, so it carries no elapsed time,
                    // but it still gets the clock's reading for `frame_now` so a
                    // driver that arms a timer at launch dates it correctly.
                    let mut cx =
                        RuntimeCx::new(self.app.as_mut(), Duration::ZERO, self.clock.now());
                    self.driver.on_launch(&mut cx);
                    (
                        cx.windows_created(),
                        cx.first_window(),
                        cx.state_dirty_requested(),
                    )
                };
                // A launch-time write leaves state pending; carry the reason so
                // the first frame's flush observes it.
                if state_dirty {
                    self.reasons.add(RedrawReason::StateDirty);
                }
                // Count the windows the driver opened so the loop knows to keep
                // running until they all close.
                self.open_windows += created;
                // If a window opened, the first frame needs a reason *and* a beat,
                // paired like the resize path — the beat alone would be dropped by
                // the idle guard. An app that opened no window stays idle → Wait,
                // so the zero-CPU-when-idle contract holds.
                if created > 0 {
                    self.reasons.add(RedrawReason::FirstFrame);
                    if let Some(window) = first {
                        self.app.request_redraw(window);
                    }
                }
            }
            RawEvent::RedrawRequested { .. } => {
                // A beat: run the frame the pending reasons ask for. Requesting
                // a redraw from the driver added a reason; the beat drains it.
                // While suspended the reasons stay pending for the resume frame.
                if !self.suspended {
                    self.maybe_run_frame();
                    self.reasons.take();
                }
            }
            RawEvent::Resized {
                window,
                width,
                height,
            } => {
                let scale = self
                    .app
                    .window(window)
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0);
                self.driver.on_geometry(window, scale, width, height);
                self.reasons.add(RedrawReason::WindowResize);
                self.app.request_redraw(window);
            }
            RawEvent::ScaleFactorChanged {
                window,
                scale,
                width,
                height,
            } => {
                self.driver.on_geometry(window, scale, width, height);
                self.reasons.add(RedrawReason::WindowResize);
                self.app.request_redraw(window);
            }
            RawEvent::CloseRequested { window, accept } => {
                // Phase 1 accepts every close (no unsaved-state veto yet); the
                // handshake plumbing is in place for later phases to deny.
                let _ = window;
                let _ = accept.is_accepted();
            }
            RawEvent::WindowClosed { window } => {
                // Tear the window's per-window state down *before* decrementing,
                // so the driver's teardown observes the pre-decrement count and
                // the exit gate (`launched && open_windows == 0`) reads the
                // post-teardown state on the next resolve.
                self.driver.on_window_closed(window);
                self.open_windows = self.open_windows.saturating_sub(1);
            }
            RawEvent::Wakeup => {
                // A cross-thread message woke us; treat as async completion so a
                // frame runs to observe whatever it delivered.
                self.reasons.add(RedrawReason::AsyncCompletion);
            }
            RawEvent::Pointer(p) => {
                // Resolve the window scale here — the scheduler owns the window,
                // so it is the one place that can — and normalize the logical
                // point sample into physical pixels before the driver sees it.
                let scale = self
                    .app
                    .window(p.window)
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0) as f32;
                let sample = normalize_pointer(p, scale);
                self.driver.on_input(InputSample::Pointer(sample));
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::Key(k) => {
                // Keys carry no coordinates, so no scale resolution is needed:
                // just drop the OS vocabulary and hand the driver a KeySample.
                let sample = KeySample {
                    window: k.window,
                    key: k.code,
                    pressed: k.pressed,
                    repeat: k.repeat,
                    modifiers: normalize_modifiers(k.modifiers),
                };
                self.driver.on_input(InputSample::Key(sample));
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::Text(t) => {
                // A committed (post-IME) text segment.
                self.driver.on_input(InputSample::Text(TextSample {
                    window: t.window,
                    text: t.text,
                }));
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::ImePreedit(p) => {
                // An in-progress IME composition update.
                self.driver
                    .on_input(InputSample::ImePreedit(ImePreeditSample {
                        window: p.window,
                        text: p.text,
                        caret: p.caret,
                    }));
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::MenuCommand { id } => {
                // A custom menu pick: dispatch to the driver and run a frame, as
                // the command typically mutates app state the UI must reflect.
                self.driver.on_menu_command(id);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::WindowChromeGeom {
                window,
                buttons_rect,
            } => {
                // The native chrome geometry (traffic-light box) of a self-drawn
                // window moved. Hand it to the driver with a live context so it can
                // store the box and push its derived draggable caption region back
                // to the platform through the same call. This runs no frame body
                // (it only updates the drag cache the next mouseDown reads), so it
                // carries a zero delta but still the clock's `now`.
                let mut cx = RuntimeCx::new(self.app.as_mut(), Duration::ZERO, self.clock.now());
                self.driver
                    .on_window_chrome_geom(&mut cx, window, buttons_rect);
            }
            RawEvent::FullscreenChanged { window, fullscreen } => {
                // The window entered/left fullscreen. Hand it to the driver so it
                // can hide/restore its self-drawn caption. Hiding the caption is a
                // visible reflow (`set_hidden` marks LAYOUT|PAINT), so flag the
                // frame input-dirty to pump the redraw — like a menu command, not
                // like the chrome-geom event (which only refreshes a drag cache).
                self.driver.on_fullscreen_changed(window, fullscreen);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::Scroll(s) => {
                // Resolve the window scale here (the scheduler owns the window)
                // and normalize the logical-point sample into physical pixels —
                // both the pointer position and the delta — before the driver
                // routes it to the scrollable target under the cursor.
                let scale = self
                    .app
                    .window(s.window)
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0) as f32;
                let sample = normalize_scroll(s, scale);
                self.driver.on_input(InputSample::Scroll(sample));
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::WindowFocused { window, focused } => {
                self.driver.on_window_focus(window, focused);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::AppearanceChanged(appearance) => {
                self.driver.on_appearance(appearance);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::SafeAreaChanged { window, insets } => {
                self.driver.on_safe_area(window, insets);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::KeyboardInsetChanged { window, height } => {
                self.driver.on_keyboard_inset(window, height);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::CopyRequested { window, cut, reply } => {
                // The driver answers synchronously through `reply`; the backend
                // reads it once this returns. A cut edits content, so redraw.
                self.driver
                    .on_input(InputSample::Copy(CopySample { window, cut, reply }));
                if cut {
                    self.reasons.add(RedrawReason::InputDirty);
                }
            }
            RawEvent::Paste { window, text } => {
                self.driver
                    .on_input(InputSample::Paste(TextSample { window, text }));
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::Suspended => {
                self.suspended = true;
                self.lifecycle(Lifecycle::Suspended);
            }
            RawEvent::Resumed => {
                self.suspended = false;
                self.lifecycle(Lifecycle::Resumed);
                // The drawable may have been rebuilt while hidden.
                self.reasons.add(RedrawReason::ExternalSurfaceInvalidation);
            }
            RawEvent::LowMemory => self.lifecycle(Lifecycle::LowMemory),
        }
        self.resolve_control_flow()
    }
}

/// Convert a raw pointer sample (logical points) into the runtime-tier
/// physical-pixel sample the driver routes. Logical → physical is a single
/// multiply by the window scale; the phase and modifier meaning is preserved.
fn normalize_pointer(p: RawPointer, scale: f32) -> PointerSample {
    use viso_platform::PointerPhase as RawPhase;
    let phase = match p.phase {
        RawPhase::Down => PointerPhase::Down,
        RawPhase::Moved => PointerPhase::Move,
        RawPhase::Up => PointerPhase::Up,
        RawPhase::Left => PointerPhase::Leave,
        RawPhase::Cancel => PointerPhase::Cancel,
    };
    PointerSample {
        window: p.window,
        pointer: p.pointer,
        kind: p.kind,
        x: p.x as f32 * scale,
        y: p.y as f32 * scale,
        pressure: p.pressure,
        buttons: p.buttons.0,
        modifiers: normalize_modifiers(p.modifiers),
        phase,
    }
}

/// Convert a raw scroll sample (logical points) into the runtime-tier
/// physical-pixel sample the driver routes. Both the pointer position and the
/// scroll delta scale by the same window factor.
fn normalize_scroll(s: RawScroll, scale: f32) -> ScrollSample {
    ScrollSample {
        window: s.window,
        x: s.x as f32 * scale,
        y: s.y as f32 * scale,
        delta_x: s.delta_x as f32 * scale,
        delta_y: s.delta_y as f32 * scale,
        modifiers: normalize_modifiers(s.modifiers),
    }
}

/// Copy platform modifier state into the runtime-tier mirror.
fn normalize_modifiers(m: RawModifiers) -> Modifiers {
    Modifiers {
        shift: m.shift,
        control: m.control,
        alt: m.alt,
        logo: m.logo,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phase::FramePhase;
    use viso_platform::WindowId;
    use viso_platform::backend::headless::HeadlessApp;

    /// A driver that reports fixed animation/timer state, so the timer seam in
    /// `resolve_control_flow` can be exercised without the full facade.
    struct SeamDriver {
        animate: bool,
        deadline: Option<Instant>,
    }

    impl FrameDriver for SeamDriver {
        fn on_launch(&mut self, _cx: &mut RuntimeCx<'_>) {}
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, _sample: InputSample) {}
        fn run_phase(&mut self, _phase: FramePhase, _cx: &mut RuntimeCx<'_>) {}
        fn wants_animation(&self) -> bool {
            self.animate
        }
        fn next_timer_deadline(&self) -> Option<Instant> {
            self.deadline
        }
    }

    /// Build an idle scheduler (one open window, no pending reasons) over a
    /// driver with the given animation/timer state.
    fn idle_scheduler(animate: bool, deadline: Option<Instant>) -> Scheduler<SeamDriver> {
        let app = Box::new(HeadlessApp::scripted(vec![]));
        let mut sched = Scheduler::new(app, SeamDriver { animate, deadline });
        // Pretend a window is open and launch has run, so `resolve_control_flow`
        // does not short-circuit to `Exit`.
        sched.launched = true;
        sched.open_windows = 1;
        sched
    }

    #[test]
    fn an_idle_loop_with_a_live_timer_waits_until_its_deadline() {
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut sched = idle_scheduler(false, Some(deadline));
        assert_eq!(
            sched.resolve_control_flow(),
            ControlFlow::WaitUntil(deadline),
            "an otherwise-idle loop blocks exactly until the earliest timer is due"
        );
    }

    #[test]
    fn an_idle_loop_with_no_timer_waits_indefinitely() {
        let mut sched = idle_scheduler(false, None);
        assert_eq!(
            sched.resolve_control_flow(),
            ControlFlow::Wait,
            "with nothing pending and no timer, the loop blocks with no deadline"
        );
    }

    #[test]
    fn an_animating_loop_polls_even_with_a_timer() {
        // Animation already wakes every beat, and the driver fires the timer
        // inside that frame — so the loop polls rather than arming a deadline.
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut sched = idle_scheduler(true, Some(deadline));
        assert_eq!(
            sched.resolve_control_flow(),
            ControlFlow::Poll,
            "an animating loop keeps polling; the timer rides its beats"
        );
    }

    /// A driver that records every `on_fullscreen_changed` it receives, so the
    /// scheduler's dispatch arm can be exercised end to end.
    #[derive(Default)]
    struct RecordingDriver {
        fullscreen: Vec<(WindowId, bool)>,
    }

    impl FrameDriver for RecordingDriver {
        fn on_launch(&mut self, _cx: &mut RuntimeCx<'_>) {}
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, _sample: InputSample) {}
        fn run_phase(&mut self, _phase: FramePhase, _cx: &mut RuntimeCx<'_>) {}
        fn on_fullscreen_changed(&mut self, window: WindowId, fullscreen: bool) {
            self.fullscreen.push((window, fullscreen));
        }
    }

    #[test]
    fn a_fullscreen_change_reaches_the_driver_and_schedules_a_redraw() {
        use viso_platform::AppHandler;

        let app = Box::new(HeadlessApp::scripted(vec![]));
        let mut sched = Scheduler::new(app, RecordingDriver::default());
        sched.launched = true;
        sched.open_windows = 1;

        // Entering fullscreen: the driver is told, and because hiding the caption
        // is a visible reflow the loop is left non-idle (Poll), not Wait.
        let flow = sched.handle(RawEvent::FullscreenChanged {
            window: WindowId(1),
            fullscreen: true,
        });
        assert_eq!(
            flow,
            ControlFlow::Poll,
            "a fullscreen change flags the frame input-dirty, so the loop polls"
        );

        // Leaving fullscreen is reported the same way, so the driver can restore.
        let _ = sched.handle(RawEvent::FullscreenChanged {
            window: WindowId(1),
            fullscreen: false,
        });
        assert_eq!(
            sched.driver.fullscreen,
            vec![(WindowId(1), true), (WindowId(1), false)],
            "each transition reaches the driver in order with its window and state"
        );
    }

    /// A driver that records lifecycle events, frames and clipboard traffic,
    /// and answers a copy request with a fixed selection.
    #[derive(Default)]
    struct LifecycleDriver {
        lifecycle: Vec<Lifecycle>,
        frames: u32,
        pasted: Vec<String>,
        copies: Vec<bool>,
    }

    impl FrameDriver for LifecycleDriver {
        fn on_launch(&mut self, _cx: &mut RuntimeCx<'_>) {}
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, sample: InputSample) {
            match sample {
                InputSample::Paste(t) => self.pasted.push(t.text),
                InputSample::Copy(c) => {
                    self.copies.push(c.cut);
                    c.reply.set("sel".to_owned());
                }
                _ => {}
            }
        }
        fn run_phase(&mut self, phase: FramePhase, _cx: &mut RuntimeCx<'_>) {
            if phase == FramePhase::Submit {
                self.frames += 1;
            }
        }
        fn on_lifecycle(&mut self, _cx: &mut RuntimeCx<'_>, event: Lifecycle) {
            self.lifecycle.push(event);
        }
    }

    fn lifecycle_scheduler() -> Scheduler<LifecycleDriver> {
        let app = Box::new(HeadlessApp::scripted(vec![]));
        let mut sched = Scheduler::new(app, LifecycleDriver::default());
        sched.launched = true;
        sched.open_windows = 1;
        sched
    }

    fn click() -> RawEvent {
        RawEvent::Pointer(RawPointer::mouse(
            WindowId(1),
            4.0,
            4.0,
            viso_platform::PointerButtons::PRIMARY,
            RawModifiers::default(),
            viso_platform::PointerPhase::Down,
        ))
    }

    #[test]
    fn a_suspended_app_runs_no_frames_and_resumes_with_one() {
        let mut sched = lifecycle_scheduler();
        assert_eq!(sched.handle(RawEvent::Suspended), ControlFlow::Wait);
        // Input and beats while backgrounded keep their reasons but draw nothing.
        assert_eq!(sched.handle(click()), ControlFlow::Wait);
        let _ = sched.handle(RawEvent::RedrawRequested {
            window: WindowId(1),
        });
        assert_eq!(sched.driver.frames, 0, "no frame while suspended");

        assert_eq!(
            sched.handle(RawEvent::Resumed),
            ControlFlow::Poll,
            "resuming leaves a frame pending"
        );
        let _ = sched.handle(RawEvent::RedrawRequested {
            window: WindowId(1),
        });
        assert_eq!(
            sched.driver.frames, 1,
            "the resume frame drains every reason"
        );
        assert_eq!(
            sched.driver.lifecycle,
            vec![Lifecycle::Suspended, Lifecycle::Resumed]
        );
    }

    #[test]
    fn a_memory_warning_reaches_the_driver_without_a_frame() {
        let mut sched = lifecycle_scheduler();
        assert_eq!(sched.handle(RawEvent::LowMemory), ControlFlow::Wait);
        assert_eq!(sched.driver.lifecycle, vec![Lifecycle::LowMemory]);
    }

    #[test]
    fn a_copy_request_is_answered_in_place_and_only_cut_dirties_the_frame() {
        let mut sched = lifecycle_scheduler();
        let reply = viso_platform::ClipboardReply::new();
        let flow = sched.handle(RawEvent::CopyRequested {
            window: WindowId(1),
            cut: false,
            reply: reply.clone(),
        });
        assert_eq!(flow, ControlFlow::Wait, "a copy changes nothing on screen");
        assert_eq!(reply.take().as_deref(), Some("sel"));

        let reply = viso_platform::ClipboardReply::new();
        let flow = sched.handle(RawEvent::CopyRequested {
            window: WindowId(1),
            cut: true,
            reply: reply.clone(),
        });
        assert_eq!(flow, ControlFlow::Poll, "a cut removes the selection");
        assert_eq!(reply.take().as_deref(), Some("sel"));
        assert_eq!(sched.driver.copies, vec![false, true]);
    }

    #[test]
    fn pasted_text_reaches_the_driver_as_input() {
        let mut sched = lifecycle_scheduler();
        let flow = sched.handle(RawEvent::Paste {
            window: WindowId(1),
            text: "clip".to_owned(),
        });
        assert_eq!(flow, ControlFlow::Poll);
        assert_eq!(sched.driver.pasted, vec!["clip".to_owned()]);
    }

    #[test]
    fn a_detached_scheduler_runs_the_pump_to_completion() {
        // The headless pump blocks until its script drains, so a detached run
        // still launches and returns; only the scheduler is never dropped.
        let app = Box::new(HeadlessApp::scripted(vec![]));
        Scheduler::new(app, LifecycleDriver::default()).run_detached();
    }
}
