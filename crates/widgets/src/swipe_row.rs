//! The [`SwipeRow`] widget: a list row whose content a swipe across slides
//! aside to show its actions.
//!
//! A `SwipeRow` wraps a row's content and gives either edge a few
//! [`SwipeAction`]s. A drag across — a finger, a mouse, a trackpad's two
//! fingers — slides the content; let go past half the actions or flicked,
//! the row stands open on them, and a press on one runs it and shuts the row.
//! The first action on an edge is its outermost, and with
//! [`SwipeRowStyle::full_trailing`] (or `full_leading`) a full swipe runs it
//! (Mail's delete). A drag up or down stays the list's, a press elsewhere in
//! the list or a scroll shuts the row, and one row stands open at a time.
//! The actions are buttons to the keyboard and to assistive technology: Tab
//! reaches them after the content and opens the row on them, Escape shuts
//! it. The gestures are `viso-ui`'s ([`viso_ui::row_gesture::swipe`]); a
//! slide marks only transforms.
//!
//! ```
//! use viso_widgets::{label, swipe_action, swipe_row};
//! use viso_ui::{BuildCx, Component, NodeStore, Rgba};
//!
//! let row = swipe_row(|cx| label("Lunch with Sam").build(cx))
//!     .leading([swipe_action("Unread")])
//!     .trailing([
//!         swipe_action("Delete").destructive().on_tap(|_ev| { /* delete it */ }),
//!         swipe_action("Archive"),
//!     ]);
//!
//! let mut store = NodeStore::new();
//! let mut cx = BuildCx::new(&mut store);
//! row.build(&mut cx);
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::{
    Access, Align, Alignment2D, Axis, BoxRules, BoxStyle, BuildCx, Component, EventCx, FlexStyle,
    Inset, Justify, Key, Length, NodeId, PointerButtons, PointerPhase, Rgba, Role, RowGesture,
    Semantics, Sizing, Span, StackPlacement, StackStyle, SwipeEdge, SwipeParts,
};

/// What an action runs when tapped or swiped through.
type TapHandler = Rc<RefCell<Box<dyn FnMut(&mut EventCx<'_>)>>>;

/// A neutral action's fill.
const NEUTRAL: Rgba = Rgba {
    r: 0.56,
    g: 0.56,
    b: 0.58,
    a: 1.0,
};

/// A destructive action's fill.
const DESTRUCTIVE: Rgba = Rgba {
    r: 1.0,
    g: 0.23,
    b: 0.19,
    a: 1.0,
};

/// An action's caption color.
const CAPTION: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// The room either side of an action's caption.
const PADDING: f32 = 16.0;

/// One action behind a [`SwipeRow`]'s content.
pub struct SwipeAction {
    label: String,
    tint: Rgba,
    color: Rgba,
    on_tap: Option<TapHandler>,
}

/// A [`SwipeAction`] captioned `label`, neutral gray, doing nothing yet.
pub fn swipe_action(label: impl Into<String>) -> SwipeAction {
    SwipeAction {
        label: label.into(),
        tint: NEUTRAL,
        color: CAPTION,
        on_tap: None,
    }
}

impl SwipeAction {
    /// Fill it with `tint`.
    pub fn tint(mut self, tint: Rgba) -> Self {
        self.tint = tint;
        self
    }

    /// Fill it red, for an action that destroys.
    pub fn destructive(self) -> Self {
        self.tint(DESTRUCTIVE)
    }

    /// Draw its caption in `color`.
    pub fn color(mut self, color: Rgba) -> Self {
        self.color = color;
        self
    }

    /// Run `handler` when it is tapped, activated or swiped through.
    pub fn on_tap(mut self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        self.on_tap = Some(Rc::new(RefCell::new(Box::new(handler))));
        self
    }
}

/// How a [`SwipeRow`] lays out and which edges a full swipe runs on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SwipeRowStyle {
    /// Its own size within its parent: across the list, as tall as its
    /// content by default.
    pub size: Sizing,
    /// The content's own background, which slides with it.
    pub background: BoxStyle,
    /// The narrowest an action is.
    pub action_width: f32,
    /// Whether a full swipe toward the trailing edge runs the first leading
    /// action.
    pub full_leading: bool,
    /// Whether a full swipe toward the leading edge runs the first trailing
    /// action.
    pub full_trailing: bool,
}

