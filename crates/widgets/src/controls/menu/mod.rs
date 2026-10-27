//! Menus — lists of commands that open over the content: a [`menu`] from
//! the anchor it hangs off, a [`context_menu`] at the press that asks for it.
//!
//! A menu holds [`MenuItem`]s split into sections by separators. An item
//! shows its label and, if it has them, an icon, the text of its shortcut,
//! a check or radio mark, and the chevron of a submenu; an item may be
//! disabled, which dims it and takes no pick. Picking an item closes the
//! menu and runs its [`on_pick`](MenuItem::on_pick); a check item turns
//! over and a radio item becomes the one chosen in its section first, and
//! [`on_check`](MenuItem::on_check) hears the new state.
//!
//! The menu is an anchored layer ([`viso_ui::anchored`]): below its anchor
//! (a context menu beside the press), flipped and shifted to stay in the
//! window. A submenu sits beside the item that opened it, its first item
//! level with that item. A press outside every open menu closes them.
//!
//! The pointer: a press on the anchor opens the menu, and a release on an
//! item picks it, so a press, drag and release picks in one gesture; a
//! release near where the menu opened does not. Hovering highlights an item;
//! resting on one with a submenu opens it after [`HOVER_OPEN`], and a click
//! opens it at once. While the pointer heads for an open submenu across
//! other items it stays open ([`HOVER_GRACE`]). A context menu opens on a
//! secondary press (Control and press on Apple systems), a long press, and,
//! off Apple systems, the menu key or Shift+F10.
//!
//! The keyboard: Enter, Space or Down on the anchor opens the menu with its
//! first item highlighted, Up with its last. In the menu Up and Down move the
//! highlight over the enabled items, wrapping, Home and End to the first and
//! last; the reading-direction key (Right, Left right to left) opens the
//! highlighted item's submenu and the other goes back; Enter and Space pick
//! the highlighted item; typing jumps to the next item whose label starts
//! with what was typed in the last second; Escape closes one level, Tab all
//! of them. A closed menu gives the focus back to where it was.
//!
//! An `&` in a label marks its access key (`&Open` answers to O, `&&` is an
//! `&` of the text): typing it in the menu picks the item, or steps between
//! the items sharing it; a menu with none jumps by what was typed. A menu
//! the keyboard opened underlines its access keys.
//!
//! A [`menu_bar`] is a row of such menus under their titles: a press on a
//! title opens its menu and, while one shows, the pointer crossing another
//! title opens that one. Off Apple systems Alt alone or F10 takes the
//! keyboard to it and Alt and a title's access key opens its menu
//! ([`viso_ui::access`]); Left and Right move between the titles and, in a
//! menu, to the next menu where no submenu opens; Escape closes the menu,
//! then leaves the bar, the focus back where it was.
//!
//! Semantics: the menu is a `menu`, its items `menuitem`,
//! `menuitemcheckbox` or `menuitemradio` announcing whether they are
//! checked, an item with a submenu whether it is expanded, a separator a
//! `separator`; a menu's anchor is a `button` announcing whether its menu is
//! expanded; a menu bar is a `menubar` of `menuitem` titles announcing the
//! same.
//!
//! Invalidation: the menus build once with their anchor. Opening and closing
//! show and hide them; a highlight is a `PAINT` of one row's look.

mod bar;
mod build;
mod state;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use viso_ui::ColorToken;

use viso_ui::{
    Anchoring, BoxStyle, BuildCx, Component, ControlHandle, EventCx, Inset, Key, Modifiers, Rgba,
    Shadow, Sizing, primary_is_logo,
};

use crate::Icon;

pub use bar::{MenuBar, MenuBarHandle, MenuBarStyle, menu_bar};
pub use state::MenuHandle;

/// How long the pointer rests on an item before its submenu opens.
pub const HOVER_OPEN: Duration = Duration::from_millis(200);

/// How long a submenu stays open while the pointer heads for it across
/// other items.
pub const HOVER_GRACE: Duration = Duration::from_millis(300);

/// How long what was typed counts toward the label it jumps to.
pub const TYPE_AHEAD: Duration = Duration::from_millis(1000);

/// A key with modifiers, as an item shows it beside its label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub key: Key,
    pub modifiers: Modifiers,
}

impl Shortcut {
    pub const fn new(key: Key, modifiers: Modifiers) -> Self {
        Shortcut { key, modifiers }
    }

