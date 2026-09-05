//! The [`TextInput`] control — a single-line editable text field with a caret,
//! selection, and IME composition.
//!
//! A `TextInput` is a focusable text leaf carrying a retained edit
//! [`Buffer`](viso_ui::Buffer): the text, the [`Selection`](viso_ui::Selection)
//! (caret + anchor), and any in-progress IME composition all live in that buffer,
//! registered in the driver-owned [`TextEdits`](viso_ui::TextEdits) registry
//! beside the store (the heap-heavy string lives off the hot node columns,
//! mirroring how a virtual list keeps its item store out of the arena). The
//! widget itself is thin: it declares the leaf through
//! [`BuildCx::text_input`](viso_ui::BuildCx::text_input) (which seeds the buffer
//! and shapes the initial text), makes it focusable, and attaches one key/IME
//! handler that translates input into [`EditIntent`](viso_ui::EditIntent)s. The
//! router applies those intents to the buffer after the handler returns, and the
//! reconcile/shape pass folds them into the text and re-shapes — the handler
//! never mutates the buffer directly (it holds no buffer reference; an
//! [`EventCx`] has none).
//!
//! Input arrives on two paths, both dispatched to the *same* handler (the key
//! router drives it for a focused node's key events and its IME events alike):
//!
//! - **navigation and deletion** come from [`KeyEvent`](viso_ui::KeyEvent)s —
//!   Left/Right move the caret ([`Motion`](viso_ui::Motion)), Home/End jump to the
//!   ends, Shift extends the selection, Backspace/Delete remove the selection or
//!   one character;
//! - **insertion and composition** come from [`ImeEvent`](viso_ui::ImeEvent)s —
//!   `Preedit` drives a [`EditIntent::Compose`](viso_ui::EditIntent::Compose)
//!   (the composing underline), `Commit` finalizes it with
//!   [`CommitCompose`](viso_ui::EditIntent::CommitCompose). Printable typing is
//!   IME-commit-driven: there is no key-to-insert path, which is what makes the
//!   control IME-aware from the first slice (AGENTS section 20).
//!
//! A primary press moves focus to the field so subsequent keystrokes route to it;
//! richer pointer editing (click-to-place-caret, drag-select) is a later slice —
//! it needs the shaped glyph geometry to hit-test a character, which the handler
//! does not have. Its accessible role is [`Role::TextField`] with the given label
//! as its accessible name (AGENTS section 15).
//!
//! Deferred to later slices: multi-line/wrap, undo/redo, word motion, clipboard,
//! placeholder/password rendering, horizontal scroll/clip, caret blink,
//! double/triple-click selection, grapheme-cluster stepping, and authoritative
//! full-state IME sync (Android/iOS).
//!
//! ```
//! use viso_widgets::text_input;
//! use viso_ui::{BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let name = text_input("Name").value("Ada").on_change(|_ev, text| {
//!     // handle the new text — e.g. write app state through the cx
//!     let _ = text;
//! });
//!
//! // A text input registers an edit buffer, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits);
//! name.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    BuildCx, Component, EditIntent, EventCx, ImeEvent, Key, LeafStyle, Length, Motion,
    PointerButtons, PointerPhase, Rgba, Role, Semantics, Size, TextRequest,
};

/// A shared, mutable change callback carrying the field's new text. It is not
/// wired to the router yet — the buffer edit is applied and re-shaped after the
/// handler returns, so the handler cannot read the resulting text in the same
/// dispatch. It is kept as a `Copy`-cheap `Rc` on the widget for a later slice
/// that surfaces the post-edit text; a `TextInput` with no handler still edits.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, &str)>>>>;

/// The text color: near-white for contrast against a dark surface.
const TEXT: Rgba = Rgba {
    r: 0.98,
    g: 0.98,
    b: 1.0,
    a: 1.0,
};

/// The default field width.
const FIELD_W: f32 = 180.0;

/// The default font size.
const FONT_SIZE: f32 = 16.0;

/// The visual and layout parameters of a [`TextInput`]: the text's font size and
/// color plus the field's own size request within its parent.
///
/// `size` defaults to a fixed width (a single-line field is a horizontal box) and
/// a `Fit` height so the field is as tall as its text; override either axis for a
/// fixed or flexing box. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextInputStyle {
    /// The field's own size request within its parent. Defaults to a fixed width
    /// and a `Fit` height (as tall as the text).
    pub size: Size,
    /// The text's font size in pixels.
    pub font_size: f32,
    /// The text's fill color.
    pub color: Rgba,
}

