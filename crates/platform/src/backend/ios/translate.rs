//! The UIKit backend's pure translation tables: HID usages to [`KeyCode`],
//! `UIKeyModifierFlags` bits to [`Modifiers`], the touch-to-pointer-id table,
//! touch pressure, key repeat classes, and the keyboard's overlap with the
//! view. Nothing here calls UIKit, so it is compiled and tested on every host.

#![cfg_attr(not(target_os = "ios"), allow(dead_code))]

use crate::event::{KeyCode, Modifiers, PointerId};

/// `UIKeyModifierFlags` bits.
const SHIFT: isize = 1 << 17;
const CONTROL: isize = 1 << 18;
const ALTERNATE: isize = 1 << 19;
const COMMAND: isize = 1 << 20;

/// Held-key repeat timing, matching the system's defaults.
pub(crate) const REPEAT_DELAY_SECS: f64 = 0.5;
pub(crate) const REPEAT_INTERVAL_SECS: f64 = 1.0 / 30.0;

pub(crate) fn modifiers(flags: isize) -> Modifiers {
    Modifiers {
        shift: flags & SHIFT != 0,
        control: flags & CONTROL != 0,
        alt: flags & ALTERNATE != 0,
        logo: flags & COMMAND != 0,
    }
}

/// A key from its USB HID keyboard-page usage.
pub(crate) fn key_code(usage: isize) -> KeyCode {
    use KeyCode as K;
    const LETTERS: [KeyCode; 26] = [
        K::A,
        K::B,
        K::C,
        K::D,
        K::E,
        K::F,
        K::G,
        K::H,
        K::I,
        K::J,
        K::K,
        K::L,
        K::M,
        K::N,
        K::O,
        K::P,
        K::Q,
        K::R,
        K::S,
        K::T,
        K::U,
        K::V,
        K::W,
        K::X,
        K::Y,
        K::Z,
    ];
    const DIGITS: [KeyCode; 10] = [
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
        K::Digit0,
    ];
    const FUNCTION: [KeyCode; 12] = [
        K::F1,
        K::F2,
        K::F3,
        K::F4,
        K::F5,
        K::F6,
        K::F7,
        K::F8,
        K::F9,
        K::F10,
        K::F11,
        K::F12,
    ];
    const FUNCTION_HIGH: [KeyCode; 12] = [
        K::F13,
        K::F14,
        K::F15,
        K::F16,
        K::F17,
        K::F18,
        K::F19,
        K::F20,
        K::F21,
        K::F22,
        K::F23,
        K::F24,
    ];
    const NUMPAD: [KeyCode; 10] = [
        K::Numpad1,
        K::Numpad2,
        K::Numpad3,
        K::Numpad4,
        K::Numpad5,
        K::Numpad6,
        K::Numpad7,
        K::Numpad8,
        K::Numpad9,
        K::Numpad0,
    ];
    let index = |base: isize| (usage - base) as usize;
    match usage {
        0x04..=0x1D => LETTERS[index(0x04)],
        0x1E..=0x27 => DIGITS[index(0x1E)],
        0x28 | 0x58 => K::Enter,
        0x29 => K::Escape,
        0x2A => K::Backspace,
        0x2B => K::Tab,
        0x2C => K::Space,
        0x2D => K::Minus,
        0x2E => K::Equal,
        0x2F => K::BracketLeft,
        0x30 => K::BracketRight,
        0x31 | 0x32 => K::Backslash,
        0x33 => K::Semicolon,
        0x34 => K::Quote,
        0x35 => K::Backquote,
        0x36 => K::Comma,
        0x37 => K::Period,
        0x38 => K::Slash,
        0x39 => K::CapsLock,
        0x3A..=0x45 => FUNCTION[index(0x3A)],
        0x46 => K::PrintScreen,
        0x47 => K::ScrollLock,
        0x48 => K::Pause,
        0x49 => K::Insert,
        0x4A => K::Home,
        0x4B => K::PageUp,
        0x4C => K::Delete,
        0x4D => K::End,
        0x4E => K::PageDown,
        0x4F => K::Right,
        0x50 => K::Left,
        0x51 => K::Down,
        0x52 => K::Up,
        0x53 => K::NumLock,
        0x54 => K::NumpadDivide,
        0x55 => K::NumpadMultiply,
        0x56 => K::NumpadSubtract,
        0x57 => K::NumpadAdd,
        0x59..=0x62 => NUMPAD[index(0x59)],
        0x63 => K::NumpadDecimal,
        0x64 => K::IntlBackslash,
        0x65 => K::ContextMenu,
        0x67 => K::NumpadEqual,
        0x68..=0x73 => FUNCTION_HIGH[index(0x68)],
        0x7F => K::VolumeMute,
        0x80 => K::VolumeUp,
        0x81 => K::VolumeDown,
        0x85 => K::NumpadComma,
        0x87 => K::IntlRo,
        0x89 => K::IntlYen,
        0x8A => K::Convert,
        0x8B => K::NonConvert,
        0x90 => K::Lang1,
        0x91 => K::Lang2,
        0xE0 => K::ControlLeft,
        0xE1 => K::ShiftLeft,
        0xE2 => K::AltLeft,
        0xE3 => K::LogoLeft,
        0xE4 => K::ControlRight,
        0xE5 => K::ShiftRight,
        0xE6 => K::AltRight,
        0xE7 => K::LogoRight,
        other => K::Other(other as u32),
    }
}

