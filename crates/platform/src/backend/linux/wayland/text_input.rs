//! Input-method composition over `text-input-v3`: enabled while the focused
//! window has a caret, which is reported as the cursor rectangle so the
//! candidate window follows it, with the text around it the input method
//! reads; deletions around the caret, commit and preedit strings apply
//! atomically at each `done`.

use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use super::State;
use crate::backend::ime_text::ImeText;
use crate::backend::linux::translate;
use crate::control::{InputKind, SurroundingText, WindowId};
use crate::event::{RawEvent, RawImePreedit};

/// The most surrounding text the protocol carries, in bytes.
const MOST_SURROUNDING: usize = 4000;

/// What of `text` the protocol carries around the selection from `anchor`
/// to `cursor` (bytes): a stretch shorter than [`MOST_SURROUNDING`], the
/// cursor and anchor into it. A selection too long to fit is carried as a
/// bare cursor.
fn wire(text: &str, cursor: usize, anchor: usize) -> (&str, u32, u32) {
    if text.len() < MOST_SURROUNDING {
        return (text, cursor as u32, anchor as u32);
    }
    let reach = MOST_SURROUNDING / 2 - 4;
    let (lo, hi) = (cursor.min(anchor), cursor.max(anchor));
    let anchor = if hi - lo < reach { anchor } else { cursor };
    let (lo, hi) = (cursor.min(anchor), cursor.max(anchor));
    let room = (MOST_SURROUNDING - 8 - (hi - lo)) / 2;
    let start = text.floor_char_boundary(lo.saturating_sub(room));
    let end = text.ceil_char_boundary((hi + room).min(text.len()));
    (
        &text[start..end],
        (cursor - start) as u32,
        (anchor - start) as u32,
    )
}

/// The content type of a field that takes a secret when `secure`, holds
/// `kind` and holds lines when `multiline`.
fn content_type(secure: bool, kind: InputKind, multiline: bool) -> (ContentHint, ContentPurpose) {
    if secure {
        let purpose = if kind == InputKind::Digits {
            ContentPurpose::Pin
        } else {
            ContentPurpose::Password
        };
        return (
            ContentHint::HiddenText | ContentHint::SensitiveData,
            purpose,
        );
    }
    let purpose = match kind {
        InputKind::Text | InputKind::Search => ContentPurpose::Normal,
        InputKind::Digits => ContentPurpose::Digits,
        InputKind::Integer | InputKind::Decimal => ContentPurpose::Number,
        InputKind::Phone => ContentPurpose::Phone,
        InputKind::Email => ContentPurpose::Email,
        InputKind::Url => ContentPurpose::Url,
    };
    let mut hint = if kind.literal() {
        ContentHint::Latin
    } else {
        ContentHint::None
    };
    if multiline {
        hint |= ContentHint::Multiline;
    }
    (hint, purpose)
}

pub(super) struct TextInput {
    input: ZwpTextInputV3,
    /// The window the compositor routes this input to.
    focus: Option<WindowId>,
    enabled: bool,
    /// The content type last sent: whether the field takes a secret, what
    /// it holds and whether it holds lines.
    content: Option<(bool, InputKind, bool)>,
    pending: Pending,
    /// The text the input method edits: the focused field's around its
    /// caret, and the composition under way.
    ime: ImeText,
    /// The surrounding text last sent.
    sent: Option<(String, u32, u32)>,
}

/// State accumulated between `done` events.
#[derive(Default)]
struct Pending {
    preedit: Option<(String, i32, i32)>,
    commit: Option<String>,
    /// Bytes to delete before and after the cursor.
    delete: Option<(u32, u32)>,
}

impl TextInput {
    pub(super) fn new(input: ZwpTextInputV3) -> Self {
        Self {
            input,
            focus: None,
            enabled: false,
            content: None,
            pending: Pending::default(),
            ime: ImeText::default(),
            sent: None,
        }
    }

    pub(super) fn destroy(self) {
        self.input.destroy();
    }

    pub(super) fn forget_window(&mut self, window: WindowId) {
        if self.focus == Some(window) {
            self.focus = None;
            self.enabled = false;
            self.ime = ImeText::default();
            self.sent = None;
            self.pending = Pending::default();
        }
    }

