//! What a text field does with its input: the one handler every text field
//! runs, and the keys it answers to.
//!
//! A press focuses the field, captures the pointer and places the caret, a
//! double click selects the word under it and a triple click the paragraph;
//! a drag with the button held extends the selection, by whole words or
//! paragraphs after a double or triple click. Keys move the caret,
//! delete, select, undo and redo by the platform's conventions; the IME
//! composes and commits, and a paste replaces the selection. Each becomes an
//! [`EditIntent`] the router applies to the field's buffer, so the handler
//! reads no buffer. Enter submits a single-line field and starts a new line
//! in a multi-line one, where the accelerator with Enter submits.
//!
//! ## Key conventions
//!
//! On Apple systems (and a browser on one) Option moves and deletes by word,
//! Command to the ends of the line and, with Up and Down, of the text, and
//! Option with Up and Down to the ends of the paragraph; Control with A, E,
//! B, F, P, N, H, D and K are the Emacs motions and deletions the system
//! text fields take. Elsewhere Control moves and deletes by word, Control
//! with Home and End goes to the ends of the text, Control with Up and Down
//! to the ends of the paragraph, and Control with Y redoes. Shift extends the
//! selection with every motion.

use viso_runtime::{Key, primary_is_logo};

use crate::context::EventCx;
use crate::input::{DispatchPhase, ImeEvent, KeyEvent, PointerButtons, PointerPhase};
use crate::text_edit::{EditIntent, Motion, Reach, Unit};
use viso_text::TextOffset;

/// What a key does in a text field.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKey {
    /// Edits the field.
    Edit(EditIntent),
    /// Submits it.
    Submit,
}

/// What a field's input asks of the control that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldEvent {
    /// Submit what the field holds.
    Submitted,
}

/// What `k` does in a text field under the platform's key conventions;
/// `None` for a release or a key a field has no use for.
pub fn field_key(k: &KeyEvent, multiline: bool) -> Option<FieldKey> {
    field_key_in(k, multiline, primary_is_logo())
}

/// [`field_key`] under Apple's conventions when `apple`, else the others'.
pub fn field_key_in(k: &KeyEvent, multiline: bool, apple: bool) -> Option<FieldKey> {
    if !k.pressed {
        return None;
    }
    let m = k.modifiers;
    let extend = m.shift;
    let primary = if apple { m.logo } else { m.control };
    // The modifier that moves by word, alone.
    let word = if apple {
        m.alt && !m.logo && !m.control
    } else {
        m.control && !m.alt && !m.logo
    };
    // Apple's Command, alone: to the ends of the line or the text.
    let far = apple && m.logo && !m.alt && !m.control;
    let emacs = apple && m.control && !m.logo && !m.alt;
    let to = |motion| Some(FieldKey::Edit(EditIntent::Move { motion, extend }));
    let edit = |intent| Some(FieldKey::Edit(intent));
    match k.key {
        Key::Left if word => to(Motion::WordLeft),
        Key::Right if word => to(Motion::WordRight),
        Key::Left if far => to(Motion::LineStart),
        Key::Right if far => to(Motion::LineEnd),
        Key::Left => to(Motion::Left),
        Key::Right => to(Motion::Right),
        Key::Up if far => to(Motion::DocStart),
        Key::Down if far => to(Motion::DocEnd),
        Key::Up if (apple && word) || (!apple && m.control) => to(Motion::ParagraphStart),
        Key::Down if (apple && word) || (!apple && m.control) => to(Motion::ParagraphEnd),
        Key::Up => to(Motion::Up),
        Key::Down => to(Motion::Down),
        Key::Home if !apple && m.control => to(Motion::DocStart),
        Key::End if !apple && m.control => to(Motion::DocEnd),
        Key::Home => to(Motion::LineStart),
        Key::End => to(Motion::LineEnd),
        Key::PageUp => to(Motion::PageUp),
        Key::PageDown => to(Motion::PageDown),
        Key::Backspace if word => edit(EditIntent::Backspace(Reach::Word)),
        Key::Backspace if far => edit(EditIntent::Backspace(Reach::Line)),
        Key::Backspace => edit(EditIntent::Backspace(Reach::Grapheme)),
        Key::Delete if word => edit(EditIntent::Delete(Reach::Word)),
        Key::Delete if far => edit(EditIntent::Delete(Reach::Line)),
        Key::Delete => edit(EditIntent::Delete(Reach::Grapheme)),
        Key::A if emacs => to(Motion::LineStart),
        Key::E if emacs => to(Motion::LineEnd),
        Key::B if emacs => to(Motion::Left),
        Key::F if emacs => to(Motion::Right),
        Key::P if emacs => to(Motion::Up),
        Key::N if emacs => to(Motion::Down),
        Key::H if emacs => edit(EditIntent::Backspace(Reach::Grapheme)),
        Key::D if emacs => edit(EditIntent::Delete(Reach::Grapheme)),
        Key::K if emacs => edit(EditIntent::Delete(Reach::Line)),
        Key::A if primary && !m.alt && !m.shift => edit(EditIntent::SelectAll),
        Key::Z if primary && !m.alt && m.shift => edit(EditIntent::Redo),
        Key::Z if primary && !m.alt => edit(EditIntent::Undo),
        Key::Y if !apple && m.control && !m.alt && !m.shift => edit(EditIntent::Redo),
        Key::Enter if multiline && !primary => edit(EditIntent::Insert("\n".into())),
        Key::Enter if !k.repeat => Some(FieldKey::Submit),
        _ => None,
    }
}

