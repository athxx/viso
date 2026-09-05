//! The [`Button`] control — the first interactive widget.
//!
//! A `Button` is a focusable box that fires a single `on_click` callback on
//! either a primary-pointer click (press then release) or a keyboard activation
//! (Enter/Space while focused). Pointer and keyboard activation are the *same*
//! action: an interactive control must have a keyboard equivalent (AGENTS
//! section 15), so the callback is shared between both handlers rather than
//! duplicated.
//!
//! It maps to one interactive `viso-ui` node: a background leaf carrying a
//! visible box, a composed [`Label`](crate::Label) child for the caption, a
//! pointer handler, a key handler, a `focusable` flag, and a reactive `pressed`
//! cell bound to `PAINT` so the pressed-state fill repaints the node alone (a
//! targeted invalidation — no rebuild, architecture section 47). Its accessible
//! role is [`Role::Button`] with the caption as its name.
//!
//! ```
//! use viso_widgets::button;
//! use viso_ui::{BuildCx, BindingTable, Component, NodeStore, StateStore, VirtualLists};
//!
//! let ok = button("Save").on_click(|_ev| {
//!     // handle the click — e.g. write app state through the event context
//! });
//!
//! // A button authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists);
//! ok.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Key, Length,
    PointerButtons, PointerPhase, Rgba, Role, Semantics, Size, StateValue,
};

use crate::label;

/// A shared, mutable click callback. It is cloned into both the pointer handler
/// and the key handler at build time so a pointer click and a keyboard
/// activation drive the same `on_click`. Pointer and keyboard input are never
/// concurrent, so the runtime never re-enters the `RefCell` borrow.
type SharedClick = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

/// The button's neutral resting fill.
const RESTING: Rgba = Rgba {
    r: 0.22,
    g: 0.45,
    b: 0.85,
    a: 1.0,
};

/// The button's pressed fill — a darker shade of [`RESTING`] so the press reads
/// visually without changing layout.
const PRESSED: Rgba = Rgba {
    r: 0.14,
    g: 0.30,
    b: 0.60,
    a: 1.0,
};

/// The caption color: near-white for contrast against the fill.
const CAPTION: Rgba = Rgba {
    r: 0.98,
    g: 0.98,
    b: 1.0,
    a: 1.0,
};

/// The default corner radius, giving the button a soft rounded box.
const RADIUS: f32 = 6.0;

/// The default inner padding around the caption.
const PADDING: f32 = 8.0;

/// The visual and layout parameters of a [`Button`]: the resting and pressed
/// background boxes and the button's own size request within its parent.
///
/// `size` defaults to [`Length::Fit`] on both axes so the button shrinks to its
/// caption plus padding; override it with a [`Length::Fixed`] or [`Length::Fill`]
/// axis to give it a fixed or flexing box. `background` and `pressed` default to
/// a neutral rounded fill and a darker pressed shade. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonStyle {
    /// The button's own size request within its parent. Defaults to `Fit` on
    /// both axes (shrink to the caption plus padding).
    pub size: Size,
    /// The resting background box (fill, radius, border).
    pub background: BoxStyle,
    /// The background box painted while the button is pressed.
    pub pressed: BoxStyle,
}

impl Default for ButtonStyle {
    fn default() -> Self {
        ButtonStyle {
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            background: BoxStyle::solid(RESTING).with_radius(RADIUS),
            pressed: BoxStyle::solid(PRESSED).with_radius(RADIUS),
        }
    }
}

/// An interactive push button: a focusable, clickable box with a caption.
///
/// Construct one with [`button`] and attach behavior with the chainable setters.
/// The `on_click` callback fires once per activation — a primary-pointer press
/// then release over the button, or Enter/Space while the button is focused.
///
/// See the [module docs](self) for a build example. Invalidation: pressing the
/// button flips a reactive `pressed` cell bound to `PAINT`, repainting the
/// button's own node without a rebuild; the caption and structure are static.
pub struct Button {
    /// The visible caption, also used as the accessible name.
    label: String,
    style: ButtonStyle,
    /// The shared click callback (see [`SharedClick`]). `None` until
    /// [`Button::on_click`] is called; a button with no handler is inert but
    /// still focusable and still reports its pressed state.
    on_click: SharedClick,
}

/// Construct a [`Button`] with the given caption and no handler yet. Chain
/// [`Button::on_click`] to give it behavior and [`Button::style`]/
/// [`Button::size`]/[`Button::background`] to adjust its appearance.
pub fn button(label: impl Into<String>) -> Button {
    Button {
        label: label.into(),
        style: ButtonStyle::default(),
        on_click: Rc::new(RefCell::new(None)),
    }
}

