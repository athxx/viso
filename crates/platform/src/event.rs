//! The raw event tier: a two-tier event model, transport semantics only.
//!
//! This is the *transport* tier: OS-normalized samples with positions already
//! in logical points, but **not yet hit-tested** against any UI tree. Hit
//! resolution and widget-facing events live above the platform layer (Phase 3+,
//! against the `NodeArena`); this crate only reports what the OS delivered.
//!
//! Design points, decided by the behavior study:
//! - DPI/scale is *window geometry state*, delivered via one geometry event
//!   ([`RawEvent::ScaleFactorChanged`]) rather than a dedicated DPI channel.
//! - `Draw`/redraw is a distinct event, separate from input and from the
//!   animation tick, matching the runtime's `RedrawReason`/`FrameDecision`
//!   split.
//! - Text/IME is a separate channel ([`RawEvent::Text`]) from key codes
//!   ([`RawEvent::Key`]).
//! - Close/quit is a *veto handshake*: the event carries an [`AcceptCell`] the
//!   handler may clear to keep the window open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::control::{LogicalRect, WindowId};
use crate::menu::MenuCommandId;

/// A shared "should this proceed?" cell for veto handshakes.
///
/// The platform layer creates it defaulting to `accept = true`, hands a clone
/// out with a [`RawEvent::CloseRequested`], and — after the handler returns —
/// reads it back. A handler that wants to keep the window open calls
/// [`AcceptCell::deny`]. A shared `Rc<Cell<bool>>` accept cell.
#[derive(Debug, Clone)]
pub struct AcceptCell(Rc<Cell<bool>>);

impl PartialEq for AcceptCell {
    /// Two cells are equal iff they share the same backing allocation. Cheap
    /// identity comparison — enough for `RawEvent`'s derived `PartialEq`.
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl AcceptCell {
    /// Create a cell that accepts by default.
    pub fn new() -> Self {
        Self(Rc::new(Cell::new(true)))
    }

    /// Veto the pending action (e.g. keep the window open).
    pub fn deny(&self) {
        self.0.set(false);
    }

    /// Explicitly accept (the default).
    pub fn accept(&self) {
        self.0.set(true);
    }

    /// Whether the action is still accepted after the handler ran.
    pub fn is_accepted(&self) -> bool {
        self.0.get()
    }
}

impl Default for AcceptCell {
    fn default() -> Self {
        Self::new()
    }
}

/// Pointer buttons as a bitmask (hand-rolled, no `bitflags` dep).
///
/// A mask, not a single button, so a sample can report chords. `PRIMARY` is the
/// left button on a conventional mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PointerButtons(pub u8);

impl PointerButtons {
    pub const NONE: PointerButtons = PointerButtons(0);
    pub const PRIMARY: PointerButtons = PointerButtons(1 << 0);
    pub const SECONDARY: PointerButtons = PointerButtons(1 << 1);
    pub const MIDDLE: PointerButtons = PointerButtons(1 << 2);

    /// Whether every button in `other` is currently pressed.
    pub fn contains(self, other: PointerButtons) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no button is pressed.
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Keyboard modifier state, with the platform "primary" fold.
///
/// `is_primary()` returns the accelerator modifier for the current OS —
/// Command on macOS, Control elsewhere — so shortcut logic stays
/// platform-agnostic above this layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// The Command/Windows/Super key.
    pub logo: bool,
}

impl Modifiers {
    /// The platform accelerator modifier: Command on Apple systems (including a
    /// browser running on one), Control elsewhere.
    pub fn is_primary(self) -> bool {
        if primary_is_logo() {
            self.logo
        } else {
            self.control
        }
    }
}

/// Whether the accelerator modifier is the logo (Command) key. Fixed per target
/// natively; on the web it depends on the OS under the browser, which the web
/// backend detects at startup.
fn primary_is_logo() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        WEB_PRIMARY_IS_LOGO.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        cfg!(target_vendor = "apple")
    }
}

