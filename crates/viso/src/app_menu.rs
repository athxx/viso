//! The menu bar the window draws where the platform shows none.
//!
//! An application's [`Menu`](viso_platform::Menu) is the system's menu bar
//! where there is one; where there is none (X11, Wayland), a captioned window
//! draws it under its caption as a [`MenuBar`](viso_widgets::MenuBar). Its
//! titles and items get access keys of their own: each the first letter of a
//! word, else any letter, that no sibling took. A pick reaches the window
//! through the shared [`Picks`] queue: a command goes to
//! [`Application::on_menu_command`](crate::Application::on_menu_command), a
//! standard action does what the system's would. The actions the window
//! cannot do itself (Hide, Minimize) are left out.

use std::cell::RefCell;
use std::rc::Rc;

use viso_platform::{Accel, Menu, MenuCommandId, SystemAction, WindowId};
use viso_widgets::{MenuBar, MenuEntry, MenuItem, menu_bar, menu_item};

/// What a pick from a drawn menu bar asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pick {
    Command(MenuCommandId),
    System(SystemAction),
}

/// The picks of the windows' drawn menu bars, by window, until the driver
/// takes them.
pub(crate) type Picks = Rc<RefCell<Vec<(WindowId, Pick)>>>;

/// The bar `menu` draws in `window`, its picks queued on `picks`; `None`
/// when it has no titled menu.
pub(crate) fn bar(menu: &Menu, window: WindowId, picks: &Picks) -> Option<MenuBar> {
    let Menu::Main { items } = menu else {
        return None;
    };
    let titled: Vec<(&str, &[Menu])> = items
        .iter()
        .filter_map(|m| match m {
            Menu::Sub { name, items } => Some((name.as_str(), items.as_slice())),
            _ => None,
        })
        .collect();
    if titled.is_empty() {
        return None;
    }
    let names: Vec<&str> = titled.iter().map(|&(name, _)| name).collect();
    let marked = with_access_keys(&names);
    let menus = titled
        .iter()
        .zip(marked)
        .map(|(&(_, items), title)| menu_item(title).items(entries(items, window, picks)));
    Some(menu_bar().menus(menus))
}

/// The entries of a menu's `items`.
fn entries(items: &[Menu], window: WindowId, picks: &Picks) -> Vec<MenuEntry> {
    let shown: Vec<&Menu> = items
        .iter()
        .filter(|m| {
            !matches!(
                m,
                Menu::System {
                    action: SystemAction::Hide | SystemAction::Minimize,
                    ..
                } | Menu::Main { .. }
            )
        })
        .collect();
    let names: Vec<String> = shown
        .iter()
        .map(|m| match m {
            Menu::Sub { name, .. } | Menu::Item { name, .. } => name.clone(),
            Menu::System { action, name } => name.clone().unwrap_or_else(|| title(*action)),
            Menu::Line | Menu::Main { .. } => String::new(),
        })
        .collect();
    let named: Vec<&str> = names.iter().map(String::as_str).collect();
    let marked = with_access_keys(&named);
    let mut out = Vec::with_capacity(shown.len());
    for (m, label) in shown.into_iter().zip(marked) {
        let picked = |pick: Pick| {
            let picks = Rc::clone(picks);
            move |_: &mut viso_ui::EventCx<'_>| picks.borrow_mut().push((window, pick))
        };
        let item: MenuItem = match m {
            Menu::Line => {
                out.push(viso_widgets::separator());
                continue;
            }
            Menu::Sub { items, .. } => menu_item(label).items(entries(items, window, picks)),
            Menu::Item {
                command,
                accel,
                enabled,
                ..
            } => {
                let item = menu_item(label)
                    .enabled(*enabled)
                    .on_pick(picked(Pick::Command(*command)));
                match accel.as_ref().filter(|a| a.is_set()) {
                    Some(accel) => item.shortcut_text(accel_text(accel)),
                    None => item,
                }
            }
            Menu::System { action, .. } => {
                let item = menu_item(label).on_pick(picked(Pick::System(*action)));
                match system_shortcut(*action) {
                    Some(text) => item.shortcut_text(text),
                    None => item,
                }
            }
            Menu::Main { .. } => continue,
        };
        out.push(item.into());
    }
    out
}