    /// Follow `window`'s text entry: enable composition while it has one,
    /// a secure one as a password the input method neither shows,
    /// suggests nor learns from; disable it otherwise.
    pub(super) fn update(&mut self, state: &State, window: WindowId) {
        if self.focus != Some(window) {
            return;
        }
        let Some(i) = state.index(window) else { return };
        match state.windows[i].text_entry {
            Some(entry) => {
                let caret = entry.caret;
                // `enable` resets the input method's state, so it is only
                // sent on the transition; caret moves just update the rect.
                if !self.enabled {
                    self.input.enable();
                    self.enabled = true;
                    self.content = None;
                    self.sent = None;
                }
                self.show(state.windows[i].surrounding.as_ref());
                let content = (entry.secure, entry.kind, entry.multiline);
                if self.content != Some(content) {
                    self.content = Some(content);
                    let (hint, purpose) = content_type(entry.secure, entry.kind, entry.multiline);
                    self.input.set_content_type(hint, purpose);
                }
                self.input.set_cursor_rectangle(
                    caret.x.round() as i32,
                    caret.y.round() as i32,
                    (caret.width.round() as i32).max(1),
                    (caret.height.round() as i32).max(1),
                );
                self.input.commit();
            }
            None if self.enabled => {
                self.input.disable();
                self.input.commit();
                self.enabled = false;
                self.ime = ImeText::default();
                self.sent = None;
            }
            None => {}
        }
    }

    /// Take the text the app shows and tell the input method of it: a
    /// change it made itself as its own, any other as the app's.
    fn show(&mut self, text: Option<&SurroundingText>) {
        let (text_changed, selection_changed) = self.ime.change(text);
        self.ime.show(text);
        if text.is_none() {
            return;
        }
        let (anchor, cursor) = self.ime.ends();
        let wire = wire(self.ime.text(), cursor, anchor);
        if self
            .sent
            .as_ref()
            .is_some_and(|(t, c, a)| (t.as_str(), *c, *a) == wire)
        {
            return;
        }
        self.input
            .set_surrounding_text(wire.0.to_owned(), wire.1 as i32, wire.2 as i32);
        self.input
            .set_text_change_cause(if text_changed || selection_changed {
                ChangeCause::Other
            } else {
                ChangeCause::InputMethod
            });
        self.sent = Some((wire.0.to_owned(), wire.1, wire.2));
    }

    fn on_event(&mut self, state: &mut State, event: zwp_text_input_v3::Event) {
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                let Some(&window) = surface.data::<WindowId>() else {
                    return;
                };
                self.focus = Some(window);
                self.enabled = false;
                self.update(state, window);
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                // The compositor ignores this input until the next enter.
                if let Some(window) = self.focus.take()
                    && self.ime.cancel(window).is_some()
                {
                    push_preedit(state, window, String::new(), 0);
                }
                self.ime = ImeText::default();
                self.sent = None;
                self.enabled = false;
                self.pending = Pending::default();
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                let text = text.unwrap_or_default();
                self.pending.preedit = Some((text, cursor_begin, cursor_end));
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                self.pending.commit = text;
            }
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                self.pending.delete = Some((before_length, after_length));
            }
            zwp_text_input_v3::Event::Done { .. } => {
                let pending = std::mem::take(&mut self.pending);
                let Some(window) = self.focus else { return };
                for event in apply(pending, &mut self.ime, window) {
                    state.pump.push(event);
                }
            }
            _ => {}
        }
    }
}

fn push_preedit(state: &mut State, window: WindowId, text: String, caret: usize) {
    state.pump.push(RawEvent::ImePreedit(RawImePreedit {
        window,
        text,
        caret,
        clauses: crate::PreeditClauses::default(),
    }));
}

/// The events one `done` produces, in the protocol's order: the old
/// preedit goes where text around the cursor is deleted, the commit
/// replaces the preedit or the selection, the new preedit shows.
fn apply(pending: Pending, ime: &mut ImeText, window: WindowId) -> Vec<RawEvent> {
    let mut out = Vec::new();
    if let Some((before, after)) = pending.delete.filter(|&(b, a)| b > 0 || a > 0) {
        out.extend(ime.cancel(window));
        out.extend(ime.delete_around(window, before as usize, after as usize));
    }
    if let Some(commit) = pending.commit.filter(|t| !t.is_empty()) {
        out.extend(ime.insert(window, &commit, None));
    }
    match pending.preedit.filter(|(text, ..)| !text.is_empty()) {
        Some((text, begin, end)) => {
            // A negative cursor hides the caret: park it at the end.
            let at = |cursor: i32| {
                usize::try_from(cursor)
                    .map_or(text.len(), |byte| translate::floor_boundary(&text, byte))
            };
            let caret = at(begin);
            // A ranged cursor highlights the clause being converted.
            let end = at(end);
            let clauses = crate::PreeditClauses {
                clauses: Vec::new(),
                selected: (begin >= 0 && caret < end).then_some((caret, end)),
            };
            out.extend(ime.set_marked(window, &text, (caret, caret), clauses, None));
        }
        None => out.extend(ime.cancel(window)),
    }
    out
}