impl Default for TextInputStyle {
    fn default() -> Self {
        TextInputStyle {
            size: Size {
                width: Length::Fixed(FIELD_W),
                height: Length::Fit,
            },
            font_size: FONT_SIZE,
            color: TEXT,
        }
    }
}

/// A single-line editable text field with a caret, selection, and IME support.
///
/// Construct one with [`text_input`] and attach behavior with the chainable
/// setters. Focus it (a primary click, or programmatic focus) and type: keystrokes
/// move the caret and delete, while IME commits insert text.
///
/// See the [module docs](self) for a build example. The edit state lives in a
/// retained [`Buffer`](viso_ui::Buffer) in the driver registry; editing queues
/// intents that the router applies and the shape pass folds in, re-shaping only
/// this field's text — a targeted invalidation, not a rebuild (architecture
/// section 47).
pub struct TextInput {
    /// The accessible name (a caption/label for the field; not the editable text).
    label: String,
    /// The initial text content.
    value: String,
    style: TextInputStyle,
    /// The shared change callback (see [`SharedChange`]). `None` until
    /// [`TextInput::on_change`] is called.
    on_change: SharedChange,
}

/// Construct a [`TextInput`] with the given accessible label, empty initial text,
/// and no handler yet. Chain [`TextInput::value`] to seed text and
/// [`TextInput::on_change`] to observe edits.
pub fn text_input(label: impl Into<String>) -> TextInput {
    TextInput {
        label: label.into(),
        value: String::new(),
        style: TextInputStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl TextInput {
    /// Set the change callback, fired with the field's new text. Replaces any
    /// previously set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, &str) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the initial text content (defaults to empty). The caret starts at the
    /// end of the seeded text.
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    /// Replace the whole [`TextInputStyle`].
    pub fn style(mut self, style: TextInputStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the field's own size request within its parent (defaults to a fixed
    /// width and a `Fit` height).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Set the text's font size in pixels.
    pub fn font_size(mut self, font_size: f32) -> Self {
        self.style.font_size = font_size;
        self
    }

    /// Set the text's fill color.
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
        self
    }
}

impl Component for TextInput {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // Declare the editable leaf: a leaf node plus a retained edit buffer
        // seeded from this request (`text_input` registers the buffer in the
        // driver's `TextEdits` and declares the request so the first frame shapes
        // the seed text). `build` takes `&self`, so the seed text is cloned once
        // at build time (cold, not a per-frame path).
        let root = cx.text_input(
            LeafStyle {
                size: self.style.size,
                style: viso_ui::BoxStyle::NONE,
            },
            TextRequest {
                text: self.value.clone(),
                font_size: self.style.font_size,
                color: self.style.color,
            },
        );

        // Focusable so keystrokes and IME events route to it while focused.
        cx.focusable(root, true);

        // A primary press focuses the field so subsequent keystrokes route here.
        // Click-to-place-caret and drag-select need the shaped glyph geometry to
        // hit-test a character (which the handler does not have) and are a later
        // slice.
        let root_id = root.id();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Down && p.buttons.contains(PointerButtons::PRIMARY) {
                ev.request_focus(root_id);
            }
        });

        // One handler for both key and IME events (the key router drives it for a
        // focused node on both paths). Key events are navigation and deletion; IME
        // events are composition and insertion. The handler records `EditIntent`s;
        // the router applies them to this node's buffer after the handler returns,
        // and the shape pass folds them into the text — the handler never mutates
        // the buffer directly.
        cx.on_key(root, move |ev| {
            if let Some(k) = ev.key() {
                // Only act on a fresh press (ignore key-up); navigation and
                // deletion honor auto-repeat so a held key keeps moving/deleting.
                if !k.pressed {
                    return;
                }
                let extend = k.modifiers.shift;
                match k.key {
                    Key::Left => ev.record_edit(EditIntent::Move {
                        motion: Motion::Left,
                        extend,
                    }),
                    Key::Right => ev.record_edit(EditIntent::Move {
                        motion: Motion::Right,
                        extend,
                    }),
                    Key::Home => ev.record_edit(EditIntent::Move {
                        motion: Motion::Home,
                        extend,
                    }),
                    Key::End => ev.record_edit(EditIntent::Move {
                        motion: Motion::End,
                        extend,
                    }),
                    Key::Backspace => ev.record_edit(EditIntent::Backspace),
                    Key::Delete => ev.record_edit(EditIntent::Delete),
                    _ => {}
                }
            } else if let Some(ime) = ev.ime() {
                match ime {
                    // A composing update: replace the composition range, keeping the
                    // composing state (empty text cancels the composition).
                    ImeEvent::Preedit { text, caret } => ev.record_edit(EditIntent::Compose {
                        text: text.clone(),
                        caret: *caret,
                    }),
                    // The commit finalizes composition (or, with no composition in
                    // flight, inserts the text at the caret): this is the insertion
                    // path — printable typing arrives here, not as a key event.
                    ImeEvent::Commit { text } => {
                        ev.record_edit(EditIntent::CommitCompose(text.clone()))
                    }
                }
            }
        });

        // A text field's accessible role, with the given label as its accessible
        // name (AGENTS section 15). The live text/selection is proven through the
        // buffer and the input tapes; surfacing them in the derived tree is a later
        // slice (the derive pass has no edit registry).
        cx.semantics(
            root,
            Semantics::role(Role::TextField).with_label(self.label.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, KeyEvent, Modifiers, NodeId, NodeStore, PointerEvent, StateStore, TextEdits,
        VirtualLists,
    };

    /// The reactive stores a text-input build writes into, kept together so a test
    /// can build a field and then drive its handlers against the same state. A
    /// `TextInput` registers an edit buffer, so it must build through a reactive cx.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
            }
        }

        /// Build a text input through a reactive cx and return its root node.
        fn build(&mut self, t: TextInput) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
            );
            t.build(&mut cx);
            cx.root().expect("text input declares a root node")
        }

        /// Feed a pointer sample to the root's pointer handler, restoring it after,
        /// and return any pending focus request the handler made.
        fn pointer(&mut self, root: NodeId, ev: PointerEvent) -> Option<Option<NodeId>> {
            let mut handler = self.store.take_handler(root).expect("pointer handler");
            let focus = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_focus_request()
            };
            self.store.restore_handler(root, handler);
            focus
        }

        /// Feed a key sample to the root's key handler, restoring it after, and
        /// return the edit intents it recorded.
        fn key(&mut self, root: NodeId, ev: KeyEvent) -> Vec<EditIntent> {
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            let edits = {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_edits()
            };
            self.store.restore_key_handler(root, handler);
            edits
        }

        /// Feed an IME sample to the root's key handler (the same handler serves
        /// both the key and IME routes), restoring it after, and return the edit
        /// intents it recorded.
        fn ime(&mut self, root: NodeId, ev: ImeEvent) -> Vec<EditIntent> {
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            let edits = {
                let mut cx = EventCx::__new_ime(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_edits()
            };
            self.store.restore_key_handler(root, handler);
            edits
        }
    }

    /// A primary-button pointer sample at the origin in the given phase.
    fn primary(phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        }
    }

    /// A key press/release sample, with an optional held-shift modifier.
    fn key_ev(key: Key, pressed: bool, shift: bool) -> KeyEvent {
        KeyEvent {
            key,
            pressed,
            repeat: false,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
        }
    }

    /// A text input builds a focusable text-field node: it has a pointer handler
    /// (for click-to-focus), a key handler (navigation/deletion and the IME route),
    /// a focusable flag, and `TextField` semantics named by its label.
    #[test]
    fn text_input_builds_a_focusable_text_field() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("Name"));

        assert!(
            rx.store.has_handler(root),
            "a text input attaches a pointer handler for click-to-focus"
        );
        assert!(
            rx.store.has_key_handler(root),
            "a text input attaches a key/IME handler"
        );
        assert!(rx.store.focusable(root), "a text input is focusable");

        let sem = rx
            .store
            .semantics(root)
            .expect("a text input authors semantics");
        assert_eq!(sem.role, Role::TextField);
        assert_eq!(sem.label.as_deref(), Some("Name"));
    }

    /// The chainable setters override the initial value and style; the default is an
    /// empty field, fixed width, `Fit` height, at the default font size and color.
    #[test]
    fn setters_override_defaults() {
        let default = text_input("x");
        assert_eq!(default.value, "");
        assert_eq!(default.style.size.width, Length::Fixed(FIELD_W));
        assert_eq!(default.style.size.height, Length::Fit);
        assert_eq!(default.style.font_size, FONT_SIZE);
        assert_eq!(default.style.color, TEXT);

        let red = Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let widget = text_input("x")
            .value("Ada")
            .font_size(20.0)
            .color(red)
            .size(Size::fill());
        assert_eq!(widget.value, "Ada");
        assert_eq!(widget.style.font_size, 20.0);
        assert_eq!(widget.style.color, red);
        assert_eq!(widget.style.size, Size::fill());
    }

    /// A primary press requests focus for the field so subsequent keystrokes route
    /// to it; it records no edit.
    #[test]
    fn primary_press_requests_focus() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("Name"));

        let focus = rx.pointer(root, primary(PointerPhase::Down));
        assert_eq!(
            focus,
            Some(Some(root)),
            "a primary press moves focus to the field"
        );

        // A non-primary press does not focus.
        let non_primary = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary(PointerPhase::Down)
        };
        let focus = rx.pointer(root, non_primary);
        assert_eq!(focus, None, "a non-primary press does not focus");
    }

    /// Arrow and Home/End presses record caret motions; Shift extends the selection.
    #[test]
    fn navigation_keys_record_motions() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("v").value("hello"));

        assert_eq!(
            rx.key(root, key_ev(Key::Left, true, false)),
            vec![EditIntent::Move {
                motion: Motion::Left,
                extend: false
            }],
            "Left records a collapsing left motion"
        );
        assert_eq!(
            rx.key(root, key_ev(Key::Right, true, true)),
            vec![EditIntent::Move {
                motion: Motion::Right,
                extend: true
            }],
            "Shift-Right extends the selection right"
        );
        assert_eq!(
            rx.key(root, key_ev(Key::Home, true, false)),
            vec![EditIntent::Move {
                motion: Motion::Home,
                extend: false
            }]
        );
        assert_eq!(
            rx.key(root, key_ev(Key::End, true, true)),
            vec![EditIntent::Move {
                motion: Motion::End,
                extend: true
            }]
        );
    }

    /// Backspace and Delete record their deletion intents.
    #[test]
    fn deletion_keys_record_intents() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("v").value("hello"));

        assert_eq!(
            rx.key(root, key_ev(Key::Backspace, true, false)),
            vec![EditIntent::Backspace]
        );
        assert_eq!(
            rx.key(root, key_ev(Key::Delete, true, false)),
            vec![EditIntent::Delete]
        );
    }

    /// A key-up (release) records nothing; an unrelated key records nothing.
    #[test]
    fn key_up_and_unrelated_keys_record_nothing() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("v"));

        assert!(
            rx.key(root, key_ev(Key::Left, false, false)).is_empty(),
            "a key release records no edit"
        );
        assert!(
            rx.key(root, key_ev(Key::Enter, true, false)).is_empty(),
            "an unhandled key records no edit"
        );
    }

    /// A held navigation key (auto-repeat) still moves — navigation honors repeat so
    /// a held arrow keeps the caret moving, unlike a one-shot activation control.
    #[test]
    fn navigation_honors_auto_repeat() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("v").value("hello"));

        let repeated = KeyEvent {
            repeat: true,
            ..key_ev(Key::Left, true, false)
        };
        assert_eq!(
            rx.key(root, repeated),
            vec![EditIntent::Move {
                motion: Motion::Left,
                extend: false
            }],
            "a held Left keeps moving the caret"
        );
    }

    /// An IME preedit records a `Compose` (composing underline); a commit records a
    /// `CommitCompose` — the insertion path, since there is no key-to-insert path.
    #[test]
    fn ime_preedit_and_commit_record_composition() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("v"));

        let preedit = ImeEvent::Preedit {
            text: "ni".into(),
            caret: 2,
        };
        assert_eq!(
            rx.ime(root, preedit),
            vec![EditIntent::Compose {
                text: "ni".into(),
                caret: 2
            }],
            "a preedit records a composing update"
        );

        let commit = ImeEvent::Commit { text: "你".into() };
        assert_eq!(
            rx.ime(root, commit),
            vec![EditIntent::CommitCompose("你".into())],
            "a commit finalizes the composition (the insertion path)"
        );
    }
}
