//! The Win32 backend's pure translation tables: scancodes to [`KeyCode`],
//! UTF-16 `WM_CHAR` units to text, cursor icons to system cursor ids, wheel
//! notches to logical points, clipboard line endings, and the menu tree to a
//! flat command/accelerator plan. Nothing here calls the OS, so it is compiled
//! and tested on every host.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

use crate::event::{CursorIcon, KeyCode, PointerId};
use crate::menu::{Accel, Menu, MenuCommandId, SystemAction};

/// Logical points per wheel line, matching the macOS backend's line height.
pub(crate) const LINE: f64 = 16.0;

/// One wheel notch in raw `WM_MOUSEWHEEL` units.
const WHEEL_DELTA: f64 = 120.0;

/// `SPI_GETWHEELSCROLLLINES` answer meaning "one page per notch".
pub(crate) const WHEEL_PAGESCROLL: u32 = u32::MAX;

/// A key's identity from the `WM_KEYDOWN` lparam: the set-1 scancode (bits
/// 16–23), the extended bit (24), and the virtual key as a fallback for input
/// injected without a scancode. `None` for the fake shifts the keyboard driver
/// wraps around some extended keys and for `VK_PACKET` (text-only injection,
/// which arrives as `WM_CHAR`).
pub(crate) fn key_code(scan: u32, extended: bool, vk: u16) -> Option<KeyCode> {
    if scan == 0 {
        return key_from_vk(vk);
    }
    let code = if extended { 0xE000 | scan } else { scan };
    match code {
        0xE02A | 0xE036 => None,
        _ => Some(key_from_scancode(code).unwrap_or(KeyCode::Other(code))),
    }
}

fn key_from_scancode(code: u32) -> Option<KeyCode> {
    use KeyCode as K;
    Some(match code {
        0x01 => K::Escape,
        0x02 => K::Digit1,
        0x03 => K::Digit2,
        0x04 => K::Digit3,
        0x05 => K::Digit4,
        0x06 => K::Digit5,
        0x07 => K::Digit6,
        0x08 => K::Digit7,
        0x09 => K::Digit8,
        0x0A => K::Digit9,
        0x0B => K::Digit0,
        0x0C => K::Minus,
        0x0D => K::Equal,
        0x0E => K::Backspace,
        0x0F => K::Tab,
        0x10 => K::Q,
        0x11 => K::W,
        0x12 => K::E,
        0x13 => K::R,
        0x14 => K::T,
        0x15 => K::Y,
        0x16 => K::U,
        0x17 => K::I,
        0x18 => K::O,
        0x19 => K::P,
        0x1A => K::BracketLeft,
        0x1B => K::BracketRight,
        0x1C | 0xE01C => K::Enter,
        0x1D => K::ControlLeft,
        0x1E => K::A,
        0x1F => K::S,
        0x20 => K::D,
        0x21 => K::F,
        0x22 => K::G,
        0x23 => K::H,
        0x24 => K::J,
        0x25 => K::K,
        0x26 => K::L,
        0x27 => K::Semicolon,
        0x28 => K::Quote,
        0x29 => K::Backquote,
        0x2A => K::ShiftLeft,
        0x2B => K::Backslash,
        0x2C => K::Z,
        0x2D => K::X,
        0x2E => K::C,
        0x2F => K::V,
        0x30 => K::B,
        0x31 => K::N,
        0x32 => K::M,
        0x33 => K::Comma,
        0x34 => K::Period,
        0x35 => K::Slash,
        0x36 => K::ShiftRight,
        0x37 => K::NumpadMultiply,
        0x38 => K::AltLeft,
        0x39 => K::Space,
        0x3A => K::CapsLock,
        0x3B => K::F1,
        0x3C => K::F2,
        0x3D => K::F3,
        0x3E => K::F4,
        0x3F => K::F5,
        0x40 => K::F6,
        0x41 => K::F7,
        0x42 => K::F8,
        0x43 => K::F9,
        0x44 => K::F10,
        // The Pause key reaches the window as a bare 0x45; Num Lock carries
        // the extended bit.
        0x45 => K::Pause,
        0xE045 => K::NumLock,
        0x46 => K::ScrollLock,
        0x47 => K::Numpad7,
        0x48 => K::Numpad8,
        0x49 => K::Numpad9,
        0x4A => K::NumpadSubtract,
        0x4B => K::Numpad4,
        0x4C => K::Numpad5,
        0x4D => K::Numpad6,
        0x4E => K::NumpadAdd,
        0x4F => K::Numpad1,
        0x50 => K::Numpad2,
        0x51 => K::Numpad3,
        0x52 => K::Numpad0,
        0x53 => K::NumpadDecimal,
        // Alt+Print Screen (SysRq).
        0x54 | 0xE037 => K::PrintScreen,
        0x56 => K::IntlBackslash,
        0x57 => K::F11,
        0x58 => K::F12,
        0x59 => K::NumpadEqual,
        0x64 => K::F13,
        0x65 => K::F14,
        0x66 => K::F15,
        0x67 => K::F16,
        0x68 => K::F17,
        0x69 => K::F18,
        0x6A => K::F19,
        0x6B => K::F20,
        0x6C => K::F21,
        0x6D => K::F22,
        0x6E => K::F23,
        0x76 => K::F24,
        0x70 | 0x72 | 0xF2 => K::Lang1,
        0x71 | 0xF1 => K::Lang2,
        0x73 => K::IntlRo,
        0x79 => K::Convert,
        0x7B => K::NonConvert,
        0x7D => K::IntlYen,
        0x7E => K::NumpadComma,
        0xE010 => K::MediaTrackPrevious,
        0xE019 => K::MediaTrackNext,
        0xE01D => K::ControlRight,
        0xE020 => K::VolumeMute,
        0xE022 => K::MediaPlayPause,
        0xE024 => K::MediaStop,
        0xE02E => K::VolumeDown,
        0xE030 => K::VolumeUp,
        0xE035 => K::NumpadDivide,
        0xE038 => K::AltRight,
        0xE046 => K::Pause,
        0xE047 => K::Home,
        0xE048 => K::Up,
        0xE049 => K::PageUp,
        0xE04B => K::Left,
        0xE04D => K::Right,
        0xE04F => K::End,
        0xE050 => K::Down,
        0xE051 => K::PageDown,
        0xE052 => K::Insert,
        0xE053 => K::Delete,
        0xE05B => K::LogoLeft,
        0xE05C => K::LogoRight,
        0xE05D => K::ContextMenu,
        0xE06A => K::Back,
        _ => return None,
    })
}

