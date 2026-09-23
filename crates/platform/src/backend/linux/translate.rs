//! The Linux backends' pure translation tables, shared by X11 and Wayland:
//! evdev scancodes to [`KeyCode`], cursor icons to theme names and
//! `cursor-shape-v1` shapes, wheel steps to logical points, monitor geometry
//! to scale factors, the appearance portal's values, IME caret offsets, and
//! the menu tree to an accelerator table. Nothing here calls the OS, so it is
//! compiled and tested on every host.

#![cfg_attr(
    not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )),
    allow(dead_code)
)]

use crate::event::{Appearance, ColorScheme, CursorIcon, KeyCode, Modifiers};
use crate::menu::{Accel, Menu, MenuCommandId, SystemAction};

/// Logical points one wheel line scrolls.
pub(crate) const LINE: f64 = 16.0;

/// Lines one wheel notch scrolls, the GTK and Qt default.
pub(crate) const LINES_PER_NOTCH: f64 = 3.0;

/// XKB keycodes are evdev scancodes offset by 8.
pub(crate) const XKB_EVDEV_OFFSET: u32 = 8;

/// The physical key behind an XKB keycode.
pub(crate) fn key_code(xkb_keycode: u32) -> KeyCode {
    let evdev = xkb_keycode.wrapping_sub(XKB_EVDEV_OFFSET);
    key_from_evdev(evdev).unwrap_or(KeyCode::Other(evdev))
}

/// Linux `input-event-codes.h` scancodes, by their W3C `code` names.
fn key_from_evdev(code: u32) -> Option<KeyCode> {
    use KeyCode as K;
    Some(match code {
        1 => K::Escape,
        2 => K::Digit1,
        3 => K::Digit2,
        4 => K::Digit3,
        5 => K::Digit4,
        6 => K::Digit5,
        7 => K::Digit6,
        8 => K::Digit7,
        9 => K::Digit8,
        10 => K::Digit9,
        11 => K::Digit0,
        12 => K::Minus,
        13 => K::Equal,
        14 => K::Backspace,
        15 => K::Tab,
        16 => K::Q,
        17 => K::W,
        18 => K::E,
        19 => K::R,
        20 => K::T,
        21 => K::Y,
        22 => K::U,
        23 => K::I,
        24 => K::O,
        25 => K::P,
        26 => K::BracketLeft,
        27 => K::BracketRight,
        28 | 96 => K::Enter,
        29 => K::ControlLeft,
        30 => K::A,
        31 => K::S,
        32 => K::D,
        33 => K::F,
        34 => K::G,
        35 => K::H,
        36 => K::J,
        37 => K::K,
        38 => K::L,
        39 => K::Semicolon,
        40 => K::Quote,
        41 => K::Backquote,
        42 => K::ShiftLeft,
        43 => K::Backslash,
        44 => K::Z,
        45 => K::X,
        46 => K::C,
        47 => K::V,
        48 => K::B,
        49 => K::N,
        50 => K::M,
        51 => K::Comma,
        52 => K::Period,
        53 => K::Slash,
        54 => K::ShiftRight,
        55 => K::NumpadMultiply,
        56 => K::AltLeft,
        57 => K::Space,
        58 => K::CapsLock,
        59 => K::F1,
        60 => K::F2,
        61 => K::F3,
        62 => K::F4,
        63 => K::F5,
        64 => K::F6,
        65 => K::F7,
        66 => K::F8,
        67 => K::F9,
        68 => K::F10,
        69 => K::NumLock,
        70 => K::ScrollLock,
        71 => K::Numpad7,
        72 => K::Numpad8,
        73 => K::Numpad9,
        74 => K::NumpadSubtract,
        75 => K::Numpad4,
        76 => K::Numpad5,
        77 => K::Numpad6,
        78 => K::NumpadAdd,
        79 => K::Numpad1,
        80 => K::Numpad2,
        81 => K::Numpad3,
        82 => K::Numpad0,
        83 => K::NumpadDecimal,
        86 => K::IntlBackslash,
        87 => K::F11,
        88 => K::F12,
        89 => K::IntlRo,
        92 => K::Convert,
        94 => K::NonConvert,
        95 | 121 => K::NumpadComma,
        97 => K::ControlRight,
        98 => K::NumpadDivide,
        99 => K::PrintScreen,
        100 => K::AltRight,
        102 => K::Home,
        103 => K::Up,
        104 => K::PageUp,
        105 => K::Left,
        106 => K::Right,
        107 => K::End,
        108 => K::Down,
        109 => K::PageDown,
        110 => K::Insert,
        111 => K::Delete,
        113 => K::VolumeMute,
        114 => K::VolumeDown,
        115 => K::VolumeUp,
        117 => K::NumpadEqual,
        119 => K::Pause,
        122 => K::Lang1,
        123 => K::Lang2,
        124 => K::IntlYen,
        125 => K::LogoLeft,
        126 => K::LogoRight,
        127 => K::ContextMenu,
        158 => K::Back,
        163 => K::MediaTrackNext,
        164 => K::MediaPlayPause,
        165 => K::MediaTrackPrevious,
        166 => K::MediaStop,
        183 => K::F13,
        184 => K::F14,
        185 => K::F15,
        186 => K::F16,
        187 => K::F17,
        188 => K::F18,
        189 => K::F19,
        190 => K::F20,
        191 => K::F21,
        192 => K::F22,
        193 => K::F23,
        194 => K::F24,
        464 => K::Fn,
        _ => return None,
    })
}

