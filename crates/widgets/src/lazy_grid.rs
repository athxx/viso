//! The [`LazyGrid`] widget — a virtualized grid of items in lanes.
//!
//! A `LazyGrid` is a [`VirtualList`](crate::VirtualList) whose items flow
//! into lanes across its axis: a fixed count of them, or as many at least a
//! given width as it fits, recounted as it resizes. Only the items about the
//! viewport are ever built, and they recycle as it scrolls, however many it
//! holds. Laid on lines, a line is as long as its longest item: a photo grid.
//! Laid as masonry, each item packs under the lane that ends soonest: a board
//! of cards of uneven length. An item that spans the line — a section header
//! always does, pinned at the start edge while its section passes — starts a
//! line of its own.

use std::cell::RefCell;

use viso_ui::{
    Axis, BoxStyle, BuildCx, Columns, Component, EventCx, Handle, ListGrid, Role, ScrollBars,
    Semantics, Sizing, StickyOf, VirtualListStyle,
};

use crate::scroll::{SnapHandler, install_viewport_events};

/// A [`LazyGrid`]'s item builder: an item's body, given its index.
type ItemBuilder = Box<dyn FnMut(usize, &mut BuildCx<'_>)>;

/// How a [`LazyGrid`] lays its items out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LazyGridStyle {
    /// The axis it scrolls along; its lanes run across it.
    pub axis: Axis,
    /// Its size within its parent.
    pub size: Sizing,
    /// Its lanes.
    pub columns: Columns,
    /// The room between two items along its axis.
    pub gap: f32,
    /// The room between two lanes.
    pub cross_gap: f32,
    /// Whether each item packs under the lane that ends soonest rather than
    /// on lines.
    pub masonry: bool,
    /// The lines (a masonry's items a lane) built past either end of the
    /// viewport.
    pub overscan: u32,
    /// An item's extent along the axis before it is measured.
    pub estimated_item: f32,
    /// Its background.
    pub background: BoxStyle,
    /// Whether it shows scroll bars.
    pub bars: ScrollBars,
}

impl Default for LazyGridStyle {
    fn default() -> Self {
        LazyGridStyle {
            axis: Axis::Column,
            size: Sizing::fill(),
            columns: Columns::Adaptive { min: 120.0 },
            gap: 8.0,
            cross_gap: 8.0,
            masonry: false,
            overscan: 2,
            estimated_item: 120.0,
            background: BoxStyle::NONE,
            bars: ScrollBars::Auto,
        }
    }
}

/// A virtualized grid: items in lanes, built only as they near the viewport.
///
/// ```
/// use viso_ui::{
///     BindingTable, BuildCx, Columns, Component, LeafStyle, Length, NodeStore,
///     SemanticProjector, Sizing, StateStore, TextEdits, VirtualLists,
/// };
/// use viso_widgets::{LazyGridStyle, lazy_grid};
///
/// let photos = lazy_grid(LazyGridStyle {
///     columns: Columns::Adaptive { min: 96.0 },
///     ..Default::default()
/// })
/// .items(10_000, |_index, cx| {
///     cx.leaf(LeafStyle {
///         size: Sizing {
///             width: Length::fill(),
///             height: Length::Fixed(96.0),
///         },
///         ..Default::default()
///     });
/// });
///
/// let mut store = NodeStore::new();
/// let mut states = StateStore::new();
/// let mut bindings = BindingTable::new();
/// let mut lists = VirtualLists::new();
/// let mut text_edits = TextEdits::new();
/// let mut projectors = SemanticProjector::new();
/// let mut cx = BuildCx::with_reactive(
///     &mut store,
///     &mut states,
///     &mut bindings,
///     &mut lists,
///     &mut text_edits,
///     &mut projectors,
/// );
/// photos.build(&mut cx);
/// ```
pub struct LazyGrid {
    style: LazyGridStyle,
    count: usize,
    item: RefCell<Option<ItemBuilder>>,
    label: Option<String>,
    headers: RefCell<Option<StickyOf>>,
    full: RefCell<Option<StickyOf>>,
    on_snap: Option<SnapHandler>,
    snap: Option<viso_ui::ScrollSnap>,
}

/// A [`LazyGrid`] laid out as `style` says, with no items yet.
pub fn lazy_grid(style: LazyGridStyle) -> LazyGrid {
    LazyGrid {
        style,
        count: 0,
        item: RefCell::new(None),
        label: None,
        headers: RefCell::new(None),
        full: RefCell::new(None),
        on_snap: None,
        snap: None,
    }
}