impl Dispatch<ZwpTextInputV3, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.with_seat(|seat, state| {
            if let Some(input) = seat.text_input() {
                input.on_event(state, event);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{ImeEdit, RawImeEdit, RawText};

    const W: WindowId = WindowId(1);

    fn pending(preedit: Option<(&str, i32)>, commit: Option<&str>) -> Pending {
        Pending {
            preedit: preedit.map(|(t, c)| (t.to_owned(), c, c)),
            commit: commit.map(str::to_owned),
            delete: None,
        }
    }

    fn preedit(text: &str, caret: usize) -> RawEvent {
        RawEvent::ImePreedit(RawImePreedit {
            window: W,
            text: text.into(),
            caret,
            clauses: crate::PreeditClauses::default(),
        })
    }

    #[test]
    fn composition_shows_then_commits() {
        let mut ime = ImeText::default();
        assert_eq!(
            apply(pending(Some(("ni", 2)), None), &mut ime, W),
            [preedit("ni", 2)]
        );
        assert_eq!(
            apply(pending(None, Some("你")), &mut ime, W),
            [RawEvent::Text(RawText {
                window: W,
                text: "你".into()
            })]
        );
        assert!(!ime.is_marked());
    }

    #[test]
    fn carets_land_on_char_boundaries_or_the_end() {
        let mut ime = ImeText::default();
        // Byte 1 is inside the three-byte "你".
        assert_eq!(
            apply(pending(Some(("你", 1)), None), &mut ime, W),
            [preedit("你", 0)]
        );
        assert_eq!(
            apply(pending(Some(("你好", -1)), None), &mut ime, W),
            [preedit("你好", 6)]
        );
    }

    #[test]
    fn a_ranged_cursor_marks_the_clause_being_converted() {
        let mut ime = ImeText::default();
        let ranged = Pending {
            preedit: Some(("日本ご".into(), 0, 6)),
            ..Pending::default()
        };
        let [RawEvent::ImePreedit(p)] = &apply(ranged, &mut ime, W)[..] else {
            panic!("one preedit");
        };
        assert_eq!((p.caret, p.clauses.selected), (0, Some((0, 6))));
        let hidden = Pending {
            preedit: Some(("日本ご".into(), -1, -1)),
            ..Pending::default()
        };
        let [RawEvent::ImePreedit(p)] = &apply(hidden, &mut ime, W)[..] else {
            panic!("one preedit");
        };
        assert_eq!((p.caret, p.clauses.selected), (9, None));
    }

    #[test]
    fn an_empty_preedit_clears_only_what_was_shown() {
        let mut ime = ImeText::default();
        assert!(apply(pending(Some(("", 0)), None), &mut ime, W).is_empty());
        apply(pending(Some(("a", 1)), None), &mut ime, W);
        assert_eq!(
            apply(pending(Some(("", 0)), None), &mut ime, W),
            [preedit("", 0)]
        );
    }

    #[test]
    fn text_around_the_cursor_is_deleted_before_the_commit() {
        let mut ime = ImeText::default();
        ime.show(Some(&SurroundingText {
            text: "say teh".into(),
            offset: 10,
            anchor: 7,
            focus: 7,
            composition: None,
        }));
        let done = Pending {
            delete: Some((3, 0)),
            ..pending(None, Some("the"))
        };
        assert_eq!(
            apply(done, &mut ime, W),
            [
                RawEvent::ImeEdit(RawImeEdit {
                    window: W,
                    edit: ImeEdit::Replace {
                        range: 14..17,
                        text: String::new(),
                    },
                }),
                RawEvent::Text(RawText {
                    window: W,
                    text: "the".into(),
                }),
            ]
        );
        assert_eq!(ime.text(), "say the");
    }

    #[test]
    fn long_text_is_carried_around_the_cursor() {
        let text = "a".repeat(10_000);
        let (wire, cursor, anchor) = wire(&text, 6000, 5990);
        assert!(wire.len() < MOST_SURROUNDING);
        assert_eq!(cursor - anchor, 10);
        let (wire, cursor, anchor) = super::wire(&text, 9000, 10);
        assert!(wire.len() < MOST_SURROUNDING);
        assert_eq!(cursor, anchor);
        assert_eq!(super::wire("short", 2, 1), ("short", 2, 1));
    }
}
