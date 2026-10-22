//! The [`TextInput`] control — a single- or multi-line editable text field with a caret,
//! selection, undo history and IME composition.
//!
//! A `TextInput` is a focusable text leaf carrying a retained edit
//! [`Buffer`](viso_ui::Buffer): the text, the [`Selection`](viso_ui::Selection)
//! (caret + anchor), the undo history and any in-progress IME composition all
//! live in that buffer, registered in the driver-owned
//! [`TextEdits`](viso_ui::TextEdits) registry beside the store (the heap-heavy
//! string lives off the hot node columns, mirroring how a virtual list keeps its
//! item store out of the arena). The widget itself is thin: it declares the leaf
//! through [`BuildCx::text_input`](viso_ui::BuildCx::text_input) (which seeds the
//! buffer and shapes the initial text), makes it focusable, and runs the shared
//! text-field handler, [`field_input`](viso_ui::field_input), on its pointer, key
//! and text samples. That handler turns them into
//! [`EditIntent`](viso_ui::EditIntent)s by the platform's key conventions; the
//! router applies those to the buffer after the handler returns, and the
//! reconcile/shape pass folds them into the text and re-shapes — the handler
//! never mutates the buffer directly (it holds no buffer reference; an
//! [`EventCx`] has none).
//!
//! Printable typing is IME-commit-driven: there is no key-to-insert path, which
//! is what makes the control IME-aware from the first slice (AGENTS section 20).
//!
//! Its accessible role is [`Role::TextField`] with the given label as its
//! accessible name (AGENTS section 15). A [`placeholder`](TextInput::placeholder)
//! draws while the field is empty, and a [`secure`](TextInput::secure) field
//! draws a bullet per grapheme, gives nothing to copy or cut, keeps no undo
//! history, and is a [`Role::SecureField`].
//!
//! What it holds ([`input`](TextInput::input)) picks the soft keyboard and,
//! for a number or a phone number, what it takes; a
//! [`max_length`](TextInput::max_length) and a [`filter`](TextInput::filter)
//! hold back the rest of what is typed, pasted or composed into it
//! ([`viso_ui::input_filter`]). Enter in a single-line field, or the
//! accelerator with Enter in a multi-line one, fires
//! [`on_submit`](TextInput::on_submit).
//!
//! ```
//! use viso_widgets::text_input;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
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
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! name.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    BuildCx, Component, EventCx, FieldEvent, FieldLook, FontSize, InputFilter, InputKind,
    LeafStyle, Length, Rgba, Role, Semantics, Size, TextRequest, field_input,
};

/// A shared, mutable change callback carrying the field's new text, fired from
/// the field's key handler when the driver reports a settled edit
/// ([`EventCx::text_change`]). Shared through an `Rc` so the builder can set it
/// after construction and the built handler still sees it.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, &str)>>>>;

/// A shared, mutable submit callback, fired from the field's key handler when
/// Enter submits it; shared like [`SharedChange`].
type SharedSubmit = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

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
    /// What it draws while empty, and whether it masks its text.
    look: FieldLook,
    style: TextInputStyle,
    /// The shared change callback (see [`SharedChange`]). `None` until
    /// [`TextInput::on_change`] is called.
    on_change: SharedChange,
    /// The shared submit callback. `None` until [`TextInput::on_submit`] is
    /// called.
    on_submit: SharedSubmit,
}

/// Construct a [`TextInput`] with the given accessible label, empty initial text,
/// and no handler yet. Chain [`TextInput::value`] to seed text and
/// [`TextInput::on_change`] to observe edits.
pub fn text_input(label: impl Into<String>) -> TextInput {
    TextInput {
        label: label.into(),
        value: String::new(),
        look: FieldLook::default(),
        style: TextInputStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
        on_submit: Rc::new(RefCell::new(None)),
    }
}