impl LazyGrid {
    /// `count` items, `item` building one's body given its index as it nears.
    pub fn items(
        mut self,
        count: usize,
        item: impl FnMut(usize, &mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.count = count;
        self.item = RefCell::new(Some(Box::new(item)));
        self
    }

    /// Give it an accessible name.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Let `is_header` say which items head a section: each spans the line
    /// and stays at the start edge while its section passes under it.
    pub fn section_headers(self, is_header: impl Fn(usize) -> bool + 'static) -> Self {
        *self.headers.borrow_mut() = Some(Box::new(is_header));
        self
    }

    /// Let `is_full` say which items span the whole line: a banner among the
    /// cards.
    pub fn full_line(self, is_full: impl Fn(usize) -> bool + 'static) -> Self {
        *self.full.borrow_mut() = Some(Box::new(is_full));
        self
    }

    /// Come to rest on a line as `snap` says, telling `handler` the first
    /// item of the line it settles on. A masonry has no lines to rest on.
    pub fn on_snap(
        mut self,
        snap: viso_ui::ScrollSnap,
        handler: impl FnMut(&mut EventCx<'_>, Handle, usize) + 'static,
    ) -> Self {
        self.snap = Some(snap);
        self.on_snap = Some(std::rc::Rc::new(RefCell::new(handler)));
        self
    }
}

impl Component for LazyGrid {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let s = self.style;
        let style = VirtualListStyle {
            axis: s.axis,
            across: false,
            size: s.size,
            overscan: s.overscan,
            estimated_row: s.estimated_item,
            style: s.background,
            bars: s.bars,
            tail: false,
            refresh: false,
            snap: self.snap,
            pages: false,
            dots: None,
            grid: Some(ListGrid {
                columns: s.columns,
                gap: s.gap,
                cross_gap: s.cross_gap,
                masonry: s.masonry,
            }),
        };
        let handle = match self.item.borrow_mut().take() {
            Some(mut item) => cx.virtual_list(style, self.count, move |i, cx| item(i, cx)),
            None => cx.virtual_list(style, self.count, |_, _| {}),
        };
        if let Some(is_header) = self.headers.borrow_mut().take() {
            cx.section_headers(handle, is_header);
        }
        if let Some(is_full) = self.full.borrow_mut().take() {
            cx.full_line_items(handle, is_full);
        }
        install_viewport_events(cx, handle, None, self.on_snap.as_ref(), None);
        let mut semantics = Semantics::role(Role::List);
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
        BindingTable, EffectStore, LeafStyle, Length, NodeId, NodeStore, Rect, SemanticProjector,
        StateStore, TextEdits, Vec2, VirtualLists, absorb_measurements, reconcile,
    };

    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        projectors: SemanticProjector,
        effects: EffectStore,
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
                effects: EffectStore::new(),
            }
        }

        fn build(&mut self, widget: &impl Component) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            widget.build(&mut cx);
            cx.root().expect("a root")
        }

        fn frames(&mut self, root: NodeId) {
            let surface = Rect {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 480.0,
            };
            if self.store.bounds(root).w == 0.0 {
                self.store.layout(root, surface, &mut Vec::new());
            }
            for _ in 0..4 {
                reconcile(
                    &mut self.store,
                    &mut self.lists,
                    &mut self.states,
                    &mut self.bindings,
                    &mut self.effects,
                );
                self.store
                    .relayout_dirty(root, surface, &mut Vec::new(), &mut Vec::new());
                absorb_measurements(&self.store, &mut self.lists);
                self.store.clear_dirty();
            }
        }
    }

    fn tile(height: f32) -> impl Fn(&mut BuildCx<'_>) {
        move |cx| {
            cx.leaf(LeafStyle {
                size: Sizing {
                    width: Length::fill(),
                    height: Length::Fixed(height),
                },
                ..Default::default()
            });
        }
    }

    #[test]
    fn an_adaptive_grid_fits_its_lanes_and_mounts_only_about_the_viewport() {
        let mut r = Reactive::new();
        let root = r.build(
            &lazy_grid(LazyGridStyle {
                size: Sizing::fixed(320.0, 480.0),
                columns: Columns::Adaptive { min: 100.0 },
                ..Default::default()
            })
            .items(10_000, |_, cx| tile(100.0)(cx)),
        );
        r.frames(root);
        let state = r.lists.get(root).unwrap();
        // floor((320 + 8) / 108) = 3 lanes.
        assert_eq!(state.lanes(), 3);
        assert_eq!(state.total_extent(), 3334.0 * 108.0 - 8.0);
        // Five lines show, two more past the end.
        assert_eq!(state.mounted_count(), 21);
        r.store.set_scroll(
            root,
            Vec2 {
                x: 0.0,
                y: 100_000.0,
            },
        );
        r.frames(root);
        // From mid-line, six lines show.
        assert_eq!(r.lists.get(root).unwrap().mounted_count(), 30);
    }

    #[test]
    fn a_masonry_packs_cards_of_uneven_length() {
        let mut r = Reactive::new();
        let root = r.build(
            &lazy_grid(LazyGridStyle {
                size: Sizing::fixed(320.0, 480.0),
                columns: Columns::Fixed(2),
                masonry: true,
                ..Default::default()
            })
            .items(100, |i, cx| tile(if i == 0 { 300.0 } else { 100.0 })(cx)),
        );
        r.frames(root);
        let canvas = r.lists.get(root).unwrap().canvas();
        let boxes: Vec<Rect> = r
            .store
            .arena()
            .children(canvas)
            .map(|c| r.store.bounds(c))
            .collect();
        // Card 0 fills lane 0 to 300; cards 1, 2, 3 stack on lane 1 first.
        let at = |x: f32, y: f32| boxes.iter().any(|b| b.x == x && b.y == y);
        assert!(at(0.0, 0.0) && at(164.0, 0.0) && at(164.0, 108.0) && at(164.0, 216.0));
        assert!(at(0.0, 308.0));
    }
}
