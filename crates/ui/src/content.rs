//! Node content payload: the measured intrinsic size plus the paint data for a
//! node that draws text, an image, a vector path, an asset or a brush program — everything the paint step
//! needs beyond a node's background quad.
//!
//! `viso-ui` cannot shape text or decode images: the architecture DAG allows it
//! to depend on `viso-render` (for the primitive data types) but not on
//! `viso-text`, so it never owns a font stack. Content is therefore produced by
//! an upper tier that owns the font resolver, shaper, and residency pools: that
//! tier shapes/measures and hands a finished [`Content`] to the node store,
//! which only *stores* it and *lowers* it to primitives at paint time. Storing
//! the already-measured [`natural`](Content::natural) size next to the paint
//! payload means the measure pass reads a cached intrinsic size (no reshape per
//! frame) and paint emits the primitive with no conversion.
//!
//! A [`Content`] lives in a cold, mostly-`None` side column
//! ([`crate::component::NodeStore`]), boxed off the hot traversal columns — only
//! content-bearing nodes pay for it, and only the paint/measure passes read it.
//!
//! Coordinates in the payload are **local** to the node's own origin: glyph and
//! path positions are relative to `(0, 0)` at the node's top-left. The paint
//! step translates them by the node's resolved `world` origin, so a scrolled or
//! repositioned node draws its content in the right place without the producer
//! re-emitting it.

use std::sync::Arc;

use crate::asset::{AssetDraw, AssetFit};
use crate::layout::Vec2;
use crate::text_style::{FontSize, TextSpan, TextStyle, WidthUse};
use viso_render::asset::ResourceId;
use viso_render::{
    GlyphInstanceData, PathCmd, ProgramSlot, Rect, Rgba, SceneSlot, Stroke, TextureId,
};

