//! Image controls — the [`Image`] widget.
//!
//! `Image` is the base texture control: a single leaf node that draws an
//! already-resident texture. It is the widget-layer face of the `viso-ui` image
//! seam — it declares a [`viso_ui::Content::Image`] payload on its node via
//! [`BuildCx::image`], which needs no shaping step. Unlike [`crate::Label`],
//! which leaves shaping to a font-stack-owning tier, an image's pixels are
//! already on the GPU as a [`TextureId`]; there is nothing to prepare, so the
//! content is written directly at build time.
//!
//! It maps to exactly one `viso-ui` node: a leaf with a transparent box (the
//! texture is the content), an image content payload, and a [`Role::Group`]
//! semantics node (a non-interactive presentational element; a dedicated
//! `Role::Image` variant is a later slice). Its size defaults to
//! [`Length::Fixed`] at the texture's intrinsic pixel size, and its intrinsic
//! size drives a [`Length::Fit`] axis's measure.

use viso_ui::{
    BoxStyle, BuildCx, Component, LeafStyle, Length, Rect, Rgba, Role, Semantics, Size, TextureId,
    Vec2,
};

/// A white (identity) tint: the texture is drawn unmodulated.
const NO_TINT: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// The full-texture source rect in normalized `0..1` UV space.
const FULL_UV: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 1.0,
    h: 1.0,
};

/// The visual and layout parameters of an [`Image`]: which sub-rect of the
/// source texture to sample (`uv`), the color it is modulated by (`tint`), and
/// the leaf's own size request within its parent (`size`).
///
/// `size` defaults to [`Length::Fixed`] at the texture's intrinsic pixel size,
/// so an image occupies its natural dimensions. Override it with a
/// [`Length::Fit`] or [`Length::Fill`] axis to shrink-to-content or flex (the
/// paint path stretches the texture to fill the node's box — object-fit
/// contain/cover is a later slice).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageStyle {
    /// Source sub-rect in normalized `0..1` UV space. Defaults to the full
    /// texture (`0,0..1,1`).
    pub uv: Rect,
    /// Straight linear RGBA tint the texture is modulated by. Defaults to white
    /// (drawn unmodulated).
    pub tint: Rgba,
    /// The image's own size request within its parent. Defaults to `Fixed` at
    /// the texture's intrinsic pixel size.
    pub size: Size,
}

/// A texture-backed image control: one leaf node that draws a resident texture.
///
/// Construct one with [`image`] and adjust it with the chainable setters:
///
/// ```
/// use viso_widgets::image;
/// use viso_ui::{BuildCx, Component, NodeStore, Rgba, TextureId};
///
/// // A `TextureId` normally comes from the GPU backend after uploading pixels.
/// let texture = TextureId(0);
/// let thumbnail = image(texture, 64.0, 64.0)
///     .tint(Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.5 });
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// thumbnail.build(&mut cx);
/// ```
///
/// Invalidation: `Image` declares an image content payload that carries the
/// texture's intrinsic size; changing the texture or its parameters is expressed
/// by rebuilding the image. Semantics are [`Role::Group`] (a presentational,
/// non-interactive element).
pub struct Image {
    texture: TextureId,
    /// The texture's intrinsic pixel size — the default box and the value a
    /// `Fit` axis measures against.
    natural: Vec2,
    style: ImageStyle,
}

/// Construct an [`Image`] drawing `texture` at its intrinsic pixel size
/// `width` x `height`. The size doubles as the default fixed box and the
/// intrinsic size a `Fit` axis measures against. Chain [`Image::uv`],
/// [`Image::tint`], and [`Image::size`] to adjust it.
pub fn image(texture: TextureId, width: f32, height: f32) -> Image {
    let natural = Vec2 {
        x: width,
        y: height,
    };
    Image {
        texture,
        natural,
        style: ImageStyle {
            uv: FULL_UV,
            tint: NO_TINT,
            size: Size {
                width: Length::Fixed(width),
                height: Length::Fixed(height),
            },
        },
    }
}

impl Image {
    /// Set the source sub-rect in normalized `0..1` UV space (defaults to the
    /// full texture).
    pub fn uv(mut self, uv: Rect) -> Self {
        self.style.uv = uv;
        self
    }

