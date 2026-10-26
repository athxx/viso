//! The runtime-scope context handed to a [`crate::FrameDriver`].
//!
//! `RuntimeCx` is the *UI-agnostic* capability handle: it lets a driver create
//! windows and request redraws through the live platform app, without the
//! runtime ever naming a UI type. The facade wraps this together with a
//! `viso-ui` `AppCx` to give the user a full application context; the runtime
//! itself sees only this.

use std::time::Duration;

use viso_platform::{GamepadId, Instant, LoopWaker, Rumble};

use viso_platform::{
    Appearance, CursorIcon, EditLabels, EditMenu, LogicalRect, Loupe, PlatformApp, PlatformCaps,
    PlatformError, RawEvent, RawWindowHandle, SemanticsDelta, TextEntry, WindowConfig, WindowId,
};

/// Capabilities available to a frame driver for the duration of one callback.
///
/// Borrows the platform app mutably, so window creation and redraw requests go
/// straight to the OS backend. Deliberately narrow: no input, no GPU, no state
/// — those belong to per-phase contexts above the runtime.
pub struct RuntimeCx<'a> {
    app: &'a mut dyn PlatformApp,
    /// How many windows the driver created during this callback. The scheduler
    /// reads it back to keep its open-window count accurate.
    windows_created: u32,
    /// The first window created during this callback, if any. The scheduler
    /// reads it to target the launch redraw at the initial window.
    first_window: Option<WindowId>,
    /// Set when the driver made a reactive state write during this callback.
    /// The scheduler reads it back and folds a state-dirty redraw reason into
    /// its bookkeeping, so a write that happens outside a running frame (e.g.
    /// from an input handler) still schedules exactly one frame to flush it.
    state_dirty_requested: bool,
    /// Real time elapsed since the previous frame ran, sampled from the
    /// scheduler's [`crate::clock::FrameClock`] at the head of this frame. The
    /// driver reads it through [`frame_delta`](Self::frame_delta) to advance
    /// time-based animations by exactly the elapsed interval. The very first
    /// frame has no predecessor, so its delta is [`Duration::ZERO`].
    frame_delta: Duration,
    /// The wall-clock instant sampled at the head of this frame, from the
    /// scheduler's [`crate::clock::FrameClock`]. The driver reads it through
    /// [`frame_now`](Self::frame_now) to fire one-shot timers whose deadline the
    /// frame has crossed — using the *same* sampled instant that produced
    /// [`frame_delta`](Self::frame_delta), so timer expiry and animation advance
    /// agree on "now".
    frame_now: Instant,
    /// Events the driver posted, which the scheduler handles once this
    /// callback returns.
    posted: Vec<RawEvent>,
}

impl<'a> RuntimeCx<'a> {
    /// Wrap a live platform app, tagging the frame with the time elapsed since
    /// the previous frame (`delta`) and the instant sampled at its head (`now`).
    /// Called by the scheduler per callback; `delta` is [`Duration::ZERO`] on the
    /// first frame (no predecessor to diff against) and for callbacks that run no
    /// frame body (launch, input routing), while `now` is always the scheduler
    /// clock's reading for this callback — so a driver can fire timers against
    /// the same instant that produced `delta`.
    pub(crate) fn new(app: &'a mut dyn PlatformApp, delta: Duration, now: Instant) -> Self {
        Self {
            app,
            windows_created: 0,
            first_window: None,
            state_dirty_requested: false,
            frame_delta: delta,
            frame_now: now,
            posted: Vec::new(),
        }
    }

    /// The events posted through this context, in order.
    pub(crate) fn take_posted(&mut self) -> Vec<RawEvent> {
        std::mem::take(&mut self.posted)
    }

    /// Delivers `event` as the platform would: once this callback returns,
    /// the scheduler handles it as the next event, so input posted here —
    /// a scenario's taps and keys on a device — takes the route the user's
    /// does, scale normalization included.
    pub fn post_event(&mut self, event: RawEvent) {
        self.posted.push(event);
    }

