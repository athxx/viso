//! The [`VirtualList`] widget — a virtualized scrolling list.
//!
//! `VirtualList` is the widget-layer face of the `viso-ui` virtual-list
//! substrate: a scroll viewport whose content is a fixed-extent canvas sized to
//! the whole logical collection, so only a window of rows is ever mounted while
//! the scroll range still spans every item. A 100k-row list mounts ~a viewport's
//! worth of row nodes (plus overscan), never 100k — the per-frame reconcile the
//! driver runs mounts the first window, then recycles a bounded handful of hosts
//! on each scroll-boundary crossing (architecture section 12.4).
//!
//! It differs from [`Scroll`](crate::Scroll) on purpose. `Scroll` lays its whole
//! content child out once and clips the overflow — right for a bounded block of
//! content. `VirtualList` never builds the whole collection: it declares an
//! `item_count` and a per-row builder, and the substrate mounts only what is
//! visible. Reach for `VirtualList` when the collection is large or unbounded;
//! reach for `Scroll` when the content is a fixed block that happens to overflow.
//!
//! Rows are declared by a builder closure invoked with each row's logical index,
//! run only when a row is (re)mounted — cold, never on the per-frame hot path.
//! The default accessible role is [`Role::Group`]: a structural grouping with no
//! interaction of its own; the wheel/drag scroll gesture is routed by `viso-ui`'s
//! `ScrollRouter`, not by a widget-level handler.
//!
//! Building a `VirtualList` requires a reactive [`BuildCx`] (one made with
//! [`BuildCx::with_reactive`]): it registers per-list state in the driver-owned
//! list registry. The facade always builds with a reactive cx, so an app that
//! declares a `VirtualList` in its `build` gets this for free.

use std::cell::RefCell;

use viso_ui::{Axis, BoxStyle, BuildCx, Component, Role, Semantics, Size, VirtualListStyle};

/// A [`VirtualList`]'s per-row builder. Boxed `FnMut(usize, &mut BuildCx)`
/// because a row is authored against the shared [`BuildCx`] given its logical
/// index, and the substrate drives it mutably as rows recycle. Cold — run only
/// when a row is (re)mounted, never on the per-frame hot path. Parallels
/// `viso_ui`'s boxed handler aliases (a cold fat pointer kept off the hot node
/// columns).
type RowBuilder = Box<dyn FnMut(usize, &mut BuildCx<'_>)>;

/// The visual and layout parameters of a [`VirtualList`]: the axis it scrolls
/// and stacks rows along, its own visible size, the overscan window, the initial
/// per-row estimate, and an optional background/border for the viewport box.
///
/// This mirrors [`VirtualListStyle`] one-to-one but is the widget-layer name, so
/// an app configures a list without naming the `viso-ui` substrate type. The
/// meaning of each field is the substrate's; see [`VirtualListStyle`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualListViewStyle {
    /// The axis the list scrolls and stacks rows along.
    pub axis: Axis,
    /// The viewport's own visible size within its parent (the clip box). The
    /// logical collection's extent along `axis` spans the full scroll range;
    /// content is clipped to this box.
    pub size: Size,
    /// Extra rows mounted on each side of the visible window, so a small scroll
    /// reveals an already-built row rather than stalling on a mount.
    pub overscan: u32,
    /// The initial per-row main extent, used to size the canvas and seed the
    /// height model before any row is measured. A row measured taller/shorter is
    /// absorbed into the model, converging the scroll range on the real extent.
    pub estimated_row: f32,
    /// The viewport's own background/border/radius. [`BoxStyle::NONE`] is a pure,
    /// transparent clip box.
    pub background: BoxStyle,
}

impl Default for VirtualListViewStyle {
    fn default() -> Self {
        let base = VirtualListStyle::default();
        VirtualListViewStyle {
            axis: base.axis,
            size: base.size,
            overscan: base.overscan,
            estimated_row: base.estimated_row,
            background: base.style,
        }
    }
}

