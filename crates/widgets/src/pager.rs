//! The [`Pager`] and [`Carousel`] widgets — viewports that come to rest on
//! their items.
//!
//! A `Pager` shows one page at a time, each exactly as long as the pager: a
//! flick turns one page, however hard, a slow drag past halfway the next. Its
//! pages are a virtual list's rows, so only the pages about the one shown are
//! ever built, and page dots under them show which is shown and take taps,
//! arrow keys and an assistive technology's steps. A `Carousel` lays a row of
//! items of their own size out with a gap, and comes to rest with an item at
//! its start (or middle, or end), its padding kept clear: a shelf of cards.
//! Both tell a handler the item they settle on.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Axis, BoxStyle, BuildCx, Component, EventCx, FlexStyle, Handle, Inset, Length, PageDotsStyle,
    Role, ScrollBars, ScrollSnap, ScrollStyle, Semantics, Sizing, SnapAlign, VirtualListStyle,
};

use crate::scroll::{SnapHandler, install_viewport_events};

/// A [`Pager`]'s page builder: a page's body, given its index.
type PageBuilder = Box<dyn FnMut(usize, &mut BuildCx<'_>)>;

/// How a [`Pager`] looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PagerStyle {
    /// The axis its pages turn along.
    pub axis: Axis,
    /// Its size within its parent; each page is as large.
    pub size: Sizing,
    /// Its background.
    pub background: BoxStyle,
    /// The page dots under its pages, or none.
    pub dots: Option<PageDotsStyle>,
}

impl Default for PagerStyle {
    fn default() -> Self {
        PagerStyle {
            axis: Axis::Row,
            size: Sizing::fill(),
            background: BoxStyle::NONE,
            dots: Some(PageDotsStyle::default()),
        }
    }
}

/// A viewport of pages, one shown at a time, built only as they near.
///
/// ```
/// use viso_ui::{
///     BindingTable, BuildCx, Component, LeafStyle, NodeStore, SemanticProjector, Sizing,
///     StateStore, TextEdits, VirtualLists,
/// };
/// use viso_widgets::{PagerStyle, pager};
///
/// let onboarding = pager(PagerStyle {
///     size: Sizing::fixed(320.0, 480.0),
///     ..Default::default()
/// })
/// .pages(3, |_page, cx| {
///     cx.leaf(LeafStyle {
///         size: Sizing::fill(),
///         ..Default::default()
///     });
/// })
/// .on_page(|_ev, _pager, _page| {});
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
/// onboarding.build(&mut cx);
/// ```
pub struct Pager {
    style: PagerStyle,
    count: usize,
    page: RefCell<Option<PageBuilder>>,
    label: Option<String>,
    on_page: Option<SnapHandler>,
}

/// A [`Pager`] looking as `style` says, with no pages yet.
pub fn pager(style: PagerStyle) -> Pager {
    Pager {
        style,
        count: 0,
        page: RefCell::new(None),
        label: None,
        on_page: None,
    }
}

impl Pager {
    /// `count` pages, `page` building one's body given its index as it nears.
    pub fn pages(
        mut self,
        count: usize,
        page: impl FnMut(usize, &mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.count = count;
        self.page = RefCell::new(Some(Box::new(page)));
        self
    }

    /// Give its pages an accessible name.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Hear the page it settles on: `handler` runs with the pages' viewport
    /// and the page's index, which
    /// [`snap_to_item`](EventCx::snap_to_item) on the viewport turns to.
    pub fn on_page(
        mut self,
        handler: impl FnMut(&mut EventCx<'_>, Handle, usize) + 'static,
    ) -> Self {
        self.on_page = Some(Rc::new(RefCell::new(handler)));
        self
    }
}

impl Component for Pager {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let page = self.page.borrow_mut().take();
        let style = VirtualListStyle {
            axis: self.style.axis,
            across: false,
            size: self.style.size,
            overscan: 1,
            estimated_row: 0.0,
            style: self.style.background,
            bars: ScrollBars::Hidden,
            tail: false,
            refresh: false,
            snap: Some(ScrollSnap::PAGES),
            pages: true,
            dots: self.style.dots,
            grid: None,
        };
        let pages = match page {
            Some(mut page) => cx.virtual_list(style, self.count, move |i, cx| page(i, cx)),
            None => cx.virtual_list(style, self.count, |_, _| {}),
        };
        install_viewport_events(cx, pages, None, self.on_page.as_ref(), None);
        let mut semantics = Semantics::role(Role::Group);
        if let Some(label) = &self.label {
            semantics = semantics.with_label(label.clone());
        }
        cx.semantics(pages, semantics);
    }
}

/// A [`Carousel`]'s content: its items, built in a row.
type ItemsBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// How a [`Carousel`] lays its items out and rests on them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CarouselStyle {
    /// Its size within its parent.
    pub size: Sizing,
    /// The room between two items.
    pub gap: f32,
    /// The room before the first item and after the last, kept clear of the
    /// item it rests on.
    pub padding: f32,
    /// Where the item it rests on sits.
    pub align: SnapAlign,
    /// Whether a flick moves one item at most.
    pub paging: bool,
    /// Its background.
    pub background: BoxStyle,
}

