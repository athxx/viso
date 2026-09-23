//! The frame-driver contract: how the runtime calls *up* into the app layer
//! without depending on it.
//!
//! `viso-runtime` must not depend on `viso-ui`/widgets/dsl. But the frame loop
//! has to drive user code: build layout, produce paint, etc. We invert the
//! edge the same way [`viso_platform::AppHandler`] does — a trait defined here,
//! implemented above (by the `viso` facade's `AppDriver`, which owns the user
//! `Application` and its `AppCx`). The scheduler is generic over this trait, so
//! the runtime orchestrates frames while staying UI-agnostic.

use std::time::Instant;

use viso_platform::{Appearance, Insets, LogicalRect, MenuCommandId, WindowId};

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
/// Every method is a no-op-able seam: Phase 1 wires the *loop*, and the facade's
/// driver leaves the phase bodies empty ("blank frames"). Real layout/paint fill
/// these in as their subsystems land.
pub trait FrameDriver {
    /// Called exactly once, after the platform pump is live (on `AppLaunched`),
    /// before any window event. The driver constructs app state and its first
    /// window here.
    fn on_launch(&mut self, cx: &mut RuntimeCx<'_>);

    /// The window's geometry (size and/or scale factor) changed.
    fn on_geometry(&mut self, window: WindowId, scale: f64, width: u32, height: u32);

    /// A normalized input sample arrived. The scheduler has already resolved
    /// the window scale and converted the sample into physical-pixel space, so
    /// the driver can hit-test and route it directly without touching raw
    /// platform types.
    fn on_input(&mut self, sample: InputSample);

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

    /// `window` became (`true`) or stopped being (`false`) the keyboard target.
    /// Default no-op.
    fn on_window_focus(&mut self, _window: WindowId, _focused: bool) {}

    /// The system appearance (light/dark, contrast, motion) changed. Default
    /// no-op.
    fn on_appearance(&mut self, _appearance: Appearance) {}

    /// The region of `window` covered by system UI changed, in logical points.
    /// Default no-op.
    fn on_safe_area(&mut self, _window: WindowId, _insets: Insets) {}

    /// The on-screen keyboard now covers `height` logical points at the bottom
    /// of `window`. Default no-op.
    fn on_keyboard_inset(&mut self, _window: WindowId, _height: f64) {}

    /// The app moved through its OS lifecycle. On
    /// [`Lifecycle::Resumed`] the driver requests a redraw for each window it
    /// owns through `cx`, since the scheduler does not track window ids.
    /// Default no-op.
    fn on_lifecycle(&mut self, _cx: &mut RuntimeCx<'_>, _event: Lifecycle) {}

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