#[cfg(target_arch = "wasm32")]
pub(crate) static WEB_PRIMARY_IS_LOGO: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Identity of one pointer for the lifetime of its contact.
///
/// The mouse is always [`PointerId::MOUSE`]; each touch contact and pen gets an
/// id that stays fixed from its `Down` to its `Up`/`Cancel` and may be reused
/// afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PointerId(pub u64);

impl PointerId {
    /// The system mouse (or the trackpad driving it).
    pub const MOUSE: PointerId = PointerId(0);
}

/// The device class a pointer sample came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PointerKind {
    #[default]
    Mouse,
    Touch,
    Pen,
}

/// A pointer (mouse/touch/pen) sample in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawPointer {
    pub window: WindowId,
    /// Which pointer this is; stable across one contact.
    pub pointer: PointerId,
    pub kind: PointerKind,
    /// Position in logical points, origin at the window's top-left.
    pub x: f64,
    pub y: f64,
    /// Normalized contact pressure in `0.0..=1.0`. Devices without pressure
    /// sensing report `0.5` while a button/contact is down and `0.0` otherwise.
    pub pressure: f32,
    /// Buttons currently held. A touch or pen contact reports
    /// [`PointerButtons::PRIMARY`] while down.
    pub buttons: PointerButtons,
    pub modifiers: Modifiers,
    /// What kind of sample this is.
    pub phase: PointerPhase,
}

impl RawPointer {
    /// A mouse sample: pointer [`PointerId::MOUSE`], pressure derived from
    /// whether any button is held.
    pub fn mouse(
        window: WindowId,
        x: f64,
        y: f64,
        buttons: PointerButtons,
        modifiers: Modifiers,
        phase: PointerPhase,
    ) -> Self {
        Self {
            window,
            pointer: PointerId::MOUSE,
            kind: PointerKind::Mouse,
            x,
            y,
            pressure: if buttons.is_empty() { 0.0 } else { 0.5 },
            buttons,
            modifiers,
            phase,
        }
    }
}

/// The lifecycle position of a pointer sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Moved,
    Down,
    Up,
    /// The pointer left the window bounds.
    Left,
    /// The OS took the contact away (a system gesture, palm rejection, the
    /// window losing the touch stream). Ends the contact like `Up` but must
    /// not activate anything.
    Cancel,
}

/// A scroll-wheel / trackpad scroll sample, in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawScroll {
    pub window: WindowId,
    pub x: f64,
    pub y: f64,
    /// Scroll delta in logical points (positive = content moves down/right).
    pub delta_x: f64,
    pub delta_y: f64,
    pub modifiers: Modifiers,
}

/// A physical key transition (not text — see [`RawEvent::Text`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawKey {
    pub window: WindowId,
    /// Platform-independent key identity.
    pub code: KeyCode,
    /// True on press, false on release.
    pub pressed: bool,
    /// True if this press is an OS auto-repeat.
    pub repeat: bool,
    pub modifiers: Modifiers,
}

