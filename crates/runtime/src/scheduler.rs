//! The frame scheduler: the runtime's implementation of the platform funnel.
//!
//! `Scheduler` owns the live platform app and the loop state — the driver and
//! the aggregated redraw reasons. The loop state implements [`AppHandler`]: it
//! is the callback the platform pump calls for every raw event, with the app
//! passed in. Its job each event:
//!
//! 1. classify the event into a [`RedrawReason`] (or run a frame, or exit),
//! 2. when a redraw beat arrives, run one frame through all phases,
//! 3. return a [`ControlFlow`] derived from the pending reasons — `Wait` when
//!    idle so the app spends zero CPU (the zero-CPU-when-idle contract), `Poll`
//!    when a frame is pending,
//!    `Exit` when the last window closed.

use std::time::Duration;

use viso_platform::Instant;

use viso_platform::{
    AppHandler, ControlFlow, GamepadEvent, Modifiers as RawModifiers, PlatformApp, RawEvent,
    RawGesture, RawPointer, RawScroll, WindowId,
};

use crate::clock::{FrameClock, WallClock};
use crate::context::RuntimeCx;
use crate::driver::{FrameDriver, Lifecycle};
use crate::frame::run_frame;
use crate::input::{
    CopySample, DropSample, EditMenuSample, GestureSample, ImePreeditSample, InputSample,
    KeySample, Modifiers, PasteRequestSample, PasteSample, PointerPhase, PointerSample,
    ScrollSample, TextSample,
};
use crate::schedule::{RedrawReason, RedrawReasons};

/// Owns the platform app and the run loop's state, and runs the pump.
///
/// Generic over the frame [`FrameClock`] so headless tests can inject a
/// deterministic clock; production uses [`WallClock`] via [`Scheduler::new`].
pub struct Scheduler<D: FrameDriver, C: FrameClock = WallClock> {
    app: Box<dyn PlatformApp>,
    state: Loop<D, C>,
}

/// The run loop's state: the platform's [`AppHandler`]. The pump hands it
/// the app with every event, so the two are never reached through one
/// another.
struct Loop<D: FrameDriver, C: FrameClock> {
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
    /// What a gamepad poll reported, kept to reuse its storage.
    gamepads: Vec<GamepadEvent>,
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
            state: Loop {
                driver,
                reasons: RedrawReasons::new(),
                open_windows: 0,
                launched: false,
                suspended: false,
                clock,
                last_frame: None,
                gamepads: Vec::new(),
            },
        }
    }

    /// Run to completion: the pump borrows the app and hands it to the
    /// loop's state with every event.
    pub fn run(mut self) {
        self.app.run(&mut self.state);
    }

    /// Run to completion and hand the driver back, for headless inspection.
    ///
    /// Identical to [`run`](Self::run) but returns the owned driver once the
    /// pump exits (a scripted headless app drains its queue and returns, or the
    /// last window closes). The point is headless testing: a test drives the
    /// *real* frame loop end to end and then reads the driver's final state —
    /// the animation registry emptied, the store's world rects re-derived — with
    /// no manually-driven `NodeStore` shim standing in for the loop.
    ///
    /// A native pump blocks forever on an empty queue, so this is only useful
    /// with the headless backend; there is no way to observe the return under a
    /// real display link.
    pub fn run_returning(mut self) -> D {
        self.app.run(&mut self.state);
        self.state.driver
    }

    /// Run under a pump that returns before the app ends — the browser's event
    /// loop cannot block, so its [`PlatformApp::run`] installs callbacks and
    /// returns at once, and the app and handler must outlive that call. The
    /// scheduler is moved to the heap and leaked, so both live for the rest of
    /// the process; when the pump does block, this behaves like
    /// [`run`](Self::run) except that the scheduler is never dropped.
    pub fn run_detached(self)
    where
        D: 'static,
        C: 'static,
    {
        let this: &'static mut Self = Box::leak(Box::new(self));
        this.app.run(&mut this.state);
    }
}

