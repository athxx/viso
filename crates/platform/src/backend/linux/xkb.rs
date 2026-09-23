//! The keymap, modifier state and compose (dead-key) sequences through
//! libxkbcommon, loaded at runtime. Wayland hands over a keymap file; X11
//! reads the core keyboard's keymap through the XKB extension. Both then feed
//! modifier masks in and read physical-key text out the same way.

use std::ffi::{c_char, c_void};
use std::ptr::{self, NonNull};

use xkbcommon_dl::{
    XKB_MOD_NAME_ALT, XKB_MOD_NAME_CTRL, XKB_MOD_NAME_LOGO, XKB_MOD_NAME_SHIFT, XkbCommon,
    XkbCommonCompose, xkb_compose_compile_flags, xkb_compose_feed_result, xkb_compose_state,
    xkb_compose_state_flags, xkb_compose_status, xkb_context, xkb_context_flags, xkb_keymap,
    xkb_keymap_compile_flags, xkb_keymap_format, xkb_state, xkb_state_component,
};

use crate::event::Modifiers;

/// A keymap and the modifier/layout state over it.
struct Keymap {
    keymap: NonNull<xkb_keymap>,
    state: NonNull<xkb_state>,
}

pub(crate) struct Keyboard {
    lib: &'static XkbCommon,
    context: NonNull<xkb_context>,
    keymap: Option<Keymap>,
    compose: Option<Compose>,
}

struct Compose {
    lib: &'static XkbCommonCompose,
    state: NonNull<xkb_compose_state>,
}

/// What a key press contributes to the text stream.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum KeyText {
    /// Insert this text.
    Text(String),
    /// The key is part of an unfinished compose sequence (a dead key).
    Composing,
    /// The key produces no text.
    None,
}

impl Keyboard {
    /// `None` when libxkbcommon is not installed.
    pub(crate) fn new() -> Option<Self> {
        let lib = xkbcommon_dl::xkbcommon_option()?;
        // SAFETY: creating a fresh context; it is unreferenced in `Drop`.
        let context = NonNull::new(unsafe {
            (lib.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_FLAGS)
        })?;
        let compose = Compose::new(context);
        Some(Self {
            lib,
            context,
            keymap: None,
            compose,
        })
    }

    /// Adopt a keymap in XKB text format, as a Wayland compositor sends it.
    /// A trailing NUL, which the protocol includes, is allowed.
    pub(crate) fn set_keymap_text(&mut self, text: &[u8]) -> bool {
        let text = text.strip_suffix(&[0]).unwrap_or(text);
        // SAFETY: `text` is a live buffer of the given length; the keymap is
        // unreferenced when replaced or dropped.
        let keymap = unsafe {
            (self.lib.xkb_keymap_new_from_buffer)(
                self.context.as_ptr(),
                text.as_ptr().cast::<c_char>(),
                text.len(),
                xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            )
        };
        self.adopt(keymap, None)
    }

    /// Adopt the X server's current keymap for the core keyboard.
    ///
    /// # Safety
    ///
    /// `connection` must be a live `xcb_connection_t` whose XKB extension
    /// was set up with [`setup_x11`].
    pub(crate) unsafe fn set_keymap_x11(&mut self, connection: *mut c_void, device: i32) -> bool {
        let Some(x11) = xkbcommon_dl::x11::xkbcommon_x11_option() else {
            return false;
        };
        // SAFETY: per the function contract; the keymap and state are owned
        // by `self` from here.
        unsafe {
            let keymap = (x11.xkb_x11_keymap_new_from_device)(
                self.context.as_ptr(),
                connection,
                device,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            );
            let Some(map) = NonNull::new(keymap) else {
                return false;
            };
            let state = (x11.xkb_x11_state_new_from_device)(map.as_ptr(), connection, device);
            self.adopt(keymap, NonNull::new(state))
        }
    }

    fn adopt(&mut self, keymap: *mut xkb_keymap, state: Option<NonNull<xkb_state>>) -> bool {
        let Some(keymap) = NonNull::new(keymap) else {
            return false;
        };
        // SAFETY: `keymap` is a fresh keymap we own one reference to.
        let state =
            state.or_else(|| NonNull::new(unsafe { (self.lib.xkb_state_new)(keymap.as_ptr()) }));
        let Some(state) = state else {
            // SAFETY: releasing the reference taken above.
            unsafe { (self.lib.xkb_keymap_unref)(keymap.as_ptr()) };
            return false;
        };
        self.release_keymap();
        self.keymap = Some(Keymap { keymap, state });
        true
    }