/// A platform-independent *physical* key identity.
///
/// Names the key's position on a US-layout keyboard (the W3C `KeyboardEvent.code`
/// model), not the character it produces: text arrives separately as
/// [`RawEvent::Text`]. Letters are `A`..`Z` whatever the active layout prints on
/// them, so shortcuts stay where the user's fingers expect them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Escape,
    /// Return, and the keypad Enter.
    Enter,
    Space,
    Tab,
    Backspace,
    /// Left arrow — directional navigation (slider decrement, caret motion).
    Left,
    /// Right arrow — directional navigation (slider increment, caret motion).
    Right,
    /// Up arrow — directional navigation (slider increment, vertical motion).
    Up,
    /// Down arrow — directional navigation (slider decrement, vertical motion).
    Down,
    /// Forward delete — removes the character after the caret (text editing).
    Delete,
    /// Home — moves the caret to the start of the line (text editing).
    Home,
    /// End — moves the caret to the end of the line (text editing).
    End,
    PageUp,
    PageDown,
    /// Insert (the Help key on Apple extended keyboards).
    Insert,

    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
    I,
    J,
    K,
    L,
    M,
    N,
    O,
    P,
    Q,
    R,
    S,
    T,
    U,
    V,
    W,
    X,
    Y,
    Z,

    Digit0,
    Digit1,
    Digit2,
    Digit3,
    Digit4,
    Digit5,
    Digit6,
    Digit7,
    Digit8,
    Digit9,

    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    F13,
    F14,
    F15,
    F16,
    F17,
    F18,
    F19,
    F20,
    F21,
    F22,
    F23,
    F24,

    ShiftLeft,
    ShiftRight,
    ControlLeft,
    ControlRight,
    AltLeft,
    AltRight,
    /// Command / Windows / Super, left.
    LogoLeft,
    /// Command / Windows / Super, right.
    LogoRight,
    CapsLock,
    /// The laptop `Fn` key, where the OS reports it.
    Fn,

    /// `` ` `` / `~`.
    Backquote,
    /// `-` / `_`.
    Minus,
    /// `=` / `+`.
    Equal,
    /// `[` / `{`.
    BracketLeft,
    /// `]` / `}`.
    BracketRight,
    /// `\` / `|`.
    Backslash,
    /// `;` / `:`.
    Semicolon,
    /// `'` / `"`.
    Quote,
    /// `,` / `<`.
    Comma,
    /// `.` / `>`.
    Period,
    /// `/` / `?`.
    Slash,
    /// The extra key left of `Z` on ISO keyboards.
    IntlBackslash,
    /// The JIS `ろ` / `_` key.
    IntlRo,
    /// The JIS `¥` key.
    IntlYen,

    NumLock,
    Numpad0,
    Numpad1,
    Numpad2,
    Numpad3,
    Numpad4,
    Numpad5,
    Numpad6,
    Numpad7,
    Numpad8,
    Numpad9,
    NumpadAdd,
    NumpadSubtract,
    NumpadMultiply,
    NumpadDivide,
    NumpadDecimal,
    NumpadEqual,
    /// The JIS keypad comma.
    NumpadComma,

    PrintScreen,
    ScrollLock,
    Pause,
    /// The context-menu (application) key.
    ContextMenu,

    /// Kana / Hangul toggle (`Lang1`).
    Lang1,
    /// Eisu / Hanja toggle (`Lang2`).
    Lang2,
    /// JIS 変換.
    Convert,
    /// JIS 無変換.
    NonConvert,

    VolumeUp,
    VolumeDown,
    VolumeMute,
    MediaPlayPause,
    MediaStop,
    MediaTrackNext,
    MediaTrackPrevious,

    /// The system back action (Android back button / gesture, browser back
    /// mouse button's keyboard twin).
    Back,

    /// Any key without a name here, carrying its raw platform scancode.
    Other(u32),
}

/// The platform-standard clipboard gesture a key press spells, if any.
///
/// Backends call this on every key-down so Copy/Cut/Paste reach the app as
/// [`RawEvent::CopyRequested`] / [`RawEvent::Paste`] however the OS spells them:
/// `primary+C/X/V` everywhere, plus `Ctrl+Insert`, `Shift+Delete` and
/// `Shift+Insert` off Apple platforms.
pub fn clipboard_shortcut(code: KeyCode, m: Modifiers) -> Option<ClipboardShortcut> {
    let primary_only = m.is_primary() && !m.alt && !m.shift;
    match code {
        KeyCode::C if primary_only => Some(ClipboardShortcut::Copy),
        KeyCode::X if primary_only => Some(ClipboardShortcut::Cut),
        KeyCode::V if primary_only => Some(ClipboardShortcut::Paste),
        _ if primary_is_logo() => None,
        KeyCode::Insert if m.control && !m.shift && !m.alt => Some(ClipboardShortcut::Copy),
        KeyCode::Delete if m.shift && !m.control && !m.alt => Some(ClipboardShortcut::Cut),
        KeyCode::Insert if m.shift && !m.control && !m.alt => Some(ClipboardShortcut::Paste),
        _ => None,
    }
}

