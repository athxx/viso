//! The web backend's pure translation tables: `KeyboardEvent.code` (and, for
//! soft keyboards that leave it empty, `key`) to [`KeyCode`], `PointerEvent`
//! types, buttons and pointer types to phases, kinds and ids, `WheelEvent`
//! deltas to logical points, [`CursorIcon`] to CSS cursors, and the platform
//! strings that make Command the primary modifier. Nothing here touches the
//! DOM, so it is compiled and tested on every host.

#![cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    allow(dead_code)
)]

use crate::event::{
    CursorIcon, KeyCode, Modifiers, PointerButtons, PointerId, PointerKind, PointerPhase,
};

/// Logical points one wheel line scrolls, as on the desktop backends.
pub(crate) const LINE: f64 = 16.0;

/// `WheelEvent.deltaMode` values.
const DOM_DELTA_LINE: u32 = 1;
const DOM_DELTA_PAGE: u32 = 2;

pub(crate) fn modifiers(shift: bool, control: bool, alt: bool, meta: bool) -> Modifiers {
    Modifiers {
        shift,
        control,
        alt,
        logo: meta,
    }
}

const LETTERS: [KeyCode; 26] = {
    use KeyCode::*;
    [
        A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    ]
};

const DIGITS: [KeyCode; 10] = {
    use KeyCode::*;
    [
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
    ]
};

const NUMPAD: [KeyCode; 10] = {
    use KeyCode::*;
    [
        Numpad0, Numpad1, Numpad2, Numpad3, Numpad4, Numpad5, Numpad6, Numpad7, Numpad8, Numpad9,
    ]
};

const FUNCTION: [KeyCode; 24] = {
    use KeyCode::*;
    [
        F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12, F13, F14, F15, F16, F17, F18, F19, F20,
        F21, F22, F23, F24,
    ]
};

/// The key at position `index` of `table`, for a one-character (letter or
/// digit) suffix.
fn indexed(table: &[KeyCode], suffix: &str, first: u8) -> Option<KeyCode> {
    match suffix.as_bytes() {
        [b] => table.get(b.wrapping_sub(first) as usize).copied(),
        _ => None,
    }
}

/// A key from its `KeyboardEvent`: the physical `code` names it; a soft
/// keyboard that leaves `code` empty is matched on the named `key` it sends;
/// anything else keeps the legacy `keyCode`.
pub(crate) fn key_code(code: &str, key: &str, legacy: u32) -> KeyCode {
    physical(code)
        .or_else(|| code.is_empty().then(|| named(key)).flatten())
        .unwrap_or(KeyCode::Other(legacy))
}

fn physical(code: &str) -> Option<KeyCode> {
    use KeyCode as K;
    if let Some(rest) = code.strip_prefix("Key") {
        return indexed(&LETTERS, rest, b'A');
    }
    if let Some(rest) = code.strip_prefix("Digit") {
        return indexed(&DIGITS, rest, b'0');
    }
    if let Some(n) = code.strip_prefix('F').and_then(|n| n.parse::<usize>().ok()) {
        return n.checked_sub(1).and_then(|i| FUNCTION.get(i)).copied();
    }
    if let Some(rest) = code.strip_prefix("Numpad")
        && let Some(k) = indexed(&NUMPAD, rest, b'0')
    {
        return Some(k);
    }
    Some(match code {
        "Escape" => K::Escape,
        "Enter" | "NumpadEnter" => K::Enter,
        "Space" => K::Space,
        "Tab" => K::Tab,
        "Backspace" => K::Backspace,
        "ArrowLeft" => K::Left,
        "ArrowRight" => K::Right,
        "ArrowUp" => K::Up,
        "ArrowDown" => K::Down,
        "Delete" => K::Delete,
        "Home" => K::Home,
        "End" => K::End,
        "PageUp" => K::PageUp,
        "PageDown" => K::PageDown,
        "Insert" => K::Insert,
        "ShiftLeft" => K::ShiftLeft,
        "ShiftRight" => K::ShiftRight,
        "ControlLeft" => K::ControlLeft,
        "ControlRight" => K::ControlRight,
        "AltLeft" => K::AltLeft,
        "AltRight" => K::AltRight,
        "MetaLeft" | "OSLeft" => K::LogoLeft,
        "MetaRight" | "OSRight" => K::LogoRight,
        "CapsLock" => K::CapsLock,
        "Fn" => K::Fn,
        "Backquote" => K::Backquote,
        "Minus" => K::Minus,
        "Equal" => K::Equal,
        "BracketLeft" => K::BracketLeft,
        "BracketRight" => K::BracketRight,
        "Backslash" => K::Backslash,
        "Semicolon" => K::Semicolon,
        "Quote" => K::Quote,
        "Comma" => K::Comma,
        "Period" => K::Period,
        "Slash" => K::Slash,
        "IntlBackslash" => K::IntlBackslash,
        "IntlRo" => K::IntlRo,
        "IntlYen" => K::IntlYen,
        "NumLock" => K::NumLock,
        "NumpadAdd" => K::NumpadAdd,
        "NumpadSubtract" => K::NumpadSubtract,
        "NumpadMultiply" => K::NumpadMultiply,
        "NumpadDivide" => K::NumpadDivide,
        "NumpadDecimal" => K::NumpadDecimal,
        "NumpadEqual" => K::NumpadEqual,
        "NumpadComma" => K::NumpadComma,
        "PrintScreen" => K::PrintScreen,
        "ScrollLock" => K::ScrollLock,
        "Pause" => K::Pause,
        "ContextMenu" => K::ContextMenu,
        "Lang1" => K::Lang1,
        "Lang2" => K::Lang2,
        "Convert" => K::Convert,
        "NonConvert" => K::NonConvert,
        "AudioVolumeUp" | "VolumeUp" => K::VolumeUp,
        "AudioVolumeDown" | "VolumeDown" => K::VolumeDown,
        "AudioVolumeMute" | "VolumeMute" => K::VolumeMute,
        "MediaPlayPause" => K::MediaPlayPause,
        "MediaStop" => K::MediaStop,
        "MediaTrackNext" => K::MediaTrackNext,
        "MediaTrackPrevious" => K::MediaTrackPrevious,
        "BrowserBack" => K::Back,
        _ => return None,
    })
}

