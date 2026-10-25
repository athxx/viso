//! The frame-driver contract: how the runtime calls *up* into the app layer
//! without depending on it.
//!
//! `viso-runtime` must not depend on `viso-ui`/widgets/dsl. But the frame loop
//! has to drive user code: build layout, produce paint, etc. We invert the
//! edge the same way [`viso_platform::AppHandler`] does — a trait defined here,
//! implemented above (by the `viso` facade's `AppDriver`, which owns the user
//! `Application` and its `AppCx`). The scheduler is generic over this trait, so
//! the runtime orchestrates frames while staying UI-agnostic.

use viso_platform::Instant;

use viso_platform::{
    AccessRequest, Appearance, DisplayFeature, GamepadEvent, LogicalInsets, LogicalRect,
    MenuCommandId, WindowId,
};

use crate::context::RuntimeCx;
use crate::input::InputSample;
use crate::phase::FramePhase;

/// Where the app stands in its OS lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lifecycle {
    /// The app went to the background. The scheduler stops running frames and
    /// animation beats until [`Lifecycle::Resumed`].
    Suspended,
    /// The app is back in the foreground; its windows need a fresh frame.
    Resumed,
    /// The OS is short on memory: drop every cache that can be rebuilt.
    LowMemory,
}

/// The app-layer hooks the frame scheduler drives.
///
/// Every method is a no-op-able seam: the scheduler owns the *loop*, and a driver
/// that leaves the phase bodies empty runs "blank frames". The facade's driver
/// fills them in.
pub trait FrameDriver {
    /// Called exactly once, after the platform pump is live (on `AppLaunched`),
    /// before any window event. The driver constructs app state and its first
    /// window here.
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>);

    /// The window's geometry (size and/or scale factor) changed.
    fn on_geometry(&mut self, window: WindowId, scale: f64, width: u32, height: u32);

    /// A normalized input sample arrived at `at`, by the loop's clock. The
    /// scheduler has already resolved the window scale and converted the
    /// sample into physical-pixel space, so the driver can hit-test and route
    /// it directly without touching raw platform types.
    fn on_input(&mut self, sample: InputSample, at: Instant);

    /// Run one frame phase. Called once per phase, in [`FramePhase::ORDER`],
    /// for each frame the scheduler decides to run.
    fn run_phase(&mut self, phase: FramePhase, cx: &mut RuntimeCx<'_>);

    /// A window was closed — by the user (OS close button) or programmatically
    /// (via [`RuntimeCx::close_window`](crate::RuntimeCx::close_window)). Both
    /// arrive as the same [`WindowClosed`](viso_platform::RawEvent::WindowClosed)
    /// event, so the driver tears down that window's per-window state through
    /// this one hook. Called *before* the scheduler decrements its open-window
    /// count, so the teardown observes the pre-decrement state. Default no-op:
    /// a single-window driver holds no per-window state to release.
    fn on_window_closed(&mut self, _window: WindowId) {}

    /// The user picked a custom application-menu item. `command` is the
    /// app-assigned [`MenuCommandId`] carried by the menu tree, so the driver
    /// matches on the small integer it chose when building the menu. Standard
    /// actions (Quit/Close/…) are performed by the OS and never reach here.
    /// Default no-op: a driver with no menu holds nothing to dispatch.
    fn on_menu_command(&mut self, _command: MenuCommandId) {}

    /// The native chrome geometry of a self-drawn-chrome window changed —
    /// `buttons_rect` is the traffic-light bounding box in logical points
    /// (top-left origin, relative to the content area). The driver uses it to
    /// size/align its own caption around the native affordances, and — through
    /// `cx` — to push the caption's draggable region back to the platform via
    /// [`RuntimeCx::set_draggable_regions`] so the next press on the caption
    /// begins a native window drag. Fired on window creation and on resize/scale
    /// changes; never for native-chrome windows. Default no-op: a driver drawing
    /// no custom caption ignores it.
    fn on_window_chrome_geom(
        &mut self,
        _cx: &mut RuntimeCx<'_>,
        _window: WindowId,
        _buttons_rect: LogicalRect,
    ) {
    }

    /// The window entered (`fullscreen = true`) or left (`false`) fullscreen. The
    /// driver hides its self-drawn caption while fullscreen — on macOS the OS
    /// draws its own auto-hiding title bar there — and restores it on exit. No
    /// `RuntimeCx`: this only flips a retained node's visibility in the driver's
    /// own window state; nothing flows back to the platform (unlike
    /// [`on_window_chrome_geom`](Self::on_window_chrome_geom), which pushes drag
    /// regions back through `cx`). Default no-op: a driver with no caption, and
    /// every backend that never reports fullscreen, ignores it.
    fn on_fullscreen_changed(&mut self, _window: WindowId, _fullscreen: bool) {}

    /// An input sample for `window` was routed ([`on_input`](Self::on_input)):
    /// the driver hands the platform, through `cx`, what the sample changed
    /// that must show at once rather than at the next frame — the cursor of
    /// the node now under the mouse, a pointer lock a handler asked for.
    /// Default no-op.
    fn after_input(&mut self, _cx: &mut RuntimeCx<'_>, _window: WindowId) {}

    /// `window` became (`true`) or stopped being (`false`) the keyboard target.
    /// Default no-op.
    fn on_window_focus(&mut self, _window: WindowId, _focused: bool) {}

    /// The system appearance (light/dark, contrast, motion) changed. Default
    /// no-op.
    fn on_appearance(&mut self, _appearance: Appearance) {}

    /// The region of `window` covered by system UI changed, in logical points.
    /// Default no-op.
    fn on_safe_area(&mut self, _window: WindowId, _insets: LogicalInsets) {}

    /// The on-screen keyboard now covers `height` logical points at the bottom
    /// of `window`. Default no-op.
    fn on_keyboard_inset(&mut self, _window: WindowId, _height: f64) {}

    /// The hinges, folds and cutouts crossing `window`'s content changed to
    /// `features`. Default no-op.
    fn on_display_features(&mut self, _window: WindowId, _features: &[DisplayFeature]) {}

    /// The user's locale changed to `locale`, a BCP 47 tag. Default no-op.
    fn on_locale(&mut self, _locale: &str) {}

    /// The app was asked to open `url`: a link of a scheme or host it
    /// declares, at launch or while it runs. The driver hands it to the app
    /// and asks through `cx` for the frames it needs. Default no-op.
    fn on_open_url(&mut self, _cx: &mut RuntimeCx<'_>, _url: &str) {}

    /// An assistive technology started or stopped listening to `window`, or
    /// asked for an action on one of its nodes. Default no-op.
    fn on_accessibility(&mut self, _window: WindowId, _request: AccessRequest) {}

    /// The app moved through its OS lifecycle. On
    /// [`Lifecycle::Resumed`] the driver requests a redraw for each window it
    /// owns through `cx`, since the scheduler does not track window ids.
    /// Default no-op.
    fn on_lifecycle(&mut self, _cx: &mut RuntimeCx<'_>, _event: Lifecycle) {}

    /// `window`'s native surface went away (`available = false`) or came back
    /// (`true`). On loss the driver destroys the window's GPU surface before
    /// returning; on return it creates one against the window's new handle
    /// and redraws through `cx`. Default no-op.
    fn on_surface(&mut self, _cx: &mut RuntimeCx<'_>, _window: WindowId, _available: bool) {}

    /// The loop was kicked through its [`LoopWaker`](viso_platform::LoopWaker):
    /// a task woke, or a thread asked for a beat. The driver requests a redraw
    /// through `cx` for each window with work due, since the kick names none.
    /// Default no-op.
    fn on_wakeup(&mut self, _cx: &mut RuntimeCx<'_>) {}

    /// Whether the driver reads gamepads: while it does, the scheduler polls
    /// them at the head of every frame and on every loop kick, handing what
    /// changed to [`on_gamepad`](Self::on_gamepad). Default `false`: the
    /// platform's gamepad support is never started.
    fn wants_gamepads(&self) -> bool {
        false
    }

    /// A gamepad connected, changed or went away. A frame's changes arrive
    /// before its first phase; the driver requests a redraw through `cx`
    /// for what a connection changes. Default no-op.
    fn on_gamepad(&mut self, _cx: &mut RuntimeCx<'_>, _event: GamepadEvent) {}

    /// Whether the driver wants continuous animation frames right now. When
    /// true, the scheduler keeps requesting redraw beats even with no input.
    fn wants_animation(&self) -> bool {
        false
    }

    /// The earliest instant at which a one-shot UI timer is due, or `None` when
    /// the driver holds no live timer.
    ///
    /// Consulted only when the driver is otherwise idle: the scheduler turns a
    /// `Some(deadline)` into [`ControlFlow::WaitUntil`](viso_platform::ControlFlow::WaitUntil),
    /// so the pump blocks until the nearest timer is due rather than polling.
    /// This is what keeps a waiting toast at zero frames — unlike animation,
    /// which spins a beat every frame, a timer costs nothing until it fires.
    fn next_timer_deadline(&self) -> Option<Instant> {
        None
    }
}
