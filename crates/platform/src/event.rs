//! The raw event tier: a two-tier event model, transport semantics only.
//!
//! This is the *transport* tier: OS-normalized samples with positions already
//! in logical points, but **not yet hit-tested** against any UI tree. Hit
//! resolution and widget-facing events live above the platform layer (against
//! the `NodeArena`); this crate only reports what the OS delivered.
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

use std::cell::Cell;
use std::rc::Rc;

use crate::accessibility::AccessRequest;
use crate::clipboard::{ClipboardData, ClipboardReply, PasteReply};
use crate::control::{LogicalRect, WindowId};
use crate::menu::MenuCommandId;

pub use crate::cursor::CursorIcon;

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

/// Whether the accelerator modifier is the logo (Command) key, and the
/// system's other key conventions are Apple's (Option moves by word, Command
/// to the line's ends). Fixed per target natively; on the web it depends on
/// the OS under the browser, which the web backend detects at startup.
pub fn primary_is_logo() -> bool {
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
    /// [`PointerButtons::PRIMARY`] while down. On an `Up` a backend may
    /// report the buttons still held after it or the one released; the
    /// runtime hands the UI the ones released either way.
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
    /// Scroll delta in logical points: positive scrolls toward the content's
    /// end, down and right, so the content itself moves up and left.
    pub delta_x: f64,
    pub delta_y: f64,
    pub modifiers: Modifiers,
    /// Where the sample stands in the gesture that made it.
    pub phase: ScrollPhase,
}

/// A trackpad's pinch or turn, in logical points: two fingers spreading or
/// turning on it. A system that reports the two apart reports one gesture
/// while either runs. Three fingers or more swiping on it are one too, which
/// neither scales nor turns but moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawGesture {
    pub window: WindowId,
    /// Where the pointer is.
    pub x: f64,
    pub y: f64,
    /// The factor the fingers' spread scaled by since the last sample.
    pub scale: f64,
    /// How far they turned since the last sample, in radians, clockwise.
    pub rotation: f64,
    /// How far the fingers' middle moved since the last sample; zero where
    /// the system tells only of the spread and the turn.
    pub dx: f64,
    pub dy: f64,
    pub modifiers: Modifiers,
    pub phase: GesturePhase,
}

/// Where a trackpad's gesture sample stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GesturePhase {
    /// The fingers began.
    Began,
    /// They moved.
    Changed,
    /// They lifted, or the gesture was cancelled.
    Ended,
}

/// Where a scroll sample stands in the gesture that made it: fingers on a
/// trackpad scroll, lift, and the system coasts on. A wheel's notch, or a
/// device that tells nothing of its gesture, is [`ScrollPhase::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollPhase {
    #[default]
    None,
    /// The fingers began scrolling.
    Began,
    /// The fingers moved.
    Changed,
    /// The fingers lifted (or the gesture was cancelled).
    Ended,
    /// The system's coast after the lift.
    Momentum,
    /// The coast ended, or a touch stopped it.
    MomentumEnded,
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

