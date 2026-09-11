//! The native application-menu model (menu bar, submenus, items, separators).
//!
//! A menu is a recursive tree the app hands down once (and rebuilds on change);
//! the platform layer walks it to construct the OS-native menu bar. This is a
//! *cold-path* structure — built at startup and on the rare menu edit, never
//! touched per frame — so it favors a plain owned tree over any packed encoding.
//!
//! Command identity is a compact [`MenuCommandId`] the app assigns, not a
//! string or hash: when the user picks a custom item the backend delivers a
//! [`RawEvent::MenuCommand`](crate::event::RawEvent::MenuCommand) carrying that
//! id, and the app matches on the small integer it chose. Standard actions
//! (Quit, Close, Hide, Minimize, …) route through the OS responder chain via
//! [`SystemAction`] instead — they need no command channel because AppKit and
//! its peers already know how to perform them.

/// A compact, app-assigned identity for a custom menu command.
///
/// The app picks the numbers (an enum `as u32`, a counter, whatever it likes);
/// the platform layer only echoes the chosen id back in
/// [`RawEvent::MenuCommand`](crate::event::RawEvent::MenuCommand). A `u32`, not
/// a string or hash: menu dispatch stays a single integer compare, and the
/// value carries no allocation across the OS boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MenuCommandId(pub u32);

/// A keyboard accelerator (shortcut) shown beside a menu item and honored by the
/// OS even when the menu is closed.
///
/// `key` is the accelerator's base character *exactly as the OS wants it* for
/// its key-equivalent field — e.g. `"q"`, `"s"`, `","` — kept as an owned
/// string rather than routed through [`KeyCode`](crate::event::KeyCode) on
/// purpose: the platform key vocabulary is deliberately minimal (no letter
/// keys), and an accelerator is fundamentally a display/OS concern, not a live
/// input sample. The [`Modifiers`](crate::event::Modifiers) `is_primary()` fold
/// is *not* reused here; menus name their modifiers concretely so the app can
/// say "Shift+Command" without the platform second-guessing it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Accel {
    /// The base key character for the OS key-equivalent (e.g. `"q"`, `","`).
    /// Empty means "no accelerator" and the modifiers are ignored.
    pub key: String,
    /// Require the platform accelerator modifier (Command on macOS). Almost
    /// every app shortcut sets this.
    pub primary: bool,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

impl Accel {
    /// A primary-modifier accelerator on `key` (the common case: Command-<key>
    /// on macOS). `Accel::primary("s")` is Command-S.
    pub fn primary(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            primary: true,
            ..Self::default()
        }
    }

    /// Whether this accelerator carries a usable key (non-empty base char).
    pub fn is_set(&self) -> bool {
        !self.key.is_empty()
    }
}

/// A standard, OS-known action that needs no app command channel.
///
/// These map to the platform's built-in responder-chain selectors (on macOS:
/// `terminate:`/pump-exit, `performClose:`, `hide:`, `miniaturize:`), so the OS
/// performs them and populates the conventional accelerators. Using these keeps
/// the standard menu behaving exactly as users expect per platform, without the
/// app reimplementing quit/close/hide itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemAction {
    /// Quit the application (clean pump exit on the Viso backend).
    Quit,
    /// Close the key window.
    CloseWindow,
    /// Hide the application.
    Hide,
    /// Minimize the key window.
    Minimize,
}

/// A node in the application-menu tree.
///
/// Recursive by construction: a [`Menu::Sub`] (and the top-level [`Menu::Main`])
/// owns a `Vec` of children, which may themselves be submenus. The backend walks
/// this once to build the OS menu.
#[derive(Debug, Clone, PartialEq)]
pub enum Menu {
    /// The root: the ordered top-level entries of the menu bar. Each child is
    /// normally a [`Menu::Sub`] (a titled bar menu), matching how a menu bar is
    /// a row of named drop-downs.
    Main { items: Vec<Menu> },
    /// A titled submenu holding its own ordered children.
    Sub { name: String, items: Vec<Menu> },
    /// A custom command item: picking it delivers `RawEvent::MenuCommand { id }`.
    Item {
        name: String,
        command: MenuCommandId,
        /// Optional keyboard accelerator; `None` (or an unset [`Accel`]) means no
        /// shortcut.
        accel: Option<Accel>,
        /// A disabled item is shown greyed and cannot be picked.
        enabled: bool,
    },
    /// A standard OS action item (Quit/Close/…), performed by the OS itself.
    /// Its title and accelerator default to the platform convention but can be
    /// overridden.
    System {
        action: SystemAction,
        /// Override the default title; `None` uses the platform's standard label
        /// (e.g. "Quit AppName").
        name: Option<String>,
    },
    /// A separator line between groups of items.
    Line,
}

impl Menu {
    /// A custom command item with an accelerator, enabled by default.
    pub fn item(name: impl Into<String>, command: MenuCommandId, accel: Accel) -> Self {
        Menu::Item {
            name: name.into(),
            command,
            accel: Some(accel),
            enabled: true,
        }
    }

    /// A custom command item with no accelerator, enabled by default.
    pub fn item_plain(name: impl Into<String>, command: MenuCommandId) -> Self {
        Menu::Item {
            name: name.into(),
            command,
            accel: None,
            enabled: true,
        }
    }

    /// A titled submenu owning `items`.
    pub fn sub(name: impl Into<String>, items: impl IntoIterator<Item = Menu>) -> Self {
        Menu::Sub {
            name: name.into(),
            items: items.into_iter().collect(),
        }
    }

    /// A standard OS action item with the platform's default title.
    pub fn system(action: SystemAction) -> Self {
        Menu::System { action, name: None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_produce_the_expected_tree() {
        let menu = Menu::Main {
            items: vec![Menu::sub(
                "File",
                [
                    Menu::item("Save", MenuCommandId(1), Accel::primary("s")),
                    Menu::Line,
                    Menu::system(SystemAction::CloseWindow),
                ],
            )],
        };
        let Menu::Main { items } = &menu else {
            panic!("expected Main");
        };
        let Menu::Sub { name, items } = &items[0] else {
            panic!("expected Sub");
        };
        assert_eq!(name, "File");
        assert_eq!(items.len(), 3);
        assert!(matches!(items[1], Menu::Line));
    }

    #[test]
    fn primary_accel_sets_the_command_modifier_only() {
        let a = Accel::primary("s");
        assert!(a.primary && a.is_set());
        assert!(!a.shift && !a.alt && !a.control);
    }

    #[test]
    fn empty_accel_is_not_set() {
        assert!(!Accel::default().is_set());
    }
}
