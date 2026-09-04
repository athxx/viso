//! Layout containers — the [`View`] widget.
//!
//! `View` is the base container: a box with a background, flex layout for its
//! children, and an optional scroll viewport. It is the widget-layer face of the
//! `viso-ui` Flex/Scroll layout nodes, giving app authors one small mental model
//! ("a container is a box with a background, a layout, and maybe scrolling")
//! instead of two separate primitives to choose between.
//!
//! It maps to exactly one `viso-ui` node: a Flex node when it does not scroll, a
//! Scroll node when it does. Its children are declared by a builder closure that
//! runs against the same [`BuildCx`], so nested `View`s and other widgets attach
//! beneath it with no intermediate allocation. The default accessible role is
//! [`Role::Group`] — a structural grouping with no interaction — matching what a
//! plain container is to an assistive technology.

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, FlexStyle, Inset, Role, ScrollStyle, Semantics, Size,
};

/// The visual and layout parameters of a [`View`]. A small, flat description of
/// "a box with a background, a layout, and maybe scrolling": the fields common
/// to the underlying Flex/Scroll nodes, plus an optional [`scroll_axis`] that
/// promotes the container from a plain layout box to a scroll viewport.
///
/// [`scroll_axis`]: ViewStyle::scroll_axis
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewStyle {
    /// Main axis children stack along.
    pub axis: Axis,
    /// Gap between adjacent children along the main axis.
    pub gap: f32,
    /// Inner padding on all four edges.
    pub padding: Inset,
    /// Cross-axis alignment of children.
    pub align: Align,
    /// The container's own size request within its parent.
    pub size: Size,
    /// The container's background/border/radius. [`BoxStyle::NONE`] is a pure,
    /// transparent layout box.
    pub background: BoxStyle,
    /// When `Some`, the container is a scroll viewport clipping and scrolling its
    /// content along the given axis; when `None`, it is a plain flex box. A
    /// scroll viewport lays out a single content child at its natural extent, so
    /// author one container child to hold the scrollable content.
    pub scroll_axis: Option<Axis>,
}

impl Default for ViewStyle {
    fn default() -> Self {
        ViewStyle {
            axis: Axis::Column,
            gap: 0.0,
            padding: Inset::default(),
            align: Align::Start,
            size: Size::fill(),
            background: BoxStyle::NONE,
            scroll_axis: None,
        }
    }
}

/// A [`View`]'s child-declaration closure. Boxed `Fn` (not `FnOnce`) because
/// [`Component::build`] takes `&self`: the widget is built by reference, so its
/// child builder is invoked through a shared borrow. Cold — run once at build
/// time, never on the per-frame hot path. Parallels `viso_ui`'s boxed handler
/// aliases (a cold fat pointer kept off the hot node columns).
type ChildBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// A layout container: a box with a background, a flex layout for its children,
/// and an optional scroll viewport.
///
/// Construct one with [`view`] and attach children with [`View::children`]:
///
/// ```
/// use viso_widgets::{view, ViewStyle};
/// use viso_ui::{Axis, BuildCx, Component, NodeStore};
///
/// let container = view(ViewStyle {
///     axis: Axis::Row,
///     gap: 8.0,
///     ..Default::default()
/// })
/// .children(|cx| {
///     // author child widgets here — they attach beneath the container
///     cx.leaf(Default::default());
/// });
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// container.build(&mut cx);
/// ```
///
/// Invalidation: `View` declares only structure/layout/paint through the layout
/// node it maps to; it holds no reactive state of its own this slice, so a
/// change to the container is expressed by rebuilding it (see the crate-level
/// note on the rebuild path for content). Semantics default to [`Role::Group`].
pub struct View {
    style: ViewStyle,
    /// The child-declaration closure, if any. See [`ChildBuilder`].
    children: Option<ChildBuilder>,
    /// An optional accessible label. `None` keeps the plain [`Role::Group`]
    /// container with no name.
    label: Option<String>,
}

/// Construct a [`View`] with the given style and no children yet. Chain
/// [`View::children`] to author its contents and [`View::label`] to give it an
/// accessible name.
pub fn view(style: ViewStyle) -> View {
    View {
        style,
        children: None,
        label: None,
    }
}