    fn release_keymap(&mut self) {
        if let Some(old) = self.keymap.take() {
            // SAFETY: the state and keymap references this value owned.
            unsafe {
                (self.lib.xkb_state_unref)(old.state.as_ptr());
                (self.lib.xkb_keymap_unref)(old.keymap.as_ptr());
            }
        }
    }

    pub(crate) fn has_keymap(&self) -> bool {
        self.keymap.is_some()
    }

    /// Apply the modifier and layout masks the server reports.
    pub(crate) fn update_mask(
        &mut self,
        depressed: u32,
        latched: u32,
        locked: u32,
        depressed_layout: u32,
        latched_layout: u32,
        locked_layout: u32,
    ) {
        if let Some(map) = &self.keymap {
            // SAFETY: a live state.
            unsafe {
                (self.lib.xkb_state_update_mask)(
                    map.state.as_ptr(),
                    depressed,
                    latched,
                    locked,
                    depressed_layout,
                    latched_layout,
                    locked_layout,
                );
            }
        }
    }

    pub(crate) fn modifiers(&self) -> Modifiers {
        let Some(map) = &self.keymap else {
            return Modifiers::default();
        };
        let active = |name: &[u8]| {
            // SAFETY: a live state and a NUL-terminated modifier name.
            unsafe {
                (self.lib.xkb_state_mod_name_is_active)(
                    map.state.as_ptr(),
                    name.as_ptr().cast::<c_char>(),
                    xkb_state_component::XKB_STATE_MODS_EFFECTIVE,
                ) > 0
            }
        };
        Modifiers {
            shift: active(XKB_MOD_NAME_SHIFT),
            control: active(XKB_MOD_NAME_CTRL),
            alt: active(XKB_MOD_NAME_ALT),
            logo: active(XKB_MOD_NAME_LOGO),
        }
    }

    /// The keysym `keycode` produces in the current state.
    pub(crate) fn key_sym(&self, keycode: u32) -> u32 {
        self.keymap.as_ref().map_or(0, |map| {
            // SAFETY: a live state; any keycode is accepted.
            unsafe { (self.lib.xkb_state_key_get_one_sym)(map.state.as_ptr(), keycode) }
        })
    }

