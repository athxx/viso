//! Deterministic, dependency-free backend for tests, CI, and idle-cost benches.
//!
//! No OS calls: it drives a *scripted* queue of [`RawEvent`]s through the
//! handler, honoring the returned [`ControlFlow`] exactly as a real pump would.
//! Because there is no OS to block on, the only things that end the loop are a
//! returned [`ControlFlow::Exit`] or a fully drained queue (a real pump would
//! block forever on empty, which is useless in a test). This lets the frame
//! loop be exercised end-to-end without a display server.
//!
//! It synthesizes the [`RawEvent::AppLaunched`] boot event, and on
//! `request_redraw` enqueues a [`RawEvent::RedrawRequested`] beat — so a driver
//! that keeps asking to redraw runs frames just like it would under a real
//! display link.

use std::collections::VecDeque;

use crate::RawWindowHandle;
use crate::control::{LogicalRect, PlatformError, WindowConfig, WindowId};
use crate::event::{Appearance, CursorIcon, RawEvent};
use crate::handler::AppHandler;
use crate::{ControlFlow, PlatformApp, Window};

/// A scriptable, headless [`PlatformApp`].
pub struct HeadlessApp {
    next_window_id: u32,
    windows: Vec<HeadlessWindow>,
    /// A fixed script delivered after `AppLaunched`. Empty for the real facade.
    script: VecDeque<RawEvent>,
    /// Redraw requests raised during handling, delivered as beats.
    pending_redraws: VecDeque<WindowId>,
    /// The in-process clipboard: what `set_clipboard_text` stored and
    /// `request_paste` hands back.
    clipboard: Option<String>,
    appearance: Appearance,
}

impl HeadlessApp {
    /// A headless app with no scripted events (ends on the first empty poll).
    pub fn new() -> Self {
        Self {
            next_window_id: 1,
            windows: Vec::new(),
            script: VecDeque::new(),
            pending_redraws: VecDeque::new(),
            clipboard: None,
            appearance: Appearance::default(),
        }
    }

    /// A headless app that will replay `script` after `AppLaunched`.
    ///
    /// The test vehicle: feed a list of events and assert on the frames they
    /// drive.
    pub fn scripted(script: impl IntoIterator<Item = RawEvent>) -> Self {
        let mut app = Self::new();
        app.script = script.into_iter().collect();
        app
    }

    /// Set the appearance [`PlatformApp::appearance`] reports. Scripts that
    /// test a change also enqueue the matching [`RawEvent::AppearanceChanged`].
    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = appearance;
    }

    /// The clipboard's current text.
    pub fn clipboard(&self) -> Option<&str> {
        self.clipboard.as_deref()
    }

    /// The cursor last set on `window`.
    pub fn cursor(&self, window: WindowId) -> Option<CursorIcon> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .map(|w| w.cursor)
    }

    /// The caret rect last handed to the IME for `window`.
    pub fn ime_area(&self, window: WindowId) -> Option<LogicalRect> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .and_then(|w| w.ime_area)
    }

    /// Whether the soft keyboard is currently requested for `window`.
    pub fn soft_keyboard_shown(&self, window: WindowId) -> bool {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .is_some_and(|w| w.soft_keyboard)
    }

    fn window_mut(&mut self, id: WindowId) -> Option<&mut HeadlessWindow> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn next_event(&mut self) -> Option<RawEvent> {
        if let Some(w) = self.pending_redraws.pop_front() {
            return Some(RawEvent::RedrawRequested { window: w });
        }
        self.script.pop_front()
    }
}

impl Default for HeadlessApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformApp for HeadlessApp {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;
        let (w, h) = config.logical_size;
        self.windows.push(HeadlessWindow {
            id,
            title: config.title,
            scale: 1.0,
            physical_size: (w as u32, h as u32),
            cursor: CursorIcon::Default,
            ime_area: None,
            soft_keyboard: false,
        });
        Ok(id)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        // Boot: launch first, exactly once, before anything else.
        if handler.handle(RawEvent::AppLaunched) == ControlFlow::Exit {
            return;
        }
        while let Some(event) = self.next_event() {
            if handler.handle(event) == ControlFlow::Exit {
                break;
            }
            // Poll/Wait/WaitUntil all continue draining: there is no OS to
            // block on. `request_redraw` refills the queue via pending_redraws.
            // A `WaitUntil(deadline)` carries no meaning here — headless has no
            // clock to sleep against — so a test that exercises a one-shot timer
            // crosses the deadline itself: it advances the scheduler's
            // `ManualClock` past the deadline and enqueues a `Wakeup`/redraw,
            // which runs a frame whose `fire_due` sees the elapsed deadline.
        }
    }

    fn window(&self, id: WindowId) -> Option<&dyn Window> {
        self.windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| w as &dyn Window)
    }

    fn request_redraw(&mut self, id: WindowId) {
        self.pending_redraws.push_back(id);
    }

    fn set_menu(&mut self, _menu: &crate::menu::Menu) {
        // No OS menu bar in headless: the app menu is a native-shell concept.
        // Kept as a no-op for interface parity so drivers install a menu
        // unconditionally without a target-specific branch.
    }

    fn close_window(&mut self, id: WindowId) {
        // Same path as a user-driven close: drop the OS-side window state and
        // deliver a `WindowClosed` beat. Front of the script queue so it lands
        // before any subsequent scripted events, exactly as a real backend would
        // report the destruction it just performed. Closing an unknown id is a
        // no-op (no window removed, no event synthesized), matching native
        // backends that ignore stale handles.
        let existed = self.windows.iter().any(|w| w.id == id);
        self.windows.retain(|w| w.id != id);
        if existed {
            self.script
                .push_front(RawEvent::WindowClosed { window: id });
        }
    }

    fn set_clipboard_text(&mut self, text: &str) {
        self.clipboard = Some(text.to_string());
    }

    fn request_paste(&mut self, window: WindowId) {
        // Delivered next, ahead of the rest of the script: the answer to a
        // request made while handling the current event.
        if let Some(text) = self.clipboard.clone() {
            self.script.push_front(RawEvent::Paste { window, text });
        }
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        if let Some(w) = self.window_mut(window) {
            w.cursor = icon;
        }
    }

    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>) {
        if let Some(w) = self.window_mut(window) {
            w.ime_area = caret;
        }
    }

    fn show_soft_keyboard(&mut self, window: WindowId, show: bool) {
        if let Some(w) = self.window_mut(window) {
            w.soft_keyboard = show;
        }
    }

    fn appearance(&self) -> Appearance {
        self.appearance
    }
}

