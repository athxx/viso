//! Image controls — the [`Image`] widget.
//!
//! `Image` is one leaf node that draws a picture: an asset ([`image_asset`])
//! — embedded with `asset!("…")`, given as bytes or read from a file, in any
//! raster format or SVG, decoded off the frame path — or an already-resident
//! texture ([`image`]). An asset's node shows nothing until it is decoded and
//! resident, then draws at its natural size unless its box is set, fitted
//! into the box by its [`AssetFit`]; a texture draws at once, written
//! directly at build time.
//!
//! An asset image may show a [`placeholder`](Image::placeholder) while it
//! loads and an [`error`](Image::error) view once it fails, layered over it
//! in one stack, and report either with [`on_load`](Image::on_load).
//!
//! Its semantics are [`Role::Image`] named by its [`label`](Image::label), the
//! text alternative; an image without one is decoration, a mute
//! [`Role::Group`].

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Alignment2D, Asset, AssetFit, BoxStyle, BuildCx, Component, Handle, LeafStyle, Length, Notice,
    NoticeKinds, Rect, Rgba, Role, Semantics, Size, StackStyle, StateStore, TextureId, Vec2,
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
/// paint path stretches the texture to fill the node's box; there is no
/// object-fit contain/cover).
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
/// let texture = TextureId::new(0);
/// let thumbnail = image(texture, 64.0, 64.0)
///     .tint(Rgba { r: 1.0, g: 1.0, b: 1.0, a: 0.5 });
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// thumbnail.build(&mut cx);
/// ```
///
/// Invalidation: `Image` declares a content payload that carries its intrinsic
/// size; changing the picture or its parameters is expressed by rebuilding the
/// image.
pub struct Image {
    source: ImageSource,
    style: ImageStyle,
    /// The text alternative; `None` for decoration.
    label: Option<String>,
    /// Shown while an asset loads.
    placeholder: Option<Box<dyn Component>>,
    /// Shown once an asset fails.
    error: Option<Box<dyn Component>>,
    on_load: Option<OnLoad>,
}

/// How an asset image's loading ended.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageLoad {
    /// It drew, at its natural size.
    Loaded { size: Vec2 },
    /// Its bytes could not be read or did not decode.
    Failed,
}

/// An [`Image::on_load`] handler, shared by the builds of the image.
type OnLoad = Rc<RefCell<dyn FnMut(ImageLoad, &mut StateStore)>>;

/// What an [`Image`] draws.
enum ImageSource {
    /// A resident texture and its intrinsic pixel size — the default box and
    /// the value a `Fit` axis measures against.
    Texture { texture: TextureId, natural: Vec2 },
    /// An asset, fitted into the box.
    Asset { asset: Asset, fit: AssetFit },
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
        source: ImageSource::Texture { texture, natural },
        style: ImageStyle {
            uv: FULL_UV,
            tint: NO_TINT,
            size: Size {
                width: Length::Fixed(width),
                height: Length::Fixed(height),
            },
        },
        label: None,
        placeholder: None,
        error: None,
        on_load: None,
    }
}

/// Construct an [`Image`] showing `asset`, from `asset!("…")`, at its natural
/// size, contained in its box. Chain [`Image::fit`], [`Image::tint`],
/// [`Image::size`] and [`Image::label`] to adjust it.
///
/// ```
/// use viso_ui::resource::{AssetKind, ResourceId};
/// use viso_ui::{Asset, AssetBytes, AssetFit, AssetSource, BuildCx, Component, NodeStore};
/// use viso_widgets::image_asset;
///
/// // `asset!("photos/day.png")` embeds a package asset as this value.
/// let day = Asset {
///     id: ResourceId::of("photos/day.png"),
///     kind: AssetKind::Raster,
///     source: AssetSource::Bytes(AssetBytes::Static(b"\x89PNG\r\n\x1a\n")),
/// };
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// image_asset(day).fit(AssetFit::Cover).label("Today's photo").build(&mut cx);
/// ```
pub fn image_asset(asset: Asset) -> Image {
    Image {
        source: ImageSource::Asset {
            asset,
            fit: AssetFit::Contain,
        },
        style: ImageStyle {
            uv: FULL_UV,
            tint: NO_TINT,
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
        },
        label: None,
        placeholder: None,
        error: None,
        on_load: None,
    }
}

