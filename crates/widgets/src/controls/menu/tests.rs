use super::*;

#[test]
fn a_shortcut_is_spelled_the_platform_way() {
    let save_as = Shortcut::primary(Key::S).shift();
    let quit = Shortcut::new(
        Key::F4,
        Modifiers {
            alt: true,
            ..Modifiers::default()
        },
    );
    if primary_is_logo() {
        assert_eq!(save_as.text(), "⇧⌘S");
        assert_eq!(quit.text(), "⌥F4");
        assert_eq!(Shortcut::primary(Key::Enter).text(), "⌘↩");
    } else {
        assert_eq!(save_as.text(), "Ctrl+Shift+S");
        assert_eq!(quit.text(), "Alt+F4");
        assert_eq!(Shortcut::primary(Key::Digit1).text(), "Ctrl+1");
    }
}

#[test]
fn an_item_with_entries_is_a_submenu() {
    let share = menu_item("Share")
        .item(menu_item("Mail"))
        .separator()
        .item(menu_item("Copy link"));
    assert_eq!(share.entries.len(), 3);
    assert!(matches!(share.entries[1], MenuEntry::Separator));
}
