//! The one node a [`CheckBox`](crate::CheckBox) and a [`Toggle`](crate::Toggle)
//! build: a focusable caption drawing its check's mark beside it.
//!
//! The node's text is the caption and its [`CheckMark`] the box or the switch
//! track at its start edge, sized from the caption's first line and inked with
//! its color. A reactive `checked` cell holds the state: each activation — a
//! primary press then release, or Enter/Space while focused — writes its
//! inverse and fires the change callback with it. The cell projects into the
//! node's semantic state, which carries it to the accessibility tree and moves
//! the mark to it, so a flip repaints the one node, its mark easing over.
//!
//! An `&` in the caption marks its access key: Alt with it takes the focus to
//! the check and flips it.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Access, BoxStyle, BuildCx, CheckMark, CheckShape, EventCx, Haptic, Key, LeafStyle,
    PointerButtons, PointerPhase, Rgba, Role, SemanticState, Semantics, Size, StateId, StateValue,
};

use crate::controls::mnemonic::Mnemonic;

/// A shared, mutable change callback carrying the new checked value, cloned
/// into the pointer and the key handler so both drive the same one. Pointer
/// and keyboard input are never concurrent, so its borrow is never re-entered.
pub(crate) type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, bool)>>>>;

/// The default caption and mark ink: near-white, to read on a dark surface.
pub(crate) const INK: Rgba = Rgba {
    r: 0.98,
    g: 0.98,
    b: 1.0,
    a: 1.0,
};

/// What a check node is built from.
pub(crate) struct Check<'a> {
    pub shape: CheckShape,
    pub caption: &'a str,
    pub checked: bool,
    pub size: Size,
    pub color: Rgba,
    pub on_change: &'a SharedChange,
}

/// Fires the change callback with `checked`, if one is set.
fn fire(cb: &SharedChange, ev: &mut EventCx<'_>, checked: bool) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, checked);
    }
}

/// Writes the inverse of the `checked` cell and fires the callback with it.
fn flip(ev: &mut EventCx<'_>, cell: StateId, cb: &SharedChange) {
    let next = !matches!(ev.get(cell), Some(StateValue::Bool(true)));
    ev.set(cell, StateValue::Bool(next));
    ev.feedback(Haptic::Selection);
    fire(cb, ev, next);
}

impl Check<'_> {
    /// Builds the check's node.
    pub(crate) fn build(&self, cx: &mut BuildCx<'_>) {
        let cell = cx.state(StateValue::Bool(self.checked));
        let node = cx.leaf(LeafStyle {
            size: self.size,
            style: BoxStyle::NONE,
        });
        let caption = Mnemonic::of(self.caption);
        cx.text_request(node, caption.label().color(self.color).request());
        cx.check_mark(
            node,
            CheckMark {
                shape: self.shape,
                checked: self.checked,
            },
        );
        cx.semantics(
            node,
            Semantics::role(Role::CheckBox).with_label(caption.text.as_str()),
        );
        cx.bind_semantic_state(node, move |cx| {
            SemanticState::checked(matches!(cx.get(cell), Some(StateValue::Bool(true))))
        });
        cx.focusable(node, true);
        if let Some(key) = caption.char() {
            cx.access_key(node, key);
        }

        let cb = self.on_change.clone();
        cx.on_pointer(node, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if p.phase == PointerPhase::Up && p.buttons.contains(PointerButtons::PRIMARY) {
                flip(ev, cell, &cb);
            }
        });
        let cb = self.on_change.clone();
        let id = node.id();
        cx.on_key(node, move |ev| {
            if ev.access() == Some(Access::Key) {
                ev.stop_propagation();
                ev.request_focus(id);
                flip(ev, cell, &cb);
            } else if let Some(k) = ev.key()
                && k.pressed
                && !k.repeat
                && matches!(k.key, Key::Enter | Key::Space)
            {
                ev.stop_propagation();
                flip(ev, cell, &cb);
            }
        });
    }
}