/// A standard action's title where the app gives none.
fn title(action: SystemAction) -> String {
    match action {
        SystemAction::Quit => "Quit",
        SystemAction::CloseWindow => "Close Window",
        SystemAction::Hide => "Hide",
        SystemAction::Minimize => "Minimize",
        SystemAction::Copy => "Copy",
        SystemAction::Cut => "Cut",
        SystemAction::Paste => "Paste",
    }
    .to_owned()
}

/// The shortcut a standard action answers to off Apple systems.
fn system_shortcut(action: SystemAction) -> Option<&'static str> {
    match action {
        SystemAction::Quit => Some("Ctrl+Q"),
        SystemAction::CloseWindow => Some("Ctrl+W"),
        SystemAction::Copy => Some("Ctrl+C"),
        SystemAction::Cut => Some("Ctrl+X"),
        SystemAction::Paste => Some("Ctrl+V"),
        SystemAction::Hide | SystemAction::Minimize => None,
    }
}

/// How an accelerator spells itself off Apple systems: `Ctrl+Shift+S`.
fn accel_text(accel: &Accel) -> String {
    let mut out = String::new();
    for (held, word) in [
        (accel.control || accel.primary, "Ctrl+"),
        (accel.alt, "Alt+"),
        (accel.shift, "Shift+"),
    ] {
        if held {
            out.push_str(word);
        }
    }
    let mut chars = accel.key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => out.extend(c.to_uppercase()),
        _ => out.push_str(&accel.key),
    }
    out
}

/// `names` with an access key marked in each: the first letter of a word no
/// earlier name took, else any letter none took, else none. An `&` of a
/// name is doubled to stay text.
fn with_access_keys(names: &[&str]) -> Vec<String> {
    let mut taken: Vec<char> = Vec::new();
    names
        .iter()
        .map(|name| {
            let letters: Vec<(usize, char, bool)> = name
                .char_indices()
                .scan(true, |start, (at, c)| {
                    let word = *start;
                    *start = !c.is_alphanumeric();
                    Some((at, c, word))
                })
                .filter(|&(_, c, _)| c.is_alphanumeric())
                .collect();
            let free = |&&(_, c, _): &&(usize, char, bool)| {
                let c = c.to_lowercase().next().unwrap_or(c);
                !taken.contains(&c)
            };
            let pick = letters
                .iter()
                .filter(|l| l.2)
                .find(free)
                .or_else(|| letters.iter().find(free))
                .map(|&(at, c, _)| (at, c));
            let mut out = String::with_capacity(name.len() + 1);
            for (at, c) in name.char_indices() {
                if pick.is_some_and(|(p, _)| p == at) {
                    out.push('&');
                }
                if c == '&' {
                    out.push('&');
                }
                out.push(c);
            }
            if let Some((_, c)) = pick {
                taken.push(c.to_lowercase().next().unwrap_or(c));
            }
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_name_takes_a_free_letter_starting_a_word_first() {
        assert_eq!(
            with_access_keys(&["File", "Edit", "Format", "Find Next", "Fix & Go", ""]),
            ["&File", "&Edit", "F&ormat", "Find &Next", "Fix && &Go", ""]
        );
    }

    #[test]
    fn an_accelerator_spells_its_modifiers_and_key() {
        let accel = Accel {
            shift: true,
            ..Accel::primary("s")
        };
        assert_eq!(accel_text(&accel), "Ctrl+Shift+S");
        assert_eq!(accel_text(&Accel::primary("F5")), "Ctrl+F5");
    }
}
