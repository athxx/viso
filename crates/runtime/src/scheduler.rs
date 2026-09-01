//! The frame scheduler: the runtime's implementation of the platform funnel.
//!
//! `Scheduler` owns the live platform app, the driver, and the aggregated
//! redraw reasons. It implements [`AppHandler`] — so it *is* the callback the
//! platform pump calls for every raw event (§11.1). Its job each event:
//!
//! 1. classify the event into a [`RedrawReason`] (or run a frame, or exit),
//! 2. when a redraw beat arrives, run one frame through all phases,
//! 3. return a [`ControlFlow`] derived from the pending reasons — `Wait` when
//!    idle so the app spends zero CPU (§12.1), `Poll` when a frame is pending,
//!    `Exit` when the last window closed.

use viso_platform::{AppHandler, ControlFlow, PlatformApp, RawEvent};

use crate::context::RuntimeCx;
use crate::driver::FrameDriver;
use crate::frame::run_frame;
use crate::schedule::{RedrawReason, RedrawReasons};

/// Owns the run loop's mutable state and routes every platform event.
pub struct Scheduler<D: FrameDriver> {
    app: Box<dyn PlatformApp>,
    driver: D,
    reasons: RedrawReasons,
    /// Windows currently open. When this empties after a close, we exit.
    open_windows: u32,
    launched: bool,
}

impl<D: FrameDriver> Scheduler<D> {
    /// Build a scheduler over a platform app and a frame driver.
    pub fn new(app: Box<dyn PlatformApp>, driver: D) -> Self {
        Self {
            app,
            driver,
            reasons: RedrawReasons::new(),
            open_windows: 0,
            launched: false,
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

    /// Run one frame if the pending reasons call for it, then reset them.
    fn maybe_run_frame(&mut self) {
        // Only spend a frame when something is actually pending (§12.1).
        if self.reasons.is_idle() {
            return;
        }
        let created = {
            let mut cx = RuntimeCx::new(self.app.as_mut());
            run_frame(&mut self.driver, &mut cx);
            cx.windows_created()
        };
        self.open_windows += created;
    }

    /// After handling an event, decide how the pump should proceed.
    fn resolve_control_flow(&mut self) -> ControlFlow {
        if self.launched && self.open_windows == 0 {
            return ControlFlow::Exit;
        }
        // If the driver wants continuous animation, keep beats coming.
        if self.driver.wants_animation() {
            self.reasons.add(RedrawReason::AnimationActive);
        }
        self.reasons.decide().to_control_flow()
    }
}

impl<D: FrameDriver> AppHandler for Scheduler<D> {
    fn handle(&mut self, event: RawEvent) -> ControlFlow {
        match event {
            RawEvent::AppLaunched => {
                self.launched = true;
                let (created, first) = {
                    let mut cx = RuntimeCx::new(self.app.as_mut());
                    self.driver.on_launch(&mut cx);
                    (cx.windows_created(), cx.first_window())
                };
                // Count the windows the driver opened so the loop knows to keep
                // running until they all close.
                self.open_windows += created;
                // If a window opened, the first frame needs a reason *and* a beat,
                // paired like the resize path — the beat alone would be dropped by
                // the idle guard. An app that opened no window stays idle → Wait,
                // so the zero-CPU-when-idle contract holds (§12.1).
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
                self.maybe_run_frame();
                self.reasons.take();
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
            RawEvent::WindowClosed { .. } => {
                self.open_windows = self.open_windows.saturating_sub(1);
            }
            RawEvent::Wakeup => {
                // A cross-thread message woke us; treat as async completion so a
                // frame runs to observe whatever it delivered.
                self.reasons.add(RedrawReason::AsyncCompletion);
            }
            RawEvent::Pointer(_) | RawEvent::Scroll(_) | RawEvent::Key(_) | RawEvent::Text(_) => {
                self.driver.on_input();
                self.reasons.add(RedrawReason::InputDirty);
            }
        }
        self.resolve_control_flow()
    }
}