    /// The text `keycode` produces in the current state, compose aside.
    pub(crate) fn key_utf8(&self, keycode: u32) -> String {
        let Some(map) = &self.keymap else {
            return String::new();
        };
        let mut buf = [0u8; 64];
        // SAFETY: `buf` is writable for its length; the call NUL-terminates
        // within it and returns the untruncated length.
        let n = unsafe {
            (self.lib.xkb_state_key_get_utf8)(
                map.state.as_ptr(),
                keycode,
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len(),
            )
        };
        let n = usize::try_from(n).unwrap_or(0).min(buf.len() - 1);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    /// Whether holding `keycode` auto-repeats.
    pub(crate) fn repeats(&self, keycode: u32) -> bool {
        self.keymap.as_ref().is_some_and(|map| {
            // SAFETY: a live keymap.
            unsafe { (self.lib.xkb_keymap_key_repeats)(map.keymap.as_ptr(), keycode) != 0 }
        })
    }

    /// The unshifted character `keycode` prints, for matching accelerators.
    /// On a non-Latin layout the first Latin layout's character is used, so
    /// Ctrl+S still means Save under a Cyrillic layout.
    pub(crate) fn base_char(&self, keycode: u32) -> Option<char> {
        let map = self.keymap.as_ref()?;
        // SAFETY: a live state and keymap.
        let (current, layouts) = unsafe {
            (
                (self.lib.xkb_state_key_get_layout)(map.state.as_ptr(), keycode),
                (self.lib.xkb_keymap_num_layouts_for_key)(map.keymap.as_ptr(), keycode),
            )
        };
        let level0 = |layout: u32| -> Option<char> {
            let mut syms: *const u32 = ptr::null();
            // SAFETY: a live keymap; on success `syms` points at `n` keysyms
            // owned by the keymap.
            let n = unsafe {
                (self.lib.xkb_keymap_key_get_syms_by_level)(
                    map.keymap.as_ptr(),
                    keycode,
                    layout,
                    0,
                    &mut syms,
                )
            };
            if n < 1 || syms.is_null() {
                return None;
            }
            // SAFETY: `n >= 1` keysyms are readable at `syms`.
            let sym = unsafe { *syms };
            // SAFETY: a pure keysym conversion.
            char::from_u32(unsafe { (self.lib.xkb_keysym_to_utf32)(sym) })
                .filter(|c| *c != '\0' && !c.is_control())
                .map(|c| c.to_lowercase().next().unwrap_or(c))
        };
        let own = level0(current);
        if own.is_some_and(|c| c.is_ascii()) {
            return own;
        }
        (0..layouts).filter_map(level0).find(char::is_ascii).or(own)
    }

    /// Run a press of `keycode` through compose and return its text.
    pub(crate) fn press_text(&mut self, keycode: u32) -> KeyText {
        let sym = self.key_sym(keycode);
        if let Some(compose) = &self.compose {
            match compose.feed(sym) {
                Some(xkb_compose_status::XKB_COMPOSE_COMPOSING) => return KeyText::Composing,
                Some(xkb_compose_status::XKB_COMPOSE_COMPOSED) => {
                    let text = compose.utf8();
                    compose.reset();
                    return text_of(text);
                }
                Some(xkb_compose_status::XKB_COMPOSE_CANCELLED) => {
                    compose.reset();
                    return KeyText::None;
                }
                Some(xkb_compose_status::XKB_COMPOSE_NOTHING) | None => {}
            }
        }
        text_of(self.key_utf8(keycode))
    }

    pub(crate) fn reset_compose(&self) {
        if let Some(compose) = &self.compose {
            compose.reset();
        }
    }
}

fn text_of(text: String) -> KeyText {
    if super::translate::is_text(&text) {
        KeyText::Text(text)
    } else {
        KeyText::None
    }
}

impl Drop for Keyboard {
    fn drop(&mut self) {
        self.release_keymap();
        self.compose = None;
        // SAFETY: the context reference `new` took.
        unsafe { (self.lib.xkb_context_unref)(self.context.as_ptr()) };
    }
}

impl Compose {
    fn new(context: NonNull<xkb_context>) -> Option<Self> {
        let lib = xkbcommon_dl::xkbcommon_compose_option()?;
        let locale = compose_locale();
        // SAFETY: a live context and a NUL-terminated locale; the table is
        // released once the state (which keeps its own reference) exists.
        unsafe {
            let table = (lib.xkb_compose_table_new_from_locale)(
                context.as_ptr(),
                locale.as_ptr(),
                xkb_compose_compile_flags::XKB_COMPOSE_COMPILE_NO_FLAGS,
            );
            if table.is_null() {
                return None;
            }
            let state = (lib.xkb_compose_state_new)(
                table,
                xkb_compose_state_flags::XKB_COMPOSE_STATE_NO_FLAGS,
            );
            (lib.xkb_compose_table_unref)(table);
            Some(Self {
                lib,
                state: NonNull::new(state)?,
            })
        }
    }

    fn feed(&self, sym: u32) -> Option<xkb_compose_status> {
        // SAFETY: a live compose state.
        unsafe {
            if (self.lib.xkb_compose_state_feed)(self.state.as_ptr(), sym)
                == xkb_compose_feed_result::XKB_COMPOSE_FEED_IGNORED
            {
                return None;
            }
            Some((self.lib.xkb_compose_state_get_status)(self.state.as_ptr()))
        }
    }