/// Whether `text` from the keymap is insertable: control characters (the
/// `\u{1}` Ctrl+A produces, Backspace's `\u{8}`, Delete's `\u{7f}`) are keys,
/// not text.
pub(crate) fn is_text(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(char::is_control)
}

/// Xcursor theme names for `icon`, most specific first: the CSS name every
/// current theme ships, then the legacy X cursor-font names older themes use.
/// Empty for [`CursorIcon::Hidden`].
pub(crate) fn cursor_names(icon: CursorIcon) -> &'static [&'static str] {
    use CursorIcon as C;
    match icon {
        C::Default => &["default", "left_ptr"],
        C::Pointer => &["pointer", "hand2", "hand1"],
        C::Text => &["text", "xterm"],
        C::VerticalText => &["vertical-text", "text", "xterm"],
        C::Crosshair => &["crosshair", "cross"],
        C::Move => &["move", "fleur"],
        C::Grab => &["grab", "openhand", "hand1"],
        C::Grabbing => &["grabbing", "closedhand", "fleur"],
        C::NotAllowed => &["not-allowed", "crossed_circle", "circle"],
        C::Wait => &["wait", "watch"],
        C::Progress => &["progress", "left_ptr_watch", "watch"],
        C::Help => &["help", "question_arrow"],
        C::ContextMenu => &["context-menu", "left_ptr"],
        C::Copy => &["copy", "left_ptr"],
        C::Alias => &["alias", "link", "left_ptr"],
        C::ZoomIn => &["zoom-in", "left_ptr"],
        C::ZoomOut => &["zoom-out", "left_ptr"],
        C::ResizeEw => &["ew-resize", "sb_h_double_arrow"],
        C::ResizeNs => &["ns-resize", "sb_v_double_arrow"],
        C::ResizeNesw => &["nesw-resize", "fd_double_arrow", "bottom_left_corner"],
        C::ResizeNwse => &["nwse-resize", "bd_double_arrow", "bottom_right_corner"],
        C::ResizeCol => &["col-resize", "sb_h_double_arrow"],
        C::ResizeRow => &["row-resize", "sb_v_double_arrow"],
        C::Hidden => &[],
    }
}

/// The `wp_cursor_shape_device_v1.shape` value for `icon`; `None` hides the
/// cursor.
pub(crate) fn cursor_shape(icon: CursorIcon) -> Option<u32> {
    use CursorIcon as C;
    Some(match icon {
        C::Default => 1,
        C::ContextMenu => 2,
        C::Help => 3,
        C::Pointer => 4,
        C::Progress => 5,
        C::Wait => 6,
        C::Crosshair => 8,
        C::Text => 9,
        C::VerticalText => 10,
        C::Alias => 11,
        C::Copy => 12,
        C::Move => 13,
        C::NotAllowed => 15,
        C::Grab => 16,
        C::Grabbing => 17,
        C::ResizeEw => 26,
        C::ResizeNs => 27,
        C::ResizeNesw => 28,
        C::ResizeNwse => 29,
        C::ResizeCol => 30,
        C::ResizeRow => 31,
        C::ZoomIn => 33,
        C::ZoomOut => 34,
        C::Hidden => return None,
    })
}

/// Logical points for `notches` discrete wheel steps (positive = towards the
/// user, which scrolls content up as on the other backends).
pub(crate) fn wheel_notches(notches: f64) -> f64 {
    notches * LINES_PER_NOTCH * LINE
}