impl TextInput {
    /// Set the change callback, fired with the field's new text. Replaces any
    /// previously set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, &str) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the submit callback, fired when Enter submits the field (the
    /// accelerator with Enter in a multi-line one). Replaces any previously
    /// set handler.
    pub fn on_submit(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_submit.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the initial text content (defaults to empty). The caret starts at the
    /// end of the seeded text.
    pub fn value(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }

    /// Set what the field draws while it holds no text (defaults to nothing).
    pub fn placeholder(mut self, placeholder: impl Into<String>) -> Self {
        self.look.placeholder = placeholder.into();
        self
    }

    /// Holds lines: the text wraps to the field's width, Enter breaks a line
    /// and the accelerator with Enter submits.
    pub fn multiline(mut self, multiline: bool) -> Self {
        self.look.multiline = multiline;
        self
    }

    /// Mask the text: a bullet per grapheme, nothing to copy or cut, and an
    /// accessible [`Role::SecureField`] that never reads its text out.
    pub fn secure(mut self, secure: bool) -> Self {
        self.look.secure = secure;
        self
    }

    /// Say what the field holds (defaults to [`InputKind::Text`]): the soft
    /// keyboard it shows, and for a number or a phone number, what it takes.
    pub fn input(mut self, kind: InputKind) -> Self {
        self.look.kind = kind;
        self
    }

    /// Hold typing, pasting and composing to at most `max` graphemes (no
    /// limit by default). A value it is given may hold more.
    pub fn max_length(mut self, max: u32) -> Self {
        self.look.max_length = Some(max);
        self
    }

    /// Map each piece of text typed, pasted or composed into the field to
    /// what it keeps, after the rule of what it holds; a piece mapped to
    /// nothing is refused.
    pub fn filter(mut self, filter: impl Fn(&str) -> String + 'static) -> Self {
        self.look.filter = Some(InputFilter::new(filter));
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
                spans: Vec::new(),
                text: self.value.clone(),
                font_size: FontSize::Fixed(self.style.font_size),
                color: self.style.color,
                // The buffer declares whether its text wraps.
                soft_wrap: false,
                locale: None,
                style: viso_ui::TextStyle::default(),
            },
            self.look.clone(),
        );

        // Focusable so keystrokes and IME events route to it while focused.
        cx.focusable(root, true);

        // Pointer, key and text samples become edit intents the router applies
        // to this node's buffer after the handler returns; the shape pass folds
        // them into the text. The handler never touches the buffer.
        let multiline = self.look.multiline;
        cx.on_pointer(root, move |ev| {
            field_input(ev, multiline);
        });
        let on_change = Rc::clone(&self.on_change);
        let on_submit = Rc::clone(&self.on_submit);
        cx.on_key(root, move |ev| {
            if let Some(text) = ev.text_change() {
                if let Some(handler) = on_change.borrow_mut().as_mut() {
                    handler(ev, text);
                }
                return;
            }
            if field_input(ev, multiline) == Some(FieldEvent::Submitted)
                && let Some(handler) = on_submit.borrow_mut().as_mut()
            {
                handler(ev);
            }
        });

        // A text field's accessible role, with the given label as its accessible
        // name (AGENTS section 15). The live text/selection is proven through the
        // buffer and the input tapes; surfacing them in the derived tree is a later
        // slice (the derive pass has no edit registry).
        let role = if self.look.secure {
            Role::SecureField
        } else {
            Role::TextField
        };
        cx.semantics(root, Semantics::role(role).with_label(self.label.clone()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, DispatchPhase, EditIntent, ImeEvent, Key, KeyEvent, Modifiers, Motion,
        NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, Reach, SemanticProjector,
        StateStore, TextEdits, TextOffset, VirtualLists,
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
        projectors: SemanticProjector,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
                projectors: SemanticProjector::new(),
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
                &mut self.projectors,
            );
            t.build(&mut cx);
            cx.root().expect("text input declares a root node")
        }

        /// Feed a pointer sample to the root's pointer handler, restoring it after,
        /// and return any pending focus request the handler made.
        fn pointer(&mut self, root: NodeId, ev: PointerEvent) -> Option<Option<NodeId>> {
            self.pointer_full(root, ev).focus
        }