/// A headless window: pure state, no OS resource.
pub struct HeadlessWindow {
    id: WindowId,
    title: String,
    scale: f64,
    physical_size: (u32, u32),
    cursor: CursorIcon,
    ime_area: Option<LogicalRect>,
    soft_keyboard: bool,
}

impl Window for HeadlessWindow {
    fn id(&self) -> WindowId {
        self.id
    }

    fn request_redraw(&self) {
        // Per-window request is a no-op in headless; the app-level
        // `PlatformApp::request_redraw` refills the beat queue. Kept for
        // interface parity with native windows.
    }

    fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }

    fn scale_factor(&self) -> f64 {
        self.scale
    }

    fn inner_size(&self) -> (u32, u32) {
        self.physical_size
    }

    fn raw_handle(&self) -> RawWindowHandle {
        // No OS surface — the GPU layer routes this to its software rasterizer.
        RawWindowHandle::Headless
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::AppHandler;

    struct Recorder(Vec<RawEvent>);

    impl AppHandler for Recorder {
        fn handle(&mut self, event: RawEvent) -> ControlFlow {
            self.0.push(event);
            ControlFlow::Wait
        }
    }

    #[test]
    fn clipboard_round_trips_through_request_paste() {
        let mut app = HeadlessApp::new();
        let id = app.create_window(WindowConfig::default()).unwrap();
        app.request_paste(id);
        let mut rec = Recorder(Vec::new());
        app.run(&mut rec);
        assert_eq!(
            rec.0,
            vec![RawEvent::AppLaunched],
            "empty clipboard answers nothing"
        );

        app.set_clipboard_text("héllo");
        assert_eq!(app.clipboard(), Some("héllo"));
        app.request_paste(id);
        let mut rec = Recorder(Vec::new());
        app.run(&mut rec);
        assert_eq!(
            rec.0,
            vec![
                RawEvent::AppLaunched,
                RawEvent::Paste {
                    window: id,
                    text: "héllo".into()
                }
            ]
        );
    }

    #[test]
    fn cursor_ime_and_keyboard_state_is_per_window() {
        let mut app = HeadlessApp::new();
        let a = app.create_window(WindowConfig::default()).unwrap();
        let b = app.create_window(WindowConfig::default()).unwrap();
        app.set_cursor(a, CursorIcon::Text);
        app.set_ime_area(a, Some(LogicalRect::new(10.0, 20.0, 1.0, 16.0)));
        app.show_soft_keyboard(a, true);
        assert_eq!(app.cursor(a), Some(CursorIcon::Text));
        assert_eq!(app.cursor(b), Some(CursorIcon::Default));
        assert_eq!(
            app.ime_area(a),
            Some(LogicalRect::new(10.0, 20.0, 1.0, 16.0))
        );
        assert_eq!(app.ime_area(b), None);
        assert!(app.soft_keyboard_shown(a));
        assert!(!app.soft_keyboard_shown(b));
        app.set_ime_area(a, None);
        assert_eq!(app.ime_area(a), None);
        // Unknown windows are ignored.
        app.set_cursor(WindowId(99), CursorIcon::Wait);
        assert_eq!(app.cursor(WindowId(99)), None);
    }

    #[test]
    fn appearance_is_scriptable() {
        use crate::event::ColorScheme;
        let mut app = HeadlessApp::new();
        assert_eq!(app.appearance(), Appearance::default());
        let dark = Appearance {
            color_scheme: ColorScheme::Dark,
            high_contrast: true,
            reduce_motion: false,
        };
        app.set_appearance(dark);
        assert_eq!(app.appearance(), dark);
    }

    #[test]
    fn set_draggable_regions_is_a_no_op_on_headless() {
        // Headless has no self-drawn chrome, so it takes the trait's default
        // no-op: the call must neither panic nor disturb window state. A driver
        // pushes caption regions unconditionally, so this keeps that path safe on
        // a backend that draws no caption.
        let mut app = HeadlessApp::new();
        let id = app.create_window(WindowConfig::default()).unwrap();
        let before = app.window(id).map(|w| w.inner_size());
        app.set_draggable_regions(id, &[LogicalRect::new(0.0, 0.0, 400.0, 28.0)]);
        // An unknown window id is equally inert.
        app.set_draggable_regions(WindowId(999), &[]);
        let after = app.window(id).map(|w| w.inner_size());
        assert_eq!(before, after, "the no-op left the window untouched");
    }
}
