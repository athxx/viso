//! What a key does in the editor: the platform's text-field conventions for
//! moving and deleting, and the editor's own commands on the keys code
//! editors put them on.

use viso_ui::text_edit::{EditIntent, Motion, Reach};
use viso_ui::{FieldKey, Key, KeyEvent, field_key};

use crate::motion::Direction;

/// A command the editor runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Move the cursors, extending what they select with `extend`.
    Move {
        motion: Motion,
        extend: bool,
    },
    Backspace(Reach),
    Delete(Reach),
    /// Break the line at every cursor.
    Newline,
    /// Open a line below every cursor's, or above.
    OpenLine {
        above: bool,
    },
    /// Indent: to the next tab stop, or the selected lines.
    Tab,
    /// Take a level of indentation off the selected lines.
    Outdent,
    IndentLines,
    ToggleComment,
    MoveLines(Direction),
    /// Copy the selected lines below them, or above.
    DuplicateLines {
        above: bool,
    },
    DeleteLines,
    SelectAll,
    /// Extend the cursors over their whole lines.
    SelectLines,
    /// Add a cursor on the next match of the selection.
    AddNextMatch,
    /// A cursor on every match of the selection.
    SelectAllMatches,
    /// Add a cursor a row above, or below.
    AddCursor(Direction),
    /// One cursor; a caret; close a popup or the find bar.
    Escape,
    Undo,
    Redo,
    Copy,
    Cut,
    /// Open the find bar; with `replace`, its replace row too.
    Find {
        replace: bool,
    },
    /// To the next match, or the one before.
    FindNext {
        back: bool,
    },
    /// Fold the range at the primary caret, or open it.
    Fold,
    Unfold,
    FoldAll,
    UnfoldAll,
    /// Ask the language service what can go at the caret.
    Complete,
    /// Ask where the symbol at the caret is declared, and go there.
    GoToDefinition,
    /// Show what the language service says of the symbol at the caret.
    Hover,
}

/// What key `k` does, on Apple's conventions when `apple`.
pub fn command_for(k: &KeyEvent, apple: bool) -> Option<Command> {
    if !k.pressed {
        return None;
    }
    let m = k.modifiers;
    let primary = if apple { m.logo } else { m.control };
    let other = if apple { m.control } else { m.logo };
    // The primary modifier alone, Shift aside.
    let cmd = primary && !m.alt && !other;
    let plain = !primary && !m.alt && !other;
    let cmd_alt = primary && m.alt && !other;
    let alt = m.alt && !primary && !other;
    use Command as C;
    let command = match k.key {
        Key::Tab if plain && m.shift => C::Outdent,
        Key::Tab if plain => C::Tab,
        Key::Escape if plain && !m.shift => C::Escape,
        Key::Enter if cmd => C::OpenLine { above: m.shift },
        Key::Enter if plain => C::Newline,
        Key::C if cmd && !m.shift => C::Copy,
        Key::X if cmd && !m.shift => C::Cut,
        Key::D if cmd && !m.shift => C::AddNextMatch,
        Key::L if cmd && m.shift => C::SelectAllMatches,
        Key::L if cmd => C::SelectLines,
        Key::K if cmd && m.shift => C::DeleteLines,
        Key::Slash if cmd && !m.shift => C::ToggleComment,
        Key::BracketRight if cmd && !m.shift => C::IndentLines,
        Key::BracketLeft if cmd && !m.shift => C::Outdent,
        Key::Up if alt && m.shift => C::DuplicateLines { above: true },
        Key::Down if alt && m.shift => C::DuplicateLines { above: false },
        Key::Up if alt => C::MoveLines(Direction::Back),
        Key::Down if alt => C::MoveLines(Direction::Forward),
        Key::Up if apple && cmd_alt => C::AddCursor(Direction::Back),
        Key::Down if apple && cmd_alt => C::AddCursor(Direction::Forward),
        Key::Up if !apple && cmd_alt && !m.shift => C::AddCursor(Direction::Back),
        Key::Down if !apple && cmd_alt && !m.shift => C::AddCursor(Direction::Forward),
        Key::F if cmd && !m.shift => C::Find { replace: false },
        Key::F if apple && cmd_alt => C::Find { replace: true },
        Key::H if !apple && cmd && !m.shift => C::Find { replace: true },
        Key::G if cmd => C::FindNext { back: m.shift },
        Key::F3 if plain => C::FindNext { back: m.shift },
        Key::BracketLeft if apple && cmd_alt => C::Fold,
        Key::BracketRight if apple && cmd_alt => C::Unfold,
        Key::BracketLeft if !apple && cmd && m.shift => C::Fold,
        Key::BracketRight if !apple && cmd && m.shift => C::Unfold,
        Key::Digit0 if cmd && m.shift => C::UnfoldAll,
        Key::Digit9 if cmd && m.shift => C::FoldAll,
        Key::Space if m.control && !m.alt && !m.logo => C::Complete,
        Key::F12 if plain && !m.shift => C::GoToDefinition,
        Key::I if cmd && m.shift => C::Hover,
        // Paste comes as text, not as its key.
        Key::V if cmd => return None,
        _ => {
            return match field_key(k, true)? {
                FieldKey::Edit(EditIntent::Move { motion, extend }) => {
                    Some(C::Move { motion, extend })
                }
                FieldKey::Edit(EditIntent::Backspace(reach)) => Some(C::Backspace(reach)),
                FieldKey::Edit(EditIntent::Delete(reach)) => Some(C::Delete(reach)),
                FieldKey::Edit(EditIntent::SelectAll) => Some(C::SelectAll),
                FieldKey::Edit(EditIntent::Undo) => Some(C::Undo),
                FieldKey::Edit(EditIntent::Redo) => Some(C::Redo),
                FieldKey::Edit(EditIntent::Insert(t)) if t == "\n" => Some(C::Newline),
                _ => None,
            };
        }
    };
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::Modifiers;

    fn press(key: Key, m: Modifiers) -> KeyEvent {
        KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: m,
        }
    }

    #[test]
    fn keys_map_to_commands_on_each_platform() {
        let cmd = Modifiers {
            logo: true,
            ..Modifiers::default()
        };
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(
            command_for(&press(Key::D, cmd), true),
            Some(Command::AddNextMatch)
        );
        assert_eq!(
            command_for(&press(Key::D, ctrl), false),
            Some(Command::AddNextMatch)
        );
        assert_eq!(command_for(&press(Key::V, cmd), true), None);
        let alt = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(
            command_for(&press(Key::Up, alt), true),
            Some(Command::MoveLines(Direction::Back))
        );
        let cmd_alt = Modifiers { alt: true, ..cmd };
        assert_eq!(
            command_for(&press(Key::Down, cmd_alt), true),
            Some(Command::AddCursor(Direction::Forward))
        );
        assert_eq!(
            command_for(&press(Key::Left, alt), true),
            Some(Command::Move {
                motion: Motion::WordLeft,
                extend: false
            })
        );
        assert_eq!(
            command_for(&press(Key::Tab, Modifiers::default()), true),
            Some(Command::Tab)
        );
    }
}