    /// `key` with the platform's accelerator: Command on Apple systems,
    /// Control elsewhere.
    pub fn primary(key: Key) -> Self {
        let apple = primary_is_logo();
        Shortcut {
            key,
            modifiers: Modifiers {
                logo: apple,
                control: !apple,
                ..Modifiers::default()
            },
        }
    }

    /// This with Shift held as well.
    pub fn shift(mut self) -> Self {
        self.modifiers.shift = true;
        self
    }

    /// This with Alt (Option) held as well.
    pub fn alt(mut self) -> Self {
        self.modifiers.alt = true;
        self
    }

    /// How the platform spells it: `⌃⌥⇧⌘K` on Apple systems, `Ctrl+Alt+Shift+K`
    /// elsewhere.
    pub fn text(self) -> String {
        let apple = primary_is_logo();
        let m = self.modifiers;
        let mut out = String::new();
        if apple {
            for (held, sign) in [
                (m.control, '⌃'),
                (m.alt, '⌥'),
                (m.shift, '⇧'),
                (m.logo, '⌘'),
            ] {
                if held {
                    out.push(sign);
                }
            }
        } else {
            for (held, word) in [
                (m.control, "Ctrl+"),
                (m.logo, "Win+"),
                (m.alt, "Alt+"),
                (m.shift, "Shift+"),
            ] {
                if held {
                    out.push_str(word);
                }
            }
        }
        out.push_str(&key_name(self.key, apple));
        out
    }
}

/// The name a shortcut shows for `key`.
fn key_name(key: Key, apple: bool) -> String {
    let named = match (key, apple) {
        (Key::Enter, true) => "↩",
        (Key::Enter, false) => "Enter",
        (Key::Escape, true) => "⎋",
        (Key::Escape, false) => "Esc",
        (Key::Tab, true) => "⇥",
        (Key::Tab, false) => "Tab",
        (Key::Space, _) => "Space",
        (Key::Backspace, true) => "⌫",
        (Key::Backspace, false) => "Backspace",
        (Key::Delete, true) => "⌦",
        (Key::Delete, false) => "Del",
        (Key::Left, _) => "←",
        (Key::Right, _) => "→",
        (Key::Up, _) => "↑",
        (Key::Down, _) => "↓",
        (Key::Home, true) => "↖",
        (Key::Home, false) => "Home",
        (Key::End, true) => "↘",
        (Key::End, false) => "End",
        (Key::PageUp, true) => "⇞",
        (Key::PageUp, false) => "PgUp",
        (Key::PageDown, true) => "⇟",
        (Key::PageDown, false) => "PgDn",
        (Key::Insert, _) => "Ins",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    // Letters, digits and function keys spell themselves.
    format!("{key:?}").trim_start_matches("Digit").to_owned()
}

/// What an item's pick runs, told whether a check or radio item is now
/// checked.
pub(crate) type Pick = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, bool)>>;

/// What runs when the menus close.
pub(crate) type Close = Rc<RefCell<dyn FnMut(&mut EventCx<'_>)>>;

/// A builder of the content a menu hangs off or opens over.
type Builder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// An item's mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    /// A check, turned over by a pick.
    Check(bool),
    /// A radio mark, one chosen in its section.
    Radio(bool),
}

/// One command of a menu. Construct one with [`menu_item`].
pub struct MenuItem {
    pub(crate) label: String,
    pub(crate) shortcut: Option<String>,
    pub(crate) mark: Option<Mark>,
    pub(crate) enabled: bool,
    pub(crate) icon: Option<Icon>,
    pub(crate) pick: Option<Pick>,
    /// Its submenu's entries; empty for a plain command.
    pub(crate) entries: Vec<MenuEntry>,
}

/// An entry of a menu: an item, or a line between sections.
pub enum MenuEntry {
    Item(Box<MenuItem>),
    Separator,
}

impl From<MenuItem> for MenuEntry {
    fn from(item: MenuItem) -> Self {
        MenuEntry::Item(Box::new(item))
    }
}

/// A menu item labelled `label`.
pub fn menu_item(label: impl Into<String>) -> MenuItem {
    MenuItem {
        label: label.into(),
        shortcut: None,
        mark: None,
        enabled: true,
        icon: None,
        pick: None,
        entries: Vec::new(),
    }
}