/// The virtual-key fallback for input that carries no scancode.
fn key_from_vk(vk: u16) -> Option<KeyCode> {
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
    const FKEYS: [KeyCode; 24] = [
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
    Some(match vk {
        0x08 => K::Backspace,
        0x09 => K::Tab,
        0x0D => K::Enter,
        0x10 | 0xA0 => K::ShiftLeft,
        0xA1 => K::ShiftRight,
        0x11 | 0xA2 => K::ControlLeft,
        0xA3 => K::ControlRight,
        0x12 | 0xA4 => K::AltLeft,
        0xA5 => K::AltRight,
        0x13 => K::Pause,
        0x14 => K::CapsLock,
        0x1B => K::Escape,
        0x20 => K::Space,
        0x21 => K::PageUp,
        0x22 => K::PageDown,
        0x23 => K::End,
        0x24 => K::Home,
        0x25 => K::Left,
        0x26 => K::Up,
        0x27 => K::Right,
        0x28 => K::Down,
        0x2C => K::PrintScreen,
        0x2D => K::Insert,
        0x2E => K::Delete,
        0x30..=0x39 => DIGITS[usize::from(vk - 0x30)],
        0x41..=0x5A => LETTERS[usize::from(vk - 0x41)],
        0x5B => K::LogoLeft,
        0x5C => K::LogoRight,
        0x5D => K::ContextMenu,
        0x60..=0x69 => NUMPAD[usize::from(vk - 0x60)],
        0x6A => K::NumpadMultiply,
        0x6B => K::NumpadAdd,
        0x6D => K::NumpadSubtract,
        0x6E => K::NumpadDecimal,
        0x6F => K::NumpadDivide,
        0x70..=0x87 => FKEYS[usize::from(vk - 0x70)],
        0x90 => K::NumLock,
        0x91 => K::ScrollLock,
        0xA6 => K::Back,
        0xAD => K::VolumeMute,
        0xAE => K::VolumeDown,
        0xAF => K::VolumeUp,
        0xB0 => K::MediaTrackNext,
        0xB1 => K::MediaTrackPrevious,
        0xB2 => K::MediaStop,
        0xB3 => K::MediaPlayPause,
        0xBA => K::Semicolon,
        0xBB => K::Equal,
        0xBC => K::Comma,
        0xBD => K::Minus,
        0xBE => K::Period,
        0xBF => K::Slash,
        0xC0 => K::Backquote,
        0xDB => K::BracketLeft,
        0xDC => K::Backslash,
        0xDD => K::BracketRight,
        0xDE => K::Quote,
        0xE2 => K::IntlBackslash,
        // VK_PACKET: injected text, delivered through WM_CHAR alone.
        0xE7 => return None,
        _ => K::Other(0x1_0000 | u32::from(vk)),
    })
}

/// Joins the UTF-16 units `WM_CHAR` delivers one at a time into characters.
/// A high surrogate waits for its low half; an unpaired half is dropped.
#[derive(Debug, Default)]
pub(crate) struct Utf16Assembler {
    high: Option<u16>,
}

impl Utf16Assembler {
    pub(crate) fn push(&mut self, unit: u16) -> Option<char> {
        match unit {
            0xD800..=0xDBFF => {
                self.high = Some(unit);
                None
            }
            0xDC00..=0xDFFF => {
                let high = self.high.take()?;
                let c = 0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                char::from_u32(c)
            }
            _ => {
                self.high = None;
                char::from_u32(u32::from(unit))
            }
        }
    }
}

/// Whether a `WM_CHAR` character is text. Control characters (Enter, Tab,
/// Backspace, Escape, Ctrl+letter) are already delivered as keys.
pub(crate) fn is_text(c: char) -> bool {
    !c.is_control()
}

/// The `IDC_*` resource id of the system cursor for `icon`; `None` hides the
/// cursor.
pub(crate) fn cursor_resource(icon: CursorIcon) -> Option<u16> {
    const ARROW: u16 = 32512;
    const IBEAM: u16 = 32513;
    const WAIT: u16 = 32514;
    const CROSS: u16 = 32515;
    const SIZENWSE: u16 = 32642;
    const SIZENESW: u16 = 32643;
    const SIZEWE: u16 = 32644;
    const SIZENS: u16 = 32645;
    const SIZEALL: u16 = 32646;
    const NO: u16 = 32648;
    const HAND: u16 = 32649;
    const APPSTARTING: u16 = 32650;
    const HELP: u16 = 32651;
    use CursorIcon as C;
    Some(match icon {
        C::Default | C::ContextMenu | C::Copy | C::Alias | C::ZoomIn | C::ZoomOut => ARROW,
        C::Pointer | C::Grab => HAND,
        C::Text | C::VerticalText => IBEAM,
        C::Crosshair => CROSS,
        C::Move | C::Grabbing => SIZEALL,
        C::NotAllowed => NO,
        C::Wait => WAIT,
        C::Progress => APPSTARTING,
        C::Help => HELP,
        C::ResizeEw | C::ResizeCol => SIZEWE,
        C::ResizeNs | C::ResizeRow => SIZENS,
        C::ResizeNesw => SIZENESW,
        C::ResizeNwse => SIZENWSE,
        C::Hidden => return None,
    })
}

/// A vertical wheel sample in logical points. A positive raw delta (the wheel
/// rolled away from the user) scrolls content up, so it reports a negative
/// delta, as on macOS. `page` is the logical height one page-scroll covers.
pub(crate) fn wheel_delta(raw: i16, lines: u32, page: f64) -> f64 {
    let notches = f64::from(raw) / WHEEL_DELTA;
    let step = if lines == WHEEL_PAGESCROLL {
        page
    } else {
        f64::from(lines) * LINE
    };
    -notches * step
}

/// A horizontal wheel sample in logical points: a tilt to the right (positive
/// raw delta) moves content right.
pub(crate) fn hwheel_delta(raw: i16, chars: u32) -> f64 {
    f64::from(raw) / WHEEL_DELTA * f64::from(chars) * LINE
}

/// NUL-terminated UTF-16 for `CF_UNICODETEXT`, with lone `\n` widened to the
/// `\r\n` Windows applications expect.
pub(crate) fn clipboard_units(text: &str) -> Vec<u16> {
    let mut out = Vec::with_capacity(text.len() + 1);
    let mut prev_cr = false;
    for unit in text.encode_utf16() {
        if unit == u16::from(b'\n') && !prev_cr {
            out.push(u16::from(b'\r'));
        }
        prev_cr = unit == u16::from(b'\r');
        out.push(unit);
    }
    out.push(0);
    out
}

/// `CF_UNICODETEXT` contents up to the first NUL, with `\r\n` and lone `\r`
/// folded to `\n`.
pub(crate) fn clipboard_text(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    let raw = String::from_utf16_lossy(&units[..end]);
    raw.replace("\r\n", "\n").replace('\r', "\n")
}

/// The byte offset in `text` of UTF-16 index `index` (IMM32 reports the
/// composition caret in UTF-16 units), clamped to the end.
pub(crate) fn utf16_to_byte(text: &str, index: usize) -> usize {
    let mut units = 0;
    for (byte, c) in text.char_indices() {
        if units >= index {
            return byte;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// The contact id Viso reports for a Win32 touch/pen pointer id; the high bit
/// keeps it clear of [`PointerId::MOUSE`].
pub(crate) fn pointer_id(win32: u32) -> PointerId {
    PointerId(1 << 32 | u64::from(win32))
}

/// Normalized pressure from a pen/touch reading in `0..=1024`, or the
/// no-sensor default while in contact.
pub(crate) fn pressure(has_pressure: bool, raw: u32, in_contact: bool) -> f32 {
    if !in_contact {
        0.0
    } else if has_pressure {
        (raw.min(1024) as f32 / 1024.0).max(f32::MIN_POSITIVE)
    } else {
        0.5
    }
}

/// What a `WM_COMMAND` id from our menu does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Command(MenuCommandId),
    System(SystemAction),
}

/// One node of the Win32 menu bar, in the order `AppendMenuW` builds it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum MenuEntry {
    Popup {
        name: Vec<u16>,
        items: Vec<MenuEntry>,
    },
    Item {
        id: u16,
        text: Vec<u16>,
        enabled: bool,
    },
    Separator,
}

/// An accelerator-table row before the key is resolved against the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccelSpec {
    pub(crate) id: u16,
    pub(crate) key: String,
    pub(crate) control: bool,
    pub(crate) shift: bool,
    pub(crate) alt: bool,
}

/// The menu bar as Win32 wants it: the entry tree (UTF-16, NUL-terminated
/// labels with the shortcut after a tab), the action behind each command id
/// (`id - FIRST_ID` indexes `actions`), and the accelerator rows.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct MenuPlan {
    pub(crate) entries: Vec<MenuEntry>,
    pub(crate) actions: Vec<MenuAction>,
    pub(crate) accels: Vec<AccelSpec>,
}

