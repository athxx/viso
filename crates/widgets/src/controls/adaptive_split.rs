//! The [`AdaptiveSplit`] control — a list beside its detail, collapsing to a
//! stack where the room is narrow.
//!
//! Where the size class at the control is wide enough (expanded by default),
//! the list pane stands at its own width with the detail pane filling the
//! rest, a divider between them. Narrower, the two panes stack, one in front
//! at a time: the list, until the detail is shown, which brings it in over
//! the list as the platform's navigation does; a back gesture (Escape or
//! Backspace while focus is in the control, or the system back action)
//! returns to the list. Whether the detail is shown is kept across size
//! classes, so a window narrowed with the detail shown keeps it in front.
//!
//! The size class is the one the environment gives the control: the window's,
//! or the nearest adaptive scope's. Both panes build once; a size class
//! change relinks them between the side-by-side row and the stack without
//! rebuilding, so their state, scroll and focus survive. Focus in a pane the
//! stack folds away moves to the pane in front: back where it was when that
//! pane last had it, else its first focusable node.
//!
//! An app shows the detail and goes back through a [`SplitHandle`], from a
//! list row's handler say, and hears each change through
//! [`AdaptiveSplit::on_change`].
//!
//! ```
//! use viso_widgets::adaptive_split;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Sizing};
//!
//! let split = adaptive_split()
//!     .list(|cx| { cx.leaf(LeafStyle { size: Sizing::fill(), style: BoxStyle::NONE }); })
//!     .detail(|cx| { cx.leaf(LeafStyle { size: Sizing::fill(), style: BoxStyle::NONE }); });
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! let handle = split.build(&mut cx);
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::adaptive::{AnchorId, EnvField, SizeClass};
use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, ColorToken, Component, Controls, DirtyClass, EventCx,
    FlexStyle, Inset, Justify, Key, LeafStyle, Length, NodeId, NodeStore, PageTransition, Rgba,
    Role, Semantics, Sizing, StackStyle, StateId, StateValue, StructureCx,
};

/// Which pane of an [`AdaptiveSplit`] is shown in front when it stacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SplitPane {
    /// The list.
    List,
    /// The detail.
    Detail,
}

/// What runs with the detail shown or the list returned to.
type SharedPane = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, SplitPane)>>>>;

/// A pane's content builder.
type PaneBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

viso_ui::control_handle! {
    /// A handle an app captures to drive a built [`AdaptiveSplit`]: show the
    /// detail, return to the list, and ask whether the panes stack. Call it
    /// from an event handler.
    ///
    /// `Copy`, an id alone: `build` returns it, or mint one first with
    /// [`BuildCx::handle`](viso_ui::BuildCx::handle) for closures declared
    /// before the control. Once the control is gone, every call is a no-op.
    pub struct SplitHandle;
}

impl SplitHandle {
    /// Show the detail: in front, over the list, while the panes stack.
    pub fn show_detail(self, ev: &mut EventCx<'_>) {
        if let Some(parts) = ev.control::<SplitParts>(self.0) {
            parts.show(ev, SplitPane::Detail);
        }
    }

    /// Return to the list: in front again while the panes stack.
    pub fn show_list(self, ev: &mut EventCx<'_>) {
        if let Some(parts) = ev.control::<SplitParts>(self.0) {
            parts.show(ev, SplitPane::List);
        }
    }

    /// Whether the panes stack at the size class the control has now, `false`
    /// once it is gone.
    pub fn stacked(self, ev: &mut EventCx<'_>) -> bool {
        ev.control::<SplitParts>(self.0)
            .is_some_and(|parts| parts.stacked.get() == Some(true))
    }
}

/// What a built control's handlers and its [`SplitHandle`] share.
struct SplitParts {
    /// Whether the detail is shown: in front while the panes stack.
    open: StateId,
    /// Whether the panes stack, `None` before they are first arranged.
    stacked: Rc<Cell<Option<bool>>>,
    on_change: SharedPane,
}

impl SplitParts {
    fn show(&self, ev: &mut EventCx<'_>, pane: SplitPane) {
        let open = pane == SplitPane::Detail;
        if ev.get(self.open) == Some(StateValue::Bool(open)) {
            return;
        }
        ev.set(self.open, StateValue::Bool(open));
        if let Some(f) = self.on_change.borrow_mut().as_mut() {
            f(ev, pane);
        }
    }
}

/// How an [`AdaptiveSplit`] looks and where it stacks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveSplitStyle {
    /// The control's own size within its parent (default `Fill` both ways).
    pub size: Sizing,
    /// The list pane's width beside the detail.
    pub list_width: f32,
    /// The divider between the panes side by side, and its width.
    pub divider: Rgba,
    pub divider_width: f32,
    /// The narrowest size class the panes stand side by side at; narrower,
    /// they stack.
    pub side_by_side: SizeClass,
    /// How the stacked panes trade places: the platform's own way by default.
    pub transition: PageTransition,
}