/// See [`clipboard_shortcut`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardShortcut {
    Copy,
    Cut,
    Paste,
}

/// The reply slot of a [`RawEvent::CopyRequested`] handshake.
///
/// The handler puts the selected text in it; after the handler returns the
/// backend writes whatever it holds to the system clipboard. Left empty, the
/// clipboard is untouched.
#[derive(Debug, Clone, Default)]
pub struct ClipboardReply(Rc<RefCell<Option<String>>>);

impl PartialEq for ClipboardReply {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl ClipboardReply {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer the request with `text`.
    pub fn set(&self, text: String) {
        *self.0.borrow_mut() = Some(text);
    }

    /// Take the answer, leaving the slot empty.
    pub fn take(&self) -> Option<String> {
        self.0.borrow_mut().take()
    }
}

/// The light/dark scheme the OS asks apps to render in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    #[default]
    Light,
    Dark,
}

/// System-wide presentation preferences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Appearance {
    pub color_scheme: ColorScheme,
    /// The user asked for increased contrast.
    pub high_contrast: bool,
    /// The user asked for reduced motion.
    pub reduce_motion: bool,
}

/// Edge insets in logical points.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Insets {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

/// The mouse cursor shape a window shows over its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorIcon {
    #[default]
    Default,
    /// A link / clickable affordance (pointing hand).
    Pointer,
    Text,
    VerticalText,
    Crosshair,
    Move,
    Grab,
    Grabbing,
    NotAllowed,
    Wait,
    /// Busy but still interactive.
    Progress,
    Help,
    ContextMenu,
    Copy,
    Alias,
    ZoomIn,
    ZoomOut,
    /// Horizontal resize (`↔`).
    ResizeEw,
    /// Vertical resize (`↕`).
    ResizeNs,
    /// Diagonal resize (`↗↙`).
    ResizeNesw,
    /// Diagonal resize (`↖↘`).
    ResizeNwse,
    /// Column splitter.
    ResizeCol,
    /// Row splitter.
    ResizeRow,
    /// No cursor at all.
    Hidden,
}

/// A committed text / IME segment (separate channel from key codes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawText {
    pub window: WindowId,
    /// The committed characters (already IME-composed).
    pub text: String,
}

/// An in-progress IME composition (preedit). Unlike [`RawText`], this is not yet
/// committed: the composing string is shown inline and replaced on each update,
/// then cleared when the IME commits (a [`RawText`]) or cancels (an empty
/// preedit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawImePreedit {
    pub window: WindowId,
    /// The current composing string (may be empty to signal cancel/clear).
    pub text: String,
    /// Caret position within `text`, in bytes (a `text.len()` caret = end).
    pub caret: usize,
}