impl Default for SwipeRowStyle {
    fn default() -> Self {
        SwipeRowStyle {
            size: Sizing {
                width: Length::fill(),
                height: Length::Fit,
            },
            background: BoxStyle::NONE,
            action_width: 74.0,
            full_leading: false,
            full_trailing: true,
        }
    }
}

/// A slot an app passes to [`SwipeRow::handle`] and reads after building for
/// the row's [`SwipeRowHandle`].
pub type SwipeRowHandleSlot = Rc<RefCell<Option<SwipeRowHandle>>>;

/// Opens or shuts a built [`SwipeRow`] from an event handler. Cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwipeRowHandle {
    node: NodeId,
}

impl SwipeRowHandle {
    /// The row's node.
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// Open it on `edge`'s actions.
    pub fn open(&self, ev: &mut EventCx<'_>, edge: SwipeEdge) {
        ev.open_swipe_row(self.node, Some(edge));
    }

    /// Shut it.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        ev.open_swipe_row(self.node, None);
    }
}

/// [`SwipeRow`]'s content-declaration closure, run once at build.
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// A row whose content a swipe across slides aside to show its actions.
/// Build one with [`swipe_row`].
pub struct SwipeRow {
    style: SwipeRowStyle,
    content: ContentBuilder,
    leading: Vec<SwipeAction>,
    trailing: Vec<SwipeAction>,
    slot: Option<SwipeRowHandleSlot>,
}

/// A [`SwipeRow`] over the content `content` declares, with no actions yet.
pub fn swipe_row(content: impl Fn(&mut BuildCx<'_>) + 'static) -> SwipeRow {
    SwipeRow {
        style: SwipeRowStyle::default(),
        content: Box::new(content),
        leading: Vec::new(),
        trailing: Vec::new(),
        slot: None,
    }
}

impl SwipeRow {
    /// The actions a slide toward the trailing edge shows, the outermost
    /// first.
    pub fn leading(mut self, actions: impl IntoIterator<Item = SwipeAction>) -> Self {
        self.leading = actions.into_iter().collect();
        self
    }

    /// The actions a slide toward the leading edge shows, the outermost
    /// first.
    pub fn trailing(mut self, actions: impl IntoIterator<Item = SwipeAction>) -> Self {
        self.trailing = actions.into_iter().collect();
        self
    }

    /// Replace the whole [`SwipeRowStyle`].
    pub fn style(mut self, style: SwipeRowStyle) -> Self {
        self.style = style;
        self
    }

    /// Fill `slot` with its [`SwipeRowHandle`] as it builds.
    pub fn handle(mut self, slot: &SwipeRowHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }
}

/// The row's node, known once it is declared.
type RowCell = Rc<Cell<Option<NodeId>>>;

/// Runs `tap`, if any, then shuts the row.
fn run(tap: Option<&TapHandler>, row: &RowCell, ev: &mut EventCx<'_>) {
    if let Some(tap) = tap {
        (tap.borrow_mut())(ev);
    }
    if let Some(row) = row.get() {
        ev.open_swipe_row(row, None);
    }
}

impl SwipeRow {
    /// Declares one edge's panel: its actions packed against the content's
    /// side, `end` for the leading panel, the outermost farthest out, on
    /// the outermost's fill.
    fn panel(
        &self,
        cx: &mut BuildCx<'_>,
        row: &RowCell,
        actions: &[SwipeAction],
        end: bool,
    ) -> NodeId {
        cx.place(StackPlacement {
            align: Some(Alignment2D::TopStart),
            layer: -1,
        });
        let fill = actions.first().map_or(NEUTRAL, |a| a.tint);
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                justify: if end { Justify::End } else { Justify::Start },
                size: Sizing::fill(),
                style: BoxStyle::solid(fill),
            },
            |cx| {
                let order: Vec<&SwipeAction> = if end {
                    actions.iter().collect()
                } else {
                    actions.iter().rev().collect()
                };
                for action in order {
                    self.action(cx, row, action);
                }
            },
        )
        .id()
    }

    /// Declares one action: a button the height of the row.
    fn action(&self, cx: &mut BuildCx<'_>, row: &RowCell, action: &SwipeAction) {
        let node = cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 0.0,
                padding: Inset {
                    left: PADDING,
                    top: 0.0,
                    right: PADDING,
                    bottom: 0.0,
                },
                align: Align::Center,
                justify: Justify::Center,
                size: Sizing {
                    width: Length::Fit,
                    height: Length::fill(),
                },
                style: BoxStyle::solid(action.tint),
            },
            |cx| {
                crate::label(action.label.clone())
                    .color(action.color)
                    .build(cx);
            },
        );
        cx.set_box_rules(
            node,
            BoxRules {
                min_width: Span::dp(self.style.action_width),
                ..BoxRules::NONE
            },
        );
        cx.focusable(node, true);
        // A press on an action shuts the row once it lifts.
        let tap = action.on_tap.clone();
        cx.on_pointer(node, move |ev| {
            if let Some(p) = ev.pointer()
                && p.phase == PointerPhase::Up
                && p.buttons.contains(PointerButtons::PRIMARY)
                && let Some(tap) = &tap
            {
                (tap.borrow_mut())(ev);
            }
        });
        let tap = action.on_tap.clone();
        let at = Rc::clone(row);
        cx.on_key(node, move |ev| {
            let activated = ev.access() == Some(Access::Key)
                || ev.key().is_some_and(|k| {
                    k.pressed && !k.repeat && matches!(k.key, Key::Enter | Key::Space)
                });
            if activated {
                ev.stop_propagation();
                run(tap.as_ref(), &at, ev);
            }
        });
        cx.semantics(
            node,
            Semantics::role(Role::Button).with_label(action.label.clone()),
        );
    }
}

