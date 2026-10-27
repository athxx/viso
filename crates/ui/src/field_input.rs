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
//! A secondary click (on Apple systems also Control with a click) focuses
//! the field and asks for its context menu at the pointer, first selecting
//! the word under it on Apple systems or placing the caret elsewhere, unless
//! it falls inside the selection; elsewhere the menu key and Shift with F10
//! ask for it under the caret.
//!
//! ## Touch
//!
//! A finger or a pen acts on lift, so a contact that turns into a pan places
//! nothing: a tap places the caret, or on the caret or inside the selection
//! of a field that has the focus shows or hides the edit menu; a double tap
//! selects the word and a triple the paragraph, with the menu. A long press
//! selects the word under the finger and extends by words as it moves (on
//! Apple systems it places the caret and carries it), and its lift shows the
//! menu. The selection a touch made shows handles that drag its ends
//! ([`crate::field_marks`]); a mouse or a key ends it.
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
//!
//! ## Number fields
//!
//! A number field ([`number_input`]) is a text field that also steps its
//! number. Until it has the focus a drag along it scrubs the number — a step
//! for every [`SCRUB_PX`] the pointer travels, ten with Shift, a tenth with
//! Alt — and a click focuses it with its text selected; with the focus it
//! edits like any text field. Up and Down step it, Page Up and Page Down ten
//! steps (Shift and Alt scale them alike), Enter settles its text and
//! submits, and Escape puts back the number it last settled on.

use viso_runtime::{Key, primary_is_logo};

use crate::context::EventCx;
use crate::drag::DragPhase;
use crate::input::{DispatchPhase, ImeEvent, KeyEvent, Modifiers, PointerButtons, PointerPhase};
use crate::text_edit::{EditIntent, Motion, Reach, TouchEdit, Unit};
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
        Key::ContextMenu if !apple => edit(EditIntent::ContextMenu {
            at: None,
            word: false,
        }),
        Key::F10 if !apple && m.shift && !m.control && !m.alt && !m.logo => {
            edit(EditIntent::ContextMenu {
                at: None,
                word: false,
            })
        }
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
    if cx.direct() {
        touch_input(cx, node);
        return None;
    }
    if let Some(p) = cx.pointer() {
        let held = p.buttons.contains(PointerButtons::PRIMARY);
        let (x, y, extend) = (p.x, p.y, p.modifiers.shift);
        if asks_for_menu(p, primary_is_logo()) {
            cx.request_focus(node);
            cx.record_edit(EditIntent::ContextMenu {
                at: Some((x, y)),
                word: primary_is_logo(),
            });
            return None;
        }
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
        ImeEvent::Edit(edit) => EditIntent::Ime(edit.clone()),
    };
    cx.record_edit(intent);
    None
}

/// Whether pointer sample `p` asks for the context menu: a secondary press,
/// or on Apple systems Control alone with a primary one.
pub(crate) fn asks_for_menu(p: &crate::input::PointerEvent, apple: bool) -> bool {
    let primary = p.buttons.contains(PointerButtons::PRIMARY);
    let m = p.modifiers;
    p.phase == PointerPhase::Down
        && ((p.buttons.contains(PointerButtons::SECONDARY) && !primary)
            || (apple && primary && m.control && !m.shift && !m.alt && !m.logo))
}

/// What a finger or a pen does to text field `node`: a tap places the
/// caret on lift — so a contact that becomes a pan places nothing — or, on
/// the caret or the selection, shows or hides the edit menu; a double tap
/// selects the word and a triple the paragraph; a long press focuses the
/// field and selects the word under it (on Apple systems places the caret),
/// carrying it while the finger moves, and the lift shows the edit menu.
fn touch_input(cx: &mut EventCx<'_>, node: crate::node::NodeId) {
    if let Some(hold) = cx.long_press() {
        cx.request_focus(node);
        cx.record_edit(EditIntent::Touch(TouchEdit::Hold {
            x: hold.at.x,
            y: hold.at.y,
            word: !primary_is_logo(),
        }));
        return;
    }
    let Some(p) = cx.pointer() else {
        return;
    };
    let (x, y) = (p.x, p.y);
    let edit = match p.phase {
        PointerPhase::Move => TouchEdit::Follow { x, y },
        PointerPhase::Up if cx.still() => {
            let had_focus = cx.focused() == Some(node);
            cx.request_focus(node);
            match cx.clicks() {
                1 => TouchEdit::Tap { x, y, had_focus },
                2 => TouchEdit::Select {
                    x,
                    y,
                    unit: Unit::Word,
                },
                _ => TouchEdit::Select {
                    x,
                    y,
                    unit: Unit::Paragraph,
                },
            }
        }
        PointerPhase::Up | PointerPhase::Leave => TouchEdit::Lift,
        _ => return,
    };
    cx.record_edit(EditIntent::Touch(edit));
}