/// Whether the key produces no text, so the view reports it itself (and
/// repeats it while held) instead of leaving it to the text system.
pub(crate) fn is_command_key(code: KeyCode) -> bool {
    use KeyCode as K;
    matches!(
        code,
        K::Enter
            | K::Tab
            | K::Backspace
            | K::Escape
            | K::Delete
            | K::Left
            | K::Right
            | K::Up
            | K::Down
            | K::Home
            | K::End
            | K::PageUp
            | K::PageDown
            | K::Insert
    ) || matches!(
        code,
        K::F1
            | K::F2
            | K::F3
            | K::F4
            | K::F5
            | K::F6
            | K::F7
            | K::F8
            | K::F9
            | K::F10
            | K::F11
            | K::F12
    )
}

/// Whether holding the key repeats it.
pub(crate) fn repeats(code: KeyCode) -> bool {
    use KeyCode as K;
    matches!(
        code,
        K::Enter
            | K::Tab
            | K::Backspace
            | K::Delete
            | K::Left
            | K::Right
            | K::Up
            | K::Down
            | K::PageUp
            | K::PageDown
    )
}

/// Normalized pressure: the force relative to its maximum where the hardware
/// measures it, a nominal half press otherwise.
pub(crate) fn pressure(force: f64, max_force: f64, pressed: bool) -> f32 {
    if !pressed {
        0.0
    } else if max_force > 0.0 {
        (force / max_force).clamp(0.0, 1.0) as f32
    } else {
        0.5
    }
}

/// How far a keyboard whose top edge sits at `keyboard_top` overlaps a view
/// whose bottom edge is at `view_bottom`, both in the view's space.
pub(crate) fn keyboard_overlap(view_bottom: f64, keyboard_top: f64) -> f64 {
    (view_bottom - keyboard_top).max(0.0)
}

/// Stable pointer ids for the touches in flight. UIKit keeps one `UITouch`
/// object per finger for the gesture's life, so its address is the key; each
/// gets the smallest free id, starting at 1 to stay clear of
/// [`PointerId::MOUSE`].
#[derive(Default)]
pub(crate) struct TouchIds {
    live: Vec<(usize, u64)>,
}

impl TouchIds {
    /// The id for `touch`, assigning one when it first goes down.
    pub(crate) fn id(&mut self, touch: usize) -> PointerId {
        if let Some(&(_, id)) = self.live.iter().find(|(t, _)| *t == touch) {
            return PointerId(id);
        }
        let id = (1..)
            .find(|id| self.live.iter().all(|(_, used)| used != id))
            .unwrap_or(1);
        self.live.push((touch, id));
        PointerId(id)
    }

    /// Retire `touch` once it has ended or been cancelled.
    pub(crate) fn release(&mut self, touch: usize) {
        self.live.retain(|(t, _)| *t != touch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hid_usages_name_physical_keys() {
        assert_eq!(key_code(0x04), KeyCode::A);
        assert_eq!(key_code(0x1D), KeyCode::Z);
        assert_eq!(key_code(0x1E), KeyCode::Digit1);
        assert_eq!(key_code(0x27), KeyCode::Digit0);
        assert_eq!(key_code(0x3A), KeyCode::F1);
        assert_eq!(key_code(0x73), KeyCode::F24);
        assert_eq!(key_code(0x62), KeyCode::Numpad0);
        assert_eq!(key_code(0x58), KeyCode::Enter);
        assert_eq!(key_code(0xE7), KeyCode::LogoRight);
        assert_eq!(key_code(0x4C), KeyCode::Delete);
        assert_eq!(key_code(0xF0), KeyCode::Other(0xF0));
    }

    #[test]
    fn modifier_flags_map_to_modifiers() {
        let m = modifiers(SHIFT | COMMAND);
        assert!(m.shift && m.logo && !m.control && !m.alt);
        assert!(modifiers(CONTROL | ALTERNATE).control);
        assert!(modifiers(ALTERNATE).alt);
    }

    #[test]
    fn only_non_text_keys_are_command_keys() {
        assert!(is_command_key(KeyCode::Enter));
        assert!(is_command_key(KeyCode::Left));
        assert!(!is_command_key(KeyCode::A));
        assert!(!is_command_key(KeyCode::Space));
        assert!(repeats(KeyCode::Backspace));
        assert!(!repeats(KeyCode::Escape));
    }

    #[test]
    fn touches_take_the_smallest_free_id() {
        let mut ids = TouchIds::default();
        assert_eq!(ids.id(0xA0), PointerId(1));
        assert_eq!(ids.id(0xB0), PointerId(2));
        assert_eq!(ids.id(0xA0), PointerId(1));
        ids.release(0xA0);
        assert_eq!(ids.id(0xC0), PointerId(1));
        assert_eq!(ids.id(0xB0), PointerId(2));
    }

    #[test]
    fn pressure_is_relative_where_measured() {
        assert_eq!(pressure(2.0, 4.0, true), 0.5);
        assert_eq!(pressure(9.0, 4.0, true), 1.0);
        assert_eq!(pressure(0.0, 0.0, true), 0.5);
        assert_eq!(pressure(3.0, 4.0, false), 0.0);
    }

    #[test]
    fn keyboard_overlap_clamps_at_zero() {
        assert_eq!(keyboard_overlap(800.0, 500.0), 300.0);
        assert_eq!(keyboard_overlap(800.0, 900.0), 0.0);
    }
}
