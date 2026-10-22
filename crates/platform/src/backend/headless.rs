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

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::RawWindowHandle;
use crate::control::{
    EditLabels, EditMenu, Loupe, PlatformCaps, PlatformError, TextEntry, WindowConfig, WindowId,
};
use crate::event::{Appearance, CursorIcon, DisplayFeature, RawEvent};
use crate::handler::AppHandler;
use crate::{ControlFlow, LoopWaker, PlatformApp, Window};

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
    locale: String,
    /// What [`PlatformApp::display_features`] reports for every window.
    display_features: Vec<DisplayFeature>,
    framed_windows: bool,
    /// What [`PlatformApp::capabilities`] reports.
    caps: PlatformCaps,
    /// Set by a [`LoopWaker`] kick from any thread; delivered as one
    /// [`RawEvent::Wakeup`] ahead of the script.
    woken: Arc<AtomicBool>,
    /// The beats left when the run is bounded ([`HeadlessApp::frame_budget`]).
    frames_left: Option<u32>,
    /// Every window's logical size and scale, whatever it asks for
    /// ([`HeadlessApp::fixed_surface`]).
    surface: Option<((f64, f64), f64)>,
    /// Events handed in while the app runs ([`HeadlessApp::feed`]).
    feed: Option<HeadlessFeed>,
}

/// Where a driver hands a running [`HeadlessApp`] input, as the user's
/// device would: each event is delivered ahead of the next frame beat.
#[derive(Clone, Default)]
pub struct HeadlessFeed(Rc<RefCell<VecDeque<RawEvent>>>);

impl HeadlessFeed {
    /// Queues `event`.
    pub fn push(&self, event: RawEvent) {
        self.0.borrow_mut().push_back(event);
    }

    fn pop(&self) -> Option<RawEvent> {
        self.0.borrow_mut().pop_front()
    }
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
            locale: "und".to_owned(),
            display_features: Vec::new(),
            framed_windows: true,
            // A desktop that also records the edit menu and the magnifier it
            // is asked to show, beside the context menu.
            caps: PlatformCaps {
                edit_menu: true,
                loupe: true,
                ..PlatformCaps::DESKTOP
            },
            woken: Arc::new(AtomicBool::new(false)),
            frames_left: None,
            surface: None,
            feed: None,
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

    /// Bounds the run to `frames` redraw beats, for an app that never stops
    /// asking for frames (a running game): each scripted event is delivered
    /// before the beats queued behind it, so input lands between frames, and
    /// redraws requested once the budget is spent are dropped.
    pub fn frame_budget(&mut self, frames: u32) {
        self.frames_left = Some(frames);
    }

    /// Gives every window `width × height` logical pixels at `scale` device
    /// pixels each, whatever size it asks for: a run that measures the same
    /// on every machine.
    pub fn fixed_surface(&mut self, width: u32, height: u32, scale: f64) {
        self.surface = Some(((f64::from(width), f64::from(height)), scale));
    }

    /// The feed events reach this app through while it runs.
    pub fn feed(&mut self) -> HeadlessFeed {
        self.feed.get_or_insert_with(HeadlessFeed::default).clone()
    }