    fn utf8(&self) -> String {
        let mut buf = [0u8; 64];
        // SAFETY: `buf` is writable for its length and NUL-terminated by the
        // call.
        let n = unsafe {
            (self.lib.xkb_compose_state_get_utf8)(
                self.state.as_ptr(),
                buf.as_mut_ptr().cast::<c_char>(),
                buf.len(),
            )
        };
        let n = usize::try_from(n).unwrap_or(0).min(buf.len() - 1);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    fn reset(&self) {
        // SAFETY: a live compose state.
        unsafe { (self.lib.xkb_compose_state_reset)(self.state.as_ptr()) };
    }
}

impl Drop for Compose {
    fn drop(&mut self) {
        // SAFETY: the state reference `new` created.
        unsafe { (self.lib.xkb_compose_state_unref)(self.state.as_ptr()) };
    }
}

/// The locale compose sequences follow: the first of `LC_ALL`, `LC_CTYPE`
/// and `LANG` that is set, as libX11 resolves it.
fn compose_locale() -> std::ffi::CString {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .find(|v| !v.is_empty())
        .unwrap_or_else(|| "C".to_string());
    std::ffi::CString::new(locale).unwrap_or_else(|_| c"C".to_owned())
}

/// Enable the XKB extension on `connection` for libxkbcommon-x11 and return
/// the core keyboard's device id.
///
/// # Safety
///
/// `connection` must be a live `xcb_connection_t`.
pub(crate) unsafe fn setup_x11(connection: *mut c_void) -> Option<i32> {
    let x11 = xkbcommon_dl::x11::xkbcommon_x11_option()?;
    let (mut major, mut minor, mut event, mut error) = (0u16, 0u16, 0u8, 0u8);
    // SAFETY: per the function contract; the out-pointers are live locals.
    unsafe {
        if (x11.xkb_x11_setup_xkb_extension)(
            connection,
            1,
            0,
            xkbcommon_dl::x11::xkb_x11_setup_xkb_extension_flags::XKB_X11_SETUP_XKB_EXTENSION_NO_FLAGS,
            &mut major,
            &mut minor,
            &mut event,
            &mut error,
        ) != 1
        {
            return None;
        }
        let device = (x11.xkb_x11_get_core_keyboard_device_id)(connection);
        (device >= 0).then_some(device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal US keymap: enough keys to cover text, shift levels, the
    /// Shift/Control modifiers and a dead key.
    const KEYMAP: &str = r#"xkb_keymap {
xkb_keycodes "t" { minimum = 8; maximum = 255;
  <AC01> = 38; <AC02> = 39; <LFSH> = 50; <LCTL> = 37; <AD12> = 35; <RTRN> = 36; <AE01> = 10; };
xkb_types "t" { include "complete" };
xkb_compatibility "t" { include "complete" };
xkb_symbols "t" {
  key <AC01> { [ a, A ] };
  key <AC02> { [ s, S ] };
  key <AE01> { [ 1, exclam ] };
  key <AD12> { [ dead_acute, dead_grave ] };
  key <RTRN> { [ Return ] };
  key <LFSH> { [ Shift_L ] };
  key <LCTL> { [ Control_L ] };
  modifier_map Shift { <LFSH> };
  modifier_map Control { <LCTL> };
};
};"#;

    fn keyboard() -> Option<Keyboard> {
        let mut kb = Keyboard::new()?;
        assert!(kb.set_keymap_text(KEYMAP.as_bytes()));
        Some(kb)
    }

    #[test]
    fn text_follows_shift_level_and_control_keys_are_not_text() {
        let Some(mut kb) = keyboard() else {
            eprintln!("libxkbcommon not installed; skipped");
            return;
        };
        assert_eq!(kb.press_text(38), KeyText::Text("a".into()));
        kb.update_mask(1, 0, 0, 0, 0, 0);
        assert!(kb.modifiers().shift);
        assert_eq!(kb.press_text(38), KeyText::Text("A".into()));
        assert_eq!(kb.press_text(10), KeyText::Text("!".into()));
        assert_eq!(kb.base_char(10), Some('1'));
        kb.update_mask(0, 0, 0, 0, 0, 0);
        assert_eq!(
            kb.press_text(36),
            KeyText::None,
            "Return is a key, not text"
        );
        assert!(kb.repeats(38));
    }

    #[test]
    fn dead_keys_compose_when_a_compose_table_exists() {
        let Some(mut kb) = keyboard() else {
            eprintln!("libxkbcommon not installed; skipped");
            return;
        };
        if kb.compose.is_none() {
            eprintln!("no compose table for this locale; skipped");
            return;
        }
        assert_eq!(kb.press_text(35), KeyText::Composing);
        assert_eq!(kb.press_text(38), KeyText::Text("á".into()));
    }
}