/// A line between sections of a menu.
pub fn separator() -> MenuEntry {
    MenuEntry::Separator
}

impl MenuItem {
    /// Show `shortcut` beside the label. The menu shows it; the app binds it.
    pub fn shortcut(mut self, shortcut: Shortcut) -> Self {
        self.shortcut = Some(shortcut.text());
        self
    }

    /// Show `text` where a shortcut goes.
    pub fn shortcut_text(mut self, text: impl Into<String>) -> Self {
        self.shortcut = Some(text.into());
        self
    }

    /// Make it a check item, checked or not; a pick turns it over.
    pub fn checked(mut self, checked: bool) -> Self {
        self.mark = Some(Mark::Check(checked));
        self
    }

    /// Make it a radio item, chosen or not; a pick chooses it and unchooses
    /// the other radio items of its section.
    pub fn radio(mut self, chosen: bool) -> Self {
        self.mark = Some(Mark::Radio(chosen));
        self
    }

    /// Enable or disable it (default enabled).
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    /// Show `icon` before the label.
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Run `pick` when it is picked, after the menus close.
    pub fn on_pick(mut self, mut pick: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        self.pick = Some(Rc::new(RefCell::new(move |ev: &mut EventCx<'_>, _| {
            pick(ev)
        })));
        self
    }

    /// Run `check` with a check or radio item's new state when it is picked,
    /// after the menus close.
    pub fn on_check(mut self, check: impl FnMut(&mut EventCx<'_>, bool) + 'static) -> Self {
        self.pick = Some(Rc::new(RefCell::new(check)));
        self
    }

    /// Add `item` to its submenu.
    pub fn item(mut self, item: MenuItem) -> Self {
        self.entries.push(item.into());
        self
    }

    /// Add a separator to its submenu.
    pub fn separator(mut self) -> Self {
        self.entries.push(MenuEntry::Separator);
        self
    }

    /// Add `entries` to its submenu.
    pub fn items(mut self, entries: impl IntoIterator<Item = MenuEntry>) -> Self {
        self.entries.extend(entries);
        self
    }
}

/// How a menu looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MenuStyle {
    /// The panel the items sit in.
    pub panel: BoxStyle,
    pub shadow: Option<Shadow>,
    /// The room between the panel's edge and its items.
    pub padding: Inset,
    /// The room inside an item.
    pub item_padding: Inset,
    /// The least width of a panel.
    pub min_width: f32,
    pub font_size: f32,
    pub ink: Rgba,
    /// The ink of shortcuts and chevrons.
    pub quiet: Rgba,
    /// A highlighted item's box.
    pub highlight: BoxStyle,
    /// A separator's line.
    pub rule: Rgba,
    /// The opacity of a disabled item.
    pub disabled: f32,
    /// Where a menu sits against its anchor.
    pub placement: Anchoring,
}

impl Default for MenuStyle {
    fn default() -> Self {
        MenuStyle {
            panel: BoxStyle {
                border: viso_ui::Border {
                    width: 1.0,
                    color: ColorToken::Outline.rgba(),
                },
                ..BoxStyle::solid(ColorToken::Surface.rgba()).with_radius(8.0)
            },
            shadow: Some(Shadow::FLOATING),
            padding: Inset::all(5.0),
            item_padding: Inset {
                left: 10.0,
                right: 12.0,
                top: 5.0,
                bottom: 5.0,
            },
            min_width: 180.0,
            font_size: 14.0,
            ink: ColorToken::Foreground.rgba(),
            quiet: ColorToken::Muted.rgba(),
            highlight: BoxStyle::solid(ColorToken::Primary.alpha(0.14)).with_radius(5.0),
            rule: ColorToken::Outline.rgba(),
            disabled: 0.4,
            placement: Anchoring::default(),
        }
    }
}

/// A menu that opens from its anchor — a button, a toolbar item.
pub struct Menu {
    entries: Vec<MenuEntry>,
    anchor: Option<Builder>,
    style: MenuStyle,
    on_close: Option<Close>,
    /// The handle `build` binds, when the app minted one up front.
    handle: Option<MenuHandle>,
}

/// A menu with no anchor and no items yet.
pub fn menu() -> Menu {
    Menu {
        entries: Vec::new(),
        anchor: None,
        style: MenuStyle::default(),
        on_close: None,
        handle: None,
    }
}