/// A raw, un-normalized platform event.
///
/// Not `Copy`: some variants ([`RawEvent::CloseRequested`], [`RawEvent::Text`])
/// carry heap/`Rc` payloads.
#[derive(Debug, Clone, PartialEq)]
pub enum RawEvent {
    /// The app finished launching; the pump is live. Fired exactly once,
    /// before any window event. The runtime creates its first window here.
    AppLaunched,
    /// A frame beat: the given window should redraw now (display link / vsync).
    RedrawRequested { window: WindowId },
    /// The window's content area resized to `w`×`h` *physical* pixels.
    Resized {
        window: WindowId,
        width: u32,
        height: u32,
    },
    /// The window's scale factor and/or size changed — the single geometry
    /// event. `width`/`height` are the new *physical* pixel size.
    ScaleFactorChanged {
        window: WindowId,
        scale: f64,
        width: u32,
        height: u32,
    },
    /// The user asked to close the window. Clearing `accept` keeps it open.
    CloseRequested {
        window: WindowId,
        accept: AcceptCell,
    },
    /// The window was actually destroyed (after an accepted close).
    WindowClosed { window: WindowId },
    /// A cross-thread wakeup was posted (mailbox has work); no OS input.
    Wakeup,
    /// A pointer sample.
    Pointer(RawPointer),
    /// A scroll sample.
    Scroll(RawScroll),
    /// A key transition.
    Key(RawKey),
    /// A committed text/IME segment.
    Text(RawText),
    /// An in-progress IME composition update (preedit); commit arrives as `Text`.
    ImePreedit(RawImePreedit),
    /// The user picked a custom application-menu item; carries the app-assigned
    /// [`MenuCommandId`](crate::menu::MenuCommandId). Standard actions (Quit,
    /// Close, …) route through the OS instead and never surface here.
    MenuCommand { id: MenuCommandId },
    /// The native chrome affordances the backend keeps for a self-drawn-chrome
    /// window moved or resized — currently the bounding box of macOS's
    /// traffic-light buttons, in logical points, top-left origin, relative to the
    /// content area. The app aligns its own caption around this box (leaves room
    /// on the correct side, centers the caption to the buttons' height). Fired
    /// after the window is created and again on resize/scale changes. Never fired
    /// for [`WindowChrome::Native`](crate::WindowChrome::Native) windows.
    WindowChromeGeom {
        window: WindowId,
        buttons_rect: LogicalRect,
    },
    /// The window entered or left fullscreen. On macOS the OS provides its own
    /// auto-hiding menu/title bar in fullscreen and removes the traffic lights,
    /// so a self-drawn caption must hide (`fullscreen: true`) and restore on
    /// exit (`false`). Emitted at the *start* of the transition animation (the
    /// platform's will-enter/will-exit hook), so the caption disappears as the
    /// animation begins rather than after it settles. Only the macOS backend
    /// emits it today; other backends never fullscreen-hide.
    FullscreenChanged { window: WindowId, fullscreen: bool },
    /// The window became (`true`) or stopped being (`false`) the target of
    /// keyboard input.
    WindowFocused { window: WindowId, focused: bool },
    /// The system appearance changed (light/dark, contrast, motion). The
    /// current value is also readable from
    /// [`PlatformApp::appearance`](crate::PlatformApp::appearance).
    AppearanceChanged(Appearance),
    /// The part of the window hidden by system UI (notch, status bar, home
    /// indicator, rounded corners) changed. Content should stay clear of it.
    SafeAreaChanged { window: WindowId, insets: Insets },
    /// The on-screen keyboard now covers `height` logical points at the bottom
    /// of the window (`0.0` once it is gone).
    KeyboardInsetChanged { window: WindowId, height: f64 },
    /// The OS asked the focused content for its selection: copy it, or cut it
    /// when `cut` is set. The handler answers through `reply`; the backend then
    /// writes the answer to the system clipboard.
    CopyRequested {
        window: WindowId,
        cut: bool,
        reply: ClipboardReply,
    },
    /// Clipboard text for the focused content: a native paste gesture, or the
    /// answer to [`PlatformApp::request_paste`](crate::PlatformApp::request_paste).
    Paste { window: WindowId, text: String },
    /// The app moved to the background: it is no longer visible and should stop
    /// animating. On mobile the drawable may be torn down after this.
    Suspended,
    /// The app returned to the foreground after [`RawEvent::Suspended`].
    Resumed,
    /// The OS is short on memory; drop caches that can be rebuilt.
    LowMemory,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ime_preedit_round_trips_through_the_raw_event() {
        let preedit = RawImePreedit {
            window: WindowId(1),
            text: "にほ".to_string(),
            caret: "にほ".len(),
        };
        let event = RawEvent::ImePreedit(preedit.clone());
        // The event is still `Clone` with the new String-carrying variant.
        let cloned = event.clone();
        match cloned {
            RawEvent::ImePreedit(p) => assert_eq!(p, preedit),
            other => panic!("expected ImePreedit, got {other:?}"),
        }
    }

    #[test]
    fn empty_preedit_is_the_cancel_signal() {
        // A zero-length composing string is representable — the IME cancel/clear.
        let cancel = RawImePreedit {
            window: WindowId(1),
            text: String::new(),
            caret: 0,
        };
        assert!(cancel.text.is_empty());
        let _ = RawEvent::ImePreedit(cancel);
    }
}
