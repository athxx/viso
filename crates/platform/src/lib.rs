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
//! At most one native backend is compiled per target, next to a deterministic
//! headless backend that is always available; a target without a native
//! backend reports [`PlatformError::NoBackend`]. The runtime calls *up* through [`AppHandler`] (defined
//! here, implemented above) so this stays the DAG bottom.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod control;
pub mod event;
pub mod handler;
pub mod menu;

pub use control::{
    ControlFlow, DEFAULT_FRAME_BUDGET, LogicalRect, PlatformError, WindowChrome, WindowConfig,
    WindowId,
};
pub use event::{
    AcceptCell, Appearance, ClipboardReply, ClipboardShortcut, ColorScheme, CursorIcon, Insets,
    KeyCode, Modifiers, PointerButtons, PointerId, PointerKind, PointerPhase, RawEvent,
    RawImePreedit, RawKey, RawPointer, RawScroll, RawText, clipboard_shortcut,
};
pub use handler::AppHandler;
pub use menu::{Accel, Menu, MenuCommandId, SystemAction};
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
    /// On a target whose event loop cannot block (Web) it installs its
    /// callbacks and returns at once; the caller then keeps `handler` alive for
    /// the rest of the process.
    fn run(&mut self, handler: &mut dyn AppHandler);

    /// Borrow a window by id, if it exists.
    fn window(&self, id: WindowId) -> Option<&dyn Window>;

    /// Schedule a redraw beat for `window` (delivered as
    /// [`RawEvent::RedrawRequested`]).
    fn request_redraw(&mut self, window: WindowId);

    /// Install (or replace) the application menu bar from a [`Menu`] tree.
    ///
    /// Cold path: called once at startup and again only when the menu changes.
    /// The backend walks the tree to build the OS-native menu; custom
    /// [`Menu::Item`]s deliver [`RawEvent::MenuCommand`] when picked, while
    /// [`Menu::System`] items route through the OS responder chain. Passing a
    /// non-[`Menu::Main`] root, or calling on a backend with no menu concept
    /// (headless), is a no-op.
    fn set_menu(&mut self, menu: &Menu);

    /// Declare which regions of `window`'s self-drawn caption are draggable, in
    /// logical points (top-left origin). The backend caches the slice and, on a
    /// primary press inside one of the regions, starts a native window drag
    /// instead of routing the press into the app as a pointer event.
    ///
    /// This is the one narrow channel the app uses to reclaim the top strip that
    /// [`WindowChrome::SelfDrawn`](crate::WindowChrome::SelfDrawn) hands over:
    /// the app knows where its caption is (the OS no longer does), so it tells
    /// the backend which parts of it behave like a title bar. The regions are
    /// small compact rects (§29), copied into the window's cache — no callback,
    /// no UI type crosses the boundary (§3.5). Replaces any prior set; an empty
    /// slice clears them. A no-op on backends without self-drawn chrome
    /// (headless/native-only), which is why this carries a default body.
    fn set_draggable_regions(&mut self, window: WindowId, regions: &[LogicalRect]) {
        let _ = (window, regions);
    }

    /// Programmatically close `window`, destroying its OS shell.
    ///
    /// The counterpart to [`create_window`](Self::create_window): it lets the
    /// app tear down a window it opened without waiting for the user to click
    /// the close button. Closing follows the *same* path as a user-driven
    /// close — the backend delivers a [`RawEvent::WindowClosed`] for `window`,
    /// so the runtime decrements its open-window count and the driver tears
    /// down that window's state through one code path, whichever side initiated
    /// the close. Closing an id that does not exist is a no-op.
    fn close_window(&mut self, window: WindowId);

    /// Put `text` on the system clipboard (plain UTF-8 text).
    ///
    /// For copies the app starts itself ("Copy link"); copies the OS starts
    /// arrive as [`RawEvent::CopyRequested`] instead.
    fn set_clipboard_text(&mut self, text: &str);

    /// Ask for the clipboard's text. It arrives as a [`RawEvent::Paste`] for
    /// `window` — immediately on systems with a synchronous clipboard, after the
    /// permission prompt on the web. Nothing arrives when the clipboard holds no
    /// text or access is refused.
    fn request_paste(&mut self, window: WindowId);

    /// Show `icon` while the mouse is over `window`'s content.
    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon);

    /// Tell the input method where the text caret is, so candidate windows and
    /// the soft keyboard's accessory views sit next to it. `None` means no text
    /// field has focus: the IME is disabled for the window.
    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>);

    /// Show or hide the on-screen keyboard. A no-op where there is none.
    fn show_soft_keyboard(&mut self, window: WindowId, show: bool);

    /// The current system appearance. Changes arrive as
    /// [`RawEvent::AppearanceChanged`].
    fn appearance(&self) -> Appearance;
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

    /// The window's current native chrome affordances box — the macOS
    /// traffic-light buttons' bounding rect, in logical points (top-left origin,
    /// relative to the content area) — or `None` when the platform draws no
    /// native buttons over this window (its OS/chrome has none, or it is
    /// `Native`-chromed).
    ///
    /// This is the *synchronous* counterpart to the later
    /// [`RawEvent::WindowChromeGeom`](crate::RawEvent::WindowChromeGeom): the same
    /// fact, readable the instant the window exists rather than delivered on a
    /// following frame. The facade queries it right after `create_window` and
    /// seeds it into the window's build so a self-drawn caption decides *at build
    /// time* whether to draw its own window buttons or yield to the OS overlay —
    /// the single build never has to wait for the event. The event still fires
    /// afterward to refine the box on resize/scale. `Some`/`None` here carries
    /// the presence of native buttons (§24 data contract, not `target_os`); the
    /// default is `None` for backends with no self-drawn chrome (headless).
    fn chrome_geom(&self) -> Option<LogicalRect> {
        None
    }
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