/// Command ids start above zero (zero means "no command") and stay below the
/// system-command range.
pub(crate) const FIRST_ID: u16 = 1;

impl MenuPlan {
    pub(crate) fn build(menu: &Menu) -> Self {
        let mut plan = MenuPlan::default();
        if let Menu::Main { items } = menu {
            plan.entries = items.iter().filter_map(|m| plan.entry(m)).collect();
        }
        plan
    }

    pub(crate) fn action(&self, id: u16) -> Option<MenuAction> {
        self.actions
            .get(usize::from(id.checked_sub(FIRST_ID)?))
            .copied()
    }

    fn entry(&mut self, menu: &Menu) -> Option<MenuEntry> {
        Some(match menu {
            Menu::Main { .. } => return None,
            Menu::Sub { name, items } => MenuEntry::Popup {
                name: wide(&mnemonic_safe(name)),
                items: items.iter().filter_map(|m| self.entry(m)).collect(),
            },
            Menu::Item {
                name,
                command,
                accel,
                enabled,
            } => self.item(
                name,
                MenuAction::Command(*command),
                accel.as_ref(),
                *enabled,
            )?,
            Menu::System { action, name } => {
                let (label, accel) = system_item(*action);
                let name = name.as_deref().unwrap_or(label);
                self.item(name, MenuAction::System(*action), accel.as_ref(), true)?
            }
            Menu::Line => MenuEntry::Separator,
        })
    }