/// A node's drawable content plus its measured intrinsic size.
///
/// Each variant carries the `viso-render` data the paint step lowers to a
/// [`viso_render::Primitive`], together with `natural` — the content's intrinsic
/// `(width, height)` in physical pixels, which the measure pass reads so a
/// [`crate::layout::Length::Fit`] axis sizes to the content.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    /// A shaped run of glyphs. `glyphs` are positioned in the node's local space
    /// (origin at the node's top-left); paint shifts them to the node's world
    /// origin. `atlas` is the exact-coverage pool they sample, `color` the run color.
    ///
    /// Color-bitmap glyphs (emoji) do not decode through the coverage ramp, so
    /// they ride in a parallel `color_glyphs` run sampling their own RGBA
    /// `color_atlas`; paint lowers each to an image quad rather than a glyph
    /// quad. A pure-text run leaves `color_glyphs` empty and `color_atlas`
    /// `None`, and pays nothing for the color path.
    Text {
        /// The positioned outline glyphs, one screen quad each, in node-local
        /// space. Their A8 texels are sampled as exact coverage from `atlas`.
        glyphs: Vec<GlyphInstanceData>,
        /// The single-channel A8 coverage pool the outline glyphs sample.
        atlas: TextureId,
        /// The positioned color-bitmap glyphs (emoji), in node-local space,
        /// sampling `color_atlas` as premultiplied RGBA. Empty for pure text.
        color_glyphs: Vec<GlyphInstanceData>,
        /// The RGBA color atlas the `color_glyphs` sample, or `None` when the
        /// run has no color glyphs.
        color_atlas: Option<TextureId>,
        /// Straight linear RGBA color applied to the whole outline run (a =
        /// opacity). Color glyphs sample their own RGBA and ignore this.
        color: Rgba,
        /// The run's intrinsic size in physical pixels.
        natural: Vec2,
        /// The first-line baseline: the distance in physical pixels from the top
        /// of the run down to the baseline of its first line (one ascender below
        /// the top). Layout containers that align on the text baseline (grid
        /// `AlignItems::Baseline`) read this; it is `0` for an empty run.
        baseline: f32,
        /// The `max_width` (physical px) this run was shaped/wrapped at, or `None`
        /// when shaped unconstrained (single-line natural). The layout pass
        /// compares a wrap-eligible leaf's assigned box width against this to
        /// decide whether the run must reflow to a narrower width (DL1
        /// constraint downflow); `viso-ui` records the mismatch as data and the
        /// facade — which owns the font stack — does the reshape.
        shaped_at_width: Option<f32>,
        /// How the run depends on its box's width, from its request's style
        /// at shape time: the layout pass reflows it to a new box width only
        /// when it wraps, ends in an ellipsis or aligns there. Carried on the
        /// payload (rather than read back from the request, which is drained
        /// at shape time) so the recorder needs only the content column.
        width_use: WidthUse,
        /// The narrowest width the run lays out in without overflowing, in
        /// physical pixels: its widest stretch no line breaks inside, what a
        /// `min_content` width sizes to and a `fit` one shrinks no further than.
        min_content: f32,
        /// The run's width laid out on one line per hard break, in physical
        /// pixels: what a `fit` width sizes to, so a run its box's bounds
        /// wrap or cut sizes the same as it is laid out again.
        max_content: f32,
        /// The colors, fills and lines of a text with styled spans; `None`
        /// for a text drawn all in `color`.
        ink: Option<Box<TextInk>>,
    },
    /// A textured image drawn into the node's box. `uv` selects the source
    /// sub-rect in normalized texture coordinates, `tint` modulates it.
    Image {
        /// The texture to sample (already resident; `viso-ui` does not decode).
        texture: TextureId,
        /// Source sub-rect in normalized texture coordinates (`0..1`).
        uv: Rect,
        /// Straight linear RGBA tint (a = opacity); white/1.0 = unmodified.
        tint: Rgba,
        /// The image's intrinsic size in physical pixels.
        natural: Vec2,
    },
    /// A filled and/or stroked vector path (icons, decorations). `cmds` are in
    /// the node's local space; paint shifts them to the node's world origin.
    Path {
        /// The outline commands, in node-local space.
        cmds: Vec<PathCmd>,
        /// Fill color, if the interior is painted.
        fill: Option<Rgba>,
        /// Stroke, if the outline is painted (drawn over the fill).
        stroke: Option<Stroke>,
        /// The path's intrinsic size in physical pixels.
        natural: Vec2,
    },
    /// An asset of the store's [`AssetTable`](crate::asset::AssetTable): what it
    /// draws now and its natural size, kept here so paint reads no table, and
    /// rewritten when the resource takes a new payload.
    Asset {
        resource: ResourceId,
        draw: AssetDraw,
        /// The asset's natural size in pixels.
        natural: Vec2,
        fit: AssetFit,
        /// Multiplies the asset's colors; `None` draws them as they are.
        tint: Option<Rgba>,
    },
    /// The node's box painted by a brush program: the program in the store's
    /// [`ShaderSlots`](crate::shader::ShaderSlots) slot `program`, its
    /// parameters encoded as one of the program's instances, and the images
    /// its texture parameters name, in their order. A program has no
    /// intrinsic size, so a `Fit` axis sizes to zero.
    Shader {
        program: ProgramSlot,
        params: Arc<[u8]>,
        images: Box<[BrushImage]>,
    },
    /// A 3D scene: the frame the store's
    /// [`SceneSlots`](crate::scenes::SceneSlots) slot `slot` holds, at its
    /// `revision`, rendered at the node's box and composited like an image.
    /// A scene has no intrinsic size, so a `Fit` axis sizes to zero.
    Scene { slot: SceneSlot, revision: u64 },
}

/// An image a brush node samples: the resource its texture parameter names,
/// and the texture holding it once the resource is resident. Until each of a
/// node's images is, the node draws nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrushImage {
    pub resource: ResourceId,
    pub texture: Option<TextureId>,
}

impl Content {
    /// The content's intrinsic `(width, height)` in physical pixels — what the
    /// measure pass resolves a `Fit`/`Fill` axis against.
    #[inline]
    /// A text's width is its `max_content`,
    /// its height that of the lines it lays out.
    pub fn natural(&self) -> Vec2 {
        match self {
            Content::Text {
                natural,
                max_content,
                ..
            } => Vec2 {
                x: *max_content,
                y: natural.y,
            },
            Content::Image { natural, .. }
            | Content::Path { natural, .. }
            | Content::Asset { natural, .. } => *natural,
            Content::Shader { .. } | Content::Scene { .. } => Vec2::ZERO,
        }
    }

    /// The narrowest width the content lays out in without overflowing: a
    /// text's `min_content`, an image's, path's or asset's natural width.
    #[inline]
    pub fn min_width(&self) -> f32 {
        match self {
            Content::Text { min_content, .. } => *min_content,
            Content::Image { natural, .. }
            | Content::Path { natural, .. }
            | Content::Asset { natural, .. } => natural.x,
            Content::Shader { .. } | Content::Scene { .. } => 0.0,
        }
    }

