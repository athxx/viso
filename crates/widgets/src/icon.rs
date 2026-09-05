//! Icon controls — the [`Icon`] widget.
//!
//! `Icon` is the base vector control: a single leaf node that draws an outline
//! given as [`PathCmd`] geometry. It is the widget-layer face of the `viso-ui`
//! path seam — it declares a [`viso_ui::Content::Path`] payload on its node via
//! [`BuildCx::path`], which needs no shaping or decode step. Like [`crate::Image`]
//! (and unlike [`crate::Label`], which leaves shaping to a font-stack-owning
//! tier), an icon's content is ready at author time; the geometry is written
//! directly at build time.
//!
//! It maps to exactly one `viso-ui` node: a leaf with a transparent box (the
//! path is the content), a path content payload, and a [`Role::Group`] semantics
//! node (a non-interactive presentational element; a dedicated `Role::Icon`
//! variant is a later slice). Its size defaults to [`Length::Fixed`] at the
//! icon's intrinsic size, and its intrinsic size drives a [`Length::Fit`] axis's
//! measure. By default the outline is filled with a near-black foreground and not
//! stroked.

use viso_ui::{
    BoxStyle, BuildCx, Component, LeafStyle, Length, PathCmd, Rgba, Role, Semantics, Size, Stroke,
    Vec2,
};

/// The default icon foreground: opaque near-black. Icons paint as a solid fill by
/// default, matching the typical monochrome glyph-like icon.
const FOREGROUND: Rgba = Rgba {
    r: 0.1,
    g: 0.1,
    b: 0.1,
    a: 1.0,
};

/// The visual and layout parameters of an [`Icon`]: the interior fill color
/// (`fill`), an optional outline (`stroke`), and the leaf's own size request
/// within its parent (`size`).
///
/// `fill` defaults to a near-black foreground and `stroke` to `None`, so an icon
/// paints as a solid monochrome shape. `size` defaults to [`Length::Fixed`] at
/// the icon's intrinsic size; override it with a [`Length::Fit`] or
/// [`Length::Fill`] axis to shrink-to-content or flex (the geometry is drawn at
/// its authored coordinates — object-fit scaling is a later slice).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconStyle {
    /// Interior fill color. Defaults to a near-black foreground; set to `None`
    /// for an outline-only icon.
    pub fill: Option<Rgba>,
    /// Optional outline stroke. Defaults to `None` (filled, not stroked).
    pub stroke: Option<Stroke>,
    /// The icon's own size request within its parent. Defaults to `Fixed` at the
    /// intrinsic size.
    pub size: Size,
}

/// A vector icon control: one leaf node that draws a [`PathCmd`] outline.
///
/// Construct one with [`icon`] and adjust it with the chainable setters:
///
/// ```
/// use viso_widgets::icon;
/// use viso_ui::{BuildCx, Component, NodeStore, PathCmd, Point, Rgba};
///
/// // A simple filled triangle in the icon's 24x24 local space.
/// let cmds = vec![
///     PathCmd::MoveTo(Point::new(12.0, 2.0)),
///     PathCmd::LineTo(Point::new(22.0, 22.0)),
///     PathCmd::LineTo(Point::new(2.0, 22.0)),
///     PathCmd::Close,
/// ];
/// let play = icon(cmds, 24.0, 24.0).fill(Rgba { r: 0.0, g: 0.5, b: 1.0, a: 1.0 });
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// play.build(&mut cx);
/// ```
///
/// Invalidation: `Icon` declares a path content payload that carries the icon's
/// intrinsic size; changing the geometry or its parameters is expressed by
/// rebuilding the icon. Semantics are [`Role::Group`] (a presentational,
/// non-interactive element).
pub struct Icon {
    /// The outline commands, in the icon's local space (paint shifts them to the
    /// node's world origin).
    cmds: Vec<PathCmd>,
    /// The icon's intrinsic size — the default box and the value a `Fit` axis
    /// measures against.
    natural: Vec2,
    style: IconStyle,
}

/// Construct an [`Icon`] drawing `cmds` at its intrinsic size `width` x `height`.
/// The size doubles as the default fixed box and the intrinsic size a `Fit` axis
/// measures against. Chain [`Icon::fill`], [`Icon::stroke`], and [`Icon::size`]
/// to adjust it.
pub fn icon(cmds: impl Into<Vec<PathCmd>>, width: f32, height: f32) -> Icon {
    let natural = Vec2 {
        x: width,
        y: height,
    };
    Icon {
        cmds: cmds.into(),
        natural,
        style: IconStyle {
            fill: Some(FOREGROUND),
            stroke: None,
            size: Size {
                width: Length::Fixed(width),
                height: Length::Fixed(height),
            },
        },
    }
}

