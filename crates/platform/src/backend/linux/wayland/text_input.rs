//! Input-method composition over `text-input-v3`: enabled while the focused
//! window has a caret, which is reported as the cursor rectangle so the
//! candidate window follows it; preedit and commit strings apply atomically
//! at each `done`.

use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use super::State;
use crate::backend::linux::translate;
use crate::control::WindowId;
use crate::event::{RawEvent, RawImePreedit, RawText};

pub(super) struct TextInput {
    input: ZwpTextInputV3,
    /// The window the compositor routes this input to.
    focus: Option<WindowId>,
    enabled: bool,
    pending: Pending,
    /// A non-empty preedit is on screen, so an empty one must clear it.
    preedit_shown: bool,
}

/// State accumulated between `done` events.
#[derive(Default)]
struct Pending {
    preedit: Option<(String, i32)>,
    commit: Option<String>,
}

impl TextInput {
    pub(super) fn new(input: ZwpTextInputV3) -> Self {
        Self {
            input,
            focus: None,
            enabled: false,
            pending: Pending::default(),
            preedit_shown: false,
        }
    }

    pub(super) fn destroy(self) {
        self.input.destroy();
    }

    pub(super) fn forget_window(&mut self, window: WindowId) {
        if self.focus == Some(window) {
            self.focus = None;
            self.enabled = false;
            self.preedit_shown = false;
            self.pending = Pending::default();
        }
    }

    /// Follow `window`'s caret: enable composition while it has one,
    /// disable it otherwise.
    pub(super) fn update(&mut self, state: &State, window: WindowId) {
        if self.focus != Some(window) {
            return;
        }
        let Some(i) = state.index(window) else { return };
        match state.windows[i].ime_area {
            Some(caret) => {
                // `enable` resets the input method's state, so it is only
                // sent on the transition; caret moves just update the rect.
                if !self.enabled {
                    self.input.enable();
                    self.input
                        .set_content_type(ContentHint::None, ContentPurpose::Normal);
                    self.enabled = true;
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
            }
            None => {}
        }
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
                    && std::mem::take(&mut self.preedit_shown)
                {
                    push_preedit(state, window, String::new(), 0);
                }
                self.enabled = false;
                self.pending = Pending::default();
            }
            zwp_text_input_v3::Event::PreeditString {
                text, cursor_begin, ..
            } => {
                self.pending.preedit = Some((text.unwrap_or_default(), cursor_begin));
            }
            zwp_text_input_v3::Event::CommitString { text } => {
                self.pending.commit = text;
            }
            zwp_text_input_v3::Event::Done { .. } => {
                let pending = std::mem::take(&mut self.pending);
                let Some(window) = self.focus else { return };
                for event in apply(pending, &mut self.preedit_shown) {
                    state.pump.push(match event {
                        Applied::Text(text) => RawEvent::Text(RawText { window, text }),
                        Applied::Preedit(text, caret) => RawEvent::ImePreedit(RawImePreedit {
                            window,
                            text,
                            caret,
                        }),
                    });
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
    }));
}

#[derive(Debug, PartialEq, Eq)]
enum Applied {
    Text(String),
    /// A preedit and its caret in bytes.
    Preedit(String, usize),
}

/// The events one `done` produces, in the protocol's order: the old
/// preedit goes, the commit is inserted, the new preedit shows.
fn apply(pending: Pending, shown: &mut bool) -> Vec<Applied> {
    let mut out = Vec::new();
    let preedit = pending.preedit.filter(|(text, _)| !text.is_empty());
    if let Some(commit) = pending.commit.filter(|t| !t.is_empty()) {
        if std::mem::take(shown) {
            out.push(Applied::Preedit(String::new(), 0));
        }
        out.push(Applied::Text(commit));
    }
    match preedit {
        Some((text, cursor)) => {
            // A negative cursor hides the caret: park it at the end.
            let caret = usize::try_from(cursor)
                .map_or(text.len(), |byte| translate::floor_boundary(&text, byte));
            out.push(Applied::Preedit(text, caret));
            *shown = true;
        }
        None if std::mem::take(shown) => out.push(Applied::Preedit(String::new(), 0)),
        None => {}
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

    fn pending(preedit: Option<(&str, i32)>, commit: Option<&str>) -> Pending {
        Pending {
            preedit: preedit.map(|(t, c)| (t.to_owned(), c)),
            commit: commit.map(str::to_owned),
        }
    }

    #[test]
    fn composition_shows_then_commits() {
        let mut shown = false;
        assert_eq!(
            apply(pending(Some(("ni", 2)), None), &mut shown),
            [Applied::Preedit("ni".into(), 2)]
        );
        assert!(shown);
        assert_eq!(
            apply(pending(None, Some("你")), &mut shown),
            [
                Applied::Preedit(String::new(), 0),
                Applied::Text("你".into())
            ]
        );
        assert!(!shown);
    }

    #[test]
    fn carets_land_on_char_boundaries_or_the_end() {
        let mut shown = false;
        // Byte 1 is inside the three-byte "你".
        assert_eq!(
            apply(pending(Some(("你", 1)), None), &mut shown),
            [Applied::Preedit("你".into(), 0)]
        );
        assert_eq!(
            apply(pending(Some(("你好", -1)), None), &mut shown),
            [Applied::Preedit("你好".into(), 6)]
        );
    }

    #[test]
    fn an_empty_preedit_clears_only_what_was_shown() {
        let mut shown = false;
        assert!(apply(pending(Some(("", 0)), None), &mut shown).is_empty());
        shown = true;
        assert_eq!(
            apply(pending(Some(("", 0)), None), &mut shown),
            [Applied::Preedit(String::new(), 0)]
        );
    }
}