    /// Set the tint the texture is modulated by (defaults to white — drawn
    /// unmodulated).
    pub fn tint(mut self, tint: Rgba) -> Self {
        self.style.tint = tint;
        self
    }

    /// Set the image's own size request within its parent (defaults to `Fixed`
    /// at the intrinsic pixel size).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }
}

impl Component for Image {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One leaf: a transparent box (the texture is the content), carrying the
        // image content payload directly — no shaping step, since the texture is
        // already resident. The payload's `natural` drives a `Fit` axis's
        // measure. `TextureId`/`Rect`/`Rgba`/`Vec2` are `Copy`, so this is a
        // cheap, allocation-free declaration.
        let handle = cx.leaf(LeafStyle {
            size: self.style.size,
            style: BoxStyle::NONE,
        });
        cx.image(
            handle,
            self.texture,
            self.style.uv,
            self.style.tint,
            self.natural,
        );
        cx.semantics(handle, Semantics::role(Role::Group));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{BuildCx, Content, NodeId, NodeStore};

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

    /// An image builds a single leaf carrying the declared image content payload
    /// and a `Group` semantics node. The default style is the full texture, no
    /// tint, and a fixed box at the intrinsic size.
    #[test]
    fn image_builds_one_leaf_with_image_content_and_group_semantics() {
        let texture = TextureId(7);
        let widget = image(texture, 64.0, 48.0);

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("image declares a root node");

        // An image is a single content leaf — no children.
        assert_eq!(child_count(&store, root), 0, "an image maps to one leaf");

        let content = store
            .content_payload(root)
            .expect("image declares content on its leaf");
        match content {
            Content::Image {
                texture: t,
                uv,
                tint,
                natural,
            } => {
                assert_eq!(*t, texture);
                assert_eq!(*uv, FULL_UV);
                assert_eq!(*tint, NO_TINT);
                assert_eq!(*natural, Vec2 { x: 64.0, y: 48.0 });
            }
            other => panic!("expected Content::Image, got {other:?}"),
        }

        let sem = store
            .semantics(root)
            .expect("image authors semantics on its leaf");
        assert_eq!(sem.role, Role::Group);
    }

    /// The chainable setters override uv, tint, and size, and the default size is
    /// `Fixed` at the intrinsic pixel dimensions.
    #[test]
    fn setters_override_style_default_size_is_fixed_natural() {
        let default_style = image(TextureId(0), 32.0, 16.0).style;
        assert_eq!(default_style.size.width, Length::Fixed(32.0));
        assert_eq!(default_style.size.height, Length::Fixed(16.0));
        assert_eq!(default_style.uv, FULL_UV);
        assert_eq!(default_style.tint, NO_TINT);

        let uv = Rect {
            x: 0.25,
            y: 0.25,
            w: 0.5,
            h: 0.5,
        };
        let tint = Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let widget = image(TextureId(3), 32.0, 16.0)
            .uv(uv)
            .tint(tint)
            .size(Size::fill());
        assert_eq!(widget.style.uv, uv);
        assert_eq!(widget.style.tint, tint);
        assert_eq!(widget.style.size, Size::fill());

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("image declares a root node");

        let content = store.content_payload(root).expect("content present");
        match content {
            Content::Image { uv: u, tint: t, .. } => {
                assert_eq!(*u, uv);
                assert_eq!(*t, tint);
            }
            other => panic!("expected Content::Image, got {other:?}"),
        }
    }

    /// An image is static, non-interactive content: it registers no pointer or
    /// key handler, and derives a presentational `Group` role.
    #[test]
    fn image_is_non_interactive() {
        let widget = image(TextureId(0), 10.0, 10.0);

        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        widget.build(&mut cx);
        let root = cx.root().expect("image declares a root node");

        assert!(
            !store.has_handler(root),
            "an image attaches no pointer handler"
        );
        assert!(
            !store.has_key_handler(root),
            "an image attaches no key handler"
        );
        let sem = store.semantics(root).expect("semantics present");
        assert_eq!(sem.role, Role::Group, "a presentational image is a Group");
    }
}