impl Default for AdaptiveSplitStyle {
    fn default() -> Self {
        AdaptiveSplitStyle {
            size: Sizing::fill(),
            list_width: 320.0,
            divider: ColorToken::Outline.rgba(),
            divider_width: 1.0,
            side_by_side: SizeClass::Expanded,
            transition: PageTransition::Native,
        }
    }
}

/// A list beside its detail, collapsing to a stack where the room is narrow.
/// Construct one with [`adaptive_split`]; see the [module docs](self).
pub struct AdaptiveSplit {
    list: Option<PaneBuilder>,
    detail: Option<PaneBuilder>,
    detail_shown: bool,
    style: AdaptiveSplitStyle,
    on_change: SharedPane,
    handle: Option<SplitHandle>,
}

/// A split with empty panes, the list in front.
pub fn adaptive_split() -> AdaptiveSplit {
    AdaptiveSplit {
        list: None,
        detail: None,
        detail_shown: false,
        style: AdaptiveSplitStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
        handle: None,
    }
}

impl AdaptiveSplit {
    /// Build the list pane's content with `pane`.
    pub fn list(mut self, pane: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.list = Some(Box::new(pane));
        self
    }

    /// Build the detail pane's content with `pane`.
    pub fn detail(mut self, pane: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.detail = Some(Box::new(pane));
        self
    }

    /// Start with the detail shown (default the list in front).
    pub fn detail_shown(mut self, shown: bool) -> Self {
        self.detail_shown = shown;
        self
    }

    /// Run `handler` with the pane shown in front each time the detail is
    /// shown or the list returned to, by the handle or a back gesture.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, SplitPane) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`AdaptiveSplitStyle`].
    pub fn style(mut self, style: AdaptiveSplitStyle) -> Self {
        self.style = style;
        self
    }

    /// Stand the panes side by side from size class `class` up (default
    /// expanded).
    pub fn side_by_side(mut self, class: SizeClass) -> Self {
        self.style.side_by_side = class;
        self
    }

    /// Bind `handle`, minted with [`BuildCx::handle`](viso_ui::BuildCx::handle)
    /// before the control is built.
    pub fn handle(mut self, handle: SplitHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Build the control into `cx` and return its handle.
    pub fn build(mut self, cx: &mut BuildCx<'_>) -> SplitHandle {
        let handle = *self.handle.get_or_insert_with(|| cx.handle());
        Component::build(&self, cx);
        handle
    }
}

/// The rank of a size class, narrowest first.
fn rank(class: SizeClass) -> u8 {
    match class {
        SizeClass::Compact => 0,
        SizeClass::Medium => 1,
        SizeClass::Expanded => 2,
    }
}

/// Whether `node` is `pane` or inside it.
fn within(store: &NodeStore, node: NodeId, pane: NodeId) -> bool {
    let mut at = Some(node);
    while let Some(n) = at {
        if n == pane {
            return true;
        }
        at = store.parent(n);
    }
    false
}

/// The first focusable node of `pane`, in tree order.
fn first_focusable(store: &NodeStore, pane: NodeId) -> Option<NodeId> {
    let arena = store.arena();
    let mut stack = vec![pane];
    while let Some(node) = stack.pop() {
        if store.focusable(node) && store.enabled(node) {
            return Some(node);
        }
        let at = stack.len();
        stack.extend(arena.children(node));
        stack[at..].reverse();
    }
    None
}

/// What the control's structure hook arranges.
struct Split {
    root: NodeId,
    /// The side-by-side row: list, divider, detail.
    row: NodeId,
    divider: NodeId,
    /// The paged stack the panes collapse into.
    stack: NodeId,
    /// The list pane, then the detail pane.
    panes: [NodeId; 2],
    anchor: Cell<Option<AnchorId>>,
    open: StateId,
    style: AdaptiveSplitStyle,
    stacked: Rc<Cell<Option<bool>>>,
    /// Where focus was in each pane when it was last folded away.
    focus: Cell<[Option<NodeId>; 2]>,
}

impl Split {
    /// Arrange the panes for the size class at the control and the pane in
    /// front, and keep focus out of a folded pane.
    fn arrange(&self, s: &mut StructureCx<'_>) {
        let Some(anchor) = self.anchor.get() else {
            return;
        };
        if !s.store.arena().is_live(self.root) {
            s.states.release_anchor(anchor);
            self.anchor.set(None);
            return;
        }
        let class = s.states.env().size_class(anchor).unwrap_or_default();
        let stacked = rank(class) < rank(self.style.side_by_side);
        let front = usize::from(s.states.get(self.open) == Some(StateValue::Bool(true)));
        if self.stacked.get() != Some(stacked) {
            self.relink(s.store, stacked, front);
            self.stacked.set(Some(stacked));
        } else if stacked {
            s.store
                .set_page(self.stack, Some(front), self.style.transition);
        }
        if stacked {
            self.refocus(s.store, front);
        }
    }