impl Component for SwipeRow {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let row = RowCell::default();
        let mut parts = None;
        let root = cx.stack(
            StackStyle {
                align: Alignment2D::TopStart,
                padding: Inset::all(0.0),
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                let content = cx
                    .stack(
                        StackStyle {
                            align: Alignment2D::TopStart,
                            padding: Inset::all(0.0),
                            size: Sizing {
                                width: Length::fill(),
                                height: Length::Fit,
                            },
                            style: self.style.background,
                        },
                        |cx| (self.content)(cx),
                    )
                    .id();
                let leading =
                    (!self.leading.is_empty()).then(|| self.panel(cx, &row, &self.leading, true));
                let trailing = (!self.trailing.is_empty())
                    .then(|| self.panel(cx, &row, &self.trailing, false));
                parts = Some(SwipeParts {
                    content,
                    leading,
                    trailing,
                    full_leading: self.style.full_leading,
                    full_trailing: self.style.full_trailing,
                });
            },
        );
        row.set(Some(root.id()));
        cx.clip(root, true);
        cx.swipe_row(root, parts);
        let leading_tap = self.leading.first().and_then(|a| a.on_tap.clone());
        let trailing_tap = self.trailing.first().and_then(|a| a.on_tap.clone());
        cx.on_key(root, move |ev| {
            let tap = match ev.row_gesture() {
                Some(RowGesture::Swiped(SwipeEdge::Leading)) => leading_tap.as_ref(),
                Some(RowGesture::Swiped(SwipeEdge::Trailing)) => trailing_tap.as_ref(),
                _ => return,
            };
            if let Some(tap) = tap {
                (tap.borrow_mut())(ev);
            }
        });
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(SwipeRowHandle { node: root.id() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{NodeStore, Rect};

    fn built(row: SwipeRow) -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            row.build(&mut cx);
            cx.root().unwrap()
        };
        store.layout(root, Rect::new(0.0, 0.0, 320.0, 44.0), &mut Vec::new());
        (store, root)
    }

    #[test]
    fn it_builds_a_clipping_swipe_row_with_its_actions_as_buttons() {
        let row = swipe_row(|cx| {
            cx.leaf(viso_ui::LeafStyle {
                size: Sizing::fixed(320.0, 44.0),
                style: BoxStyle::NONE,
            });
        })
        .trailing([swipe_action("Delete").destructive(), swipe_action("Flag")]);
        let (store, root) = built(row);
        assert!(store.is_swipe_row(root) && store.clips(root));
        let kids: Vec<NodeId> = store.arena().children(root).collect();
        assert_eq!(kids.len(), 2, "the content and the trailing panel");
        let actions: Vec<NodeId> = store.arena().children(kids[1]).collect();
        let names: Vec<_> = actions
            .iter()
            .map(|&a| store.semantics(a).and_then(|s| s.label.clone()).unwrap())
            .collect();
        assert_eq!(names, ["Flag", "Delete"], "the outermost farthest out");
        assert!(actions.iter().all(|&a| store.focusable(a)));
        assert!(store.bounds(actions[0]).w >= 74.0);
        assert_eq!(store.swipe_open(root), None);
    }
}