impl Icon {
    /// Set the interior fill color (defaults to a near-black foreground).
    pub fn fill(mut self, fill: Rgba) -> Self {
        self.style.fill = Some(fill);
        self
    }

    /// Set the outline stroke (defaults to `None` — filled, not stroked).
    pub fn stroke(mut self, stroke: Stroke) -> Self {
        self.style.stroke = Some(stroke);
        self
    }

    /// Set the icon's own size request within its parent (defaults to `Fixed` at
    /// the intrinsic size).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }
}

impl Component for Icon {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One leaf: a transparent box (the path is the content), carrying the
        // path content payload directly — no shaping or decode step. The
        // payload's `natural` drives a `Fit` axis's measure. `build` takes
        // `&self`, so the geometry is cloned here (a cold, build-time cost, not a
        // steady-state frame path); `fill`/`stroke`/`natural` are `Copy`.
        let handle = cx.leaf(LeafStyle {
            size: self.style.size,
            style: BoxStyle::NONE,
        });
        cx.path(
            handle,
            self.cmds.clone(),
            self.style.fill,
            self.style.stroke,
            self.natural,
        );
        cx.semantics(handle, Semantics::role(Role::Group));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{BuildCx, Content, LineJoin, NodeId, NodeStore, Point};

    /// A deterministic square outline in a 20x20 local space, used across tests.
    fn square() -> Vec<PathCmd> {
        vec![
            PathCmd::MoveTo(Point::new(0.0, 0.0)),
            PathCmd::LineTo(Point::new(20.0, 0.0)),
            PathCmd::LineTo(Point::new(20.0, 20.0)),
            PathCmd::LineTo(Point::new(0.0, 20.0)),
            PathCmd::Close,
        ]
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

    /// An icon builds a single leaf carrying the declared path content payload
    /// and a `Group` semantics node. The default style is a foreground fill, no
    /// stroke, and a fixed box at the intrinsic size.
    #[test]
    fn icon_builds_one_leaf_with_path_content_and_group_semantics() {
        let cmds = square();
        let widget = icon(cmds.clone(), 20.0, 20.0);

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("icon declares a root node");

        // An icon is a single content leaf — no children.
        assert_eq!(child_count(&store, root), 0, "an icon maps to one leaf");

        let content = store
            .content_payload(root)
            .expect("icon declares content on its leaf");
        match content {
            Content::Path {
                cmds: c,
                fill,
                stroke,
                natural,
            } => {
                assert_eq!(*c, cmds);
                assert_eq!(*fill, Some(FOREGROUND));
                assert_eq!(*stroke, None);
                assert_eq!(*natural, Vec2 { x: 20.0, y: 20.0 });
            }
            other => panic!("expected Content::Path, got {other:?}"),
        }

        let sem = store
            .semantics(root)
            .expect("icon authors semantics on its leaf");
        assert_eq!(sem.role, Role::Group);
    }

    /// The chainable setters override fill, stroke, and size, and the default size
    /// is `Fixed` at the intrinsic dimensions.
    #[test]
    fn setters_override_style_default_size_is_fixed_natural() {
        let default_style = icon(square(), 32.0, 16.0).style;
        assert_eq!(default_style.size.width, Length::Fixed(32.0));
        assert_eq!(default_style.size.height, Length::Fixed(16.0));
        assert_eq!(default_style.fill, Some(FOREGROUND));
        assert_eq!(default_style.stroke, None);

        let fill = Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let stroke = Stroke {
            width: 2.0,
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            join: LineJoin::Miter,
        };
        let widget = icon(square(), 32.0, 16.0)
            .fill(fill)
            .stroke(stroke)
            .size(Size::fill());
        assert_eq!(widget.style.fill, Some(fill));
        assert_eq!(widget.style.stroke, Some(stroke));
        assert_eq!(widget.style.size, Size::fill());

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("icon declares a root node");

        let content = store.content_payload(root).expect("content present");
        match content {
            Content::Path {
                fill: f, stroke: s, ..
            } => {
                assert_eq!(*f, Some(fill));
                assert_eq!(*s, Some(stroke));
            }
            other => panic!("expected Content::Path, got {other:?}"),
        }
    }

    /// An icon is static, non-interactive content: it registers no pointer or key
    /// handler, and derives a presentational `Group` role.
    #[test]
    fn icon_is_non_interactive() {
        let widget = icon(square(), 10.0, 10.0);

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("icon declares a root node");

        assert!(
            !store.has_handler(root),
            "an icon attaches no pointer handler"
        );
        assert!(
            !store.has_key_handler(root),
            "an icon attaches no key handler"
        );
        let sem = store.semantics(root).expect("semantics present");
        assert_eq!(sem.role, Role::Group, "a presentational icon is a Group");
    }
}