/// How far a scrub travels for one step of a number field, in the node's
/// space.
pub const SCRUB_PX: f32 = 2.0;

/// How many steps Page Up and Page Down move a number field.
const PAGE_STEPS: f64 = 10.0;

/// The scale `m` puts on a number field's steps: ten with Shift, a tenth with
/// Alt.
fn step_scale(m: Modifiers) -> f64 {
    match (m.shift, m.alt) {
        (true, false) => 10.0,
        (false, true) => 0.1,
        _ => 1.0,
    }
}

/// What a key does in a number field beyond what it does in a text field;
/// `None` for a key it leaves to the text field.
pub fn number_key(k: &KeyEvent) -> Option<FieldKey> {
    if !k.pressed {
        return None;
    }
    let scale = step_scale(k.modifiers);
    let step = |steps: f64| Some(FieldKey::Edit(EditIntent::Step(steps * scale)));
    let m = k.modifiers;
    if m.control || m.logo {
        return None;
    }
    match k.key {
        Key::Up => step(1.0),
        Key::Down => step(-1.0),
        Key::PageUp => step(PAGE_STEPS),
        Key::PageDown => step(-PAGE_STEPS),
        Key::Escape => Some(FieldKey::Edit(EditIntent::Revert)),
        _ => None,
    }
}

/// Runs a number field's response to the sample under dispatch: scrubs and
/// steps its number and settles its text, and otherwise edits it as
/// [`field_input`] does a single-line field. Enter settles the text and
/// reports a submission.
pub fn number_input(cx: &mut EventCx<'_>) -> Option<FieldEvent> {
    if cx.phase() != DispatchPhase::Target {
        return None;
    }
    let node = cx.node()?;
    if let Some(p) = cx.pointer()
        && cx.focused() != Some(node)
        && !asks_for_menu(p, primary_is_logo())
    {
        let (phase, modifiers) = (p.phase, p.modifiers);
        let held = p.buttons.contains(PointerButtons::PRIMARY);
        if let Some(drag) = cx.drag() {
            let intent = match drag.phase {
                DragPhase::Start | DragPhase::Move => {
                    let steps = f64::from(drag.total.x / SCRUB_PX).trunc();
                    EditIntent::Scrub(steps * step_scale(modifiers))
                }
                DragPhase::End => EditIntent::Settle,
            };
            cx.record_edit(intent);
            return None;
        }
        let direct = cx.direct();
        match phase {
            PointerPhase::Down if held && !direct => cx.capture_pointer(node),
            PointerPhase::Up if !direct || cx.still() => {
                cx.release_pointer();
                cx.request_focus(node);
                cx.record_edit(EditIntent::SelectAll);
            }
            _ => {}
        }
        return None;
    }
    if let Some(k) = cx.key() {
        if let Some(FieldKey::Edit(intent)) = number_key(k) {
            cx.record_edit(intent);
            return None;
        }
        if k.pressed && !k.repeat && k.key == Key::Enter {
            cx.record_edit(EditIntent::Settle);
            return Some(FieldEvent::Submitted);
        }
    }
    field_input(cx, false)
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
    fn the_menu_key_and_shift_f10_ask_for_the_context_menu_off_apple_systems() {
        let menu = edit(EditIntent::ContextMenu {
            at: None,
            word: false,
        });
        let key = |k, m, apple| field_key_in(&press(k, m), false, apple);
        assert_eq!(key(Key::ContextMenu, NONE, false), menu);
        assert_eq!(key(Key::F10, SHIFT, false), menu);
        assert_eq!(key(Key::F10, NONE, false), None);
        assert_eq!(key(Key::ContextMenu, NONE, true), None);
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
    fn a_number_field_steps_on_the_arrows_scaled_by_shift_and_alt() {
        let key = |k, m| number_key(&press(k, m));
        let step = |steps| edit(EditIntent::Step(steps));
        assert_eq!(key(Key::Up, NONE), step(1.0));
        assert_eq!(key(Key::Down, SHIFT), step(-10.0));
        assert_eq!(key(Key::PageUp, ALT), step(1.0));
        assert_eq!(key(Key::Escape, NONE), edit(EditIntent::Revert));
        assert_eq!(key(Key::Up, CMD), None, "the text field's");
        assert_eq!(key(Key::Left, NONE), None);
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