impl Default for CarouselStyle {
    fn default() -> Self {
        CarouselStyle {
            size: Sizing {
                width: Length::fill(),
                height: Length::Fit,
            },
            gap: 12.0,
            padding: 16.0,
            align: SnapAlign::Start,
            paging: false,
            background: BoxStyle::NONE,
        }
    }
}

/// A row of items that comes to rest on one: a shelf of cards.
pub struct Carousel {
    style: CarouselStyle,
    items: Option<ItemsBuilder>,
    label: Option<String>,
    on_snap: Option<SnapHandler>,
}

/// A [`Carousel`] laid out as `style` says, with no items yet.
pub fn carousel(style: CarouselStyle) -> Carousel {
    Carousel {
        style,
        items: None,
        label: None,
        on_snap: None,
    }
}

impl Carousel {
    /// Its items: `items` builds them, each a child of its row.
    pub fn items(mut self, items: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.items = Some(Box::new(items));
        self
    }

    /// Give it an accessible name.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Hear the item it settles on, with its viewport and the item's index.
    pub fn on_snap(
        mut self,
        handler: impl FnMut(&mut EventCx<'_>, Handle, usize) + 'static,
    ) -> Self {
        self.on_snap = Some(Rc::new(RefCell::new(handler)));
        self
    }
}

impl Component for Carousel {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let s = self.style;
        let handle = cx.scroll(
            ScrollStyle {
                axis: Axis::Row,
                across: false,
                size: s.size,
                style: s.background,
                bars: ScrollBars::Hidden,
                tail: false,
                refresh: false,
                snap: Some(ScrollSnap {
                    align: s.align,
                    paging: s.paging,
                    proximity: false,
                    inset: s.padding,
                }),
                dots: None,
            },
            |cx| {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: s.gap,
                        padding: Inset {
                            left: s.padding,
                            top: 0.0,
                            right: s.padding,
                            bottom: 0.0,
                        },
                        ..FlexStyle::default()
                    },
                    |cx| {
                        if let Some(items) = &self.items {
                            items(cx);
                        }
                    },
                );
            },
        );
        install_viewport_events(cx, handle, None, self.on_snap.as_ref(), None);
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
        BindingTable, EffectStore, LeafStyle, NodeId, NodeStore, Rect, SemanticProjector,
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

        fn frame(&mut self, root: NodeId) {
            let surface = Rect {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 480.0,
            };
            if self.store.bounds(root).w == 0.0 {
                self.store.layout(root, surface, &mut Vec::new());
            }
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

    #[test]
    fn a_pager_s_dots_count_its_pages_and_follow_the_one_shown() {
        let mut r = Reactive::new();
        let root = r.build(
            &pager(PagerStyle {
                size: Sizing::fixed(320.0, 480.0),
                ..Default::default()
            })
            .pages(4, |_, cx| {
                cx.leaf(LeafStyle {
                    size: Sizing::fill(),
                    ..Default::default()
                });
            }),
        );
        for _ in 0..3 {
            r.frame(root);
        }
        let pages = root;
        let layer = r.store.arena().children(pages).last().unwrap();
        let dots = r.store.arena().children(layer).next().unwrap();
        assert_eq!(r.store.scroll_snap(pages), Some(ScrollSnap::PAGES));
        let shown = r.store.page_dots(dots).unwrap();
        assert_eq!((shown.count, shown.current), (4, 0));
        let b = r.store.bounds(dots);
        assert_eq!(b.y + b.h, 480.0, "under the pages");
        assert!((b.x + b.w * 0.5 - 160.0).abs() < 0.01, "centered");
        r.store.set_scroll(pages, Vec2 { x: 640.0, y: 0.0 });
        r.frame(root);
        assert_eq!(r.store.page_dots(dots).unwrap().current, 2);
    }

    #[test]
    fn a_carousel_rests_an_item_past_its_padding() {
        let mut r = Reactive::new();
        let root = r.build(
            &carousel(CarouselStyle {
                size: Sizing::fixed(320.0, 100.0),
                ..Default::default()
            })
            .items(|cx| {
                for _ in 0..6 {
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(200.0, 100.0),
                        ..Default::default()
                    });
                }
            }),
        );
        r.frame(root);
        r.store.set_scroll(root, Vec2 { x: 190.0, y: 0.0 });
        r.frame(root);
        r.store.fling(root, 0.0);
        while r.store.is_scroll_moving() {
            r.store
                .tick_scroll_motion(std::time::Duration::from_millis(16));
            r.frame(root);
        }
        // Item 1 starts at 16 + 212 = 228; it rests 16 in: at 212.
        assert_eq!(r.store.scroll(root).x, 212.0);
        assert_eq!(r.store.snap_index(root), Some(1));
    }
}
