//! `viso-platform` — OS abstraction layer (bottom of the dependency DAG).
//!
//! Responsibilities (the `viso-platform` boundary):
//! window/surface, raw pointer/keyboard/IME events, clipboard, cursor,
//! system appearance, lifecycle, app activation, native handles,
//! accessibility bridge hook, audio output, standard output.
//!
//! This crate MUST NOT depend on ui/widgets/dsl/studio, and MUST NOT pull in
//! script, network, video, or live-reload.
//!
//! At most one native backend is compiled per target, next to a deterministic
//! headless backend that is always available; a target without a native
//! backend reports [`PlatformError::NoBackend`]. The runtime calls *up* through [`AppHandler`] (defined
//! here, implemented above) so this stays the DAG bottom.

#![forbid(unsafe_op_in_unsafe_fn)]

#[cfg(any(
    target_os = "ios",
    all(target_arch = "wasm32", target_os = "unknown"),
    test
))]
mod access_mirror;
pub mod accessibility;
pub mod audio;
pub mod backend;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub mod browser;
pub mod clipboard;
pub mod control;
pub mod cursor;
pub mod drop;
pub mod event;
pub mod gamepad;
pub mod handler;
mod launch;
mod locale;
pub mod menu;
#[cfg(any(
    all(
        feature = "kms",
        any(
            target_os = "linux",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "netbsd",
            target_os = "dragonfly"
        )
    ),
    all(feature = "emulate", target_os = "macos"),
    test
))]
mod panel;
pub mod stdio;
pub mod time;
pub mod wake;

pub use accessibility::{
    AccessAction, AccessActions, AccessNode, AccessRequest, SemanticsDelta, SemanticsId,
    SemanticsLive, SemanticsRect, SemanticsRole, SemanticsSort, SemanticsState,
};
pub use clipboard::{ClipboardData, ClipboardImage, ClipboardKinds, ClipboardReply, PasteReply};
pub use control::{
    ControlFlow, DEFAULT_FRAME_BUDGET, EditLabels, EditMenu, EditMenuAction, InputKind,
    LogicalRect, Loupe, PlatformCaps, PlatformError, TextEntry, WindowChrome, WindowConfig,
    WindowId,
};
pub use cursor::{CursorError, CursorImage, CustomCursor, MAX_CURSOR_SIDE, ScaledCursor};
pub use drop::{
    DragOut, DropAction, DropActions, DropKinds, DropOffer, DropPhase, DropReply, RawDrop,
    file_path, file_uri, uri_list,
};
pub use event::{
    AcceptCell, Appearance, ClipboardShortcut, ColorScheme, CursorIcon, DisplayFeature,
    DisplayFeatureKind, GesturePhase, KeyCode, LogicalInsets, Modifiers, PointerButtons, PointerId,
    PointerKind, PointerPhase, RawEvent, RawGesture, RawImePreedit, RawKey, RawPointer, RawScroll,
    RawText, ScrollPhase, clipboard_shortcut, primary_is_logo,
};
pub use gamepad::{GamepadButton, GamepadEvent, GamepadId, GamepadInfo, GamepadState, Rumble};
pub use handler::AppHandler;
pub use launch::launch_var;
pub use menu::{Accel, Menu, MenuCommandId, SystemAction};
pub use time::Instant;
pub use wake::LoopWaker;
// The native window handle lives in the `viso-handle` leaf crate so `viso-gpu`
// can name it without depending on `viso-platform` (the DAG rule). Re-exported here
// because platform is where it's produced (`Window::raw_handle`).
pub use viso_handle::RawWindowHandle;