    /// Set the appearance [`PlatformApp::appearance`] reports. Scripts that
    /// test a change also enqueue the matching [`RawEvent::AppearanceChanged`].
    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance = appearance;
    }

    /// Set the locale [`PlatformApp::locale`] reports. Scripts that test a
    /// change also enqueue the matching [`RawEvent::LocaleChanged`].
    pub fn set_locale(&mut self, locale: &str) {
        self.locale = locale.to_owned();
    }

    /// Set the display features [`PlatformApp::display_features`] reports
    /// for every window. Scripts that test a change also enqueue the
    /// matching [`RawEvent::DisplayFeaturesChanged`].
    pub fn set_display_features(&mut self, features: Vec<DisplayFeature>) {
        self.display_features = features;
    }

    /// Set what [`PlatformApp::framed_windows`] reports, to stand in for a
    /// full-screen host (mobile, a browser tab).
    pub fn set_framed_windows(&mut self, framed: bool) {
        self.framed_windows = framed;
    }

    /// Set what [`PlatformApp::capabilities`] reports, to stand in for a
    /// host without a system edit or context menu.
    pub fn set_capabilities(&mut self, caps: PlatformCaps) {
        self.caps = caps;
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

    /// Whether `window` is fullscreen.
    pub fn is_fullscreen(&self, window: WindowId) -> Option<bool> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .map(|w| w.fullscreen)
    }

    /// The text field last handed to the IME for `window`.
    pub fn text_entry(&self, window: WindowId) -> Option<TextEntry> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .and_then(|w| w.text_entry)
    }

    /// The edit menu last shown for `window`, while it shows.
    pub fn edit_menu(&self, window: WindowId) -> Option<EditMenu> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .and_then(|w| w.edit_menu)
    }

    /// The context menu last popped up for `window` and its titles; a
    /// headless menu stays up until the script closes it.
    pub fn context_menu(&self, window: WindowId) -> Option<(EditMenu, EditLabels)> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .and_then(|w| w.context_menu)
    }

    /// The magnifier last shown for `window`, while it shows.
    pub fn loupe(&self, window: WindowId) -> Option<Loupe> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .and_then(|w| w.loupe)
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
        if let Some(event) = self.feed.as_ref().and_then(HeadlessFeed::pop) {
            return Some(event);
        }
        if let Some(left) = &mut self.frames_left {
            if let Some(event) = self.script.pop_front() {
                return Some(event);
            }
            if *left == 0 {
                self.pending_redraws.clear();
            } else if let Some(w) = self.pending_redraws.pop_front() {
                *left -= 1;
                return Some(RawEvent::RedrawRequested { window: w });
            }
            if self.woken.swap(false, Ordering::AcqRel) {
                return Some(RawEvent::Wakeup);
            }
            return None;
        }
        if let Some(w) = self.pending_redraws.pop_front() {
            return Some(RawEvent::RedrawRequested { window: w });
        }
        if self.woken.swap(false, Ordering::AcqRel) {
            return Some(RawEvent::Wakeup);
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
        let ((w, h), scale) = self.surface.unwrap_or((config.logical_size, 1.0));
        self.windows.push(HeadlessWindow {
            id,
            title: config.title,
            scale,
            physical_size: ((w * scale).round() as u32, (h * scale).round() as u32),
            cursor: CursorIcon::Default,
            text_entry: None,
            edit_menu: None,
            context_menu: None,
            loupe: None,
            soft_keyboard: false,
            fullscreen: false,
        });
        Ok(id)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        // Boot: launch first, exactly once, before anything else.
        if handler.handle(self, RawEvent::AppLaunched) == ControlFlow::Exit {
            return;
        }
        while let Some(event) = self.next_event() {
            // A scripted close is the context menu's own closing.
            if let RawEvent::EditMenu {
                window,
                action: crate::EditMenuAction::Closed,
            } = event
                && let Some(w) = self.window_mut(window)
            {
                w.context_menu = None;
            }
            if handler.handle(self, event) == ControlFlow::Exit {
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
        // A closed window draws no more frames, as on a native backend: a
        // frame client that asks for one each frame must not keep its
        // `WindowClosed` from being delivered.
        if self.windows.iter().any(|w| w.id == id) {
            self.pending_redraws.push_back(id);
        }
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
        self.pending_redraws.retain(|w| *w != id);
        if existed {
            self.script
                .push_front(RawEvent::WindowClosed { window: id });
        }
    }

    fn set_clipboard_text(&mut self, text: &str) {
        self.clipboard = Some(text.to_string());
    }

    fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        // Reported next, like a real backend's transition, and only when the
        // state changes.
        if let Some(w) = self.window_mut(window)
            && w.fullscreen != fullscreen
        {
            w.fullscreen = fullscreen;
            self.script
                .push_front(RawEvent::FullscreenChanged { window, fullscreen });
        }
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

    fn set_text_entry(&mut self, window: WindowId, entry: Option<TextEntry>) {
        if let Some(w) = self.window_mut(window) {
            w.text_entry = entry;
        }
    }

    fn show_soft_keyboard(&mut self, window: WindowId, show: bool) {
        if let Some(w) = self.window_mut(window) {
            w.soft_keyboard = show;
        }
    }

    fn set_edit_menu(&mut self, window: WindowId, menu: Option<EditMenu>) {
        if let Some(w) = self.window_mut(window) {
            w.edit_menu = menu;
        }
    }

    fn show_context_menu(&mut self, window: WindowId, menu: EditMenu, labels: EditLabels) {
        if let Some(w) = self.window_mut(window) {
            w.context_menu = Some((menu, labels));
        }
    }

    fn set_loupe(&mut self, window: WindowId, loupe: Option<Loupe>) {
        if let Some(w) = self.window_mut(window) {
            w.loupe = loupe;
        }
    }

    fn capabilities(&self) -> PlatformCaps {
        self.caps
    }

    fn appearance(&self) -> Appearance {
        self.appearance
    }

    fn locale(&self) -> String {
        self.locale.clone()
    }

    fn display_features(&self, window: WindowId) -> Vec<DisplayFeature> {
        if self.windows.iter().any(|w| w.id == window) {
            self.display_features.clone()
        } else {
            Vec::new()
        }
    }

    fn framed_windows(&self) -> bool {
        self.framed_windows
    }

    fn loop_waker(&self) -> LoopWaker {
        let woken = self.woken.clone();
        LoopWaker::new(move || woken.store(true, Ordering::Release))
    }
}

/// A headless window: pure state, no OS resource.
pub struct HeadlessWindow {
    id: WindowId,
    title: String,
    scale: f64,
    physical_size: (u32, u32),
    cursor: CursorIcon,
    text_entry: Option<TextEntry>,
    edit_menu: Option<EditMenu>,
    context_menu: Option<(EditMenu, EditLabels)>,
    loupe: Option<Loupe>,
    soft_keyboard: bool,
    fullscreen: bool,
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
    use crate::control::LogicalRect;
    use crate::handler::AppHandler;

    struct Recorder(Vec<RawEvent>);

    impl AppHandler for Recorder {
        fn handle(&mut self, _app: &mut dyn PlatformApp, event: RawEvent) -> ControlFlow {
            self.0.push(event);
            ControlFlow::Wait
        }
    }

    #[test]
    fn fed_input_lands_ahead_of_the_next_beat() {
        struct Feeder {
            feed: HeadlessFeed,
            seen: Vec<RawEvent>,
            window: WindowId,
            fed: bool,
        }
        impl AppHandler for Feeder {
            fn handle(&mut self, _app: &mut dyn PlatformApp, event: RawEvent) -> ControlFlow {
                if let RawEvent::RedrawRequested { .. } = event
                    && !self.fed
                {
                    self.fed = true;
                    self.feed.push(RawEvent::Text(crate::event::RawText {
                        window: self.window,
                        text: "a".into(),
                    }));
                }
                self.seen.push(event);
                ControlFlow::Wait
            }
        }
        let mut app = HeadlessApp::new();
        let window = app.create_window(WindowConfig::default()).unwrap();
        app.request_redraw(window);
        let mut feeder = Feeder {
            feed: app.feed(),
            seen: Vec::new(),
            window,
            fed: false,
        };
        app.run(&mut feeder);
        assert!(matches!(
            feeder.seen[..],
            [
                RawEvent::AppLaunched,
                RawEvent::RedrawRequested { .. },
                RawEvent::Text(_)
            ]
        ));
    }

    #[test]
    fn a_closed_window_takes_no_more_redraws() {
        let mut app = HeadlessApp::new();
        let window = app.create_window(WindowConfig::default()).unwrap();
        app.request_redraw(window);
        app.close_window(window);
        app.request_redraw(window);
        let mut recorder = Recorder(Vec::new());
        app.run(&mut recorder);
        assert!(matches!(
            recorder.0[..],
            [RawEvent::AppLaunched, RawEvent::WindowClosed { .. }]
        ));
    }

    #[test]
    fn a_fixed_surface_sizes_every_window() {
        let mut app = HeadlessApp::new();
        app.fixed_surface(1280, 720, 1.5);
        let id = app.create_window(WindowConfig::default()).unwrap();
        let window = app.window(id).unwrap();
        assert_eq!(window.inner_size(), (1920, 1080));
        assert_eq!(window.scale_factor(), 1.5);

        let mut app = HeadlessApp::new();
        let id = app.create_window(WindowConfig::default()).unwrap();
        assert_eq!(app.window(id).unwrap().inner_size(), (800, 600));
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
        let entry = TextEntry {
            caret: LogicalRect::new(10.0, 20.0, 1.0, 16.0),
            secure: true,
            kind: crate::InputKind::Text,
            multiline: false,
        };
        app.set_text_entry(a, Some(entry));
        app.show_soft_keyboard(a, true);
        assert_eq!(app.cursor(a), Some(CursorIcon::Text));
        assert_eq!(app.cursor(b), Some(CursorIcon::Default));
        assert_eq!(app.text_entry(a), Some(entry));
        assert_eq!(app.text_entry(b), None);
        assert!(app.soft_keyboard_shown(a));
        assert!(!app.soft_keyboard_shown(b));
        app.set_text_entry(a, None);
        assert_eq!(app.text_entry(a), None);
        // Unknown windows are ignored.
        app.set_cursor(WindowId(99), CursorIcon::Wait);
        assert_eq!(app.cursor(WindowId(99)), None);
    }

    #[test]
    fn a_wake_from_another_thread_delivers_one_wakeup() {
        let mut app = HeadlessApp::scripted([RawEvent::Resumed]);
        let waker = app.loop_waker();
        std::thread::spawn(move || {
            waker.wake();
            waker.wake();
        })
        .join()
        .unwrap();
        let mut rec = Recorder(Vec::new());
        app.run(&mut rec);
        assert_eq!(
            rec.0,
            vec![RawEvent::AppLaunched, RawEvent::Wakeup, RawEvent::Resumed]
        );
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
            text_scale: 1.5,
        };
        app.set_appearance(dark);
        assert_eq!(app.appearance(), dark);
    }

    #[test]
    fn locale_and_display_features_are_scriptable() {
        use crate::control::LogicalRect;
        use crate::event::DisplayFeatureKind;
        let mut app = HeadlessApp::new();
        assert_eq!(app.locale(), "und");
        app.set_locale("he-IL");
        assert_eq!(app.locale(), "he-IL");
        let window = app.create_window(WindowConfig::default()).unwrap();
        assert!(app.display_features(window).is_empty());
        let fold = DisplayFeature {
            kind: DisplayFeatureKind::Fold,
            bounds: LogicalRect::new(0.0, 400.0, 600.0, 0.0),
        };
        app.set_display_features(vec![fold]);
        assert_eq!(app.display_features(window), [fold]);
        assert!(app.display_features(WindowId(window.0 + 1)).is_empty());
    }

    #[test]
    fn set_fullscreen_reports_each_change_once() {
        let mut app = HeadlessApp::new();
        let id = app.create_window(WindowConfig::default()).unwrap();
        app.set_fullscreen(id, true);
        app.set_fullscreen(id, true);
        assert_eq!(app.is_fullscreen(id), Some(true));
        app.set_fullscreen(id, false);
        app.set_fullscreen(WindowId(999), true);
        let reported = app
            .script
            .iter()
            .filter_map(|e| match e {
                RawEvent::FullscreenChanged { window, fullscreen } => Some((*window, *fullscreen)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(reported, [(id, false), (id, true)]);
        assert_eq!(app.is_fullscreen(id), Some(false));
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