    fn item(
        &mut self,
        name: &str,
        action: MenuAction,
        accel: Option<&Accel>,
        enabled: bool,
    ) -> Option<MenuEntry> {
        let id = FIRST_ID.checked_add(u16::try_from(self.actions.len()).ok()?)?;
        if id >= 0xF000 {
            return None;
        }
        self.actions.push(action);
        let mut text = mnemonic_safe(name);
        if let Some(accel) = accel.filter(|a| a.is_set()) {
            text.push('\t');
            text.push_str(&accel_label(accel));
            self.accels.push(AccelSpec {
                id,
                key: accel.key.clone(),
                control: accel.primary || accel.control,
                shift: accel.shift,
                alt: accel.alt,
            });
        }
        Some(MenuEntry::Item {
            id,
            text: wide(&text),
            enabled,
        })
    }
}

/// The Windows label and shortcut for a standard action.
fn system_item(action: SystemAction) -> (&'static str, Option<Accel>) {
    let ctrl = |key: &str| {
        Some(Accel {
            key: key.to_string(),
            control: true,
            ..Accel::default()
        })
    };
    match action {
        // Alt+F4 stays the system's close-window gesture.
        SystemAction::Quit => ("Exit", None),
        SystemAction::CloseWindow => ("Close", ctrl("w")),
        SystemAction::Hide | SystemAction::Minimize => ("Minimize", None),
        SystemAction::Copy => ("Copy", ctrl("c")),
        SystemAction::Cut => ("Cut", ctrl("x")),
        SystemAction::Paste => ("Paste", ctrl("v")),
    }
}