/// The scale an `Xft.dpi` resource asks for, if it is a sane value.
pub(crate) fn xft_scale(dpi: f64) -> Option<f64> {
    (dpi.is_finite() && dpi >= 48.0).then(|| dpi / 96.0)
}

/// The scale for a monitor `width_px` wide and `width_mm` millimetres across,
/// snapped to quarter steps in `1.0..=4.0`. Monitors that report no physical
/// size (projectors, some virtual outputs) and implausible sizes get 1.
pub(crate) fn monitor_scale(width_px: u32, width_mm: u32) -> f64 {
    if width_mm < 40 || width_px == 0 {
        return 1.0;
    }
    let dpi = f64::from(width_px) * 25.4 / f64::from(width_mm);
    ((dpi / 96.0) * 4.0).round().clamp(4.0, 16.0) / 4.0
}

/// The appearance the desktop portal's settings describe:
/// `org.freedesktop.appearance color-scheme` (1 prefers dark, 2 light, 0 no
/// preference), `org.freedesktop.appearance contrast` (1 asks for more) and
/// `org.gnome.desktop.interface enable-animations`. Unset values keep the
/// light, standard-contrast, animated default.
pub(crate) fn portal_appearance(
    color_scheme: Option<u32>,
    contrast: Option<u32>,
    animations: Option<bool>,
) -> Appearance {
    Appearance {
        color_scheme: if color_scheme == Some(1) {
            ColorScheme::Dark
        } else {
            ColorScheme::Light
        },
        high_contrast: contrast == Some(1),
        reduce_motion: animations == Some(false),
    }
}

/// The byte offset of character `chars` in `text`, clamped to its end.
pub(crate) fn char_to_byte(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(byte, _)| byte)
}

/// The largest char boundary at or below `byte`.
pub(crate) fn floor_boundary(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

/// How far inside a self-drawn window's edge a press resizes, in logical
/// points.
pub(crate) const RESIZE_BORDER: f64 = 6.0;

/// Two primary presses on a caption this close in time (milliseconds) and
/// space (logical points) toggle maximize.
pub(crate) const DOUBLE_CLICK_MS: u32 = 400;
pub(crate) const DOUBLE_CLICK_SLOP: f64 = 4.0;

/// The edge or corner of a self-drawn window a press resizes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    TopLeft,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
}

impl Edge {
    pub(crate) fn cursor(self) -> CursorIcon {
        match self {
            Edge::TopLeft | Edge::BottomRight => CursorIcon::ResizeNwse,
            Edge::TopRight | Edge::BottomLeft => CursorIcon::ResizeNesw,
            Edge::Top | Edge::Bottom => CursorIcon::ResizeNs,
            Edge::Left | Edge::Right => CursorIcon::ResizeEw,
        }
    }
}

/// The edge a point within `border` of a `size`d window's edge resizes;
/// corners take a band twice as long so they are easy to hit.
pub(crate) fn resize_edge(x: f64, y: f64, size: (f64, f64), border: f64) -> Option<Edge> {
    let corner = border * 2.0;
    let (left, right) = (x < border, x >= size.0 - border);
    let (top, bottom) = (y < border, y >= size.1 - border);
    let (near_left, near_right) = (x < corner, x >= size.0 - corner);
    let (near_top, near_bottom) = (y < corner, y >= size.1 - corner);
    Some(match () {
        () if (top && near_left) || (left && near_top) => Edge::TopLeft,
        () if (top && near_right) || (right && near_top) => Edge::TopRight,
        () if (bottom && near_right) || (right && near_bottom) => Edge::BottomRight,
        () if (bottom && near_left) || (left && near_bottom) => Edge::BottomLeft,
        () if top => Edge::Top,
        () if right => Edge::Right,
        () if bottom => Edge::Bottom,
        () if left => Edge::Left,
        () => return None,
    })
}

/// Whether a press at `(x, y)` at `time` follows `last` closely enough to
/// be a double click.
pub(crate) fn is_double_click(last: Option<(u32, f64, f64)>, time: u32, x: f64, y: f64) -> bool {
    last.is_some_and(|(t, px, py)| {
        time.wrapping_sub(t) <= DOUBLE_CLICK_MS
            && (px - x).abs() <= DOUBLE_CLICK_SLOP
            && (py - y).abs() <= DOUBLE_CLICK_SLOP
    })
}

pub(crate) fn rect_contains(r: &crate::LogicalRect, x: f64, y: f64) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.width && y < r.y + r.height
}