/// The editing keys a soft keyboard names in `key` without a `code`.
fn named(key: &str) -> Option<KeyCode> {
    use KeyCode as K;
    Some(match key {
        "Enter" => K::Enter,
        "Backspace" => K::Backspace,
        "Delete" => K::Delete,
        "Tab" => K::Tab,
        "Escape" => K::Escape,
        "ArrowLeft" => K::Left,
        "ArrowRight" => K::Right,
        "ArrowUp" => K::Up,
        "ArrowDown" => K::Down,
        "Home" => K::Home,
        "End" => K::End,
        _ => return None,
    })
}

/// The device class of a `PointerEvent.pointerType`.
pub(crate) fn pointer_kind(pointer_type: &str) -> PointerKind {
    match pointer_type {
        "touch" => PointerKind::Touch,
        "pen" => PointerKind::Pen,
        _ => PointerKind::Mouse,
    }
}

/// The stable id of a pointer: the mouse is [`PointerId::MOUSE`], every
/// other contact its `pointerId` shifted clear of it.
pub(crate) fn pointer_id(kind: PointerKind, id: i32) -> PointerId {
    match kind {
        PointerKind::Mouse => PointerId::MOUSE,
        _ => PointerId(u64::from(id as u32) + 1),
    }
}

/// `MouseEvent.buttons` to pointer buttons: the primary, secondary (a pen's
/// barrel button) and middle bits line up with [`PointerButtons`].
pub(crate) fn buttons(mask: u16) -> PointerButtons {
    PointerButtons((mask & 0b111) as u8)
}

/// The phase of a pointer event of type `kind`. `button` is the event's
/// `button` (`-1` when no button changed), `before` the buttons last
/// reported for the pointer, `now` the event's. A mouse reports only its
/// first press and last release as `pointerdown`/`pointerup`; the buttons
/// pressed and released in between arrive as `pointermove`s with a
/// `button`, and become `Down`/`Up` here. `None` drops the event: a touch
/// leaving after it lifted.
pub(crate) fn pointer_phase(
    kind: &str,
    pointer: PointerKind,
    button: i16,
    before: PointerButtons,
    now: PointerButtons,
) -> Option<PointerPhase> {
    Some(match kind {
        "pointerdown" => PointerPhase::Down,
        "pointerup" => PointerPhase::Up,
        "pointercancel" => PointerPhase::Cancel,
        "pointerleave" if pointer != PointerKind::Touch => PointerPhase::Left,
        "pointermove" if button >= 0 && now.0 & !before.0 != 0 => PointerPhase::Down,
        "pointermove" if button >= 0 && before.0 & !now.0 != 0 => PointerPhase::Up,
        "pointermove" => PointerPhase::Moved,
        _ => return None,
    })
}