        /// Feed a pointer sample to the root's pointer handler, restoring it after,
        /// and return every deferred request the handler made.
        fn pointer_full(&mut self, root: NodeId, ev: PointerEvent) -> PointerOutcome {
            let mut handler = self.store.take_handler(root).expect("pointer handler");
            let outcome = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                cx.__set_place(&self.store, root, DispatchPhase::Target, None);
                handler(&mut cx);
                PointerOutcome {
                    focus: cx.__take_focus_request(),
                    capture: cx.__take_capture_request(),
                    edits: cx.__take_edits(),
                }
            };
            self.store.restore_handler(root, handler);
            outcome
        }

        /// Feed a key sample to the root's key handler, restoring it after, and
        /// return the edit intents it recorded.
        fn key(&mut self, root: NodeId, ev: KeyEvent) -> Vec<EditIntent> {
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            let edits = {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                cx.__set_place(&self.store, root, DispatchPhase::Target, None);
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
                cx.__set_place(&self.store, root, DispatchPhase::Target, None);
                handler(&mut cx);
                cx.__take_edits()
            };
            self.store.restore_key_handler(root, handler);
            edits
        }
    }

    /// The deferred requests one pointer dispatch made.
    struct PointerOutcome {
        focus: Option<Option<NodeId>>,
        capture: Option<Option<NodeId>>,
        edits: Vec<EditIntent>,
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

    #[test]
    fn enter_fires_on_submit() {
        let submitted = Rc::new(std::cell::Cell::new(0));
        let seen = Rc::clone(&submitted);
        let mut rx = Reactive::new();
        let root = rx.build(
            text_input("Amount")
                .input(InputKind::Decimal)
                .max_length(8)
                .on_submit(move |_| seen.set(seen.get() + 1)),
        );
        assert!(rx.key(root, key_ev(Key::Enter, true, false)).is_empty());
        assert_eq!(submitted.get(), 1);
        rx.key(root, key_ev(Key::Enter, false, false));
        assert_eq!(submitted.get(), 1, "a release submits nothing");
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

    /// A press captures the pointer and places the caret at the press point;
    /// a held move extends the selection; the release frees the capture. A move
    /// without the button held records nothing.
    #[test]
    fn press_and_drag_record_placements() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("Name").value("hello"));

        let down = rx.pointer_full(
            root,
            PointerEvent {
                x: 12.0,
                y: 3.0,
                ..primary(PointerPhase::Down)
            },
        );
        assert_eq!(down.capture, Some(Some(root)));
        assert_eq!(
            down.edits,
            vec![EditIntent::PlaceAt {
                x: 12.0,
                y: 3.0,
                extend: false
            }]
        );

        let drag = rx.pointer_full(
            root,
            PointerEvent {
                x: 30.0,
                y: 4.0,
                ..primary(PointerPhase::Move)
            },
        );
        assert_eq!(
            drag.edits,
            vec![EditIntent::PlaceAt {
                x: 30.0,
                y: 4.0,
                extend: true
            }]
        );

        let up = rx.pointer_full(
            root,
            PointerEvent {
                buttons: PointerButtons::NONE,
                ..primary(PointerPhase::Up)
            },
        );
        assert_eq!(up.capture, Some(None));
        assert!(up.edits.is_empty());

        let hover = rx.pointer_full(
            root,
            PointerEvent {
                buttons: PointerButtons::NONE,
                ..primary(PointerPhase::Move)
            },
        );
        assert!(hover.edits.is_empty());
        assert_eq!(hover.capture, None);
    }

    /// Shift-press extends from the existing anchor instead of collapsing.
    #[test]
    fn shift_press_extends() {
        let mut rx = Reactive::new();
        let root = rx.build(text_input("Name"));
        let down = rx.pointer_full(
            root,
            PointerEvent {
                modifiers: Modifiers {
                    shift: true,
                    ..Modifiers::default()
                },
                ..primary(PointerPhase::Down)
            },
        );
        assert_eq!(
            down.edits,
            vec![EditIntent::PlaceAt {
                x: 0.0,
                y: 0.0,
                extend: true
            }]
        );
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
                motion: Motion::LineStart,
                extend: false
            }]
        );
        assert_eq!(
            rx.key(root, key_ev(Key::End, true, true)),
            vec![EditIntent::Move {
                motion: Motion::LineEnd,
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
            vec![EditIntent::Backspace(Reach::Grapheme)]
        );
        assert_eq!(
            rx.key(root, key_ev(Key::Delete, true, false)),
            vec![EditIntent::Delete(Reach::Grapheme)]
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
                caret: TextOffset(2)
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