impl KeyCode {
    /// Every key but [`Other`](KeyCode::Other), in declaration order: a
    /// key's place here is its stable number in a recorded input tape.
    pub const NAMED: [KeyCode; 133] = [
        KeyCode::Escape,
        KeyCode::Enter,
        KeyCode::Space,
        KeyCode::Tab,
        KeyCode::Backspace,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Delete,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Insert,
        KeyCode::A,
        KeyCode::B,
        KeyCode::C,
        KeyCode::D,
        KeyCode::E,
        KeyCode::F,
        KeyCode::G,
        KeyCode::H,
        KeyCode::I,
        KeyCode::J,
        KeyCode::K,
        KeyCode::L,
        KeyCode::M,
        KeyCode::N,
        KeyCode::O,
        KeyCode::P,
        KeyCode::Q,
        KeyCode::R,
        KeyCode::S,
        KeyCode::T,
        KeyCode::U,
        KeyCode::V,
        KeyCode::W,
        KeyCode::X,
        KeyCode::Y,
        KeyCode::Z,
        KeyCode::Digit0,
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
        KeyCode::F1,
        KeyCode::F2,
        KeyCode::F3,
        KeyCode::F4,
        KeyCode::F5,
        KeyCode::F6,
        KeyCode::F7,
        KeyCode::F8,
        KeyCode::F9,
        KeyCode::F10,
        KeyCode::F11,
        KeyCode::F12,
        KeyCode::F13,
        KeyCode::F14,
        KeyCode::F15,
        KeyCode::F16,
        KeyCode::F17,
        KeyCode::F18,
        KeyCode::F19,
        KeyCode::F20,
        KeyCode::F21,
        KeyCode::F22,
        KeyCode::F23,
        KeyCode::F24,
        KeyCode::ShiftLeft,
        KeyCode::ShiftRight,
        KeyCode::ControlLeft,
        KeyCode::ControlRight,
        KeyCode::AltLeft,
        KeyCode::AltRight,
        KeyCode::LogoLeft,
        KeyCode::LogoRight,
        KeyCode::CapsLock,
        KeyCode::Fn,
        KeyCode::Backquote,
        KeyCode::Minus,
        KeyCode::Equal,
        KeyCode::BracketLeft,
        KeyCode::BracketRight,
        KeyCode::Backslash,
        KeyCode::Semicolon,
        KeyCode::Quote,
        KeyCode::Comma,
        KeyCode::Period,
        KeyCode::Slash,
        KeyCode::IntlBackslash,
        KeyCode::IntlRo,
        KeyCode::IntlYen,
        KeyCode::NumLock,
        KeyCode::Numpad0,
        KeyCode::Numpad1,
        KeyCode::Numpad2,
        KeyCode::Numpad3,
        KeyCode::Numpad4,
        KeyCode::Numpad5,
        KeyCode::Numpad6,
        KeyCode::Numpad7,
        KeyCode::Numpad8,
        KeyCode::Numpad9,
        KeyCode::NumpadAdd,
        KeyCode::NumpadSubtract,
        KeyCode::NumpadMultiply,
        KeyCode::NumpadDivide,
        KeyCode::NumpadDecimal,
        KeyCode::NumpadEqual,
        KeyCode::NumpadComma,
        KeyCode::PrintScreen,
        KeyCode::ScrollLock,
        KeyCode::Pause,
        KeyCode::ContextMenu,
        KeyCode::Lang1,
        KeyCode::Lang2,
        KeyCode::Convert,
        KeyCode::NonConvert,
        KeyCode::VolumeUp,
        KeyCode::VolumeDown,
        KeyCode::VolumeMute,
        KeyCode::MediaPlayPause,
        KeyCode::MediaStop,
        KeyCode::MediaTrackNext,
        KeyCode::MediaTrackPrevious,
        KeyCode::Back,
    ];

    /// The key's place in [`NAMED`](Self::NAMED); `None` for
    /// [`Other`](KeyCode::Other).
    pub fn named_index(self) -> Option<u16> {
        KeyCode::NAMED
            .iter()
            .position(|&k| k == self)
            .map(|at| at as u16)
    }
}

/// The platform-standard clipboard gesture a key press spells, if any.
///
/// Backends call this on every key-down so Copy/Cut/Paste reach the app as
/// [`RawEvent::CopyRequested`] / [`RawEvent::PasteRequested`] however the OS spells them:
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

/// The light/dark scheme the OS asks apps to render in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    #[default]
    Light,
    Dark,
}

/// System-wide presentation preferences.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Appearance {
    pub color_scheme: ColorScheme,
    /// The user asked for increased contrast.
    pub high_contrast: bool,
    /// The user asked for reduced motion.
    pub reduce_motion: bool,
    /// The user asked for less transparency: glass frostier and denser.
    pub reduce_transparency: bool,
    /// The user's preferred text size relative to the platform's default
    /// size: `1.0` where the platform has no such setting.
    pub text_scale: f32,
    /// Scroll bars show only while scrolling, over the content; else the
    /// user asked to always see them, beside it.
    pub overlay_scroll_bars: bool,
    /// The user's accent color as gamma-encoded sRGB channels in `[0, 1]`,
    /// where the system has one.
    pub accent: Option<[f32; 3]>,
    /// How soon after a click the next one makes it a double click, where
    /// the system says.
    pub double_click: Option<core::time::Duration>,
}