/// Opaque handle to a platform GPU surface.
///
/// Distinct from [`WindowId`]: a window is an OS shell; a surface is the
/// drawable it hosts. Today they map 1:1, but the split lets a window host
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
    /// small compact rects, copied into the window's cache — no callback, no UI
    /// type crosses the boundary. Replaces any prior set; an empty
    /// slice clears them. A no-op on backends without self-drawn chrome
    /// (headless/native-only), which is why this carries a default body.
    fn set_draggable_regions(&mut self, window: WindowId, regions: &[LogicalRect]) {
        let _ = (window, regions);
    }

    /// Enter or leave fullscreen for `window`. The backend reports the state it
    /// reached with [`RawEvent::FullscreenChanged`](crate::RawEvent). A no-op
    /// where windows are always fullscreen (mobile, a browser tab).
    fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        let _ = (window, fullscreen);
    }

    /// Publish a semantics delta for `window`. The app
    /// sends a full tree after [`AccessRequest::Activated`] and only changed
    /// nodes after that. A no-op on backends without an accessibility bridge
    /// ([`PlatformCaps::accessibility`] is `false`).
    fn update_accessibility(&mut self, window: WindowId, delta: SemanticsDelta) {
        let _ = (window, delta);
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

    /// Replace what the system clipboard holds with `data`: its text, its
    /// HTML and its image, each in the forms other apps read. An image of
    /// pixels is encoded as a PNG file where the OS keeps one, when someone
    /// first asks for it where the OS lets the copy wait.
    ///
    /// For copies the app starts itself ("Copy link"); copies the OS starts
    /// arrive as [`RawEvent::CopyRequested`] instead.
    fn set_clipboard(&mut self, data: ClipboardData);

    /// Ask for what the clipboard holds of `kinds` — the richest of them, and
    /// its text ([`ClipboardKinds::pick`]). It arrives as one
    /// [`RawEvent::Paste`] for `window` carrying `token`, empty when it holds
    /// none or access is refused: on the next turn of the loop where the
    /// clipboard reads at once, once read where it reads in the background or
    /// after a permission prompt (the web). The loop never waits on it.
    fn request_paste(&mut self, window: WindowId, kinds: ClipboardKinds, token: u32);

    /// Show `icon` while the mouse is over `window`'s content. An image
    /// cursor's OS cursor is built the first time any window shows it and
    /// reused after; where [`PlatformCaps::custom_cursors`] is false it shows
    /// as the default arrow.
    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon);

    /// Lock the mouse's pointer to `window`, or (`lock` false) release it.
    /// Locked, the cursor hides and stays where it was, and the mouse's
    /// motion arrives as [`RawEvent::PointerMotion`] instead of moves; its
    /// buttons and wheel still arrive, at that place. The lock ends when the
    /// app releases it, when the window loses focus, or when the OS takes it
    /// back; each change arrives as [`RawEvent::PointerLockChanged`], and a
    /// lock the OS refuses (the window is not focused, the browser saw no
    /// user gesture) as one with `locked: false`. Nothing happens where
    /// [`PlatformCaps::pointer_lock`] is false.
    fn set_pointer_lock(&mut self, window: WindowId, lock: bool) {
        let _ = (window, lock);
    }

    /// Tell the input method about the focused text field: where its caret
    /// is and whether it takes a secret. `None` means no text field has
    /// focus: the IME is disabled for the window.
    fn set_text_entry(&mut self, window: WindowId, entry: Option<TextEntry>);

    /// Show or hide the on-screen keyboard. A no-op where there is none.
    fn show_soft_keyboard(&mut self, window: WindowId, show: bool);

    /// Show the system edit menu for the focused text field's selection, or
    /// move it to `menu`'s place and items while it shows; `None` closes it.
    /// What the user picks arrives as [`RawEvent::CopyRequested`],
    /// [`RawEvent::PasteRequested`] or [`RawEvent::EditMenu`]. A no-op where
    /// [`PlatformCaps::edit_menu`] is false.
    fn set_edit_menu(&mut self, window: WindowId, menu: Option<EditMenu>) {
        let _ = (window, menu);
    }

    /// Pop up the system context menu of the focused text field at the
    /// origin of `menu.at`, titled by `labels`. It closes on a pick or a
    /// click elsewhere, never while the app is mid-frame; what the user
    /// picks arrives as [`RawEvent::CopyRequested`],
    /// [`RawEvent::PasteRequested`] or [`RawEvent::EditMenu`], and then its closing as
    /// [`EditMenuAction::Closed`]. A no-op where
    /// [`PlatformCaps::context_menu`] is false.
    fn show_context_menu(&mut self, window: WindowId, menu: EditMenu, labels: EditLabels) {
        let _ = (window, menu, labels);
    }

    /// Show the system magnifier over the caret a finger carries, or move it
    /// with the finger; `None` puts it away. A no-op where
    /// [`PlatformCaps::loupe`] is false.
    fn set_loupe(&mut self, window: WindowId, loupe: Option<Loupe>) {
        let _ = (window, loupe);
    }

    /// Carry `drag` on from `window` as a drag the OS runs, the mouse button
    /// that carries it still held: other apps' windows, and the app's own,
    /// see it as a drag from outside. Returns whether it started; a started
    /// drag ends with one [`RawEvent::DragEnded`]. `false` where the OS
    /// starts drags only from its own gestures (web, iOS) or has none.
    fn start_drag(&mut self, window: WindowId, drag: DragOut) -> bool {
        let _ = (window, drag);
        false
    }

    /// The current system appearance. Changes arrive as
    /// [`RawEvent::AppearanceChanged`].
    fn appearance(&self) -> Appearance;

    /// The user's locale, a BCP 47 tag (`und` when the platform reports
    /// none). Changes arrive as [`RawEvent::LocaleChanged`].
    fn locale(&self) -> String {
        "und".to_owned()
    }

    /// The hinges, folds and cutouts crossing `window`'s content now.
    /// Changes arrive as [`RawEvent::DisplayFeaturesChanged`].
    fn display_features(&self, window: WindowId) -> Vec<DisplayFeature> {
        let _ = window;
        Vec::new()
    }

    /// Whether windows are desktop windows the user moves and resizes, so a
    /// self-drawn caption belongs on them. `false` where the app owns the
    /// whole screen or tab (mobile, a browser page): the facade then draws no
    /// caption and insets content by the reported safe area instead.
    fn framed_windows(&self) -> bool {
        true
    }

    /// What this backend offers beyond its one window.
    fn capabilities(&self) -> PlatformCaps {
        PlatformCaps::DESKTOP
    }

    /// A handle that wakes this loop from any thread with a
    /// [`RawEvent::Wakeup`]. Inert on a backend without a cross-thread wake.
    fn loop_waker(&self) -> LoopWaker {
        LoopWaker::inert()
    }

    /// Reads the gamepads, appending to `events` what changed since the
    /// last call: connections, departures and new states. The runtime calls
    /// it once per frame; the first call starts the backend's gamepad
    /// support, and a connection noticed between calls wakes the loop. No
    /// allocation while nothing connects. A no-op where there are none.
    fn poll_gamepads(&mut self, events: &mut Vec<GamepadEvent>) {
        let _ = events;
    }

    /// Shakes gamepad `id` with `rumble`; whether it can.
    fn rumble_gamepad(&mut self, id: GamepadId, rumble: Rumble) -> bool {
        let _ = (id, rumble);
        false
    }
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

    /// The window's outer top-left corner on the desktop: logical points from
    /// the top-left of the primary screen on macOS, physical pixels on Windows
    /// and X11, where a desktop spanning monitors of different scales has no
    /// single logical unit. `None` where the platform does not say (Wayland,
    /// headless, mobile, the browser).
    fn outer_position(&self) -> Option<(f64, f64)> {
        None
    }

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
    /// the presence of native buttons (a data contract, not `target_os`); the
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