/// The shortcut text Windows menus print after the tab, e.g. `Ctrl+Shift+S`.
pub(crate) fn accel_label(accel: &Accel) -> String {
    let mut out = String::new();
    if accel.primary || accel.control {
        out.push_str("Ctrl+");
    }
    if accel.shift {
        out.push_str("Shift+");
    }
    if accel.alt {
        out.push_str("Alt+");
    }
    let key = accel.key.as_str();
    match key.chars().count() {
        1 => out.extend(key.chars().flat_map(char::to_uppercase)),
        _ => out.push_str(key),
    }
    out
}

/// The virtual key for an accelerator key that does not depend on the
/// keyboard layout (letters, digits, named keys). Punctuation returns `None`;
/// the backend resolves it against the active layout with `VkKeyScanW`.
pub(crate) fn accel_vk(key: &str) -> Option<u16> {
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        return match c.to_ascii_uppercase() {
            c @ ('A'..='Z' | '0'..='9') => Some(c as u16),
            ' ' => Some(0x20),
            _ => None,
        };
    }
    let lower = key.to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|n| n.parse::<u16>().ok())
        && (1..=24).contains(&n)
    {
        return Some(0x6F + n);
    }
    Some(match lower.as_str() {
        "enter" | "return" => 0x0D,
        "tab" => 0x09,
        "escape" | "esc" => 0x1B,
        "space" => 0x20,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "left" => 0x25,
        "up" => 0x26,
        "right" => 0x27,
        "down" => 0x28,
        _ => return None,
    })
}

/// A label with `&` doubled, so Win32 prints it instead of underlining the
/// next letter as a mnemonic.
fn mnemonic_safe(name: &str) -> String {
    name.replace('&', "&&")
}

/// NUL-terminated UTF-16.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Physical pixels to logical points at `dpi`.
pub(crate) fn to_logical(px: i32, dpi: u32) -> f64 {
    f64::from(px) * 96.0 / f64::from(dpi.max(1))
}

/// Logical points to physical pixels at `dpi`, rounded.
pub(crate) fn to_physical(pt: f64, dpi: u32) -> i32 {
    (pt * f64::from(dpi) / 96.0).round() as i32
}