/// Runs a text field's response to the sample under dispatch: records the
/// edit intents its pointer, key and text samples ask for and reports a
/// submission. It answers only on the field itself, not for a sample passing
/// through it.
pub fn field_input(cx: &mut EventCx<'_>, multiline: bool) -> Option<FieldEvent> {
    if cx.phase() != DispatchPhase::Target {
        return None;
    }
    let node = cx.node()?;
    if let Some(p) = cx.pointer() {
        let held = p.buttons.contains(PointerButtons::PRIMARY);
        let (x, y, extend) = (p.x, p.y, p.modifiers.shift);
        match p.phase {
            PointerPhase::Down if held => {
                cx.request_focus(node);
                cx.capture_pointer(node);
                let intent = match cx.clicks() {
                    _ if extend => EditIntent::PlaceAt { x, y, extend },
                    1 => EditIntent::PlaceAt { x, y, extend },
                    2 => EditIntent::SelectAt {
                        x,
                        y,
                        unit: Unit::Word,
                    },
                    _ => EditIntent::SelectAt {
                        x,
                        y,
                        unit: Unit::Paragraph,
                    },
                };
                cx.record_edit(intent);
            }
            PointerPhase::Move if held => {
                cx.record_edit(EditIntent::PlaceAt { x, y, extend: true });
            }
            PointerPhase::Up => cx.release_pointer(),
            _ => {}
        }
        return None;
    }
    if let Some(k) = cx.key() {
        return match field_key(k, multiline)? {
            FieldKey::Edit(intent) => {
                cx.record_edit(intent);
                None
            }
            FieldKey::Submit => Some(FieldEvent::Submitted),
        };
    }
    let intent = match cx.ime()? {
        ImeEvent::Preedit { text, caret } => EditIntent::Compose {
            text: text.clone(),
            caret: TextOffset(*caret),
        },
        ImeEvent::Commit { text } => EditIntent::CommitCompose(text.clone()),
        ImeEvent::Paste { text } => EditIntent::Paste(text.clone()),
    };
    cx.record_edit(intent);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Modifiers;

    fn press(key: Key, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    const NONE: Modifiers = Modifiers {
        shift: false,
        control: false,
        alt: false,
        logo: false,
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };
    const ALT: Modifiers = Modifiers { alt: true, ..NONE };
    const CTRL: Modifiers = Modifiers {
        control: true,
        ..NONE
    };
    const CMD: Modifiers = Modifiers { logo: true, ..NONE };

    fn moves(motion: Motion, extend: bool) -> Option<FieldKey> {
        Some(FieldKey::Edit(EditIntent::Move { motion, extend }))
    }

    fn edit(intent: EditIntent) -> Option<FieldKey> {
        Some(FieldKey::Edit(intent))
    }

    #[test]
    fn apple_moves_by_word_with_option_and_to_the_ends_with_command() {
        let key = |k, m| field_key_in(&press(k, m), false, true);
        assert_eq!(key(Key::Left, ALT), moves(Motion::WordLeft, false));
        assert_eq!(
            key(Key::Right, Modifiers { shift: true, ..ALT }),
            moves(Motion::WordRight, true)
        );
        assert_eq!(key(Key::Left, CMD), moves(Motion::LineStart, false));
        assert_eq!(key(Key::Up, CMD), moves(Motion::DocStart, false));
        assert_eq!(key(Key::Down, ALT), moves(Motion::ParagraphEnd, false));
        assert_eq!(
            key(Key::Backspace, ALT),
            edit(EditIntent::Backspace(Reach::Word))
        );
        assert_eq!(
            key(Key::Backspace, CMD),
            edit(EditIntent::Backspace(Reach::Line))
        );
        assert_eq!(key(Key::A, CTRL), moves(Motion::LineStart, false));
        assert_eq!(key(Key::K, CTRL), edit(EditIntent::Delete(Reach::Line)));
        assert_eq!(key(Key::A, CMD), edit(EditIntent::SelectAll));
        assert_eq!(key(Key::Z, CMD), edit(EditIntent::Undo));
        assert_eq!(
            key(Key::Z, Modifiers { shift: true, ..CMD }),
            edit(EditIntent::Redo)
        );
        assert_eq!(key(Key::Y, CTRL), None, "no Control-Y on Apple systems");
    }

    #[test]
    fn elsewhere_control_moves_by_word_and_to_the_text_ends() {
        let key = |k, m| field_key_in(&press(k, m), false, false);
        assert_eq!(key(Key::Left, CTRL), moves(Motion::WordLeft, false));
        assert_eq!(key(Key::Left, ALT), moves(Motion::Left, false));
        assert_eq!(key(Key::Home, CTRL), moves(Motion::DocStart, false));
        assert_eq!(key(Key::End, SHIFT), moves(Motion::LineEnd, true));
        assert_eq!(key(Key::Up, CTRL), moves(Motion::ParagraphStart, false));
        assert_eq!(
            key(Key::Delete, CTRL),
            edit(EditIntent::Delete(Reach::Word))
        );
        assert_eq!(key(Key::A, CTRL), edit(EditIntent::SelectAll));
        assert_eq!(key(Key::Y, CTRL), edit(EditIntent::Redo));
        assert_eq!(key(Key::A, CMD), None);
    }

    #[test]
    fn enter_submits_a_single_line_field_and_breaks_a_line_in_a_multi_line_one() {
        for apple in [true, false] {
            let primary = if apple { CMD } else { CTRL };
            let key = |m, multiline| field_key_in(&press(Key::Enter, m), multiline, apple);
            assert_eq!(key(NONE, false), Some(FieldKey::Submit));
            assert_eq!(key(NONE, true), edit(EditIntent::Insert("\n".into())));
            assert_eq!(key(SHIFT, true), edit(EditIntent::Insert("\n".into())));
            assert_eq!(key(primary, true), Some(FieldKey::Submit));
        }
        let held = KeyEvent {
            repeat: true,
            ..press(Key::Enter, NONE)
        };
        assert_eq!(
            field_key_in(&held, false, true),
            None,
            "a held Enter submits once"
        );
    }

    #[test]
    fn a_release_or_an_unused_key_does_nothing() {
        let release = KeyEvent {
            pressed: false,
            ..press(Key::Left, NONE)
        };
        assert_eq!(field_key_in(&release, false, true), None);
        assert_eq!(field_key_in(&press(Key::Q, NONE), false, true), None);
        assert_eq!(field_key_in(&press(Key::Tab, NONE), false, false), None);
    }
}