impl Default for Appearance {
    fn default() -> Self {
        Appearance {
            color_scheme: ColorScheme::Light,
            high_contrast: false,
            reduce_motion: false,
            reduce_transparency: false,
            text_scale: 1.0,
            overlay_scroll_bars: true,
            accent: None,
            double_click: None,
        }
    }
}

/// What part of the display a [`DisplayFeature`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayFeatureKind {
    /// A physical hinge between two screens, which shows nothing.
    Hinge,
    /// A fold in a flexible screen, which shows content but bends it.
    Fold,
    /// A camera or sensor cutout, which shows nothing.
    Cutout,
}

/// A hinge, fold or cutout crossing a window's content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayFeature {
    /// What it is.
    pub kind: DisplayFeatureKind,
    /// Where it crosses the content, in logical points from its top-left.
    pub bounds: LogicalRect,
}

/// Edge insets in logical points.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LogicalInsets {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
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
    /// Its clauses and the one being converted.
    pub clauses: PreeditClauses,
}

/// An input method's edit of the text around the caret, which it reads
/// from the [`SurroundingText`](crate::SurroundingText) the app publishes.
/// Offsets are bytes into the field's whole text, as it stands once the
/// events before this one are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImeEdit {
    /// Replace `range` with `text`, as typed; an empty `text` deletes it. A
    /// composition outside the range goes on, one it touches ends first. A
    /// selection outside the range keeps its place; else the caret follows
    /// the new text.
    Replace {
        range: core::ops::Range<usize>,
        text: String,
    },
    /// End any composition as typed, then make `range` the composition,
    /// shown as `text` with the caret `caret` bytes into it: an autocorrect
    /// taking up a typed word again, or a reconversion of committed text.
    Recompose {
        range: core::ops::Range<usize>,
        text: String,
        caret: usize,
    },
    /// Select from `anchor` to `focus`.
    Select { anchor: usize, focus: usize },
}

/// An [`ImeEdit`] to `window`'s focused field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawImeEdit {
    pub window: WindowId,
    pub edit: ImeEdit,
}

/// How an input method marks a composition: the clauses it splits it into,
/// and the one it is converting now. Offsets are bytes of the preedit text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreeditClauses {
    /// The clauses in order; empty when the whole preedit is one.
    pub clauses: Vec<(usize, usize)>,
    /// The clause being converted, if any.
    pub selected: Option<(usize, usize)>,
}