/// A virtualized scrolling list: a scroll viewport that mounts only the visible
/// window of rows (plus overscan) over a canvas sized to the whole collection.
///
/// Construct one with [`virtual_list`] and author its rows with
/// [`VirtualList::item`]:
///
/// ```
/// use viso_widgets::{virtual_list, VirtualListViewStyle};
/// use viso_ui::{
///     Axis, BindingTable, BuildCx, Component, LeafStyle, Length, NodeStore, Size,
///     StateStore, TextEdits, VirtualLists,
/// };
///
/// let list = virtual_list(VirtualListViewStyle {
///     axis: Axis::Column,
///     size: Size::fixed(200.0, 300.0),
///     ..Default::default()
/// })
/// .items(100_000, |index, cx| {
///     // one row's body, built only when the row is mounted
///     cx.leaf(LeafStyle {
///         size: Size {
///             width: Length::fill(),
///             height: Length::Fixed(30.0),
///         },
///         ..Default::default()
///     });
///     let _ = index;
/// });
///
/// // A VirtualList registers list state, so it builds through a reactive cx.
/// let mut store = NodeStore::new();
/// let mut states = StateStore::new();
/// let mut bindings = BindingTable::new();
/// let mut lists = VirtualLists::new();
/// let mut text_edits = TextEdits::new();
/// let mut cx = BuildCx::with_reactive(
///     &mut store,
///     &mut states,
///     &mut bindings,
///     &mut lists,
///     &mut text_edits,
/// );
/// list.build(&mut cx);
/// ```
///
/// Invalidation: mounting/recycling a row invalidates only that row's subtree —
/// its `MEASURE` stops rising at the fixed-extent canvas, so a row remount never
/// forces the ancestors above the list to relayout. A scroll within the mounted
/// window is a pure transform (no relayout, no rebind). That targeted
/// invalidation lives in the `viso-ui` substrate, not here. Semantics default to
/// [`Role::Group`].
pub struct VirtualList {
    style: VirtualListViewStyle,
    /// The number of logical rows. The canvas is sized to this times
    /// `estimated_row`; only a window is ever mounted.
    item_count: usize,
    /// The per-row builder, taken by `build`. Kept in a `RefCell<Option<..>>`
    /// because [`Component::build`] takes `&self` while [`BuildCx::virtual_list`]
    /// needs to *own* the `FnMut` (the substrate stores and drives it as rows
    /// recycle). `build` moves it out with `take`; a second `build` on the same
    /// value finds `None` and declares an empty (no-row-body) list rather than
    /// panicking. Building a widget value once is the norm.
    item: RefCell<Option<RowBuilder>>,
    /// An optional accessible label. `None` keeps the plain [`Role::Group`] list
    /// with no name.
    label: Option<String>,
}

/// Construct a [`VirtualList`] with the given style, no rows yet. Chain
/// [`VirtualList::items`] to set the row count and per-row builder, and
/// [`VirtualList::label`] to give it an accessible name.
pub fn virtual_list(style: VirtualListViewStyle) -> VirtualList {
    VirtualList {
        style,
        item_count: 0,
        item: RefCell::new(None),
        label: None,
    }
}

impl Default for VirtualList {
    fn default() -> Self {
        virtual_list(VirtualListViewStyle::default())
    }
}

impl VirtualList {
    /// Set the logical row count and the per-row builder. The builder is invoked
    /// with a row's logical index only when that row is (re)mounted — it authors
    /// one row's body into the given [`BuildCx`]. No row is built until it enters
    /// the visible window.
    pub fn items(
        mut self,
        item_count: usize,
        item: impl FnMut(usize, &mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.item_count = item_count;
        self.item = RefCell::new(Some(Box::new(item)));
        self
    }

    /// Give the list an accessible label, surfaced on its [`Role::Group`]
    /// semantics node.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Component for VirtualList {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // `virtual_list` takes an owned `FnMut`; `build` has only `&self`, so move
        // the row builder out of the interior `RefCell`. When none was authored
        // (or the value was already built once), declare a no-body list so the
        // viewport/canvas/scroll-range still exist — rows would just build empty.
        let item = self.item.borrow_mut().take();
        let handle = match item {
            Some(mut item) => cx.virtual_list(
                VirtualListStyle {
                    axis: self.style.axis,
                    size: self.style.size,
                    overscan: self.style.overscan,
                    estimated_row: self.style.estimated_row,
                    style: self.style.background,
                },
                self.item_count,
                move |index, cx| item(index, cx),
            ),
            None => cx.virtual_list(
                VirtualListStyle {
                    axis: self.style.axis,
                    size: self.style.size,
                    overscan: self.style.overscan,
                    estimated_row: self.style.estimated_row,
                    style: self.style.background,
                },
                self.item_count,
                |_index, _cx| {},
            ),
        };

        // A virtual list is a `Group` to an assistive technology; carry the label
        // when one was authored.
        let mut semantics = Semantics::role(Role::Group);
        if let Some(label) = &self.label {
            semantics = semantics.with_label(label.clone());
        }
        cx.semantics(handle, semantics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, LeafStyle, Length, NodeId, NodeStore, StateStore, TextEdits, VirtualLists,
    };

    /// The reactive stores a virtual-list build writes into, kept together so a
    /// test can build a list and then inspect the registered state. A
    /// `VirtualList` registers list state, so it must build through a reactive cx.
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

        /// Build a virtual list through a reactive cx and return its root node.
        fn build(&mut self, list: VirtualList) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
            );
            list.build(&mut cx);
            cx.root().expect("virtual list declares a root node")
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