impl<D: FrameDriver, C: FrameClock> Loop<D, C> {
    /// Run one frame if the pending reasons call for it, taking them; a
    /// reason added while it runs or by the events it posts is the next
    /// frame's.
    fn maybe_run_frame(&mut self, app: &mut dyn PlatformApp) {
        // Only spend a frame when something is actually pending — the
        // zero-CPU-when-idle contract. A backgrounded app draws nothing.
        if self.suspended || self.reasons.is_idle() {
            return;
        }
        self.reasons.take();
        // The gamepads as of the frame head, so the frame's games read them.
        self.poll_gamepads(app);
        // Sample the clock at the frame head and diff against the previous
        // frame; the first frame has no predecessor, so its delta is ZERO.
        let now = self.clock.now();
        let delta = self
            .last_frame
            .map(|prev| now.saturating_duration_since(prev))
            .unwrap_or(Duration::ZERO);
        self.last_frame = Some(now);
        let (created, state_dirty, posted) = {
            let mut cx = RuntimeCx::new(app, delta, now);
            run_frame(&mut self.driver, &mut cx);
            (
                cx.windows_created(),
                cx.state_dirty_requested(),
                cx.take_posted(),
            )
        };
        self.open_windows += created;
        // A write made during this frame (e.g. an input handler that ran in an
        // earlier phase) leaves state pending for the next frame's flush; carry
        // the reason forward so that frame actually runs.
        if state_dirty {
            self.reasons.add(RedrawReason::StateDirty);
        }
        self.deliver(app, posted);
    }

    /// Reads the gamepads, if the driver wants them, and hands it what
    /// changed; a connection or departure is input the next frame shows.
    fn poll_gamepads(&mut self, app: &mut dyn PlatformApp) {
        if !self.driver.wants_gamepads() {
            return;
        }
        app.poll_gamepads(&mut self.gamepads);
        if self.gamepads.is_empty() {
            return;
        }
        let mut events = std::mem::take(&mut self.gamepads);
        let mut connections = false;
        let state_dirty = {
            let mut cx = RuntimeCx::new(app, Duration::ZERO, self.clock.peek());
            for event in events.drain(..) {
                connections |= !matches!(event, GamepadEvent::Changed(..));
                self.driver.on_gamepad(&mut cx, event);
            }
            cx.state_dirty_requested()
        };
        self.gamepads = events;
        if connections {
            self.reasons.add(RedrawReason::InputDirty);
        }
        if state_dirty {
            self.reasons.add(RedrawReason::StateDirty);
        }
    }

    /// Handles the events the driver posted, in order.
    fn deliver(&mut self, app: &mut dyn PlatformApp, posted: Vec<RawEvent>) {
        for event in posted {
            let _ = self.handle(app, event);
        }
    }

    /// Hand a lifecycle transition to the driver with a live context, so it can
    /// request redraws for its windows on resume.
    fn lifecycle(&mut self, app: &mut dyn PlatformApp, event: Lifecycle) {
        let state_dirty = {
            let mut cx = RuntimeCx::new(app, Duration::ZERO, self.clock.now());
            self.driver.on_lifecycle(&mut cx, event);
            cx.state_dirty_requested()
        };
        if state_dirty {
            self.reasons.add(RedrawReason::StateDirty);
        }
    }

    /// Hand a URL to open to the driver with a live context: what the app
    /// does with it is like input, and the driver names the windows to
    /// frame.
    fn open_url(&mut self, app: &mut dyn PlatformApp, url: &str) {
        let (state_dirty, posted) = {
            let mut cx = RuntimeCx::new(app, Duration::ZERO, self.clock.now());
            self.driver.on_open_url(&mut cx, url);
            (cx.state_dirty_requested(), cx.take_posted())
        };
        self.reasons.add(RedrawReason::InputDirty);
        if state_dirty {
            self.reasons.add(RedrawReason::StateDirty);
        }
        self.deliver(app, posted);
    }

    /// Hand a loop kick to the driver with a live context.
    fn wakeup(&mut self, app: &mut dyn PlatformApp) {
        let (state_dirty, posted) = {
            let mut cx = RuntimeCx::new(app, Duration::ZERO, self.clock.peek());
            self.driver.on_wakeup(&mut cx);
            (cx.state_dirty_requested(), cx.take_posted())
        };
        if state_dirty {
            self.reasons.add(RedrawReason::StateDirty);
        }
        self.deliver(app, posted);
    }