    /// Move the panes into the stack, showing pane `front`, or back into the
    /// row beside each other.
    fn relink(&self, store: &mut NodeStore, stacked: bool, front: usize) {
        let [list, detail] = self.panes;
        let from = store.parent(list);
        for pane in self.panes {
            store.arena_detach(pane);
        }
        let all =
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS;
        if let Some(from) = from {
            store.mark_dirty(from, all);
        }
        let list_size = if stacked {
            Sizing::fill()
        } else {
            Sizing {
                width: Length::Fixed(self.style.list_width),
                height: Length::fill(),
            }
        };
        store.set_fixed_size(list, list_size);
        if stacked {
            store.arena_insert_before(self.stack, list, None);
            store.arena_insert_before(self.stack, detail, None);
            store.set_page(self.stack, Some(front), PageTransition::None);
        } else {
            store.arena_insert_before(self.row, list, Some(self.divider));
            store.arena_insert_before(self.row, detail, None);
        }
        store.set_hidden(self.stack, !stacked);
        store.set_hidden(self.row, stacked);
        for pane in self.panes {
            store.mark_dirty(pane, all);
        }
    }

    /// Move focus held in the pane the stack folds to pane `front`: where it
    /// was there last, else its first focusable node.
    fn refocus(&self, store: &mut NodeStore, front: usize) {
        let Some(focused) = store.focused() else {
            return;
        };
        let folded = 1 - front;
        if !within(store, focused, self.panes[folded]) {
            return;
        }
        let mut focus = self.focus.get();
        focus[folded] = Some(focused);
        let back = focus[front]
            .filter(|&n| store.arena().is_live(n) && within(store, n, self.panes[front]))
            .filter(|&n| store.focusable(n) && store.enabled(n))
            .or_else(|| first_focusable(store, self.panes[front]));
        focus[front] = None;
        self.focus.set(focus);
        store.set_focused(back);
    }
}

impl Component for AdaptiveSplit {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let open = cx.state(StateValue::Bool(self.detail_shown));
        let fill = FlexStyle {
            axis: Axis::Column,
            gap: 0.0,
            padding: Inset::all(0.0),
            align: Align::Stretch,
            justify: Justify::Start,
            size: Sizing::fill(),
            style: BoxStyle::NONE,
        };
        let mut row = None;
        let mut divider = None;
        let mut panes = Vec::with_capacity(2);
        let mut stack = None;
        let root = cx.flex(
            FlexStyle {
                size: style.size,
                ..fill
            },
            |cx| {
                row = Some(cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        ..fill
                    },
                    |cx| {
                        for pane in [&self.list, &self.detail] {
                            let node = cx.flex(fill, |cx| {
                                if let Some(pane) = pane {
                                    pane(cx);
                                }
                            });
                            cx.semantics(node, Semantics::role(Role::Group));
                            panes.push(node.id());
                            if divider.is_none() {
                                divider = Some(cx.leaf(LeafStyle {
                                    size: Sizing {
                                        width: Length::Fixed(style.divider_width),
                                        height: Length::fill(),
                                    },
                                    style: BoxStyle::solid(style.divider),
                                }));
                            }
                        }
                    },
                ));
                stack = Some(cx.stack(
                    StackStyle {
                        size: Sizing::fill(),
                        ..Default::default()
                    },
                    |_| {},
                ));
            },
        );
        let (row, divider, stack) = (
            row.expect("built"),
            divider.expect("built"),
            stack.expect("built"),
        );
        cx.clip(stack, true);
        cx.set_hidden(stack, true);
        cx.semantics(root, Semantics::role(Role::Group));

        let stacked = Rc::new(Cell::new(None));
        let parts = Rc::new(SplitParts {
            open,
            stacked: Rc::clone(&stacked),
            on_change: Rc::clone(&self.on_change),
        });
        // A back gesture returns to the list while the detail is in front.
        let back = Rc::clone(&parts);
        cx.on_key(root, move |ev| {
            let Some(k) = ev.key().copied() else { return };
            if !k.pressed || k.repeat {
                return;
            }
            if matches!(k.key, Key::Escape | Key::Backspace | Key::Back)
                && back.stacked.get() == Some(true)
                && ev.get(back.open) == Some(StateValue::Bool(true))
            {
                back.show(ev, SplitPane::List);
                ev.stop_propagation();
            }
        });
        if let Some(handle) = self.handle {
            cx.bind_control(handle.0, root.id(), Rc::clone(&parts));
        }

        let panes = [panes[0], panes[1]];
        cx.structure(|sx| {
            let anchor = sx.states.anchor_env(root.id(), None);
            let class = sx
                .states
                .anchor_cell(anchor, EnvField::SizeClass)
                .expect("an anchored field");
            let split = Rc::new(Split {
                root: root.id(),
                row: row.id(),
                divider: divider.id(),
                stack: stack.id(),
                panes,
                anchor: Cell::new(Some(anchor)),
                open,
                style,
                stacked,
                focus: Cell::new([None; 2]),
            });
            split.arrange(sx);
            sx.store
                .add_structure_hook([class, open], move |sx, _| split.arrange(sx));
        });
    }
}