    fn row(_index: usize, cx: &mut BuildCx<'_>) {
        cx.leaf(LeafStyle {
            size: Size {
                width: Length::fill(),
                height: Length::Fixed(30.0),
            },
            ..Default::default()
        });
    }

    /// A `VirtualList` maps to a single scroll viewport node with exactly one
    /// child — the fixed-extent canvas — and mounts no rows at build time,
    /// however large the logical collection.
    #[test]
    fn virtual_list_builds_a_scroll_viewport_over_a_canvas_with_no_rows() {
        let mut rx = Reactive::new();
        let root = rx.build(
            virtual_list(VirtualListViewStyle {
                axis: Axis::Column,
                size: Size::fixed(200.0, 300.0),
                ..Default::default()
            })
            .items(100_000, row),
        );

        assert!(
            rx.store.is_scroll(root),
            "a VirtualList maps to a scroll viewport node"
        );
        assert_eq!(
            child_count(&rx.store, root),
            1,
            "the viewport's single child is the fixed-extent canvas; no rows are \
             mounted at build time"
        );
        let state = rx
            .lists
            .get(root)
            .expect("the list registers per-list state on its viewport");
        assert_eq!(
            state.mounted_count(),
            0,
            "no rows are mounted until the first reconcile"
        );
    }

    /// The viewport carries the requested background as its own box style, and the
    /// registered state seeds its extent from `estimated_row * item_count`.
    #[test]
    fn virtual_list_carries_background_and_seeds_its_extent() {
        let bg = BoxStyle::solid(viso_ui::Rgba {
            r: 0.1,
            g: 0.1,
            b: 0.12,
            a: 1.0,
        });
        let mut rx = Reactive::new();
        let root = rx.build(
            virtual_list(VirtualListViewStyle {
                axis: Axis::Column,
                size: Size::fixed(200.0, 300.0),
                estimated_row: 30.0,
                background: bg,
                ..Default::default()
            })
            .items(1_000, row),
        );

        assert_eq!(rx.store.style(root), bg);
        let state = rx.lists.get(root).expect("registered state");
        assert_eq!(
            state.total_extent(),
            30.0 * 1_000.0,
            "the seeded extent is estimated_row * item_count"
        );
    }

    /// The default accessible role of a list is `Group`, and an authored label
    /// surfaces on that node. A `VirtualList` declares no interactive handlers of
    /// its own — the scroll gesture is routed by `viso-ui`, not a widget handler —
    /// so it stays a structural `Group`.
    #[test]
    fn virtual_list_default_role_is_group_and_carries_label() {
        let mut rx = Reactive::new();
        let root = rx.build(
            virtual_list(VirtualListViewStyle::default())
                .items(10, row)
                .label("Messages"),
        );

        let sem = rx
            .store
            .semantics(root)
            .expect("virtual list authors semantics on its node");
        assert_eq!(sem.role, Role::Group);
        assert_eq!(sem.label.as_deref(), Some("Messages"));
        assert!(
            !rx.store.has_handler(root) && !rx.store.has_key_handler(root),
            "a VirtualList declares no interactive handlers of its own"
        );
    }

    /// A `VirtualList` with no authored rows still builds a valid empty viewport
    /// (a zero-item list): the viewport and canvas exist, no rows mount, and the
    /// extent is zero.
    #[test]
    fn virtual_list_without_items_is_a_valid_empty_list() {
        let mut rx = Reactive::new();
        let root = rx.build(virtual_list(VirtualListViewStyle::default()));

        assert!(rx.store.is_scroll(root));
        assert_eq!(
            child_count(&rx.store, root),
            1,
            "an empty list still has its canvas child"
        );
        let state = rx.lists.get(root).expect("registered state");
        assert_eq!(state.mounted_count(), 0);
        assert_eq!(state.total_extent(), 0.0);
    }
}
