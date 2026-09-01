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
use crate::control::{PlatformError, WindowConfig, WindowId};
use crate::event::RawEvent;
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
}

impl HeadlessApp {
    /// A headless app with no scripted events (ends on the first empty poll).
    pub fn new() -> Self {
        Self {
            next_window_id: 1,
            windows: Vec::new(),
            script: VecDeque::new(),
            pending_redraws: VecDeque::new(),
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
}

/// A headless window: pure state, no OS resource.
pub struct HeadlessWindow {
    id: WindowId,
    title: String,
    scale: f64,
    physical_size: (u32, u32),
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