impl PreeditClauses {
    /// The clauses of `text` that UTF-16 ranges `clauses` and `selected`
    /// name, on character boundaries, ordered and within it; empty ones
    /// dropped.
    pub fn from_utf16(
        text: &str,
        clauses: &[(usize, usize)],
        selected: Option<(usize, usize)>,
    ) -> PreeditClauses {
        let byte = |units: usize| {
            let mut seen = 0;
            for (at, c) in text.char_indices() {
                if seen >= units {
                    return at;
                }
                seen += c.len_utf16();
            }
            text.len()
        };
        let span = |(a, b): (usize, usize)| {
            let (a, b) = (byte(a), byte(b));
            (a < b).then_some((a, b)).or((b < a).then_some((b, a)))
        };
        let mut clauses: Vec<_> = clauses.iter().filter_map(|&c| span(c)).collect();
        clauses.sort_unstable();
        PreeditClauses {
            clauses,
            selected: selected.and_then(span),
        }
    }
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
    /// The window moved: its outer top-left corner is at `(x, y)`, in the
    /// coordinates [`Window::outer_position`](crate::Window::outer_position)
    /// reports. Not sent where the platform places windows itself (Wayland,
    /// mobile, the browser).
    Moved { window: WindowId, x: f64, y: f64 },
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
    /// The mouse moved by `dx`, `dy` logical points (right and down are
    /// positive) while its pointer is locked to `window`
    /// ([`PlatformApp::set_pointer_lock`](crate::PlatformApp::set_pointer_lock)):
    /// its motion, as the pointer would have moved, where a locked pointer
    /// moves no more and reports no [`RawEvent::Pointer`] moves. Motion keeps
    /// coming at a screen's edge. Several reported between two looks at the
    /// queue may arrive summed as one.
    PointerMotion { window: WindowId, dx: f64, dy: f64 },
    /// The pointer lock on `window` engaged (`true`) or ended (`false`): the
    /// app released it, the window lost focus, the OS took it back (the
    /// browser's Escape), or the OS refused it.
    PointerLockChanged { window: WindowId, locked: bool },
    /// A scroll sample.
    Scroll(RawScroll),
    /// A trackpad's pinch or turn.
    Gesture(RawGesture),
    /// A key transition.
    Key(RawKey),
    /// A committed text/IME segment.
    Text(RawText),
    /// An in-progress IME composition update (preedit); commit arrives as `Text`.
    ImePreedit(RawImePreedit),
    /// An input method's edit of the text around the caret.
    ImeEdit(RawImeEdit),
    /// The user picked a custom application-menu item; carries the app-assigned
    /// [`MenuCommandId`](crate::menu::MenuCommandId). Standard actions (Quit,
    /// Close, …) route through the OS instead and never surface here.
    MenuCommand { id: MenuCommandId },
    /// The user clicked the app's tray icon
    /// ([`PlatformApp::set_tray`](crate::PlatformApp::set_tray)); picks of
    /// its menu arrive as [`RawEvent::MenuCommand`].
    Tray(crate::shell::TrayEvent),
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
    /// The window entered or left fullscreen, asked by the app or by the
    /// user (the green button, F11, the browser's Escape). On macOS the OS
    /// provides its own auto-hiding menu/title bar in fullscreen and removes
    /// the traffic lights, so a self-drawn caption must hide
    /// (`fullscreen: true`) and restore on exit (`false`); it is emitted at
    /// the *start* of the transition animation (the platform's
    /// will-enter/will-exit hook), so the caption disappears as the animation
    /// begins rather than after it settles. On a phone, fullscreen hides the
    /// system bars.
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
    SafeAreaChanged {
        window: WindowId,
        insets: LogicalInsets,
    },
    /// The on-screen keyboard now covers `height` logical points at the bottom
    /// of the window (`0.0` once it is gone).
    KeyboardInsetChanged { window: WindowId, height: f64 },
    /// The hinges, folds and cutouts crossing `window`'s content changed;
    /// `features` is the whole new list. The current list is also readable
    /// from [`PlatformApp::display_features`](crate::PlatformApp::display_features).
    DisplayFeaturesChanged {
        window: WindowId,
        features: Vec<DisplayFeature>,
    },
    /// The app was asked to open `url`: a URL of a scheme it declares, or a
    /// verified web link to a host it declares, whether it started for it or
    /// was running. Delivered after [`RawEvent::AppLaunched`], in the order
    /// the OS handed them over; one that came before launch waits for it.
    OpenUrl { url: String },
    /// The user's locale changed, a BCP 47 tag such as `en-US` or
    /// `zh-Hans-CN`. The current one is also readable from
    /// [`PlatformApp::locale`](crate::PlatformApp::locale).
    LocaleChanged { locale: String },
    /// The OS asked the focused content for its selection: copy it, or cut it
    /// when `cut` is set. The handler answers through `reply`; the backend then
    /// writes the answer to the system clipboard.
    CopyRequested {
        window: WindowId,
        cut: bool,
        reply: ClipboardReply,
    },
    /// The user asked to paste into the focused content (a shortcut, an edit
    /// menu). The handler says through `reply` which kinds it takes; the
    /// backend then reads them and delivers what the clipboard holds of them
    /// as a [`RawEvent::Paste`], with no `token` — none when it holds none.
    PasteRequested { window: WindowId, reply: PasteReply },
    /// What the clipboard holds of the kinds a paste takes: the answer to a
    /// [`RawEvent::PasteRequested`], or to
    /// [`PlatformApp::request_paste`](crate::PlatformApp::request_paste),
    /// whose `token` it carries — always, empty when the clipboard holds
    /// none of them or reading it was refused.
    Paste {
        window: WindowId,
        data: ClipboardData,
        token: Option<u32>,
    },
    /// A step of a drag from outside the app over a window; the handler
    /// answers through its [`DropReply`](crate::DropReply).
    Drop(crate::drop::RawDrop),
    /// The drag [`PlatformApp::start_drag`](crate::PlatformApp::start_drag)
    /// started from `window` ended: let go where it did `action`, or put back
    /// (`None`).
    DragEnded {
        window: WindowId,
        action: Option<crate::drop::DropAction>,
    },
    /// The app moved to the background: it is no longer visible and should stop
    /// animating. On mobile the drawable may be torn down after this.
    Suspended,
    /// The app returned to the foreground after [`RawEvent::Suspended`].
    Resumed,
    /// The OS is short on memory; drop caches that can be rebuilt.
    LowMemory,
    /// The native surface behind `window` went away: its GPU swapchain must
    /// be destroyed before this returns, and nothing drawn until
    /// [`RawEvent::SurfaceCreated`]. Only platforms that reclaim a live
    /// window's surface (Android) send it.
    SurfaceDestroyed { window: WindowId },
    /// `window` has a native surface again after
    /// [`RawEvent::SurfaceDestroyed`]; [`Window::raw_handle`](crate::Window::raw_handle)
    /// returns it and [`Window::inner_size`](crate::Window::inner_size) its size.
    SurfaceCreated { window: WindowId },
    /// The user picked an item of `window`'s
    /// [`EditMenu`](crate::EditMenu) other than cut, copy and paste, or it
    /// closed on its own.
    EditMenu {
        window: WindowId,
        action: crate::EditMenuAction,
    },
    /// An assistive technology started or stopped listening to `window`, or
    /// asked for an action on one of its published nodes.
    Accessibility {
        window: WindowId,
        request: AccessRequest,
    },
}

