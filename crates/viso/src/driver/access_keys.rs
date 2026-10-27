//! The access keys: Alt with a letter or digit, and the menu bar's F10.

use viso_ui::Modifiers;

/// Whether Alt reaches the access keys and the menu bar: off Apple systems,
/// where Option types characters instead.
pub(crate) fn alt_access() -> bool {
    #[cfg(feature = "headless")]
    if let Some(on) = crate::__test_support::ALT_ACCESS.with(std::cell::Cell::get) {
        return on;
    }
    !viso_runtime::primary_is_logo()
}

/// What a key reaches among the access keys: a letter or digit with Alt
/// alone held, or the menu bar's F10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessKey {
    Char(char),
    MenuBar,
}

/// The access key `key` with `modifiers` held presses, if any.
pub(crate) fn access_key_of(key: viso_runtime::Key, modifiers: Modifiers) -> Option<AccessKey> {
    use viso_runtime::Key as K;
    let plain = !(modifiers.control || modifiers.logo || modifiers.shift);
    if key == K::F10 && plain && !modifiers.alt {
        return Some(AccessKey::MenuBar);
    }
    if !modifiers.alt || modifiers.control || modifiers.logo {
        return None;
    }
    const LETTERS: [K; 26] = [
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
    const DIGITS: [K; 10] = [
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
    if let Some(at) = LETTERS.iter().position(|&l| l == key) {
        return Some(AccessKey::Char((b'a' + at as u8) as char));
    }
    DIGITS
        .iter()
        .position(|&d| d == key)
        .map(|at| AccessKey::Char((b'0' + at as u8) as char))
}