/// Remember `now` as pointer `id`'s buttons, or forget a contact that ended.
pub(crate) fn track_buttons(
    tracked: &mut Vec<(i32, PointerButtons)>,
    id: i32,
    now: PointerButtons,
    ended: bool,
) {
    let slot = tracked.iter().position(|(p, _)| *p == id);
    match (slot, ended) {
        (Some(i), true) => {
            tracked.swap_remove(i);
        }
        (Some(i), false) => tracked[i].1 = now,
        (None, false) => tracked.push((id, now)),
        (None, true) => {}
    }
}

/// A computed CSS length (`"12px"`) in pixels, or 0.
pub(crate) fn css_px(value: &str) -> f64 {
    value
        .trim()
        .strip_suffix("px")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

/// A `WheelEvent`'s deltas in logical points, signed as on the other
/// backends: positive scrolls content down and right (the DOM's positive
/// deltas scroll the page the other way). `page` is the logical size one
/// page-scroll covers.
pub(crate) fn wheel_delta(dx: f64, dy: f64, mode: u32, page: (f64, f64)) -> (f64, f64) {
    let (sx, sy) = match mode {
        DOM_DELTA_LINE => (LINE, LINE),
        DOM_DELTA_PAGE => page,
        _ => (1.0, 1.0),
    };
    (-dx * sx, -dy * sy)
}

/// The CSS `cursor` value for an icon.
pub(crate) fn css_cursor(icon: CursorIcon) -> &'static str {
    use CursorIcon as C;
    match icon {
        C::Default => "default",
        C::Pointer => "pointer",
        C::Text => "text",
        C::VerticalText => "vertical-text",
        C::Crosshair => "crosshair",
        C::Move => "move",
        C::Grab => "grab",
        C::Grabbing => "grabbing",
        C::NotAllowed => "not-allowed",
        C::Wait => "wait",
        C::Progress => "progress",
        C::Help => "help",
        C::ContextMenu => "context-menu",
        C::Copy => "copy",
        C::Alias => "alias",
        C::ZoomIn => "zoom-in",
        C::ZoomOut => "zoom-out",
        C::ResizeEw => "ew-resize",
        C::ResizeNs => "ns-resize",
        C::ResizeNesw => "nesw-resize",
        C::ResizeNwse => "nwse-resize",
        C::ResizeCol => "col-resize",
        C::ResizeRow => "row-resize",
        C::Hidden => "none",
    }
}

/// Whether the page runs on an Apple platform, where Command (not Control)
/// is the primary shortcut modifier. `platform` is
/// `navigator.userAgentData.platform` or, without it, `navigator.platform`.
pub(crate) fn is_apple(platform: &str) -> bool {
    ["Mac", "iPhone", "iPad", "iPod", "iOS"]
        .iter()
        .any(|p| platform.starts_with(p))
        || platform == "macOS"
}

/// A CSS length in physical pixels.
pub(crate) fn pixels(css: f64, scale: f64) -> u32 {
    (css * scale).round().clamp(0.0, f64::from(u32::MAX)) as u32
}

/// The logical height the on-screen keyboard covers: the part of the
/// layout viewport (`layout` tall) below the visual viewport (`visual` tall,
/// `offset_top` down). Sub-point slivers from rounding count as none.
pub(crate) fn keyboard_inset(layout: f64, visual: f64, offset_top: f64) -> f64 {
    let covered = layout - (visual + offset_top);
    if covered >= 1.0 { covered } else { 0.0 }
}

