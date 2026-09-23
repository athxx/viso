//! The Android backend's pure translation tables: `AKEYCODE_*` to
//! [`KeyCode`], `META_*` bits to [`Modifiers`], `MotionEvent` actions and tool
//! types to pointer phases and kinds, button state, pixel insets to logical
//! ones, and [`CursorIcon`] to `PointerIcon` types. Nothing here calls the
//! JVM, so it is compiled and tested on every host.

#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use crate::event::{
    CursorIcon, Insets, KeyCode, Modifiers, PointerButtons, PointerId, PointerKind, PointerPhase,
};

/// `KeyEvent.META_*` bits.
const META_SHIFT_ON: i32 = 0x1;
const META_ALT_ON: i32 = 0x2;
const META_CTRL_ON: i32 = 0x1000;
const META_META_ON: i32 = 0x10000;

pub(crate) fn modifiers(meta: i32) -> Modifiers {
    Modifiers {
        shift: meta & META_SHIFT_ON != 0,
        control: meta & META_CTRL_ON != 0,
        alt: meta & META_ALT_ON != 0,
        logo: meta & META_META_ON != 0,
    }
}

/// Whether a key's character should be left out of the text stream: with
/// Control or Meta held the press is a shortcut, not typing.
pub(crate) fn is_shortcut(meta: i32) -> bool {
    meta & (META_CTRL_ON | META_META_ON) != 0
}

/// A key from its `KeyEvent.getKeyCode()`.
pub(crate) fn key_code(code: i32) -> KeyCode {
    use KeyCode as K;
    const DIGITS: [KeyCode; 10] = [
        K::Digit0,
        K::Digit1,
        K::Digit2,
        K::Digit3,
        K::Digit4,
        K::Digit5,
        K::Digit6,
        K::Digit7,
        K::Digit8,
        K::Digit9,
    ];
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
    const NUMPAD: [KeyCode; 10] = [
        K::Numpad0,
        K::Numpad1,
        K::Numpad2,
        K::Numpad3,
        K::Numpad4,
        K::Numpad5,
        K::Numpad6,
        K::Numpad7,
        K::Numpad8,
        K::Numpad9,
    ];
    let index = |base: i32| (code - base) as usize;
    match code {
        4 => K::Back,
        7..=16 => DIGITS[index(7)],
        19 => K::Up,
        20 => K::Down,
        21 => K::Left,
        22 => K::Right,
        23 | 66 | 160 => K::Enter,
        24 => K::VolumeUp,
        25 => K::VolumeDown,
        29..=54 => LETTERS[index(29)],
        55 => K::Comma,
        56 => K::Period,
        57 => K::AltLeft,
        58 => K::AltRight,
        59 => K::ShiftLeft,
        60 => K::ShiftRight,
        61 => K::Tab,
        62 => K::Space,
        67 => K::Backspace,
        68 => K::Backquote,
        69 => K::Minus,
        70 => K::Equal,
        71 => K::BracketLeft,
        72 => K::BracketRight,
        73 => K::Backslash,
        74 => K::Semicolon,
        75 => K::Quote,
        76 => K::Slash,
        82 => K::ContextMenu,
        85 => K::MediaPlayPause,
        86 => K::MediaStop,
        87 => K::MediaTrackNext,
        88 => K::MediaTrackPrevious,
        92 => K::PageUp,
        93 => K::PageDown,
        111 => K::Escape,
        112 => K::Delete,
        113 => K::ControlLeft,
        114 => K::ControlRight,
        115 => K::CapsLock,
        116 => K::ScrollLock,
        117 => K::LogoLeft,
        118 => K::LogoRight,
        119 => K::Fn,
        120 => K::PrintScreen,
        121 => K::Pause,
        122 => K::Home,
        123 => K::End,
        124 => K::Insert,
        131..=142 => FUNCTION[index(131)],
        143 => K::NumLock,
        144..=153 => NUMPAD[index(144)],
        154 => K::NumpadDivide,
        155 => K::NumpadMultiply,
        156 => K::NumpadSubtract,
        157 => K::NumpadAdd,
        158 => K::NumpadDecimal,
        159 => K::NumpadComma,
        161 => K::NumpadEqual,
        164 => K::VolumeMute,
        212 => K::Lang2,
        213 => K::NonConvert,
        214 => K::Convert,
        216 => K::IntlYen,
        217 => K::IntlRo,
        218 => K::Lang1,
        other => K::Other(other as u32),
    }
}