    /// Hand a window's surface loss or return to the driver with a live
    /// context.
    fn surface(&mut self, app: &mut dyn PlatformApp, window: WindowId, available: bool) {
        let state_dirty = {
            let mut cx = RuntimeCx::new(app, Duration::ZERO, self.clock.now());
            self.driver.on_surface(&mut cx, window, available);
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

impl<D: FrameDriver, C: FrameClock> AppHandler for Loop<D, C> {
    fn handle(&mut self, app: &mut dyn PlatformApp, event: RawEvent) -> ControlFlow {
        let input = input_window(&event);
        let flow = self.dispatch(app, event);
        if let Some(window) = input {
            self.after_input(app, window);
        }
        flow
    }
}

/// The window an input event is routed to, `None` for any other event.
fn input_window(event: &RawEvent) -> Option<WindowId> {
    Some(match event {
        RawEvent::Pointer(p) => p.window,
        RawEvent::Key(k) => k.window,
        RawEvent::Text(t) => t.window,
        RawEvent::ImePreedit(p) => p.window,
        RawEvent::Scroll(s) => s.window,
        RawEvent::Gesture(g) => g.window,
        RawEvent::Paste { window, .. }
        | RawEvent::CopyRequested { window, .. }
        | RawEvent::DragEnded { window, .. }
        | RawEvent::EditMenu { window, .. } => *window,
        RawEvent::Drop(d) => d.window,
        RawEvent::PointerMotion { window, .. } | RawEvent::PointerLockChanged { window, .. } => {
            *window
        }
        _ => return None,
    })
}

impl<D: FrameDriver, C: FrameClock> Loop<D, C> {
    /// Lets the driver push what the input it just routed changed for
    /// `window` to the platform at once.
    fn after_input(&mut self, app: &mut dyn PlatformApp, window: WindowId) {
        let posted = {
            let mut cx = RuntimeCx::new(app, Duration::ZERO, self.clock.peek());
            self.driver.after_input(&mut cx, window);
            if cx.state_dirty_requested() {
                self.reasons.add(RedrawReason::StateDirty);
            }
            cx.take_posted()
        };
        self.deliver(app, posted);
    }

    fn dispatch(&mut self, app: &mut dyn PlatformApp, event: RawEvent) -> ControlFlow {
        match event {
            RawEvent::AppLaunched => {
                self.launched = true;
                let (created, first, state_dirty) = {
                    // Launch runs no frame body, so it carries no elapsed time,
                    // but it still gets the clock's reading for `frame_now` so a
                    // driver that arms a timer at launch dates it correctly.
                    let mut cx = RuntimeCx::new(app, Duration::ZERO, self.clock.now());
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
                        app.request_redraw(window);
                    }
                }
            }
            RawEvent::RedrawRequested { .. } => {
                // A beat: run the frame the pending reasons ask for. Requesting
                // a redraw from the driver added a reason; the beat drains it.
                // While suspended the reasons stay pending for the resume frame.
                self.maybe_run_frame(app);
            }
            RawEvent::Resized {
                window,
                width,
                height,
            } => {
                let scale = app.window(window).map(|w| w.scale_factor()).unwrap_or(1.0);
                self.driver.on_geometry(window, scale, width, height);
                self.reasons.add(RedrawReason::WindowResize);
                app.request_redraw(window);
            }
            RawEvent::ScaleFactorChanged {
                window,
                scale,
                width,
                height,
            } => {
                self.driver.on_geometry(window, scale, width, height);
                self.reasons.add(RedrawReason::WindowResize);
                app.request_redraw(window);
            }
            RawEvent::CloseRequested { window, accept } => {
                // Every close is accepted (there is no unsaved-state veto); the
                // handshake plumbing is in place for a handler to deny.
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
                // A cross-thread kick woke us; treat as async completion so a
                // frame runs to observe whatever it delivered, and let the
                // driver name the windows that frame must cover.
                self.reasons.add(RedrawReason::AsyncCompletion);
                // A kick may be a gamepad connecting while no frame runs.
                self.poll_gamepads(app);
                self.wakeup(app);
            }
            RawEvent::Pointer(p) => {
                // Resolve the window scale here — the scheduler owns the window,
                // so it is the one place that can — and normalize the logical
                // point sample into physical pixels before the driver sees it.
                let scale = app
                    .window(p.window)
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0) as f32;
                let sample = normalize_pointer(p, scale);
                self.driver
                    .on_input(InputSample::Pointer(sample), self.clock.peek());
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
                self.driver
                    .on_input(InputSample::Key(sample), self.clock.peek());
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::Text(t) => {
                // A committed (post-IME) text segment.
                self.driver.on_input(
                    InputSample::Text(TextSample {
                        window: t.window,
                        text: t.text,
                    }),
                    self.clock.peek(),
                );
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::ImePreedit(p) => {
                // An in-progress IME composition update.
                self.driver.on_input(
                    InputSample::ImePreedit(ImePreeditSample {
                        window: p.window,
                        text: p.text,
                        caret: p.caret,
                    }),
                    self.clock.peek(),
                );
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
                let mut cx = RuntimeCx::new(app, Duration::ZERO, self.clock.now());
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
                let scale = app
                    .window(s.window)
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0) as f32;
                let sample = normalize_scroll(s, scale);
                self.driver
                    .on_input(InputSample::Scroll(sample), self.clock.peek());
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::Gesture(g) => {
                let scale = app
                    .window(g.window)
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0) as f32;
                let sample = normalize_gesture(g, scale);
                self.driver
                    .on_input(InputSample::Gesture(sample), self.clock.peek());
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
            RawEvent::DisplayFeaturesChanged { window, features } => {
                self.driver.on_display_features(window, &features);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::LocaleChanged { locale } => {
                self.driver.on_locale(&locale);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::OpenUrl { url } => self.open_url(app, &url),
            RawEvent::CopyRequested { window, cut, reply } => {
                // The driver answers synchronously through `reply`; the backend
                // reads it once this returns. A cut edits content, so redraw.
                self.driver.on_input(
                    InputSample::Copy(CopySample { window, cut, reply }),
                    self.clock.peek(),
                );
                if cut {
                    self.reasons.add(RedrawReason::InputDirty);
                }
            }
            RawEvent::PasteRequested { window, reply } => {
                // The driver answers synchronously through `reply`; the
                // backend reads the clipboard once this returns.
                self.driver.on_input(
                    InputSample::PasteRequest(PasteRequestSample { window, reply }),
                    self.clock.peek(),
                );
            }
            RawEvent::Paste {
                window,
                data,
                token,
            } => {
                self.driver.on_input(
                    InputSample::Paste(PasteSample {
                        window,
                        data,
                        token,
                    }),
                    self.clock.peek(),
                );
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::Drop(d) => {
                // The driver answers through the reply, which the backend
                // reads as the OS next asks.
                let scale = app
                    .window(d.window)
                    .map(|w| w.scale_factor())
                    .unwrap_or(1.0);
                self.driver.on_input(
                    InputSample::Drop(DropSample {
                        window: d.window,
                        phase: d.phase,
                        x: (d.x * scale) as f32,
                        y: (d.y * scale) as f32,
                        offer: d.offer,
                        reply: d.reply,
                    }),
                    self.clock.peek(),
                );
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::DragEnded { window, action } => {
                self.driver
                    .on_input(InputSample::DragEnded { window, action }, self.clock.peek());
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::EditMenu { window, action } => {
                self.driver.on_input(
                    InputSample::EditMenu(EditMenuSample { window, action }),
                    self.clock.peek(),
                );
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::PointerMotion { window, dx, dy } => {
                let scale = app.window(window).map(|w| w.scale_factor()).unwrap_or(1.0);
                self.driver.on_input(
                    InputSample::Motion {
                        window,
                        dx: (dx * scale) as f32,
                        dy: (dy * scale) as f32,
                    },
                    self.clock.peek(),
                );
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::PointerLockChanged { window, locked } => {
                self.driver.on_pointer_lock(window, locked);
                self.reasons.add(RedrawReason::InputDirty);
            }
            RawEvent::Suspended => {
                self.suspended = true;
                self.lifecycle(app, Lifecycle::Suspended);
            }
            RawEvent::Resumed => {
                self.suspended = false;
                self.lifecycle(app, Lifecycle::Resumed);
                // The drawable may have been rebuilt while hidden.
                self.reasons.add(RedrawReason::ExternalSurfaceInvalidation);
            }
            RawEvent::LowMemory => self.lifecycle(app, Lifecycle::LowMemory),
            RawEvent::SurfaceDestroyed { window } => self.surface(app, window, false),
            RawEvent::SurfaceCreated { window } => {
                self.surface(app, window, true);
                self.reasons.add(RedrawReason::ExternalSurfaceInvalidation);
            }
            RawEvent::Accessibility { window, request } => {
                // Activation needs a frame to publish the first tree; an action
                // edits state like any input. No OS input precedes either, so
                // nothing else asks the backend for the beat.
                self.driver.on_accessibility(window, request);
                self.reasons.add(RedrawReason::InputDirty);
                app.request_redraw(window);
            }
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
fn normalize_gesture(g: RawGesture, scale: f32) -> GestureSample {
    GestureSample {
        window: g.window,
        x: g.x as f32 * scale,
        y: g.y as f32 * scale,
        scale: g.scale as f32,
        rotation: g.rotation as f32,
        modifiers: normalize_modifiers(g.modifiers),
        phase: g.phase,
    }
}

fn normalize_scroll(s: RawScroll, scale: f32) -> ScrollSample {
    ScrollSample {
        window: s.window,
        x: s.x as f32 * scale,
        y: s.y as f32 * scale,
        delta_x: s.delta_x as f32 * scale,
        delta_y: s.delta_y as f32 * scale,
        modifiers: normalize_modifiers(s.modifiers),
        phase: s.phase,
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

    impl<D: FrameDriver, C: FrameClock> Scheduler<D, C> {
        /// Hand one event to the loop, as the pump would.
        fn handle(&mut self, event: RawEvent) -> ControlFlow {
            self.state.handle(self.app.as_mut(), event)
        }
    }

    /// A driver that reports fixed animation/timer state, so the timer seam in
    /// `resolve_control_flow` can be exercised without the full facade.
    struct SeamDriver {
        animate: bool,
        deadline: Option<Instant>,
    }

    impl FrameDriver for SeamDriver {
        fn on_launch(&mut self, _cx: &mut RuntimeCx<'_>) {}
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, _sample: InputSample, _at: Instant) {}
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
        sched.state.launched = true;
        sched.state.open_windows = 1;
        sched
    }

    #[test]
    fn an_idle_loop_with_a_live_timer_waits_until_its_deadline() {
        let deadline = Instant::now() + Duration::from_secs(4);
        let mut sched = idle_scheduler(false, Some(deadline));
        assert_eq!(
            sched.state.resolve_control_flow(),
            ControlFlow::WaitUntil(deadline),
            "an otherwise-idle loop blocks exactly until the earliest timer is due"
        );
    }

    #[test]
    fn an_idle_loop_with_no_timer_waits_indefinitely() {
        let mut sched = idle_scheduler(false, None);
        assert_eq!(
            sched.state.resolve_control_flow(),
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
            sched.state.resolve_control_flow(),
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
        fn on_input(&mut self, _sample: InputSample, _at: Instant) {}
        fn run_phase(&mut self, _phase: FramePhase, _cx: &mut RuntimeCx<'_>) {}
        fn on_fullscreen_changed(&mut self, window: WindowId, fullscreen: bool) {
            self.fullscreen.push((window, fullscreen));
        }
    }

    #[test]
    fn a_fullscreen_change_reaches_the_driver_and_schedules_a_redraw() {
        let app = Box::new(HeadlessApp::scripted(vec![]));
        let mut sched = Scheduler::new(app, RecordingDriver::default());
        sched.state.launched = true;
        sched.state.open_windows = 1;

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
            sched.state.driver.fullscreen,
            vec![(WindowId(1), true), (WindowId(1), false)],
            "each transition reaches the driver in order with its window and state"
        );
    }

    /// A driver that opens one window and counts its frames and the
    /// accessibility requests it receives.
    #[derive(Default)]
    struct AccessDriver {
        frames: u32,
        requests: Vec<viso_platform::AccessRequest>,
    }

    impl FrameDriver for AccessDriver {
        fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
            cx.create_window(viso_platform::WindowConfig::default())
                .expect("headless window");
        }
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, _sample: InputSample, _at: Instant) {}
        fn run_phase(&mut self, phase: FramePhase, _cx: &mut RuntimeCx<'_>) {
            if phase == FramePhase::Submit {
                self.frames += 1;
            }
        }
        fn on_accessibility(&mut self, _window: WindowId, request: viso_platform::AccessRequest) {
            self.requests.push(request);
        }
    }

    #[test]
    fn an_accessibility_request_gets_a_frame_of_its_own() {
        let activated = RawEvent::Accessibility {
            window: WindowId(1),
            request: viso_platform::AccessRequest::Activated,
        };
        let app = Box::new(HeadlessApp::scripted(vec![activated]));
        let driver = Scheduler::new(app, AccessDriver::default()).run_returning();
        assert_eq!(driver.requests, [viso_platform::AccessRequest::Activated]);
        assert_eq!(
            driver.frames, 2,
            "the first frame, then the one that publishes the tree"
        );
    }

    /// A driver that opens one window, kicks its own loop at launch, and asks
    /// for that window's beat on the wakeup.
    #[derive(Default)]
    struct WakeDriver {
        window: Option<WindowId>,
        wakeups: u32,
        frames: u32,
    }

    impl FrameDriver for WakeDriver {
        fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
            let window = cx
                .create_window(viso_platform::WindowConfig::default())
                .expect("headless window");
            self.window = Some(window);
            let waker = cx.loop_waker();
            std::thread::spawn(move || waker.wake()).join().unwrap();
        }
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, _sample: InputSample, _at: Instant) {}
        fn run_phase(&mut self, phase: FramePhase, _cx: &mut RuntimeCx<'_>) {
            if phase == FramePhase::Submit {
                self.frames += 1;
            }
        }
        fn on_wakeup(&mut self, cx: &mut RuntimeCx<'_>) {
            self.wakeups += 1;
            if let Some(window) = self.window {
                cx.request_redraw(window);
            }
        }
    }

    #[test]
    fn a_loop_kick_reaches_the_driver_and_its_redraw_runs_a_frame() {
        let app = Box::new(HeadlessApp::scripted(vec![]));
        let driver = Scheduler::new(app, WakeDriver::default()).run_returning();
        assert_eq!(driver.wakeups, 1);
        assert_eq!(driver.frames, 2, "the first frame, then the wakeup's");
    }

    /// A driver that opens one window, reads gamepads when `wants` says,
    /// records what they did and keeps asking for frames.
    #[derive(Default)]
    struct PadDriver {
        wants: bool,
        window: Option<WindowId>,
        events: Vec<(u32, GamepadEvent)>,
        frames: u32,
    }

    impl FrameDriver for PadDriver {
        fn on_launch(&mut self, cx: &mut RuntimeCx<'_>) {
            let window = cx
                .create_window(viso_platform::WindowConfig::default())
                .expect("headless window");
            self.window = Some(window);
        }
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, _sample: InputSample, _at: Instant) {}
        fn run_phase(&mut self, phase: FramePhase, cx: &mut RuntimeCx<'_>) {
            if phase == FramePhase::Submit {
                self.frames += 1;
                if let Some(window) = self.window {
                    cx.request_redraw(window);
                }
            }
        }
        fn wants_animation(&self) -> bool {
            true
        }
        fn wants_gamepads(&self) -> bool {
            self.wants
        }
        fn on_gamepad(&mut self, cx: &mut RuntimeCx<'_>, event: GamepadEvent) {
            if let GamepadEvent::Connected(info) = &event {
                assert!(cx.rumble_gamepad(
                    info.id,
                    viso_platform::Rumble {
                        strong: 1.0,
                        weak: 0.5,
                        duration: Duration::from_millis(80),
                    }
                ));
            }
            self.events.push((self.frames, event));
        }
    }

    fn pad_run(
        wants: bool,
    ) -> (
        PadDriver,
        viso_platform::backend::headless::HeadlessGamepads,
    ) {
        let pads = viso_platform::backend::headless::HeadlessGamepads::default();
        let pad = pads.connect("Pad");
        let mut held = viso_platform::GamepadState::default();
        held.set(viso_platform::GamepadButton::Start, true);
        pads.set(pad, held);
        pads.set(pad, held);
        pads.disconnect(pad);
        let mut app = HeadlessApp::scripted(vec![]);
        app.frame_budget(6);
        app.attach_gamepads(pads.clone());
        let driver = PadDriver {
            wants,
            ..PadDriver::default()
        };
        let driver = Scheduler::new(Box::new(app), driver).run_returning();
        (driver, pads)
    }

    #[test]
    fn the_gamepads_are_polled_at_each_frame_head_only_when_the_driver_reads_them() {
        let (driver, pads) = pad_run(true);
        let at: Vec<_> = driver.events.iter().map(|(frame, _)| *frame).collect();
        assert_eq!(
            at,
            [0, 1, 3],
            "before each frame's phases; no change, no event"
        );
        assert!(matches!(driver.events[0].1, GamepadEvent::Connected(_)));
        assert!(matches!(driver.events[1].1, GamepadEvent::Changed(..)));
        assert!(matches!(driver.events[2].1, GamepadEvent::Disconnected(_)));
        assert_eq!(pads.rumbles().len(), 1, "the connection rumbled it");

        let (driver, pads) = pad_run(false);
        assert!(driver.events.is_empty());
        assert_eq!(pads.pending(), 4, "never polled");
    }

    /// A driver that records lifecycle events, frames and clipboard traffic,
    /// and answers a copy request with a fixed selection.
    #[derive(Default)]
    struct LifecycleDriver {
        lifecycle: Vec<Lifecycle>,
        frames: u32,
        pasted: Vec<String>,
        copies: Vec<bool>,
        surfaces: Vec<(WindowId, bool)>,
        urls: Vec<String>,
    }

    impl FrameDriver for LifecycleDriver {
        fn on_launch(&mut self, _cx: &mut RuntimeCx<'_>) {}
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, sample: InputSample, _at: Instant) {
            match sample {
                InputSample::Paste(p) => self.pasted.push(p.data.text.unwrap_or_default()),
                InputSample::Copy(c) => {
                    self.copies.push(c.cut);
                    c.reply.set(viso_platform::ClipboardData::text("sel"));
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
        fn on_surface(&mut self, _cx: &mut RuntimeCx<'_>, window: WindowId, available: bool) {
            self.surfaces.push((window, available));
        }
        fn on_open_url(&mut self, _cx: &mut RuntimeCx<'_>, url: &str) {
            self.urls.push(url.to_owned());
        }
    }

    fn lifecycle_scheduler() -> Scheduler<LifecycleDriver> {
        let app = Box::new(HeadlessApp::scripted(vec![]));
        let mut sched = Scheduler::new(app, LifecycleDriver::default());
        sched.state.launched = true;
        sched.state.open_windows = 1;
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
    fn a_url_to_open_reaches_the_driver_and_asks_for_a_frame() {
        let mut sched = lifecycle_scheduler();
        let flow = sched.handle(RawEvent::OpenUrl {
            url: "notes://item/7".to_owned(),
        });
        assert_eq!(sched.state.driver.urls, ["notes://item/7"]);
        assert_eq!(flow, ControlFlow::Poll, "what the app did with it is drawn");
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
        assert_eq!(sched.state.driver.frames, 0, "no frame while suspended");

        assert_eq!(
            sched.handle(RawEvent::Resumed),
            ControlFlow::Poll,
            "resuming leaves a frame pending"
        );
        let _ = sched.handle(RawEvent::RedrawRequested {
            window: WindowId(1),
        });
        assert_eq!(
            sched.state.driver.frames, 1,
            "the resume frame drains every reason"
        );
        assert_eq!(
            sched.state.driver.lifecycle,
            vec![Lifecycle::Suspended, Lifecycle::Resumed]
        );
    }

    #[test]
    fn a_lost_surface_draws_nothing_and_a_new_one_draws_once() {
        let mut sched = lifecycle_scheduler();
        let window = WindowId(1);
        assert_eq!(
            sched.handle(RawEvent::SurfaceDestroyed { window }),
            ControlFlow::Wait,
            "losing the surface schedules no frame"
        );
        assert_eq!(
            sched.handle(RawEvent::SurfaceCreated { window }),
            ControlFlow::Poll,
            "the new surface needs its first frame"
        );
        let _ = sched.handle(RawEvent::RedrawRequested { window });
        assert_eq!(sched.state.driver.frames, 1);
        assert_eq!(
            sched.state.driver.surfaces,
            vec![(window, false), (window, true)]
        );
    }

    #[test]
    fn a_memory_warning_reaches_the_driver_without_a_frame() {
        let mut sched = lifecycle_scheduler();
        assert_eq!(sched.handle(RawEvent::LowMemory), ControlFlow::Wait);
        assert_eq!(sched.state.driver.lifecycle, vec![Lifecycle::LowMemory]);
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
        assert_eq!(
            reply.take(),
            Some(viso_platform::ClipboardData::text("sel"))
        );

        let reply = viso_platform::ClipboardReply::new();
        let flow = sched.handle(RawEvent::CopyRequested {
            window: WindowId(1),
            cut: true,
            reply: reply.clone(),
        });
        assert_eq!(flow, ControlFlow::Poll, "a cut removes the selection");
        assert_eq!(
            reply.take(),
            Some(viso_platform::ClipboardData::text("sel"))
        );
        assert_eq!(sched.state.driver.copies, vec![false, true]);
    }

    #[test]
    fn pasted_text_reaches_the_driver_as_input() {
        let mut sched = lifecycle_scheduler();
        let flow = sched.handle(RawEvent::Paste {
            window: WindowId(1),
            data: viso_platform::ClipboardData::text("clip"),
            token: None,
        });
        assert_eq!(flow, ControlFlow::Poll);
        assert_eq!(sched.state.driver.pasted, vec!["clip".to_owned()]);
    }

    #[test]
    fn a_detached_scheduler_runs_the_pump_to_completion() {
        // The headless pump blocks until its script drains, so a detached run
        // still launches and returns; only the scheduler is never dropped.
        let app = Box::new(HeadlessApp::scripted(vec![]));
        Scheduler::new(app, LifecycleDriver::default()).run_detached();
    }

    /// A driver that posts a click from its first frame and records the
    /// pointer samples it then receives.
    #[derive(Default)]
    struct PostDriver {
        frames: u32,
        presses: Vec<(f32, f32)>,
    }

    impl FrameDriver for PostDriver {
        fn on_launch(&mut self, _cx: &mut RuntimeCx<'_>) {}
        fn on_geometry(&mut self, _window: WindowId, _scale: f64, _width: u32, _height: u32) {}
        fn on_input(&mut self, sample: InputSample, _at: Instant) {
            if let InputSample::Pointer(p) = sample {
                self.presses.push((p.x, p.y));
            }
        }
        fn run_phase(&mut self, phase: FramePhase, cx: &mut RuntimeCx<'_>) {
            if phase == FramePhase::Submit {
                self.frames += 1;
                if self.frames == 1 {
                    cx.post_event(click());
                }
            }
        }
    }

    #[test]
    fn an_event_a_frame_posts_is_handled_as_the_platforms_once_it_returns() {
        let app = Box::new(HeadlessApp::scripted(vec![]));
        let mut sched = Scheduler::new(app, PostDriver::default());
        sched.state.launched = true;
        sched.state.open_windows = 1;
        sched.state.reasons.add(RedrawReason::FirstFrame);
        let _ = sched.handle(RawEvent::RedrawRequested {
            window: WindowId(1),
        });
        assert_eq!(sched.state.driver.frames, 1);
        assert_eq!(
            sched.state.driver.presses,
            vec![(4.0, 4.0)],
            "the posted click reaches the driver as input, normalized"
        );
        // The input it caused is the next beat's reason: that frame runs.
        let _ = sched.handle(RawEvent::RedrawRequested {
            window: WindowId(1),
        });
        assert_eq!(sched.state.driver.frames, 2);
    }
}