impl Image {
    /// Set how an asset fills the box (defaults to [`AssetFit::Contain`]); a
    /// texture always fills it.
    pub fn fit(mut self, fit: AssetFit) -> Self {
        if let ImageSource::Asset { fit: own, .. } = &mut self.source {
            *own = fit;
        }
        self
    }

    /// Set the text alternative, which names the image to assistive
    /// technology; an image without one is decoration.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Show `placeholder` over an asset image's box while it loads — a
    /// skeleton, a spinner, a blurred thumbnail.
    pub fn placeholder(mut self, placeholder: impl Component + 'static) -> Self {
        self.placeholder = Some(Box::new(placeholder));
        self
    }

    /// Show `error` over an asset image's box once its bytes could not be
    /// read or did not decode.
    pub fn error(mut self, error: impl Component + 'static) -> Self {
        self.error = Some(Box::new(error));
        self
    }

    /// Call `handler` once an asset image draws — with its natural size, and
    /// again whenever a new picture of it loads at another — or fails; what
    /// it writes is the next frame's transaction.
    pub fn on_load(mut self, handler: impl FnMut(ImageLoad, &mut StateStore) + 'static) -> Self {
        self.on_load = Some(Rc::new(RefCell::new(handler)));
        self
    }

    /// Set the source sub-rect of a texture in normalized `0..1` UV space
    /// (defaults to the full texture).
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

impl Image {
    /// The leaf drawing the picture, sized `size`: a transparent box (the
    /// picture is the content) whose payload's `natural` drives a `Fit`
    /// axis's measure.
    fn picture(&self, cx: &mut BuildCx<'_>, size: Size) -> Handle {
        let handle = cx.leaf(LeafStyle {
            size,
            style: BoxStyle::NONE,
        });
        match &self.source {
            ImageSource::Texture { texture, natural } => {
                cx.image(handle, *texture, self.style.uv, self.style.tint, *natural);
            }
            ImageSource::Asset { asset, fit } => {
                let tint = (self.style.tint != NO_TINT).then_some(self.style.tint);
                cx.asset(handle, asset, *fit, tint);
                if let Some(on_load) = &self.on_load {
                    let on_load = Rc::clone(on_load);
                    let report = move |notice: &Notice, states: &mut StateStore| {
                        let load = match *notice {
                            Notice::AssetLoaded { size } => ImageLoad::Loaded { size },
                            Notice::AssetFailed => ImageLoad::Failed,
                            _ => return,
                        };
                        (on_load.borrow_mut())(load, states);
                    };
                    cx.watch(handle, NoticeKinds::ASSET, Box::new(report));
                }
            }
        }
        handle
    }
}

impl Component for Image {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let fallbacks = matches!(self.source, ImageSource::Asset { .. })
            && (self.placeholder.is_some() || self.error.is_some());
        let semantics = match &self.label {
            Some(label) => Semantics::role(Role::Image).with_label(label.clone()),
            None => Semantics::role(Role::Group),
        };
        if !fallbacks {
            let handle = self.picture(cx, self.style.size);
            cx.semantics(handle, semantics);
            return;
        }
        // The picture under its fallbacks, one stack sized as the image: an
        // axis that fits its content fits the picture, any other fills it.
        let inner = |l: Length| match l {
            Length::Fit => Length::Fit,
            _ => Size::fill().width,
        };
        let size = Size {
            width: inner(self.style.size.width),
            height: inner(self.style.size.height),
        };
        let (mut picture, mut loading, mut failed) = (None, None, None);
        let layer = StackStyle {
            align: Alignment2D::Center,
            size: Size::fill(),
            ..StackStyle::default()
        };
        let outer = cx.stack(
            StackStyle {
                align: Alignment2D::Center,
                size: self.style.size,
                ..StackStyle::default()
            },
            |cx| {
                picture = Some(self.picture(cx, size));
                for (view, at) in [
                    (&self.placeholder, &mut loading),
                    (&self.error, &mut failed),
                ] {
                    if let Some(view) = view {
                        *at = Some(cx.stack(layer, |cx| view.build(cx)));
                    }
                }
            },
        );
        if let Some(picture) = picture {
            cx.asset_fallbacks(picture, loading, failed);
        }
        cx.semantics(outer, semantics);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{BuildCx, Content, NodeId, NodeStore};

