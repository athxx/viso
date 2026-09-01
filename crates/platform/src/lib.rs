//! `viso-platform` — OS abstraction layer (bottom of the dependency DAG).
//!
//! Responsibilities (the `viso-platform` boundary):
//! window/surface, raw pointer/keyboard/IME events, clipboard, cursor,
//! system appearance, lifecycle, app activation, native handles,
//! accessibility bridge hook.
//!
//! This crate MUST NOT depend on ui/widgets/dsl/studio, and MUST NOT pull in
//! script, network, video, or live-reload.
//!
//! Phase 1 status: the event vocabulary, loop-control types, and the
//! window/app traits are defined, with a deterministic headless backend and
//! native backends (macOS/Windows/X11) selected by target. The runtime calls
//! *up* through [`AppHandler`] (defined here, implemented above) so this stays
//! the DAG bottom.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod control;
pub mod event;
pub mod handler;

pub use control::{ControlFlow, DEFAULT_FRAME_BUDGET, PlatformError, WindowConfig, WindowId};
pub use event::{
    AcceptCell, KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawImePreedit, RawKey,
    RawPointer, RawScroll, RawText,
};
pub use handler::AppHandler;
// The native window handle lives in the `viso-handle` leaf crate so `viso-gpu`
// can name it without depending on `viso-platform` (the DAG rule). Re-exported here
// because platform is where it's produced (`Window::raw_handle`).
pub use viso_handle::RawWindowHandle;

/// Opaque handle to a platform GPU surface.
///
/// Distinct from [`WindowId`]: a window is an OS shell; a surface is the
/// drawable it hosts. In Phase 1 they map 1:1, but the split lets a window host
/// several surfaces later without churning event routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SurfaceId(pub u32);

/// A live platform application: owns the OS event pump and its windows.
///
/// Created via [`create_app`] (native, target-selected) or
/// [`create_headless_app`] (deterministic, always available). The runtime drives
/// it by calling [`PlatformApp::run`] with its [`AppHandler`].
pub trait PlatformApp {
    /// Create a native window; returns its stable [`WindowId`].
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError>;

    /// Run the OS event pump to completion, funneling every event through
    /// `handler` and obeying the [`ControlFlow`] it returns. Returns when the
    /// handler asks to [`ControlFlow::Exit`] (or the OS terminates the app).
    fn run(&mut self, handler: &mut dyn AppHandler);

    /// Borrow a window by id, if it exists.
    fn window(&self, id: WindowId) -> Option<&dyn Window>;

    /// Schedule a redraw beat for `window` (delivered as
    /// [`RawEvent::RedrawRequested`]).
    fn request_redraw(&mut self, window: WindowId);
}

/// A single native window / drawable shell.
pub trait Window {
    /// This window's stable id.
    fn id(&self) -> WindowId;

    /// Ask the OS to redraw this window at the next beat.
    fn request_redraw(&self);

    /// Set the title-bar text.
    fn set_title(&mut self, title: &str);

    /// The display scale factor (physical pixels per logical point).
    fn scale_factor(&self) -> f64;

    /// The content area size in physical pixels.
    fn inner_size(&self) -> (u32, u32);

    /// The OS-native handle for GPU surface creation (a native handle).
    ///
    /// `viso-gpu` uses this to attach a swapchain/drawable layer to the window.
    /// The returned handle borrows from `self` and must not outlive this window.
    fn raw_handle(&self) -> RawWindowHandle;
}

/// Create the native platform app for this target.
///
/// Returns [`PlatformError::NoBackend`] on targets with no compiled backend;
/// callers that can tolerate it (tests, CI) fall back to
/// [`create_headless_app`].
pub fn create_app() -> Result<Box<dyn PlatformApp>, PlatformError> {
    backend::create_native()
}

/// Create the deterministic headless app (no OS, always available).
pub fn create_headless_app() -> Box<dyn PlatformApp> {
    Box::new(backend::headless::HeadlessApp::new())
}