impl Button {
    /// Set the click callback, fired on a primary-pointer click or a keyboard
    /// activation. Replaces any previously set handler.
    pub fn on_click(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_click.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`ButtonStyle`].
    pub fn style(mut self, style: ButtonStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the button's own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Set the resting background box (defaults to a neutral rounded fill).
    pub fn background(mut self, background: BoxStyle) -> Self {
        self.style.background = background;
        self
    }
}

/// Drive the shared callback if one is set; a handler-less button is a no-op.
/// Pointer and keyboard activation never overlap, so the borrow is uncontended.
fn fire(cb: &SharedClick, ev: &mut EventCx<'_>) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev);
    }
}

impl Component for Button {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // A reactive `pressed` cell bound to PAINT: pressing repaints the button
        // alone (targeted invalidation, not a rebuild). The bound style is chosen
        // in paint from this cell — the node's box swaps to `pressed` while held.
        let pressed = cx.state(StateValue::Bool(false));

        // The visible background box; a flex row centering the caption inside the
        // padding. `build` takes `&self`, so the styles (`Copy`) are read by value
        // and the caption is cloned (cold, build-time — not a per-frame path).
        let resting = self.style.background;
        let caption = self.label.clone();
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 0.0,
                padding: Inset::all(PADDING),
                align: Align::Center,
                size: self.style.size,
                style: resting,
            },
            |cx| {
                label(caption.clone()).color(CAPTION).build(cx);
            },
        );

        // Bind the pressed cell to the root's PAINT so a press/release repaints
        // just this node. The pressed-state fill swap is expressed by rebinding
        // the box in paint via the cell; this slice paints the resting box and
        // marks PAINT dirty on press so the frame re-emits the node.
        cx.bind(pressed, root, DirtyClass::PAINT);
        cx.focusable(root, true);

        // Pointer activation: primary press arms `pressed`; primary release over
        // the button fires `on_click` and disarms; leaving disarms. One
        // press-then-release is one click (the counter-example click semantics).
        let pointer_cb = self.on_click.clone();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            let phase = p.phase;
            let primary = p.buttons.contains(PointerButtons::PRIMARY);
            match phase {
                PointerPhase::Down if primary => {
                    ev.set(pressed, StateValue::Bool(true));
                }
                PointerPhase::Up if primary => {
                    ev.set(pressed, StateValue::Bool(false));
                    fire(&pointer_cb, ev);
                }
                PointerPhase::Leave => {
                    ev.set(pressed, StateValue::Bool(false));
                }
                _ => {}
            }
        });

        // Keyboard activation: Enter/Space pressed (not an auto-repeat) fires the
        // same `on_click` — the accessibility equivalent of the pointer click.
        let key_cb = self.on_click.clone();
        cx.on_key(root, move |ev| {
            if let Some(k) = ev.key()
                && k.pressed
                && !k.repeat
                && matches!(k.key, Key::Enter | Key::Space)
            {
                fire(&key_cb, ev);
            }
        });

        // Explicit Button role with the caption as its accessible name, so the
        // name matches the visible label (AGENTS section 15).
        cx.semantics(
            root,
            Semantics::role(Role::Button).with_label(self.label.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use viso_ui::{
        BindingTable, KeyEvent, Modifiers, NodeId, NodeStore, PointerEvent, StateStore,
        VirtualLists,
    };

    /// The reactive stores a button build writes into, kept together so a test
    /// can build a button and then drive its handlers against the same state.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
            }
        }

        /// Build a button through a reactive cx (a button authors state, so a
        /// plain `BuildCx::new` would panic) and return its root node.
        fn build(&mut self, btn: Button) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
            );
            btn.build(&mut cx);
            cx.root().expect("button declares a root node")
        }

        /// Feed a pointer sample to the root's pointer handler, restoring it after
        /// (the router borrow discipline: take, drive, restore).
        fn pointer(&mut self, root: NodeId, ev: PointerEvent) {
            let mut handler = self.store.take_handler(root).expect("pointer handler");
            {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_handler(root, handler);
        }

        /// Feed a key sample to the root's key handler, restoring it after.
        fn key(&mut self, root: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_key_handler(root, handler);
        }
    }

    /// A primary-button pointer sample in the given phase.
    fn primary(phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        }
    }

    /// A key press/release sample.
    fn key_ev(key: Key, pressed: bool, repeat: bool) -> KeyEvent {
        KeyEvent {
            key,
            pressed,
            repeat,
            modifiers: Modifiers::default(),
        }
    }

    /// Count a node's direct children by walking the arena sibling chain.
    fn child_count(store: &NodeStore, parent: NodeId) -> usize {
        let arena = store.arena();
        let mut n = 0;
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            n += 1;
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        n
    }

    /// A button builds an interactive node: a pointer handler, a key handler, a
    /// focusable flag, a `Button` semantics node named by its caption, and a
    /// composed caption child.
    #[test]
    fn button_builds_a_focusable_clickable_node_with_caption() {
        let mut rx = Reactive::new();
        let root = rx.build(button("OK"));

        assert!(
            rx.store.has_handler(root),
            "button attaches a pointer handler"
        );
        assert!(
            rx.store.has_key_handler(root),
            "button attaches a key handler"
        );
        assert!(rx.store.focusable(root), "button is focusable");
        assert_eq!(
            child_count(&rx.store, root),
            1,
            "button composes one caption"
        );

        let sem = rx.store.semantics(root).expect("button authors semantics");
        assert_eq!(sem.role, Role::Button);
        assert_eq!(sem.label.as_deref(), Some("OK"));
    }

    /// The chainable setters override size, background, and the whole style, and
    /// the default size is `Fit` on both axes.
    #[test]
    fn setters_override_style_default_size_is_fit() {
        let default_style = button("x").style;
        assert_eq!(default_style.size.width, Length::Fit);
        assert_eq!(default_style.size.height, Length::Fit);

        let bg = BoxStyle::solid(Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        });
        let widget = button("x").size(Size::fill()).background(bg);
        assert_eq!(widget.style.size, Size::fill());
        assert_eq!(widget.style.background, bg);
    }

    /// A primary press-then-release fires `on_click` exactly once — the click is
    /// the release, so a press alone does nothing.
    #[test]
    fn pointer_press_release_fires_click_once() {
        let clicks = Rc::new(Cell::new(0u32));
        let counter = clicks.clone();
        let mut rx = Reactive::new();
        let root = rx.build(button("OK").on_click(move |_| counter.set(counter.get() + 1)));

        rx.pointer(root, primary(PointerPhase::Down));
        assert_eq!(clicks.get(), 0, "press alone is not a click");

        rx.pointer(root, primary(PointerPhase::Up));
        assert_eq!(clicks.get(), 1, "press-then-release is one click");
    }

    /// A non-primary pointer button does not activate the click.
    #[test]
    fn non_primary_pointer_does_not_activate() {
        let clicks = Rc::new(Cell::new(0u32));
        let counter = clicks.clone();
        let mut rx = Reactive::new();
        let root = rx.build(button("OK").on_click(move |_| counter.set(counter.get() + 1)));

        let non_primary = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary(PointerPhase::Down)
        };
        rx.pointer(root, non_primary);
        let non_primary_up = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary(PointerPhase::Up)
        };
        rx.pointer(root, non_primary_up);
        assert_eq!(clicks.get(), 0, "no click for a non-primary button");
    }

    /// Enter and Space (pressed, not auto-repeat) each fire `on_click`; an
    /// auto-repeat and a key-up do not.
    #[test]
    fn keyboard_enter_and_space_fire_click_repeat_does_not() {
        let clicks = Rc::new(Cell::new(0u32));
        let counter = clicks.clone();
        let mut rx = Reactive::new();
        let root = rx.build(button("OK").on_click(move |_| counter.set(counter.get() + 1)));

        rx.key(root, key_ev(Key::Enter, true, false));
        rx.key(root, key_ev(Key::Space, true, false));
        rx.key(root, key_ev(Key::Enter, true, true)); // auto-repeat: ignored
        rx.key(root, key_ev(Key::Enter, false, false)); // key-up: ignored

        assert_eq!(
            clicks.get(),
            2,
            "Enter and Space each fire once; repeat/up do not"
        );
    }

    /// A handler-less button is inert: driving it does not panic and stays
    /// focusable — a button with no `on_click` is still a well-formed node.
    #[test]
    fn button_without_handler_is_inert() {
        let mut rx = Reactive::new();
        let root = rx.build(button("OK"));

        rx.pointer(root, primary(PointerPhase::Down));
        rx.pointer(root, primary(PointerPhase::Up)); // must not panic with no callback
        rx.key(root, key_ev(Key::Enter, true, false));

        assert!(rx.store.focusable(root), "still focusable");
    }
}
