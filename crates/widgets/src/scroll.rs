//! The [`Scroll`] widget — a scroll viewport.
//!
//! `Scroll` is the minimal scrollable region: a clip box that lays its content
//! out at the content's natural extent along a single axis, so any overflow
//! becomes scrollable, and clips to its own visible box. It is the widget-layer
//! face of the `viso-ui` Scroll layout node.
//!
//! It differs from [`View`](crate::View) on purpose. `View` is a layout box: a
//! flex container with a background, alignment, and gap, which can *optionally*
//! scroll. `Scroll` carries none of that flex layout mental model — it expresses
//! a single intent, "this region of content scrolls," and maps to exactly one
//! `viso-ui` Scroll node. Reach for `Scroll` when you have a block of content
//! that should scroll; reach for `View` when you are laying children out in a
//! row or column that also happens to overflow.
//!
//! Its content is declared by a builder closure that runs against the same
//! [`BuildCx`], so the scrollable content attaches beneath the viewport with no
//! intermediate allocation. A scroll viewport lays out a single content child at
//! its natural extent, so author one content child (typically a [`View`] column)
//! to hold everything that scrolls. The default accessible role is
//! [`Role::Group`] — a structural grouping with no interaction of its own; the
//! wheel/drag scroll gesture is routed by `viso-ui`'s `ScrollRouter`, not by a
//! widget-level handler.

use viso_ui::{Axis, BoxStyle, BuildCx, Component, Role, ScrollStyle, Semantics, Size};

/// The visual and layout parameters of a [`Scroll`] viewport: the axis it
/// scrolls along, its own visible size within its parent, and an optional
/// background/border for the clip box itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollViewStyle {
    /// The axis the viewport scrolls along. Its single content child is laid out
    /// at its natural extent along this axis, so overflow past the viewport's
    /// visible size becomes scrollable.
    pub axis: Axis,
    /// The viewport's own visible size within its parent (the clip box). Content
    /// larger than this along `axis` scrolls; content is clipped to this box.
    pub size: Size,
    /// The viewport's own background/border/radius. [`BoxStyle::NONE`] is a pure,
    /// transparent clip box.
    pub background: BoxStyle,
}

impl Default for ScrollViewStyle {
    fn default() -> Self {
        ScrollViewStyle {
            axis: Axis::Column,
            size: Size::fill(),
            background: BoxStyle::NONE,
        }
    }
}

/// A [`Scroll`]'s content-declaration closure. Boxed `Fn` (not `FnOnce`) because
/// [`Component::build`] takes `&self`: the widget is built by reference, so its
/// content builder is invoked through a shared borrow. Cold — run once at build
/// time, never on the per-frame hot path. Parallels `viso_ui`'s boxed handler
/// aliases (a cold fat pointer kept off the hot node columns).
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// A scroll viewport: a clip box that scrolls its content along one axis.
///
/// Construct one with [`scroll`] and attach content with [`Scroll::content`]:
///
/// ```
/// use viso_widgets::{scroll, ScrollViewStyle, view, ViewStyle};
/// use viso_ui::{Axis, BuildCx, Component, NodeStore, Size};
///
/// let region = scroll(ScrollViewStyle {
///     axis: Axis::Column,
///     size: Size::fixed(200.0, 300.0),
///     ..Default::default()
/// })
/// .content(|cx| {
///     // one content child holds everything that scrolls
///     view(ViewStyle::default()).children(|_cx| {}).build(cx);
/// });
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// region.build(&mut cx);
/// ```
///
/// Invalidation: a scroll gesture moves the viewport's offset and dirties only
/// transform/hit-test/paint (never layout), so scrolling re-derives world rects
/// and repaints without a relayout — that targeted invalidation lives in the
/// `viso-ui` Scroll node and `ScrollRouter`, not here. Semantics default to
/// [`Role::Group`].
pub struct Scroll {
    style: ScrollViewStyle,
    /// The content-declaration closure, if any. See [`ContentBuilder`].
    content: Option<ContentBuilder>,
    /// An optional accessible label. `None` keeps the plain [`Role::Group`]
    /// viewport with no name.
    label: Option<String>,
}