    /// How many windows were created through this context so far.
    pub(crate) fn windows_created(&self) -> u32 {
        self.windows_created
    }

    /// The first window created through this context, if any.
    pub(crate) fn first_window(&self) -> Option<WindowId> {
        self.first_window
    }

    /// Whether the driver requested a state flush during this callback.
    pub(crate) fn state_dirty_requested(&self) -> bool {
        self.state_dirty_requested
    }

    /// Real time elapsed since the previous frame, for advancing time-based
    /// animation. [`Duration::ZERO`] on the first frame and on non-frame
    /// callbacks (launch, input), so a tick against it is a well-defined no-op.
    pub fn frame_delta(&self) -> Duration {
        self.frame_delta
    }

    /// The instant sampled at the head of this frame, from the scheduler clock.
    /// The driver fires one-shot timers against it (the deadline it compares to
    /// `now`), so timer expiry uses the same reading that produced
    /// [`frame_delta`](Self::frame_delta) — never a fresh `Instant::now()`,
    /// which would drift from the frame's notion of time and break the headless
    /// [`ManualClock`](crate::clock::ManualClock) determinism.
    pub fn frame_now(&self) -> Instant {
        self.frame_now
    }

    /// Create a native window; returns its stable id.
    pub fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        let id = self.app.create_window(config)?;
        self.windows_created += 1;
        if self.first_window.is_none() {
            self.first_window = Some(id);
        }
        Ok(id)
    }

    /// Ask the platform to schedule a redraw beat for `window`.
    pub fn request_redraw(&mut self, window: WindowId) {
        self.app.request_redraw(window);
    }

    /// Install (or replace) the application menu bar from a menu tree.
    ///
    /// Custom [`Menu::Item`](viso_platform::Menu::Item)s deliver their command
    /// to the driver's `on_menu_command` when picked; standard actions
    /// (Quit/Close/…) are performed by the OS. A no-op on backends without a
    /// native menu bar (headless).
    pub fn set_menu(&mut self, menu: &viso_platform::Menu) {
        self.app.set_menu(menu);
    }

    /// Show, update or (with `None`) remove the app's tray icon; see
    /// [`PlatformApp::set_tray`](viso_platform::PlatformApp::set_tray).
    pub fn set_tray(&mut self, tray: Option<&viso_platform::Tray>) {
        self.app.set_tray(tray);
    }

    /// Show or (with `None`) clear the badge on the app's icon; see
    /// [`PlatformApp::set_badge`](viso_platform::PlatformApp::set_badge).
    pub fn set_badge(&mut self, badge: Option<&viso_platform::AppBadge>) {
        self.app.set_badge(badge);
    }

    /// Declare `window`'s draggable self-drawn-caption regions (logical points,
    /// top-left origin). The platform starts a native window drag on a primary
    /// press inside one instead of routing a pointer event. A no-op on backends
    /// without self-drawn chrome. Cold path — called when the caption layout
    /// changes, not per frame.
    pub fn set_draggable_regions(&mut self, window: WindowId, regions: &[LogicalRect]) {
        self.app.set_draggable_regions(window, regions);
    }

    /// Publish a semantics delta for `window` to the OS bridge. A no-op on
    /// backends without one. Called only while an assistive technology
    /// listens.
    pub fn update_accessibility(&mut self, window: WindowId, delta: SemanticsDelta) {
        self.app.update_accessibility(window, delta);
    }

    /// Enter or leave fullscreen for `window`; the platform reports the
    /// transition as a `FullscreenChanged` event.
    pub fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        self.app.set_fullscreen(window, fullscreen);
    }

    /// Programmatically close `window`, destroying its OS shell.
    ///
    /// The counterpart to [`create_window`](Self::create_window). The platform
    /// delivers a [`WindowClosed`](viso_platform::RawEvent::WindowClosed) for
    /// `window`, so the scheduler decrements its open-window count and the driver
    /// tears the window's state down through the one close path — the same one a
    /// user-driven close takes. Closing an unknown id is a no-op.
    pub fn close_window(&mut self, window: WindowId) {
        self.app.close_window(window);
    }

    /// Signal that reactive state changed and a frame must run to flush it.
    ///
    /// Records a state-dirty request the scheduler folds into its redraw
    /// reasons, and schedules the redraw beat for `window` so the frame
    /// actually runs. Called by the facade the first time a transaction records
    /// a pending write; subsequent writes in the same transaction need no
    /// further signal (the frame flushes them all at once).
    pub fn request_state_flush(&mut self, window: WindowId) {
        self.state_dirty_requested = true;
        self.app.request_redraw(window);
    }

    /// The scale factor currently reported for `window`, if it exists.
    pub fn scale_factor(&self, window: WindowId) -> Option<f64> {
        self.app.window(window).map(|w| w.scale_factor())
    }

    /// The physical inner size currently reported for `window`, if it exists.
    pub fn inner_size(&self, window: WindowId) -> Option<(u32, u32)> {
        self.app.window(window).map(|w| w.inner_size())
    }

    /// Where `window`'s outer top-left corner is on the desktop, where the
    /// platform says (see
    /// [`Window::outer_position`](viso_platform::Window::outer_position)).
    pub fn window_position(&self, window: WindowId) -> Option<(f64, f64)> {
        self.app.window(window).and_then(|w| w.outer_position())
    }

    /// The OS-native handle for `window` (for GPU surface creation), if it
    /// exists. The facade passes this to [`viso_gpu::GpuBackend::create_surface`]
    /// on launch. The handle borrows the window; use it immediately.
    pub fn raw_handle(&self, window: WindowId) -> Option<RawWindowHandle> {
        self.app.window(window).map(|w| w.raw_handle())
    }

    /// The window's native chrome affordances box (the macOS traffic-light
    /// buttons' bounding rect, in logical points), read synchronously the instant
    /// the window exists — the fast counterpart to the later
    /// [`RawEvent::WindowChromeGeom`](viso_platform::RawEvent::WindowChromeGeom).
    /// `None` when the window is gone or the platform draws no native buttons over
    /// it. The facade queries this right after `create_window` so a self-drawn
    /// caption can decide at build time whether to yield to the OS overlay.
    pub fn window_chrome_geom(&self, window: WindowId) -> Option<LogicalRect> {
        self.app.window(window).and_then(|w| w.chrome_geom())
    }

    /// Replace what the system clipboard holds with `data`.
    pub fn set_clipboard(&mut self, data: viso_platform::ClipboardData) {
        self.app.set_clipboard(data);
    }

    /// Ask for what the clipboard holds of `kinds`; it arrives as one
    /// [`InputSample::Paste`](crate::InputSample::Paste) for `window`
    /// carrying `token`, empty when it holds none of them.
    pub fn request_paste(
        &mut self,
        window: WindowId,
        kinds: viso_platform::ClipboardKinds,
        token: u32,
    ) {
        self.app.request_paste(window, kinds, token);
    }

    /// Carry `drag` on from `window` as a drag the OS runs; whether it
    /// started. A started drag ends with an
    /// [`InputSample::DragEnded`](crate::InputSample::DragEnded).
    pub fn start_drag(&mut self, window: WindowId, drag: viso_platform::DragOut) -> bool {
        self.app.start_drag(window, drag)
    }

    /// Show `icon` while the mouse is over `window`'s content.
    pub fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        self.app.set_cursor(window, icon);
    }

    /// Lock the mouse's pointer to `window` for relative motion, or release
    /// it; the platform reports each change through the driver's
    /// [`on_pointer_lock`](crate::FrameDriver::on_pointer_lock).
    pub fn set_pointer_lock(&mut self, window: WindowId, lock: bool) {
        self.app.set_pointer_lock(window, lock);
    }

    /// Tell the input method about `window`'s focused text field (its caret,
    /// whether it takes a secret); `None` disables the IME for `window`.
    pub fn set_text_entry(&mut self, window: WindowId, entry: Option<TextEntry>) {
        self.app.set_text_entry(window, entry);
    }

    /// Show or hide the on-screen keyboard for `window`.
    pub fn show_soft_keyboard(&mut self, window: WindowId, show: bool) {
        self.app.show_soft_keyboard(window, show);
    }

    /// Show, move or (with `None`) close `window`'s edit menu.
    pub fn set_edit_menu(&mut self, window: WindowId, menu: Option<EditMenu>) {
        self.app.set_edit_menu(window, menu);
    }

    /// Pop up `window`'s context menu of edits, titled by `labels`.
    pub fn show_context_menu(&mut self, window: WindowId, menu: EditMenu, labels: EditLabels) {
        self.app.show_context_menu(window, menu, labels);
    }

    /// Show, move or (with `None`) put away `window`'s magnifier.
    pub fn set_loupe(&mut self, window: WindowId, loupe: Option<Loupe>) {
        self.app.set_loupe(window, loupe);
    }

    /// The current system appearance.
    pub fn appearance(&self) -> Appearance {
        self.app.appearance()
    }

    /// The user's locale now, a BCP 47 tag.
    pub fn locale(&self) -> String {
        self.app.locale()
    }

    /// The hinges, folds and cutouts crossing `window`'s content now.
    pub fn display_features(&self, window: WindowId) -> Vec<viso_platform::DisplayFeature> {
        self.app.display_features(window)
    }

    /// Whether windows are movable desktop windows that carry a caption; see
    /// [`PlatformApp::framed_windows`](viso_platform::PlatformApp::framed_windows).
    pub fn framed_windows(&self) -> bool {
        self.app.framed_windows()
    }

    /// What the backend offers beyond its one window; see
    /// [`PlatformApp::capabilities`](viso_platform::PlatformApp::capabilities).
    pub fn capabilities(&self) -> PlatformCaps {
        self.app.capabilities()
    }

    /// A handle that kicks this loop from any thread.
    pub fn loop_waker(&self) -> LoopWaker {
        self.app.loop_waker()
    }

    /// Shakes gamepad `id`; whether it can. See
    /// [`PlatformApp::rumble_gamepad`](viso_platform::PlatformApp::rumble_gamepad).
    pub fn rumble_gamepad(&mut self, id: GamepadId, rumble: Rumble) -> bool {
        self.app.rumble_gamepad(id, rumble)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_platform::backend::headless::HeadlessApp;

    #[test]
    fn first_window_records_the_first_created_window() {
        let mut app = HeadlessApp::scripted(vec![]);
        let mut cx = RuntimeCx::new(&mut app, Duration::ZERO, Instant::now());
        assert_eq!(cx.first_window(), None, "no window yet");

        let a = cx.create_window(WindowConfig::default()).unwrap();
        let b = cx.create_window(WindowConfig::default()).unwrap();
        assert_ne!(a, b, "each window gets a distinct id");
        assert_eq!(
            cx.first_window(),
            Some(a),
            "first_window stays the first created, not the latest"
        );
        assert_eq!(cx.windows_created(), 2);
    }

    #[test]
    fn frame_delta_is_the_value_the_scheduler_tagged() {
        let mut app = HeadlessApp::scripted(vec![]);
        let dt = Duration::from_millis(16);
        let now = Instant::now();
        let cx = RuntimeCx::new(&mut app, dt, now);
        assert_eq!(cx.frame_delta(), dt, "the reader returns the tagged delta");
        assert_eq!(cx.frame_now(), now, "the reader returns the tagged instant");

        let mut app = HeadlessApp::scripted(vec![]);
        let cx = RuntimeCx::new(&mut app, Duration::ZERO, now);
        assert_eq!(
            cx.frame_delta(),
            Duration::ZERO,
            "a zero delta (first frame / non-frame callback) reads back as zero"
        );
    }
}