/// `MotionEvent` action codes (the masked part of `getActionMasked()`).
pub(crate) mod action {
    pub const DOWN: i32 = 0;
    pub const UP: i32 = 1;
    pub const MOVE: i32 = 2;
    pub const CANCEL: i32 = 3;
    pub const POINTER_DOWN: i32 = 5;
    pub const POINTER_UP: i32 = 6;
    pub const HOVER_MOVE: i32 = 7;
    pub const HOVER_ENTER: i32 = 9;
    pub const HOVER_EXIT: i32 = 10;
    pub const BUTTON_PRESS: i32 = 11;
    pub const BUTTON_RELEASE: i32 = 12;
}

/// The device class of a `MotionEvent.getToolType()`.
pub(crate) fn pointer_kind(tool: i32) -> PointerKind {
    match tool {
        2 | 4 => PointerKind::Pen,
        3 => PointerKind::Mouse,
        _ => PointerKind::Touch,
    }
}

/// The stable id of a pointer: the mouse is [`PointerId::MOUSE`], every
/// other contact its `getPointerId` shifted clear of it.
pub(crate) fn pointer_id(kind: PointerKind, id: i32) -> PointerId {
    match kind {
        PointerKind::Mouse => PointerId::MOUSE,
        _ => PointerId(id as u64 + 1),
    }
}

/// `MotionEvent.getButtonState()` bits to pointer buttons. The stylus
/// primary button counts as secondary, its secondary as middle.
pub(crate) fn buttons(state: i32) -> PointerButtons {
    const PRIMARY: i32 = 1;
    const SECONDARY: i32 = 2;
    const TERTIARY: i32 = 4;
    const STYLUS_PRIMARY: i32 = 32;
    const STYLUS_SECONDARY: i32 = 64;
    let mut out = 0;
    if state & PRIMARY != 0 {
        out |= PointerButtons::PRIMARY.0;
    }
    if state & (SECONDARY | STYLUS_PRIMARY) != 0 {
        out |= PointerButtons::SECONDARY.0;
    }
    if state & (TERTIARY | STYLUS_SECONDARY) != 0 {
        out |= PointerButtons::MIDDLE.0;
    }
    PointerButtons(out)
}

/// The phase of pointer `index` in a `MotionEvent` whose masked action is
/// `action` and whose action index is `action_index`. `reported` is the
/// mouse buttons last reported, `now` the event's. `None` drops the sample:
/// the first button's press and the last one's release are already reported
/// by the `DOWN`/`UP` Android sends with them.
pub(crate) fn phase(
    action: i32,
    action_index: usize,
    index: usize,
    kind: PointerKind,
    reported: PointerButtons,
    now: PointerButtons,
) -> Option<PointerPhase> {
    use self::action as a;
    let is_actor = index == action_index;
    Some(match action {
        a::DOWN | a::POINTER_DOWN if is_actor => PointerPhase::Down,
        a::UP | a::POINTER_UP if is_actor => PointerPhase::Up,
        a::DOWN | a::POINTER_DOWN | a::UP | a::POINTER_UP | a::MOVE => PointerPhase::Moved,
        a::CANCEL => PointerPhase::Cancel,
        a::HOVER_MOVE | a::HOVER_ENTER => PointerPhase::Moved,
        a::HOVER_EXIT => PointerPhase::Left,
        a::BUTTON_PRESS if kind == PointerKind::Mouse && now != reported => PointerPhase::Down,
        a::BUTTON_RELEASE if kind == PointerKind::Mouse && !now.is_empty() && now != reported => {
            PointerPhase::Up
        }
        _ => return None,
    })
}