/// Construct a [`Scroll`] viewport with the given style and no content yet.
/// Chain [`Scroll::content`] to author its scrollable content and
/// [`Scroll::label`] to give it an accessible name.
pub fn scroll(style: ScrollViewStyle) -> Scroll {
    Scroll {
        style,
        content: None,
        label: None,
    }
}

impl Default for Scroll {
    fn default() -> Self {
        scroll(ScrollViewStyle::default())
    }
}

impl Scroll {
    /// Attach the content-declaration closure. The closure runs with the viewport
    /// as the active parent, so the scrollable content attaches beneath it. A
    /// scroll viewport lays out a single content child at its natural extent, so
    /// author one content child to hold everything that scrolls.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Give the viewport an accessible label, surfaced on its [`Role::Group`]
    /// semantics node.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Component for Scroll {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // `scroll` takes `FnOnce(&mut BuildCx)`. `self.content` is a shared `Fn`
        // (see the field note); wrap it so the borrow is invoked through the
        // closure. `|_cx| {}` when there is no content keeps the viewport node
        // without adding an empty child.
        let handle = cx.scroll(
            ScrollStyle {
                axis: self.style.axis,
                size: self.style.size,
                style: self.style.background,
            },
            |cx| {
                if let Some(content) = &self.content {
                    content(cx);
                }
            },
        );

        // A scroll viewport is a `Group` to an assistive technology; carry the
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
    use viso_ui::{LeafStyle, Length, NodeId, NodeStore};

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

    /// A `Scroll` maps to a single Scroll viewport node, and its content closure
    /// runs beneath it.
    #[test]
    fn scroll_builds_a_scroll_node_with_content() {
        let region = scroll(ScrollViewStyle {
            axis: Axis::Column,
            size: Size::fixed(100.0, 100.0),
            ..Default::default()
        })
        .content(|cx| {
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
        region.build(&mut cx);
        let root = cx.root().expect("scroll declares a root node");

        assert!(
            store.is_scroll(root),
            "a Scroll widget maps to a scroll viewport node"
        );
        assert_eq!(
            child_count(&store, root),
            1,
            "the content closure's single child attaches beneath the viewport"
        );
    }

    /// The viewport carries the requested background as its own box style, and a
    /// `Scroll` with no content is still a valid empty viewport node.
    #[test]
    fn scroll_carries_background_and_tolerates_empty_content() {
        let bg = BoxStyle::solid(viso_ui::Rgba {
            r: 0.15,
            g: 0.15,
            b: 0.18,
            a: 1.0,
        });
        let region = scroll(ScrollViewStyle {
            background: bg,
            ..Default::default()
        });

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        region.build(&mut cx);
        let root = cx.root().expect("scroll declares a root node");

        assert!(store.is_scroll(root));
        assert_eq!(store.style(root), bg);
        assert_eq!(
            child_count(&store, root),
            0,
            "a Scroll with no content is an empty viewport"
        );
    }

    /// The default accessible role of a viewport is `Group`, and an authored
    /// label surfaces on that node. A `Scroll` declares no interactive handlers
    /// of its own — the scroll gesture is routed by `viso-ui`, not a widget
    /// handler — so it stays a structural `Group`.
    #[test]
    fn scroll_default_role_is_group_and_carries_label() {
        let region = scroll(ScrollViewStyle::default()).label("Log");

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        region.build(&mut cx);
        let root = cx.root().expect("scroll declares a root node");

        let sem = store
            .semantics(root)
            .expect("scroll authors semantics on its node");
        assert_eq!(sem.role, Role::Group);
        assert_eq!(sem.label.as_deref(), Some("Log"));
        assert!(
            !store.has_handler(root) && !store.has_key_handler(root),
            "a Scroll declares no interactive handlers of its own"
        );
    }
}
