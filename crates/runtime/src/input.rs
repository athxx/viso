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
//! Coordinates are resolved here; pure identities that need no resolution —
//! the physical key code, the pointer id and device class, the clipboard reply
//! slot — are the platform's own types, so one vocabulary runs from the OS to
//! the widget.

pub use viso_platform::{
    ClipboardReply, DropAction, DropOffer, DropPhase, DropReply, GesturePhase, KeyCode as Key,
    PointerId, PointerKind, ScrollPhase, primary_is_logo,
};
use viso_platform::{EditMenuAction, WindowId};

/// The lifecycle position of a pointer sample. Mirrors the platform phase, with
/// `Leave` naming the pointer-exited-the-window case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    /// The pointer left the window bounds.
    Leave,
    /// The OS took the contact away; it ends like `Up` but activates nothing.
    Cancel,
}

/// A pointer sample already resolved to physical pixels (window-top-left
/// origin) — the space node bounds and hit testing use, so no further
/// conversion happens above this point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerSample {
    /// The window the sample belongs to.
    pub window: WindowId,
    /// Which pointer this is; stable from `Down` to `Up`/`Cancel`.
    pub pointer: PointerId,
    pub kind: PointerKind,
    /// Position in physical pixels, origin at the window's top-left.
    pub x: f32,
    pub y: f32,
    /// Normalized contact pressure in `0.0..=1.0`.
    pub pressure: f32,
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
    /// Where the sample stands in the gesture that made it.
    pub phase: ScrollPhase,
}

/// A trackpad's pinch or turn resolved to physical pixels (window-top-left
/// origin), at the pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GestureSample {
    /// The window the sample belongs to.
    pub window: WindowId,
    /// Pointer position in physical pixels, origin at the window's top-left.
    pub x: f32,
    pub y: f32,
    /// The factor the fingers' spread scaled by since the last sample.
    pub scale: f32,
    /// How far they turned since the last sample, in radians, clockwise.
    pub rotation: f32,
    pub modifiers: Modifiers,
    pub phase: GesturePhase,
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

/// A step of a drag from outside the app, at (`x`, `y`) in physical pixels.
/// The driver answers through `reply` with what a drop there would do.
#[derive(Debug, Clone, PartialEq)]
pub struct DropSample {
    pub window: WindowId,
    pub phase: DropPhase,
    pub x: f32,
    pub y: f32,
    pub offer: DropOffer,
    pub reply: DropReply,
}

/// The OS asked the focused content for its selection (a native copy/cut
/// gesture). The driver answers through `reply`; the backend writes the answer
/// to the system clipboard once the event returns.
#[derive(Debug, Clone, PartialEq)]
pub struct CopySample {
    pub window: WindowId,
    /// Remove the selection after answering.
    pub cut: bool,
    pub reply: ClipboardReply,
}

/// The user picked an edit-menu item other than cut, copy and paste, or the
/// menu closed on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditMenuSample {
    pub window: WindowId,
    pub action: EditMenuAction,
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
    /// A trackpad's pinch or turn, positioned at the pointer so routing can
    /// find the node under it that takes pinches.
    Gesture(GestureSample),
    /// A key transition routed to the focused node.
    Key(KeySample),
    /// A committed text segment (the IME commit).
    Text(TextSample),
    /// An in-progress IME composition update (preedit).
    ImePreedit(ImePreeditSample),
    /// Clipboard text to insert at the focused content.
    Paste(TextSample),
    /// A copy/cut request for the focused content's selection.
    Copy(CopySample),
    /// What the user did with the focused field's edit menu.
    EditMenu(EditMenuSample),
    /// A step of a drag from outside the app.
    Drop(DropSample),
    /// The drag the app carried out of `window` ended: let go where it did
    /// `action`, or put back (`None`).
    DragEnded {
        window: WindowId,
        action: Option<DropAction>,
    },
}

impl InputSample {
    /// The window this sample belongs to. Every variant is window-scoped (the
    /// scheduler resolved it from the raw event's `window` field), so a
    /// multi-window driver reads this to route the sample to the right window's
    /// tree without matching on the variant.
    pub fn window(&self) -> WindowId {
        match self {
            InputSample::Pointer(p) => p.window,
            InputSample::Scroll(s) => s.window,
            InputSample::Gesture(g) => g.window,
            InputSample::Key(k) => k.window,
            InputSample::Text(t) => t.window,
            InputSample::ImePreedit(p) => p.window,
            InputSample::Paste(t) => t.window,
            InputSample::Copy(c) => c.window,
            InputSample::EditMenu(e) => e.window,
            InputSample::Drop(d) => d.window,
            InputSample::DragEnded { window, .. } => *window,
        }
    }
}