/// Queues the motion of a locked pointer, adding it to a motion of the same
/// window still at the back of `queue` so a fast mouse queues one event.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn queue_motion(
    queue: &mut std::collections::VecDeque<RawEvent>,
    window: WindowId,
    dx: f64,
    dy: f64,
) {
    if let Some(RawEvent::PointerMotion {
        window: w,
        dx: x,
        dy: y,
    }) = queue.back_mut()
        && *w == window
    {
        *x += dx;
        *y += dy;
        return;
    }
    queue.push_back(RawEvent::PointerMotion { window, dx, dy });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn motions_queued_back_to_back_sum() {
        let mut queue = std::collections::VecDeque::new();
        let (a, b) = (WindowId(1), WindowId(2));
        queue_motion(&mut queue, a, 1.0, 2.0);
        queue_motion(&mut queue, a, 0.5, -1.0);
        queue_motion(&mut queue, b, 3.0, 0.0);
        queue_motion(&mut queue, a, 1.0, 1.0);
        assert_eq!(
            Vec::from(queue),
            vec![
                RawEvent::PointerMotion {
                    window: a,
                    dx: 1.5,
                    dy: 1.0
                },
                RawEvent::PointerMotion {
                    window: b,
                    dx: 3.0,
                    dy: 0.0
                },
                RawEvent::PointerMotion {
                    window: a,
                    dx: 1.0,
                    dy: 1.0
                },
            ]
        );
    }

    #[test]
    fn ime_preedit_round_trips_through_the_raw_event() {
        let preedit = RawImePreedit {
            window: WindowId(1),
            text: "にほ".to_string(),
            caret: "にほ".len(),
            clauses: PreeditClauses::default(),
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
            clauses: PreeditClauses::default(),
        };
        assert!(cancel.text.is_empty());
        let _ = RawEvent::ImePreedit(cancel);
    }
}