    /// The content's first-line baseline in physical pixels, or `None` when it
    /// has no text baseline (image/path). A grid cell aligned on the baseline
    /// falls back to the child's full extent when this is `None`.
    #[inline]
    pub fn baseline(&self) -> Option<f32> {
        match self {
            Content::Text { baseline, .. } => Some(*baseline),
            Content::Image { .. }
            | Content::Path { .. }
            | Content::Asset { .. }
            | Content::Shader { .. }
            | Content::Scene { .. } => None,
        }
    }
}

/// An unshaped request to render text on a node, the input counterpart to a
/// shaped [`Content::Text`]. `viso-ui` cannot shape text (it holds no font
/// stack — see the module docs), so a node declares *what* it wants drawn and an
/// upper tier that owns text preparation shapes the request into a [`Content`]
/// and writes it back with [`crate::component::NodeStore::set_content_payload`].
///
/// This lives in its own cold, mostly-`None` side column next to
/// [`Content`]: it is the single source of truth for a text node's declared
/// content, keyed by the node it describes, so a later reactive rebuild just
/// re-sets the request and re-shapes — no facade-side bookkeeping to keep in
/// sync with the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct TextRequest {
    /// The text to shape and draw.
    pub text: String,
    /// The size it draws at (scaled by DPI at shape time).
    pub font_size: FontSize,
    /// Straight linear RGBA run color (a = opacity).
    pub color: Rgba,
    /// Whether the run may soft-wrap to a width-constrained box. When `false`
    /// (the default) a width-constrained (`Fill`/`Fixed`) leaf stays single-line
    /// and clips/overflows its box; when `true` the shaper wraps it to the box
    /// width the layout pass assigns. A `Fit`-width leaf ignores this — it sizes
    /// to its content and is never width-constrained, so it never wraps.
    pub soft_wrap: bool,
    /// The content language as a BCP-47 tag (`"ja"`, `"zh-Hant"`, `"ko"`), or
    /// `None` for the process locale. It selects locale line breaking and
    /// locale-specific fallback faces; it never changes the text itself.
    pub locale: Option<String>,
    /// Its face, line height, line limit and alignment.
    pub style: TextStyle,
    /// The stretches of it drawn their own way, in order; empty for a text
    /// drawn all alike.
    pub spans: Vec<TextSpan>,
}

/// How a text of styled spans is painted beyond its glyphs: the colors runs
/// of them draw in, and the fills and lines its spans add.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextInk {
    /// Where the outline glyphs change color, in order: each run covers the
    /// glyphs up to its `end`, the glyphs past the last run the text's color.
    pub runs: Vec<InkRun>,
    /// Backgrounds, drawn under the glyphs, in node-local space.
    pub fills: Vec<TextMark>,
    /// Underlines and strikethroughs, drawn over them.
    pub lines: Vec<TextMark>,
}

/// A run of a text's outline glyphs in one color.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkRun {
    /// The index past its last glyph.
    pub end: u32,
    /// Its color; the text's own when `None`.
    pub color: Option<Rgba>,
}

/// A fill or a line a span adds to its text, in node-local space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextMark {
    pub rect: Rect,
    /// Its color; the text's own when `None`.
    pub color: Option<Rgba>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_reads_each_variant() {
        let text = Content::Text {
            ink: None,
            glyphs: Vec::new(),
            atlas: TextureId::new(1),
            color_glyphs: Vec::new(),
            color_atlas: None,
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            natural: Vec2 { x: 42.0, y: 12.0 },
            baseline: 9.0,
            shaped_at_width: None,
            width_use: WidthUse::Ignored,
            min_content: 42.0,
            max_content: 42.0,
        };
        assert_eq!(text.natural(), Vec2 { x: 42.0, y: 12.0 });
        assert_eq!(text.baseline(), Some(9.0));

        let image = Content::Image {
            texture: TextureId::new(2),
            uv: Rect {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 1.0,
            },
            tint: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 1.0,
            },
            natural: Vec2 { x: 64.0, y: 64.0 },
        };
        assert_eq!(image.natural(), Vec2 { x: 64.0, y: 64.0 });
        assert_eq!(image.baseline(), None, "an image has no text baseline");

        let path = Content::Path {
            cmds: Vec::new(),
            fill: None,
            stroke: None,
            natural: Vec2 { x: 16.0, y: 16.0 },
        };
        assert_eq!(path.natural(), Vec2 { x: 16.0, y: 16.0 });
    }
}
