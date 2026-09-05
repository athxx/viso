//! Runtime-tier normalized input: the sample the scheduler hands the driver.
//!
//! Raw platform events arrive in logical points and are not yet resolved to a
//! coordinate space the UI tree can hit-test. The scheduler owns the window, so
//! it is the one place that can read the window scale factor; it resolves the
//! scale at the event seam, converts pointer coordinates logical → physical
//! pixels (the same space as node bounds), and hands the driver an
//! `InputSample` that already carries physical-space data.
//!
//! These are runtime-tier value types, deliberately small and `Copy` on the
//! pointer path so threading a sample through the driver allocates nothing.
//! They mirror the platform event meaning without re-exporting the platform
//! types, keeping the driver contract independent of the raw event shape.

use viso_platform::WindowId;

/// The lifecycle position of a pointer sample. Mirrors the platform phase, with
/// `Leave` naming the pointer-exited-the-window case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    /// The pointer left the window bounds.
    Leave,
}

/// A pointer sample already resolved to physical pixels (window-top-left
/// origin) — the space node bounds and hit testing use, so no further
/// conversion happens above this point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerSample {
    /// The window the sample belongs to.
    pub window: WindowId,
    /// Position in physical pixels, origin at the window's top-left.
    pub x: f32,
    pub y: f32,
    /// Buttons currently held, as a raw bitmask (matches the UI-tier mask).
    pub buttons: u8,
    /// Keyboard modifier state at the time of the sample.
    pub modifiers: Modifiers,
    pub phase: PointerPhase,
}

/// Keyboard modifier state accompanying an input sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// The Command/Windows/Super key.
    pub logo: bool,
}

/// A physical key transition, normalized (window-scoped, OS vocabulary dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySample {
    /// The window the sample belongs to.
    pub window: WindowId,
    pub key: Key,
    /// True on press, false on release.
    pub pressed: bool,
    /// True if this press is an OS auto-repeat.
    pub repeat: bool,
    pub modifiers: Modifiers,
}

/// A minimal platform-independent key identity (runtime-tier mirror of the
/// platform key code — kept crate-local so no OS vocabulary rides upward).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
    Space,
    Tab,
    Backspace,
    /// Left arrow — directional navigation.
    Left,
    /// Right arrow — directional navigation.
    Right,
    /// Up arrow — directional navigation.
    Up,
    /// Down arrow — directional navigation.
    Down,
    /// Any key not in the minimal set, carrying its raw platform scancode.
    Other(u32),
}

/// A scroll sample resolved to physical pixels (window-top-left origin), the
/// same space node bounds use. Carries both the pointer position (so routing can
/// pick the scroll target under the cursor) and the scroll delta.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollSample {
    /// The window the sample belongs to.
    pub window: WindowId,
    /// Pointer position in physical pixels, origin at the window's top-left.
    pub x: f32,
    pub y: f32,
    /// Scroll delta in physical pixels (positive = content moves down/right).
    pub delta_x: f32,
    pub delta_y: f32,
    pub modifiers: Modifiers,
}

/// A committed text segment (post-IME).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextSample {
    pub window: WindowId,
    pub text: String,
}

/// An in-progress IME composition update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImePreeditSample {
    pub window: WindowId,
    pub text: String,
    /// Caret position within `text`, in bytes.
    pub caret: usize,
}

/// A normalized input sample handed to the driver.
///
/// Each variant carries data already normalized for its channel, so the driver
/// never touches raw platform types or resolves a scale factor itself. Not
/// `Copy`: the text/preedit variants own a `String`.
#[derive(Debug, Clone, PartialEq)]
pub enum InputSample {
    /// A pointer (mouse/touch/pen) sample in physical pixels.
    Pointer(PointerSample),
    /// A scroll (wheel/trackpad) delta in physical pixels, positioned at the
    /// pointer so routing can find the scrollable target under the cursor.
    Scroll(ScrollSample),
    /// A key transition routed to the focused node.
    Key(KeySample),
    /// A committed text segment (the IME commit).
    Text(TextSample),
    /// An in-progress IME composition update (preedit).
    ImePreedit(ImePreeditSample),
}
