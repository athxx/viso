//! Node content payload: the measured intrinsic size plus the paint data for a
//! node that draws text, an image, or a vector path — everything the paint step
//! needs beyond a node's background quad.
//!
//! `viso-ui` cannot shape text or decode images: the architecture DAG allows it
//! to depend on `viso-render` (for the primitive data types) but not on
//! `viso-text`, so it never owns a font stack. Content is therefore produced by
//! an upper tier that does hold a `TextSystem` (the facade/widget layer): that
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

use crate::layout::Vec2;
use viso_render::{GlyphInstanceData, PathCmd, Rect, Rgba, Stroke, TextureId};

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
    /// origin. `atlas` is the SDF atlas they sample, `color` the run color.
    Text {
        /// The positioned glyphs, one screen quad each, in node-local space.
        glyphs: Vec<GlyphInstanceData>,
        /// The single-channel R8 SDF atlas the glyphs sample.
        atlas: TextureId,
        /// Straight linear RGBA color applied to the whole run (a = opacity).
        color: Rgba,
        /// The run's intrinsic size in physical pixels.
        natural: Vec2,
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
}

impl Content {
    /// The content's intrinsic `(width, height)` in physical pixels — what the
    /// measure pass resolves a `Fit`/`Fill` axis against.
    #[inline]
    pub fn natural(&self) -> Vec2 {
        match self {
            Content::Text { natural, .. }
            | Content::Image { natural, .. }
            | Content::Path { natural, .. } => *natural,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_reads_each_variant() {
        let text = Content::Text {
            glyphs: Vec::new(),
            atlas: TextureId(1),
            color: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 1.0,
            },
            natural: Vec2 { x: 42.0, y: 12.0 },
        };
        assert_eq!(text.natural(), Vec2 { x: 42.0, y: 12.0 });

        let image = Content::Image {
            texture: TextureId(2),
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

        let path = Content::Path {
            cmds: Vec::new(),
            fill: None,
            stroke: None,
            natural: Vec2 { x: 16.0, y: 16.0 },
        };
        assert_eq!(path.natural(), Vec2 { x: 16.0, y: 16.0 });
    }
}