/// Whether the page takes a key press from the browser (cancels its default
/// action): Tab and Alt stay in the page, and shortcuts save, print or find
/// nothing. Clipboard shortcuts are left alone so the browser's `copy`,
/// `cut` and `paste` events fire with clipboard access, and so is typing,
/// AltGr (Control and Alt together) included, so text still reaches the
/// input element.
pub(crate) fn page_claims(code: KeyCode, m: Modifiers) -> bool {
    if crate::event::clipboard_shortcut(code, m).is_some() {
        return false;
    }
    let alt_gr = m.control && m.alt;
    matches!(code, KeyCode::Tab | KeyCode::AltLeft | KeyCode::AltRight)
        || ((m.control || m.logo) && !alt_gr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_name_physical_keys() {
        assert_eq!(key_code("KeyA", "q", 81), KeyCode::A);
        assert_eq!(key_code("KeyZ", "z", 90), KeyCode::Z);
        assert_eq!(key_code("Digit0", "0", 48), KeyCode::Digit0);
        assert_eq!(key_code("Digit9", "(", 57), KeyCode::Digit9);
        assert_eq!(key_code("F1", "F1", 112), KeyCode::F1);
        assert_eq!(key_code("F24", "F24", 135), KeyCode::F24);
        assert_eq!(key_code("Numpad7", "7", 103), KeyCode::Numpad7);
        assert_eq!(key_code("NumpadEnter", "Enter", 13), KeyCode::Enter);
        assert_eq!(key_code("ArrowUp", "ArrowUp", 38), KeyCode::Up);
        assert_eq!(key_code("MetaLeft", "Meta", 91), KeyCode::LogoLeft);
        assert_eq!(key_code("OSRight", "OS", 92), KeyCode::LogoRight);
        assert_eq!(key_code("AudioVolumeUp", "", 175), KeyCode::VolumeUp);
        assert_eq!(key_code("BrowserBack", "", 166), KeyCode::Back);
        assert_eq!(key_code("IntlYen", "\\", 220), KeyCode::IntlYen);
    }

    #[test]
    fn malformed_codes_fall_back_to_the_legacy_code() {
        assert_eq!(key_code("KeyAA", "", 1), KeyCode::Other(1));
        assert_eq!(key_code("F0", "", 2), KeyCode::Other(2));
        assert_eq!(key_code("F25", "", 3), KeyCode::Other(3));
        assert_eq!(key_code("Keya", "", 4), KeyCode::Other(4));
        assert_eq!(key_code("Unidentified", "Enter", 229), KeyCode::Other(229));
    }

    #[test]
    fn soft_keyboards_are_matched_on_the_key() {
        assert_eq!(key_code("", "Enter", 13), KeyCode::Enter);
        assert_eq!(key_code("", "Backspace", 8), KeyCode::Backspace);
        assert_eq!(key_code("", "Unidentified", 229), KeyCode::Other(229));
        assert_eq!(key_code("", "a", 229), KeyCode::Other(229));
    }

    #[test]
    fn pointer_types_map_to_kinds_and_ids() {
        assert_eq!(pointer_kind("mouse"), PointerKind::Mouse);
        assert_eq!(pointer_kind("touch"), PointerKind::Touch);
        assert_eq!(pointer_kind("pen"), PointerKind::Pen);
        assert_eq!(pointer_kind(""), PointerKind::Mouse);
        assert_eq!(pointer_id(PointerKind::Mouse, 1), PointerId::MOUSE);
        assert_eq!(pointer_id(PointerKind::Touch, 0), PointerId(1));
        assert_eq!(pointer_id(PointerKind::Pen, 7), PointerId(8));
    }

    #[test]
    fn button_masks_map_to_buttons() {
        assert_eq!(buttons(0), PointerButtons::NONE);
        assert_eq!(buttons(1), PointerButtons::PRIMARY);
        assert_eq!(buttons(2), PointerButtons::SECONDARY);
        assert_eq!(buttons(4), PointerButtons::MIDDLE);
        // Back, forward and eraser have no button of their own.
        assert_eq!(buttons(1 | 8 | 16 | 32), PointerButtons::PRIMARY);
    }

    #[test]
    fn mouse_chords_report_extra_buttons() {
        let (p, m) = (PointerButtons::PRIMARY, PointerKind::Mouse);
        let both = PointerButtons(PointerButtons::PRIMARY.0 | PointerButtons::SECONDARY.0);
        assert_eq!(
            pointer_phase("pointermove", m, 2, p, both),
            Some(PointerPhase::Down)
        );
        assert_eq!(
            pointer_phase("pointermove", m, 2, both, p),
            Some(PointerPhase::Up)
        );
        assert_eq!(
            pointer_phase("pointermove", m, -1, p, p),
            Some(PointerPhase::Moved)
        );
        assert_eq!(
            pointer_phase("pointerdown", m, 0, PointerButtons::NONE, p),
            Some(PointerPhase::Down)
        );
        assert_eq!(
            pointer_phase("pointerup", m, 0, p, PointerButtons::NONE),
            Some(PointerPhase::Up)
        );
    }

    #[test]
    fn only_hovering_pointers_leave() {
        let none = PointerButtons::NONE;
        assert_eq!(
            pointer_phase("pointerleave", PointerKind::Mouse, -1, none, none),
            Some(PointerPhase::Left)
        );
        assert_eq!(
            pointer_phase("pointerleave", PointerKind::Pen, -1, none, none),
            Some(PointerPhase::Left)
        );
        assert_eq!(
            pointer_phase("pointerleave", PointerKind::Touch, -1, none, none),
            None
        );
        assert_eq!(
            pointer_phase("pointercancel", PointerKind::Touch, -1, none, none),
            Some(PointerPhase::Cancel)
        );
        assert_eq!(
            pointer_phase("pointerover", PointerKind::Mouse, -1, none, none),
            None
        );
    }

    #[test]
    fn wheel_deltas_become_logical_points() {
        assert_eq!(wheel_delta(0.0, 120.0, 0, (0.0, 0.0)), (-0.0, -120.0));
        assert_eq!(wheel_delta(1.0, 3.0, 1, (0.0, 0.0)), (-16.0, -48.0));
        assert_eq!(wheel_delta(0.0, 1.0, 2, (800.0, 600.0)), (-0.0, -600.0));
        assert_eq!(wheel_delta(-1.0, 0.0, 2, (800.0, 600.0)), (800.0, -0.0));
    }

    #[test]
    fn cursors_map_to_css() {
        assert_eq!(css_cursor(CursorIcon::Default), "default");
        assert_eq!(css_cursor(CursorIcon::ResizeNwse), "nwse-resize");
        assert_eq!(css_cursor(CursorIcon::Hidden), "none");
        assert_eq!(css_cursor(CursorIcon::ContextMenu), "context-menu");
    }

    #[test]
    fn apple_platforms_are_recognized() {
        for p in ["MacIntel", "macOS", "iPhone", "iPad", "iPod touch"] {
            assert!(is_apple(p), "{p}");
        }
        for p in [
            "Win32",
            "Windows",
            "Linux x86_64",
            "Android",
            "Chrome OS",
            "",
        ] {
            assert!(!is_apple(p), "{p}");
        }
    }

    #[test]
    fn css_lengths_round_to_pixels() {
        assert_eq!(pixels(100.0, 2.0), 200);
        assert_eq!(pixels(100.3, 1.5), 150);
        assert_eq!(pixels(-4.0, 2.0), 0);
        assert_eq!(pixels(f64::NAN, 2.0), 0);
    }

    #[test]
    fn the_keyboard_covers_what_the_visual_viewport_lost() {
        assert_eq!(keyboard_inset(800.0, 500.0, 0.0), 300.0);
        assert_eq!(keyboard_inset(800.0, 500.0, 100.0), 200.0);
        assert_eq!(keyboard_inset(800.0, 799.6, 0.0), 0.0);
        assert_eq!(keyboard_inset(800.0, 900.0, 0.0), 0.0);
    }

    #[test]
    fn the_page_claims_shortcuts_but_not_clipboard_or_typing() {
        let control = modifiers(false, true, false, false);
        let logo = modifiers(false, false, false, true);
        let primary = if logo.is_primary() { logo } else { control };
        assert!(!page_claims(KeyCode::C, primary));
        assert!(!page_claims(KeyCode::V, primary));
        assert!(page_claims(KeyCode::S, primary));
        assert!(page_claims(KeyCode::Tab, Modifiers::default()));
        assert!(page_claims(KeyCode::AltLeft, Modifiers::default()));
        assert!(!page_claims(KeyCode::A, Modifiers::default()));
        assert!(!page_claims(
            KeyCode::Q,
            modifiers(false, true, true, false)
        ));
        assert!(!page_claims(KeyCode::Enter, Modifiers::default()));
    }

    #[test]
    fn contacts_are_tracked_until_they_end() {
        let mut tracked = Vec::new();
        track_buttons(&mut tracked, 3, PointerButtons::PRIMARY, false);
        track_buttons(&mut tracked, 5, PointerButtons::PRIMARY, false);
        track_buttons(&mut tracked, 3, PointerButtons::NONE, false);
        assert_eq!(
            tracked,
            [(3, PointerButtons::NONE), (5, PointerButtons::PRIMARY)]
        );
        track_buttons(&mut tracked, 3, PointerButtons::NONE, true);
        track_buttons(&mut tracked, 9, PointerButtons::NONE, true);
        assert_eq!(tracked, [(5, PointerButtons::PRIMARY)]);
    }

    #[test]
    fn computed_lengths_parse_as_pixels() {
        assert_eq!(css_px("12px"), 12.0);
        assert_eq!(css_px(" 0.5px "), 0.5);
        assert_eq!(css_px("auto"), 0.0);
        assert_eq!(css_px(""), 0.0);
    }
}