/// The scale factor for `dpi`.
pub(crate) fn scale_of(dpi: u32) -> f64 {
    f64::from(dpi.max(1)) / 96.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancodes_name_physical_keys() {
        assert_eq!(key_code(0x1E, false, 0x41), Some(KeyCode::A));
        assert_eq!(key_code(0x10, false, 0x41), Some(KeyCode::Q));
        assert_eq!(key_code(0x1C, true, 0x0D), Some(KeyCode::Enter));
        assert_eq!(key_code(0x1D, true, 0x11), Some(KeyCode::ControlRight));
        assert_eq!(key_code(0x48, true, 0x26), Some(KeyCode::Up));
        assert_eq!(key_code(0x48, false, 0x68), Some(KeyCode::Numpad8));
        assert_eq!(key_code(0x45, true, 0x90), Some(KeyCode::NumLock));
        assert_eq!(key_code(0x45, false, 0x13), Some(KeyCode::Pause));
        assert_eq!(key_code(0x7D, false, 0xDC), Some(KeyCode::IntlYen));
        assert_eq!(key_code(0x30, true, 0xAF), Some(KeyCode::VolumeUp));
        assert_eq!(key_code(0x5B, true, 0x5B), Some(KeyCode::LogoLeft));
    }

    #[test]
    fn fake_shifts_are_dropped_and_unknown_scancodes_are_kept_raw() {
        assert_eq!(key_code(0x2A, true, 0x10), None);
        assert_eq!(key_code(0x5F, true, 0x5F), Some(KeyCode::Other(0xE05F)));
    }

    #[test]
    fn scancode_less_input_falls_back_to_the_virtual_key() {
        assert_eq!(key_code(0, false, 0x5A), Some(KeyCode::Z));
        assert_eq!(key_code(0, false, 0x37), Some(KeyCode::Digit7));
        assert_eq!(key_code(0, false, 0x7B), Some(KeyCode::F12));
        assert_eq!(key_code(0, false, 0x87), Some(KeyCode::F24));
        assert_eq!(key_code(0, false, 0x65), Some(KeyCode::Numpad5));
        assert_eq!(key_code(0, false, 0xE7), None);
        assert_eq!(key_code(0, false, 0xFE), Some(KeyCode::Other(0x100FE)));
    }

    #[test]
    fn surrogate_pairs_join_into_one_character() {
        let mut asm = Utf16Assembler::default();
        let units: Vec<u16> = "a😀".encode_utf16().collect();
        let out: Vec<char> = units.iter().filter_map(|&u| asm.push(u)).collect();
        assert_eq!(out, vec!['a', '😀']);
    }

    #[test]
    fn unpaired_surrogates_are_dropped() {
        let mut asm = Utf16Assembler::default();
        assert_eq!(asm.push(0xDC00), None);
        assert_eq!(asm.push(0xD83D), None);
        assert_eq!(asm.push(u16::from(b'x')), Some('x'));
        assert_eq!(asm.push(0xDE00), None);
    }

    #[test]
    fn control_characters_are_not_text() {
        assert!(!is_text('\r'));
        assert!(!is_text('\u{8}'));
        assert!(!is_text('\u{1}'));
        assert!(!is_text('\u{7f}'));
        assert!(is_text('é'));
        assert!(is_text(' '));
    }

    #[test]
    fn cursors_map_to_system_resources() {
        assert_eq!(cursor_resource(CursorIcon::Default), Some(32512));
        assert_eq!(cursor_resource(CursorIcon::Text), Some(32513));
        assert_eq!(cursor_resource(CursorIcon::Pointer), Some(32649));
        assert_eq!(cursor_resource(CursorIcon::ResizeEw), Some(32644));
        assert_eq!(cursor_resource(CursorIcon::NotAllowed), Some(32648));
        assert_eq!(cursor_resource(CursorIcon::Hidden), None);
    }

    #[test]
    fn wheel_notches_scroll_lines_in_the_macos_direction() {
        assert_eq!(wheel_delta(120, 3, 500.0), -48.0);
        assert_eq!(wheel_delta(-240, 3, 500.0), 96.0);
        assert_eq!(wheel_delta(60, 3, 500.0), -24.0);
        assert_eq!(wheel_delta(120, WHEEL_PAGESCROLL, 500.0), -500.0);
        assert_eq!(hwheel_delta(120, 3), 48.0);
        assert_eq!(hwheel_delta(-120, 1), -16.0);
    }

    #[test]
    fn clipboard_text_round_trips_line_endings() {
        let units = clipboard_units("a\nb\r\nc");
        assert_eq!(String::from_utf16_lossy(&units), "a\r\nb\r\nc\0");
        assert_eq!(clipboard_text(&units), "a\nb\nc");
        assert_eq!(clipboard_text(&wide("x\ry")), "x\ny");
        let tail: Vec<u16> = "ok\0junk".encode_utf16().collect();
        assert_eq!(clipboard_text(&tail), "ok");
    }

    #[test]
    fn utf16_indices_convert_to_byte_offsets() {
        let s = "a😀b";
        assert_eq!(utf16_to_byte(s, 0), 0);
        assert_eq!(utf16_to_byte(s, 1), 1);
        assert_eq!(utf16_to_byte(s, 3), 5);
        assert_eq!(utf16_to_byte(s, 4), 6);
        assert_eq!(utf16_to_byte(s, 99), 6);
        assert_eq!(utf16_to_byte("にほ", 1), 3);
    }

    #[test]
    fn touch_ids_stay_clear_of_the_mouse() {
        assert_ne!(pointer_id(0), PointerId::MOUSE);
        assert_eq!(pointer_id(7), PointerId(1 << 32 | 7));
    }

    #[test]
    fn pressure_normalizes_or_defaults() {
        assert_eq!(pressure(true, 512, true), 0.5);
        assert_eq!(pressure(true, 2048, true), 1.0);
        assert!(pressure(true, 0, true) > 0.0);
        assert_eq!(pressure(false, 0, true), 0.5);
        assert_eq!(pressure(true, 900, false), 0.0);
    }

    #[test]
    fn accelerators_print_and_resolve() {
        let a = Accel {
            key: "s".into(),
            primary: true,
            shift: true,
            ..Accel::default()
        };
        assert_eq!(accel_label(&a), "Ctrl+Shift+S");
        assert_eq!(accel_vk("s"), Some(0x53));
        assert_eq!(accel_vk("5"), Some(0x35));
        assert_eq!(accel_vk("F4"), Some(0x73));
        assert_eq!(accel_vk("f24"), Some(0x87));
        assert_eq!(accel_vk("Enter"), Some(0x0D));
        assert_eq!(accel_vk(","), None);
        assert_eq!(accel_vk("f25"), None);
    }

    #[test]
    fn menu_plan_numbers_items_and_collects_accelerators() {
        let menu = Menu::Main {
            items: vec![Menu::sub(
                "File & Edit",
                [
                    Menu::item("Save", MenuCommandId(9), Accel::primary("s")),
                    Menu::Line,
                    Menu::item_plain("Info", MenuCommandId(4)),
                    Menu::system(SystemAction::Quit),
                ],
            )],
        };
        let plan = MenuPlan::build(&menu);
        let [MenuEntry::Popup { name, items }] = plan.entries.as_slice() else {
            panic!("one popup expected: {:?}", plan.entries);
        };
        assert_eq!(name, &wide("File && Edit"));
        assert_eq!(items.len(), 4);
        assert_eq!(
            items[0],
            MenuEntry::Item {
                id: 1,
                text: wide("Save\tCtrl+S"),
                enabled: true
            }
        );
        assert_eq!(items[1], MenuEntry::Separator);
        assert_eq!(
            items[3],
            MenuEntry::Item {
                id: 3,
                text: wide("Exit"),
                enabled: true
            }
        );
        assert_eq!(plan.action(1), Some(MenuAction::Command(MenuCommandId(9))));
        assert_eq!(plan.action(2), Some(MenuAction::Command(MenuCommandId(4))));
        assert_eq!(plan.action(3), Some(MenuAction::System(SystemAction::Quit)));
        assert_eq!(plan.action(0), None);
        assert_eq!(plan.action(4), None);
        assert_eq!(plan.accels.len(), 1);
        assert!(plan.accels[0].control && !plan.accels[0].alt && plan.accels[0].key == "s");
    }

    #[test]
    fn a_non_main_root_builds_no_menu() {
        assert_eq!(
            MenuPlan::build(&Menu::item_plain("x", MenuCommandId(1))),
            MenuPlan::default()
        );
    }

    #[test]
    fn dpi_conversions_round_trip() {
        assert_eq!(scale_of(144), 1.5);
        assert_eq!(to_physical(100.0, 144), 150);
        assert_eq!(to_logical(150, 144), 100.0);
        assert_eq!(to_logical(10, 0), 960.0);
    }
}