/// What a menu accelerator does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Command(MenuCommandId),
    System(SystemAction),
}

/// The key half of an accelerator: a layout character (matched against the
/// unshifted character the pressed key prints) or a named physical key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccelKey {
    Char(char),
    Code(KeyCode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AccelRow {
    pub(crate) key: AccelKey,
    pub(crate) control: bool,
    pub(crate) shift: bool,
    pub(crate) alt: bool,
    pub(crate) action: MenuAction,
}

/// The menu's shortcuts. X11 and Wayland have no system menu bar for an app
/// to populate, so only the accelerators take effect: a matching key press
/// runs the item's action.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct AccelTable {
    rows: Vec<AccelRow>,
}

impl AccelTable {
    pub(crate) fn build(menu: &Menu) -> Self {
        let mut table = AccelTable::default();
        if let Menu::Main { items } = menu {
            for item in items {
                table.walk(item);
            }
        }
        table
    }

    fn walk(&mut self, menu: &Menu) {
        match menu {
            Menu::Main { .. } | Menu::Line => {}
            Menu::Sub { items, .. } => {
                for item in items {
                    self.walk(item);
                }
            }
            Menu::Item {
                command,
                accel,
                enabled,
                ..
            } => {
                if *enabled && let Some(accel) = accel {
                    self.push(accel, MenuAction::Command(*command));
                }
            }
            Menu::System { action, .. } => {
                if let Some(accel) = system_accel(*action) {
                    self.push(&accel, MenuAction::System(*action));
                }
            }
        }
    }

    fn push(&mut self, accel: &Accel, action: MenuAction) {
        let Some(key) = accel_key(&accel.key) else {
            return;
        };
        self.rows.push(AccelRow {
            key,
            control: accel.primary || accel.control,
            shift: accel.shift,
            alt: accel.alt,
            action,
        });
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[AccelRow] {
        &self.rows
    }

    /// The action a press of `code` (whose unshifted character is `base`)
    /// with `mods` held triggers. The logo key never takes part, so a
    /// desktop shortcut on Super never also fires an app command.
    pub(crate) fn lookup(
        &self,
        code: KeyCode,
        base: Option<char>,
        mods: Modifiers,
    ) -> Option<MenuAction> {
        if mods.logo {
            return None;
        }
        let base = base.map(|c| c.to_lowercase().next().unwrap_or(c));
        self.rows
            .iter()
            .find(|row| {
                row.control == mods.control
                    && row.shift == mods.shift
                    && row.alt == mods.alt
                    && match row.key {
                        AccelKey::Code(c) => c == code,
                        AccelKey::Char(c) => base == Some(c),
                    }
            })
            .map(|row| row.action)
    }
}

/// The desktop-standard shortcut for a system action. Copy/Cut/Paste already
/// arrive through [`crate::event::clipboard_shortcut`], so they carry none
/// here.
fn system_accel(action: SystemAction) -> Option<Accel> {
    let ctrl = |key: &str| Accel {
        key: key.to_string(),
        control: true,
        ..Accel::default()
    };
    match action {
        SystemAction::Quit => Some(ctrl("q")),
        SystemAction::CloseWindow => Some(ctrl("w")),
        SystemAction::Hide
        | SystemAction::Minimize
        | SystemAction::Copy
        | SystemAction::Cut
        | SystemAction::Paste => None,
    }
}

/// Parse an accelerator key: one character, or a key name (`F5`, `Delete`).
fn accel_key(key: &str) -> Option<AccelKey> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        return Some(AccelKey::Char(c.to_lowercase().next().unwrap_or(c)));
    }
    let lower = key.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u32>().ok())
        && (1..=24).contains(&n)
    {
        const F: [KeyCode; 24] = [
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
        ];
        return Some(AccelKey::Code(F[n as usize - 1]));
    }
    Some(AccelKey::Code(match lower.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "escape" | "esc" => KeyCode::Escape,
        "space" => KeyCode::Space,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "left" => KeyCode::Left,
        "up" => KeyCode::Up,
        "right" => KeyCode::Right,
        "down" => KeyCode::Down,
        _ => return None,
    }))
}

/// Which windowing system to connect to, from the session's environment:
/// Wayland whenever a compositor is advertised, X11 otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Session {
    Wayland,
    X11,
}