impl Menu {
    /// Set what the menu hangs off: a press on it opens the menu.
    pub fn anchor(mut self, anchor: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.anchor = Some(Box::new(anchor));
        self
    }

    /// Add `item`.
    pub fn item(mut self, item: MenuItem) -> Self {
        self.entries.push(item.into());
        self
    }

    /// Add a separator.
    pub fn separator(mut self) -> Self {
        self.entries.push(MenuEntry::Separator);
        self
    }

    /// Add `entries`.
    pub fn items(mut self, entries: impl IntoIterator<Item = MenuEntry>) -> Self {
        self.entries.extend(entries);
        self
    }

    /// Replace the whole [`MenuStyle`].
    pub fn style(mut self, style: MenuStyle) -> Self {
        self.style = style;
        self
    }

    /// Set where the menu sits against its anchor.
    pub fn placement(mut self, placement: Anchoring) -> Self {
        self.style.placement = placement;
        self
    }

    /// Run `close` whenever the menus close, picked or dismissed.
    pub fn on_close(mut self, close: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        self.on_close = Some(Rc::new(RefCell::new(close)));
        self
    }

    /// Bind `handle`, minted with [`BuildCx::handle`] before the menu is
    /// built, so closures declared earlier can drive it.
    pub fn handle(mut self, handle: MenuHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Build the menu into `cx` and return its handle.
    pub fn build(mut self, cx: &mut BuildCx<'_>) -> MenuHandle {
        let handle = *self.handle.get_or_insert_with(|| cx.handle());
        Component::build(&self, cx);
        handle
    }
}

impl Component for Menu {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let menu = build::menu_button(
            cx,
            &self.entries,
            self.anchor.as_deref(),
            &self.style,
            self.on_close.clone(),
        );
        if let Some(bound) = self.handle {
            let panel = menu.levels[0].panel;
            cx.bind_control(bound.id(), panel, menu);
        }
    }
}

/// Content that opens a menu at the press that asks for one.
pub struct ContextMenu {
    entries: Vec<MenuEntry>,
    content: Option<Builder>,
    size: Sizing,
    style: MenuStyle,
    on_close: Option<Close>,
    /// The handle `build` binds, when the app minted one up front.
    handle: Option<MenuHandle>,
}

/// A context menu with no content and no items yet; it fills its parent's
/// room.
pub fn context_menu() -> ContextMenu {
    ContextMenu {
        entries: Vec::new(),
        content: None,
        size: Sizing::fill(),
        style: MenuStyle::default(),
        on_close: None,
        handle: None,
    }
}

impl ContextMenu {
    /// Set the content a press on which opens the menu.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Add `item`.
    pub fn item(mut self, item: MenuItem) -> Self {
        self.entries.push(item.into());
        self
    }

    /// Add a separator.
    pub fn separator(mut self) -> Self {
        self.entries.push(MenuEntry::Separator);
        self
    }

    /// Add `entries`.
    pub fn items(mut self, entries: impl IntoIterator<Item = MenuEntry>) -> Self {
        self.entries.extend(entries);
        self
    }

    /// Set the content's size within its parent (default: its room).
    pub fn size(mut self, size: Sizing) -> Self {
        self.size = size;
        self
    }

    /// Replace the whole [`MenuStyle`].
    pub fn style(mut self, style: MenuStyle) -> Self {
        self.style = style;
        self
    }

    /// Run `close` whenever the menus close, picked or dismissed.
    pub fn on_close(mut self, close: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        self.on_close = Some(Rc::new(RefCell::new(close)));
        self
    }

    /// Bind `handle`, minted with [`BuildCx::handle`] before the menu is
    /// built, so closures declared earlier can drive it.
    pub fn handle(mut self, handle: MenuHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Build the menu into `cx` and return its handle.
    pub fn build(mut self, cx: &mut BuildCx<'_>) -> MenuHandle {
        let handle = *self.handle.get_or_insert_with(|| cx.handle());
        Component::build(&self, cx);
        handle
    }
}

impl Component for ContextMenu {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let menu = build::context_region(
            cx,
            &self.entries,
            self.content.as_deref(),
            self.size,
            &self.style,
            self.on_close.clone(),
        );
        if let Some(bound) = self.handle {
            let panel = menu.levels[0].panel;
            cx.bind_control(bound.id(), panel, menu);
        }
    }
}

#[cfg(test)]
mod tests;
