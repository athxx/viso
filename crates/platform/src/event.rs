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

use std::cell::Cell;
use std::rc::Rc;

use crate::control::WindowId;

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
    /// The platform accelerator modifier: Command on macOS, Control elsewhere.
    pub fn is_primary(self) -> bool {
        if cfg!(target_os = "macos") {
            self.logo
        } else {
            self.control
        }
    }
}

/// A pointer (mouse/touch/pen) sample in logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawPointer {
    pub window: WindowId,
    /// Position in logical points, origin at the window's top-left.
    pub x: f64,
    pub y: f64,
    /// Buttons currently held.
    pub buttons: PointerButtons,
    pub modifiers: Modifiers,
    /// What kind of sample this is.
    pub phase: PointerPhase,
}

/// The lifecycle position of a pointer sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Moved,
    Down,
    Up,
    /// The pointer left the window bounds.
    Left,
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

/// A minimal, platform-independent key identity.
///
/// Deliberately small in Phase 1 — enough to prove the channel and to route
/// Escape/quit shortcuts. The full keymap lands with the input subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Escape,
    Enter,
    Space,
    Tab,
    Backspace,
    /// Any key not yet in the minimal set, carrying its raw platform scancode.
    Other(u32),
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
