//! Normalized pointer input: the UI-tier event and the router that dispatches
//! it along the retained ancestry chain.
//!
//! The transport tier reports pointer samples in logical points; this tier
//! works in physical pixels — the same space as `bounds` and hit testing — so
//! the facade converts once at the boundary. These value types are UI-tier
//! mirrors (the UI crate does not depend on the platform crate); they carry the
//! same meaning without inverting the dependency direction.

/// The lifecycle position of a pointer sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    /// The pointer left the window bounds.
    Leave,
}

/// Pointer buttons as a bitmask; a mask (not a single button) so a sample can
/// report chords. `PRIMARY` is the left button on a conventional mouse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PointerButtons(pub u8);

impl PointerButtons {
    pub const NONE: PointerButtons = PointerButtons(0);
    pub const PRIMARY: PointerButtons = PointerButtons(1 << 0);
    pub const SECONDARY: PointerButtons = PointerButtons(1 << 1);
    pub const MIDDLE: PointerButtons = PointerButtons(1 << 2);

    /// Whether every button in `other` is currently pressed.
    #[inline]
    pub fn contains(self, other: PointerButtons) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether no button is pressed.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Keyboard modifier state accompanying a pointer sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// The Command/Windows/Super key.
    pub logo: bool,
}

/// A normalized pointer event in physical pixels (window-top-left origin) — the
/// same coordinate space as node `bounds`, so hit testing needs no conversion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerEvent {
    pub x: f32,
    pub y: f32,
    pub phase: PointerPhase,
    pub buttons: PointerButtons,
    pub modifiers: Modifiers,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_mask_contains_and_empty() {
        assert!(PointerButtons::NONE.is_empty());
        assert!(!PointerButtons::PRIMARY.is_empty());
        let chord = PointerButtons(PointerButtons::PRIMARY.0 | PointerButtons::SECONDARY.0);
        assert!(chord.contains(PointerButtons::PRIMARY));
        assert!(chord.contains(PointerButtons::SECONDARY));
        assert!(!chord.contains(PointerButtons::MIDDLE));
    }

    #[test]
    fn event_is_copy_and_holds_physical_coords() {
        let ev = PointerEvent {
            x: 12.0,
            y: 34.0,
            phase: PointerPhase::Down,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        };
        let copied = ev; // Copy
        assert_eq!(copied.x, 12.0);
        assert_eq!(ev.phase, PointerPhase::Down);
    }
}