pub(crate) fn session_order(wayland_display: Option<&str>, display: Option<&str>) -> Vec<Session> {
    let mut order = Vec::with_capacity(2);
    if wayland_display.is_some_and(|v| !v.is_empty()) {
        order.push(Session::Wayland);
    }
    if display.is_some_and(|v| !v.is_empty()) {
        order.push(Session::X11);
    }
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_and_corners_of_a_self_drawn_window() {
        let size = (200.0, 100.0);
        assert_eq!(resize_edge(100.0, 50.0, size, 6.0), None);
        assert_eq!(resize_edge(100.0, 1.0, size, 6.0), Some(Edge::Top));
        assert_eq!(resize_edge(199.0, 50.0, size, 6.0), Some(Edge::Right));
        assert_eq!(resize_edge(100.0, 99.0, size, 6.0), Some(Edge::Bottom));
        assert_eq!(resize_edge(0.0, 50.0, size, 6.0), Some(Edge::Left));
        assert_eq!(resize_edge(1.0, 1.0, size, 6.0), Some(Edge::TopLeft));
        assert_eq!(
            resize_edge(10.0, 1.0, size, 6.0),
            Some(Edge::TopLeft),
            "corner band"
        );
        assert_eq!(resize_edge(199.0, 1.0, size, 6.0), Some(Edge::TopRight));
        assert_eq!(resize_edge(199.0, 99.0, size, 6.0), Some(Edge::BottomRight));
        assert_eq!(resize_edge(1.0, 99.0, size, 6.0), Some(Edge::BottomLeft));
        assert_eq!(Edge::TopLeft.cursor(), CursorIcon::ResizeNwse);
        assert_eq!(Edge::BottomLeft.cursor(), CursorIcon::ResizeNesw);
        assert_eq!(Edge::Left.cursor(), CursorIcon::ResizeEw);
    }

    #[test]
    fn double_clicks_need_time_and_place() {
        let last = Some((1000, 10.0, 10.0));
        assert!(is_double_click(last, 1300, 12.0, 9.0));
        assert!(!is_double_click(last, 1500, 10.0, 10.0), "too slow");
        assert!(!is_double_click(last, 1100, 20.0, 10.0), "too far");
        assert!(!is_double_click(None, 1100, 10.0, 10.0));
        assert!(
            is_double_click(Some((u32::MAX - 10, 0.0, 0.0)), 50, 0.0, 0.0),
            "clock wrap"
        );
    }

    #[test]
    fn evdev_keys_map_to_physical_codes() {
        assert_eq!(key_code(9), KeyCode::Escape);
        assert_eq!(key_code(38), KeyCode::A);
        assert_eq!(key_code(36), KeyCode::Enter);
        assert_eq!(key_code(104), KeyCode::Enter, "keypad Enter");
        assert_eq!(key_code(65), KeyCode::Space);
        assert_eq!(key_code(133), KeyCode::LogoLeft);
        assert_eq!(key_code(113), KeyCode::Left);
        assert_eq!(key_code(119), KeyCode::Delete);
        assert_eq!(key_code(94), KeyCode::IntlBackslash);
        assert_eq!(key_code(191), KeyCode::F13);
        assert_eq!(key_code(8 + 464), KeyCode::Fn);
        assert_eq!(key_code(8 + 250), KeyCode::Other(250));
    }

    #[test]
    fn every_named_evdev_key_is_distinct_except_enter_and_keypad_comma() {
        let mut seen = std::collections::HashMap::new();
        for code in 0..512 {
            if let Some(k) = key_from_evdev(code)
                && let Some(prev) = seen.insert(k, code)
            {
                assert!(
                    matches!(k, KeyCode::Enter | KeyCode::NumpadComma),
                    "{k:?} from both {prev} and {code}"
                );
            }
        }
    }

    #[test]
    fn control_characters_are_not_text() {
        assert!(is_text("a"));
        assert!(is_text("é"));
        assert!(!is_text(""));
        assert!(!is_text("\u{1}"));
        assert!(!is_text("\u{8}"));
        assert!(!is_text("\u{7f}"));
        assert!(!is_text("\r"));
    }

    #[test]
    fn every_visible_cursor_has_a_theme_name_and_a_shape() {
        for icon in [
            CursorIcon::Default,
            CursorIcon::Pointer,
            CursorIcon::Text,
            CursorIcon::ResizeNwse,
            CursorIcon::ZoomOut,
        ] {
            assert!(!cursor_names(icon).is_empty());
            assert!(cursor_shape(icon).is_some());
        }
        assert!(cursor_names(CursorIcon::Hidden).is_empty());
        assert_eq!(cursor_shape(CursorIcon::Hidden), None);
        assert_eq!(cursor_shape(CursorIcon::Default), Some(1));
        assert_eq!(cursor_shape(CursorIcon::Text), Some(9));
    }

    #[test]
    fn scale_follows_xft_dpi_then_monitor_density() {
        assert_eq!(xft_scale(96.0), Some(1.0));
        assert_eq!(xft_scale(192.0), Some(2.0));
        assert_eq!(xft_scale(0.0), None);
        assert_eq!(xft_scale(f64::NAN), None);
        // A 27" 4K panel (~163 dpi) snaps to 1.75; a 1080p 24" to 1.
        assert_eq!(monitor_scale(3840, 597), 1.75);
        assert_eq!(monitor_scale(1920, 527), 1.0);
        assert_eq!(monitor_scale(2560, 0), 1.0);
        assert_eq!(monitor_scale(9000, 100), 4.0);
    }

    #[test]
    fn wheel_notches_scroll_three_lines() {
        assert_eq!(wheel_notches(1.0), 48.0);
        assert_eq!(wheel_notches(-0.5), -24.0);
    }

    #[test]
    fn portal_values_decode() {
        let dark = portal_appearance(Some(1), Some(1), Some(false));
        assert_eq!(dark.color_scheme, ColorScheme::Dark);
        assert!(dark.high_contrast);
        assert!(dark.reduce_motion);
        assert_eq!(
            portal_appearance(Some(2), None, None),
            Appearance::default()
        );
        assert_eq!(
            portal_appearance(Some(0), Some(0), Some(true)),
            Appearance::default()
        );
        assert_eq!(portal_appearance(None, None, None), Appearance::default());
    }

    #[test]
    fn caret_offsets_convert_chars_to_bytes() {
        assert_eq!(char_to_byte("にほん", 0), 0);
        assert_eq!(char_to_byte("にほん", 2), 6);
        assert_eq!(char_to_byte("にほん", 9), 9);
        assert_eq!(floor_boundary("にほ", 4), 3);
        assert_eq!(floor_boundary("にほ", 99), 6);
    }

    #[test]
    fn accelerators_match_by_layout_character_and_named_key() {
        let menu = Menu::Main {
            items: vec![Menu::Sub {
                name: "File".into(),
                items: vec![
                    Menu::Item {
                        name: "Save".into(),
                        command: MenuCommandId(7),
                        accel: Some(Accel::primary("S")),
                        enabled: true,
                    },
                    Menu::Item {
                        name: "Reload".into(),
                        command: MenuCommandId(8),
                        accel: Some(Accel {
                            key: "F5".into(),
                            ..Accel::default()
                        }),
                        enabled: true,
                    },
                    Menu::Item {
                        name: "Off".into(),
                        command: MenuCommandId(9),
                        accel: Some(Accel::primary("o")),
                        enabled: false,
                    },
                    Menu::System {
                        action: SystemAction::Quit,
                        name: None,
                    },
                    Menu::System {
                        action: SystemAction::Copy,
                        name: None,
                    },
                ],
            }],
        };
        let table = AccelTable::build(&menu);
        assert_eq!(table.rows().len(), 3, "disabled and clipboard items skip");
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        // An AZERTY `S` key still sits where `s` prints.
        assert_eq!(
            table.lookup(KeyCode::S, Some('s'), ctrl),
            Some(MenuAction::Command(MenuCommandId(7)))
        );
        assert_eq!(
            table.lookup(KeyCode::F5, None, Modifiers::default()),
            Some(MenuAction::Command(MenuCommandId(8)))
        );
        assert_eq!(
            table.lookup(KeyCode::Q, Some('q'), ctrl),
            Some(MenuAction::System(SystemAction::Quit))
        );
        assert_eq!(
            table.lookup(KeyCode::S, Some('s'), Modifiers::default()),
            None
        );
        let super_ctrl = Modifiers { logo: true, ..ctrl };
        assert_eq!(table.lookup(KeyCode::S, Some('s'), super_ctrl), None);
        assert_eq!(table.lookup(KeyCode::O, Some('o'), ctrl), None);
    }

    #[test]
    fn wayland_is_preferred_when_advertised() {
        assert_eq!(
            session_order(Some("wayland-0"), Some(":0")),
            vec![Session::Wayland, Session::X11]
        );
        assert_eq!(session_order(Some(""), Some(":0")), vec![Session::X11]);
        assert_eq!(session_order(None, None), vec![]);
        assert_eq!(
            session_order(Some("wayland-1"), None),
            vec![Session::Wayland]
        );
    }
}