impl Default for View {
    fn default() -> Self {
        view(ViewStyle::default())
    }
}

impl View {
    /// Attach a child-declaration closure. The closure runs with this container
    /// as the active parent, so nested widgets and layout nodes authored inside
    /// it attach beneath the container.
    pub fn children(mut self, children: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.children = Some(Box::new(children));
        self
    }

    /// Give the container an accessible label, surfaced on its [`Role::Group`]
    /// semantics node.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Component for View {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // `flex`/`scroll` take `FnOnce(&mut BuildCx)`. `self.children` is a
        // shared `Fn` (see the field note); wrap it so the borrow is invoked
        // through the closure. `|_cx| {}` when there are no children keeps the
        // container's own node without adding an empty child.
        let handle = match self.style.scroll_axis {
            None => {
                let flex_style = FlexStyle {
                    axis: self.style.axis,
                    gap: self.style.gap,
                    padding: self.style.padding,
                    align: self.style.align,
                    size: self.style.size,
                    style: self.style.background,
                };
                cx.flex(flex_style, |cx| {
                    if let Some(children) = &self.children {
                        children(cx);
                    }
                })
            }
            Some(scroll_axis) => {
                let scroll_style = ScrollStyle {
                    axis: scroll_axis,
                    size: self.style.size,
                    style: self.style.background,
                };
                cx.scroll(scroll_style, |cx| {
                    if let Some(children) = &self.children {
                        children(cx);
                    }
                })
            }
        };

        // A plain container is a `Group` to an assistive technology; carry the
        // label when one was authored.
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
    use viso_ui::{LeafStyle, Length, NodeId, NodeStore, Rgba};

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

    /// A non-scrolling `View` maps to a single Flex node carrying the requested
    /// background, and its child closure runs beneath it.
    #[test]
    fn flex_view_builds_a_flex_node_with_background_and_children() {
        let bg = BoxStyle::solid(Rgba {
            r: 0.2,
            g: 0.2,
            b: 0.2,
            a: 1.0,
        });
        let container = view(ViewStyle {
            axis: Axis::Row,
            gap: 8.0,
            background: bg,
            ..Default::default()
        })
        .children(|cx| {
            cx.leaf(LeafStyle::default());
            cx.leaf(LeafStyle::default());
        });

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        container.build(&mut cx);
        let root = cx.root().expect("view declares a root node");

        // Root is the container; it has the two authored children.
        assert_eq!(
            child_count(&store, root),
            2,
            "the child closure's two leaves attach beneath the container"
        );
        // The container carries the requested background as its box style.
        assert_eq!(store.style(root), bg);
    }

    /// A `View` with `scroll_axis` maps to a Scroll node instead of a Flex node,
    /// so its content clips and scrolls.
    #[test]
    fn scroll_view_builds_a_scroll_node() {
        let container = view(ViewStyle {
            scroll_axis: Some(Axis::Column),
            size: Size {
                width: Length::Fixed(100.0),
                height: Length::Fixed(100.0),
            },
            ..Default::default()
        })
        .children(|cx| {
            cx.leaf(LeafStyle {
                size: Size {
                    width: Length::Fixed(100.0),
                    height: Length::Fixed(1000.0),
                },
                style: BoxStyle::NONE,
            });
        });

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        container.build(&mut cx);
        let root = cx.root().expect("view declares a root node");

        assert!(
            store.is_scroll(root),
            "a view with a scroll_axis maps to a scroll viewport node"
        );
    }

    /// The default accessible role of a container is `Group`, and an authored
    /// label surfaces on that node.
    #[test]
    fn view_default_role_is_group_and_carries_label() {
        let container = view(ViewStyle::default()).label("Sidebar");

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        container.build(&mut cx);
        let root = cx.root().expect("view declares a root node");

        let sem = store
            .semantics(root)
            .expect("view authors semantics on its node");
        assert_eq!(sem.role, Role::Group);
        assert_eq!(sem.label.as_deref(), Some("Sidebar"));
    }
}