    /// Count a node's direct children by walking the arena sibling chain.
    fn child_count(store: &NodeStore, parent: NodeId) -> usize {
        let arena = store.arena();
        arena.children(parent).count()
    }

    /// An image builds a single leaf carrying the declared image content payload
    /// and a `Group` semantics node. The default style is the full texture, no
    /// tint, and a fixed box at the intrinsic size.
    #[test]
    fn image_builds_one_leaf_with_image_content_and_group_semantics() {
        let texture = TextureId::new(7);
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
        let default_style = image(TextureId::new(0), 32.0, 16.0).style;
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
        let widget = image(TextureId::new(3), 32.0, 16.0)
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

    /// An asset image offers its bytes, shows the resource fitted and tinted at
    /// its natural size, and with a label is an `Image` named by it.
    #[test]
    fn an_asset_image_shows_its_resource_and_is_named_by_its_label() {
        use viso_ui::resource::{AssetKind, ResourceId};
        use viso_ui::{AssetBytes, AssetSource};

        let id = ResourceId::of("icons/star.svg");
        let star = Asset {
            id,
            kind: AssetKind::Vector,
            source: AssetSource::Bytes(AssetBytes::Static(b"<svg/>")),
        };
        let red = Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        };
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        image_asset(star)
            .fit(AssetFit::Cover)
            .tint(red)
            .label("Favorite")
            .build(&mut cx);
        let root = cx.root().expect("image declares a root node");
        match store.content_payload(root) {
            Some(Content::Asset {
                resource,
                fit,
                tint,
                ..
            }) => {
                assert_eq!(*resource, id);
                assert_eq!(*fit, AssetFit::Cover);
                assert_eq!(*tint, Some(red));
            }
            other => panic!("expected Content::Asset, got {other:?}"),
        }
        assert_eq!(
            store.assets().undecoded().count(),
            1,
            "the bytes are offered"
        );
        let sem = store.semantics(root).expect("semantics present");
        assert_eq!(sem.role, Role::Image);
        assert_eq!(sem.label.as_deref(), Some("Favorite"));
    }

    /// An image is static, non-interactive content: it registers no pointer or
    /// key handler, and derives a presentational `Group` role.
    #[test]
    fn image_is_non_interactive() {
        let widget = image(TextureId::new(0), 10.0, 10.0);

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

    /// An asset image with fallbacks is a stack: the picture, the placeholder
    /// shown while it loads, and the error view, hidden until it fails.
    #[test]
    fn an_asset_image_with_fallbacks_layers_them_over_its_picture() {
        use viso_ui::resource::ResourceRevision;
        use viso_ui::{AssetDraw, BuildCx, Content, NodeStore};

        let photo = Asset::bytes(&b"not an image"[..]);
        let id = photo.id;
        let swatch = |r: f32| {
            image(TextureId::new(9), 4.0, 4.0).tint(Rgba {
                r,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            })
        };
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        image_asset(photo)
            .placeholder(swatch(0.5))
            .error(swatch(1.0))
            .label("Photo")
            .build(&mut cx);
        let root = cx.root().expect("a root");
        let layers: Vec<_> = store.arena().children(root).collect();
        assert_eq!(layers.len(), 3);
        assert!(matches!(
            store.content_payload(layers[0]),
            Some(Content::Asset { resource, .. }) if *resource == id
        ));
        assert!(
            !store.hidden(layers[1]),
            "the placeholder shows while it loads"
        );
        assert!(store.hidden(layers[2]));
        assert_eq!(store.semantics(root).expect("semantics").role, Role::Image);

        store.fail_asset(id, ResourceRevision::default());
        assert!(matches!(
            store.content_payload(layers[0]),
            Some(Content::Asset {
                draw: AssetDraw::Failed,
                ..
            })
        ));
        assert!(store.hidden(layers[1]));
        assert!(!store.hidden(layers[2]), "the error shows once it fails");
    }
}