/// Whether a pointer sample of a `MotionEvent` with masked `action` is a
/// contact that is down (not hovering), so it carries the primary button
/// and its pressure.
pub(crate) fn touching(action: i32, kind: PointerKind, phase: PointerPhase) -> bool {
    use self::action as a;
    let hovering = matches!(action, a::HOVER_MOVE | a::HOVER_ENTER | a::HOVER_EXIT);
    kind != PointerKind::Mouse
        && !hovering
        && matches!(phase, PointerPhase::Down | PointerPhase::Moved)
}

/// Physical-pixel insets to logical points.
pub(crate) fn insets(px: [i32; 4], density: f64) -> Insets {
    let d = if density > 0.0 { density } else { 1.0 };
    let [top, left, bottom, right] = px.map(|v| f64::from(v.max(0)) / d);
    Insets {
        top,
        left,
        bottom,
        right,
    }
}

/// The `PointerIcon.TYPE_*` for a cursor.
pub(crate) fn pointer_icon(icon: CursorIcon) -> i32 {
    use CursorIcon as C;
    match icon {
        C::Default => 1000,
        C::ContextMenu => 1001,
        C::Pointer => 1002,
        C::Help => 1003,
        C::Wait | C::Progress => 1004,
        C::Crosshair => 1007,
        C::Text => 1008,
        C::VerticalText => 1009,
        C::Alias => 1010,
        C::Copy => 1011,
        C::NotAllowed => 1012,
        C::Move => 1013,
        C::ResizeEw | C::ResizeCol => 1014,
        C::ResizeNs | C::ResizeRow => 1015,
        C::ResizeNesw => 1016,
        C::ResizeNwse => 1017,
        C::ZoomIn => 1018,
        C::ZoomOut => 1019,
        C::Grab => 1020,
        C::Grabbing => 1021,
        C::Hidden => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycodes_name_physical_keys() {
        assert_eq!(key_code(29), KeyCode::A);
        assert_eq!(key_code(54), KeyCode::Z);
        assert_eq!(key_code(7), KeyCode::Digit0);
        assert_eq!(key_code(16), KeyCode::Digit9);
        assert_eq!(key_code(131), KeyCode::F1);
        assert_eq!(key_code(142), KeyCode::F12);
        assert_eq!(key_code(144), KeyCode::Numpad0);
        assert_eq!(key_code(160), KeyCode::Enter);
        assert_eq!(key_code(67), KeyCode::Backspace);
        assert_eq!(key_code(112), KeyCode::Delete);
        assert_eq!(key_code(4), KeyCode::Back);
        assert_eq!(key_code(218), KeyCode::Lang1);
        assert_eq!(key_code(999), KeyCode::Other(999));
    }

    #[test]
    fn meta_state_maps_to_modifiers() {
        let m = modifiers(META_SHIFT_ON | META_META_ON);
        assert!(m.shift && m.logo && !m.control && !m.alt);
        assert!(modifiers(META_CTRL_ON).control);
        assert!(modifiers(META_ALT_ON).alt);
        assert!(is_shortcut(META_CTRL_ON));
        assert!(!is_shortcut(META_SHIFT_ON | META_ALT_ON));
    }

    #[test]
    fn tools_map_to_kinds_and_ids() {
        assert_eq!(pointer_kind(1), PointerKind::Touch);
        assert_eq!(pointer_kind(2), PointerKind::Pen);
        assert_eq!(pointer_kind(4), PointerKind::Pen);
        assert_eq!(pointer_kind(3), PointerKind::Mouse);
        assert_eq!(pointer_kind(0), PointerKind::Touch);
        assert_eq!(pointer_id(PointerKind::Mouse, 5), PointerId::MOUSE);
        assert_eq!(pointer_id(PointerKind::Touch, 0), PointerId(1));
    }

    #[test]
    fn button_state_maps_to_buttons() {
        assert_eq!(buttons(1), PointerButtons::PRIMARY);
        assert_eq!(buttons(2 | 4).0, 0b110);
        assert_eq!(buttons(32), PointerButtons::SECONDARY);
        assert!(buttons(0).is_empty());
    }

    #[test]
    fn only_the_acting_pointer_goes_down_or_up() {
        use action as a;
        let n = PointerButtons::NONE;
        let t = PointerKind::Touch;
        assert_eq!(
            phase(a::POINTER_DOWN, 1, 1, t, n, n),
            Some(PointerPhase::Down)
        );
        assert_eq!(
            phase(a::POINTER_DOWN, 1, 0, t, n, n),
            Some(PointerPhase::Moved)
        );
        assert_eq!(phase(a::POINTER_UP, 0, 0, t, n, n), Some(PointerPhase::Up));
        assert_eq!(phase(a::MOVE, 0, 2, t, n, n), Some(PointerPhase::Moved));
        assert_eq!(phase(a::CANCEL, 0, 1, t, n, n), Some(PointerPhase::Cancel));
        assert_eq!(
            phase(a::HOVER_EXIT, 0, 0, t, n, n),
            Some(PointerPhase::Left)
        );
    }

    #[test]
    fn mouse_chords_report_extra_buttons() {
        use action as a;
        let m = PointerKind::Mouse;
        let (n, p) = (PointerButtons::NONE, PointerButtons::PRIMARY);
        let both = PointerButtons(p.0 | PointerButtons::SECONDARY.0);
        // DOWN already reported the first button; its BUTTON_PRESS repeats it.
        assert_eq!(phase(a::BUTTON_PRESS, 0, 0, m, p, p), None);
        assert_eq!(
            phase(a::BUTTON_PRESS, 0, 0, m, p, both),
            Some(PointerPhase::Down)
        );
        assert_eq!(
            phase(a::BUTTON_RELEASE, 0, 0, m, both, p),
            Some(PointerPhase::Up)
        );
        // The last button's release is the UP that follows.
        assert_eq!(phase(a::BUTTON_RELEASE, 0, 0, m, p, n), None);
        assert_eq!(phase(a::BUTTON_PRESS, 0, 0, PointerKind::Touch, n, p), None);
    }

    #[test]
    fn insets_scale_by_density() {
        let i = insets([96, 0, 48, -3], 3.0);
        assert_eq!((i.top, i.left, i.bottom, i.right), (32.0, 0.0, 16.0, 0.0));
        assert_eq!(insets([10, 0, 0, 0], 0.0).top, 10.0);
    }

    #[test]
    fn cursors_map_to_pointer_icons() {
        assert_eq!(pointer_icon(CursorIcon::Default), 1000);
        assert_eq!(pointer_icon(CursorIcon::Text), 1008);
        assert_eq!(pointer_icon(CursorIcon::ResizeCol), 1014);
        assert_eq!(pointer_icon(CursorIcon::Grabbing), 1021);
        assert_eq!(pointer_icon(CursorIcon::Hidden), 0);
    }

    #[test]
    fn only_down_contacts_touch() {
        let (pen, touch, mouse) = (PointerKind::Pen, PointerKind::Touch, PointerKind::Mouse);
        assert!(touching(action::DOWN, touch, PointerPhase::Down));
        assert!(touching(action::MOVE, pen, PointerPhase::Moved));
        assert!(!touching(action::UP, touch, PointerPhase::Up));
        assert!(!touching(action::HOVER_MOVE, pen, PointerPhase::Moved));
        assert!(!touching(action::MOVE, mouse, PointerPhase::Moved));
    }
}
