//! Text shadows: the soft copies of a node's glyphs its text casts.
//!
//! An outer shadow is the glyphs moved by the offset in the shadow's color,
//! blurred, under the text. An inset one falls inside the glyphs: the glyphs
//! in the shadow's color with the moved, blurred glyphs cut out of them,
//! over the text — an isolated layer the cut cannot reach past. A text casts
//! any number, painted in order. The table is cold: one entry for each node
//! with any, kept in slot order so paint finds a node's in a binary search
//! and a tree with none checks an empty table.

use crate::component::NodeStore;
use crate::content::Content;
use crate::layout::Vec2;
use crate::node::NodeId;
use viso_render::{
    Blend, GlyphInstanceData, GlyphRunDraw, ImageDraw, LayerClip, Primitive, Rect, Rgba,
    SamplerDesc,
};

/// A soft copy of a text's glyphs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextShadow {
    pub color: Rgba,
    /// How far it falls from the glyphs.
    pub offset: Vec2,
    /// How far its edge fades, as CSS's blur radius: twice the Gaussian's
    /// standard deviation.
    pub blur: f32,
    /// Whether it falls inside the glyphs rather than under them.
    pub inset: bool,
}

/// The nodes whose text casts shadows, in slot order.
#[derive(Debug, Default)]
pub(crate) struct TextShadows {
    entries: Vec<(u32, Box<[TextShadow]>)>,
}

impl TextShadows {
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if let Ok(at) = self.find(index) {
            self.entries.remove(at);
        }
    }

    fn find(&self, index: u32) -> Result<usize, usize> {
        self.entries.binary_search_by_key(&index, |e| e.0)
    }

    #[inline]
    pub(crate) fn get(&self, index: u32) -> &[TextShadow] {
        if self.entries.is_empty() {
            return &[];
        }
        self.find(index).map_or(&[], |at| &self.entries[at].1)
    }
}

impl NodeStore {
    /// The shadows node `id`'s text casts, in paint order.
    pub fn text_shadows(&self, id: NodeId) -> &[TextShadow] {
        self.text_shadows.get(id.index())
    }

    /// Makes node `id`'s text cast `shadows`, painted in order: the outer ones
    /// under it, the inset ones over it. A change repaints it. A live-guarded
    /// write — a stale handle is a no-op.
    pub fn set_text_shadows(&mut self, id: NodeId, shadows: &[TextShadow]) {
        if !self.arena().is_live(id) || self.text_shadows(id) == shadows {
            return;
        }
        let index = id.index();
        let found = self.text_shadows.find(index);
        let table = &mut self.text_shadows.entries;
        match (found, shadows.is_empty()) {
            (Ok(at), false) => table[at].1 = shadows.into(),
            (Ok(at), true) => {
                table.remove(at);
            }
            (Err(at), false) => table.insert(at, (index, shadows.into())),
            (Err(_), true) => {}
        }
        self.mark_dirty(id, crate::dirty::DirtyClass::PAINT);
    }
}

/// The color a cut is drawn in: only its coverage counts.
const OPAQUE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// Below this standard deviation a blur is not seen: the shadow is a sharp
/// copy, drawn without a layer.
const SHARP_SIGMA: f32 = 0.5;

/// The `shadows` of `content` (a text) at `world` that fall on the side
/// `inset` says, in order: the outer ones before the text paints, the inset
/// ones after.
#[cold]
pub(crate) fn paint_text_shadows(
    content: &Content,
    world: Rect,
    shadows: &[TextShadow],
    inset: bool,
    out: &mut Vec<Primitive>,
) {
    let Content::Text {
        glyphs,
        atlas,
        lane,
        color_glyphs,
        color_atlas,
        ..
    } = content
    else {
        return;
    };
    let run = |offset: Vec2, color: Rgba| {
        Primitive::GlyphRun(GlyphRunDraw {
            glyphs: placed(glyphs, world, offset),
            atlas: *atlas,
            color,
            lane: *lane,
        })
    };
    for shadow in shadows.iter().filter(|s| s.inset == inset) {
        let sigma = (shadow.blur * 0.5).max(0.0);
        let reach = 3.0 * sigma + shadow.offset.x.abs().max(shadow.offset.y.abs()) + 1.0;
        let clip = Rect::new(
            world.x - reach,
            world.y - reach,
            world.w + 2.0 * reach,
            world.h + 2.0 * reach,
        );
        let blurred = sigma >= SHARP_SIGMA;
        if !shadow.inset {
            if blurred {
                out.push(layer(clip, sigma));
            }
            out.push(run(shadow.offset, shadow.color));
            // A color glyph's shadow is its own silhouette in the color.
            if let Some(color_atlas) = color_atlas {
                for g in color_glyphs {
                    let rect = shifted(g.rect, world, shadow.offset);
                    out.push(Primitive::Image(ImageDraw {
                        rect,
                        uv: g.uv,
                        tint: shadow.color,
                        texture: *color_atlas,
                        sampler: SamplerDesc::LINEAR_CLAMP,
                        mask: None,
                    }));
                }
            }
            if blurred {
                out.push(Primitive::LayerEnd);
            }
            continue;
        }
        // The glyphs in the shadow's color, the moved glyphs' blur cut out.
        out.push(layer(clip, 0.0));
        out.push(Primitive::Isolate);
        out.push(run(Vec2::new(0.0, 0.0), shadow.color));
        out.push(layer(clip, sigma));
        out.push(Primitive::Blend(Blend::DstOut));
        out.push(run(shadow.offset, OPAQUE));
        out.push(Primitive::LayerEnd);
        out.push(Primitive::LayerEnd);
    }
}

fn layer(clip: Rect, blur_sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma,
        backdrop_sigma: 0.0,
    })
}

fn shifted(rect: Rect, world: Rect, offset: Vec2) -> Rect {
    Rect::new(
        rect.x + world.x + offset.x,
        rect.y + world.y + offset.y,
        rect.w,
        rect.h,
    )
}

fn placed(glyphs: &[GlyphInstanceData], world: Rect, offset: Vec2) -> Vec<GlyphInstanceData> {
    glyphs
        .iter()
        .map(|g| GlyphInstanceData {
            rect: shifted(g.rect, world, offset),
            ..*g
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle};
    use crate::layout::Sizing;
    use crate::style::BoxStyle;
    use viso_render::TextureId;

    const INK: Rgba = Rgba {
        r: 0.1,
        g: 0.1,
        b: 0.1,
        a: 1.0,
    };
    const SHADE: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.5,
    };

    fn painted(shadows: &[TextShadow]) -> (Vec<Primitive>, Rect) {
        let mut store = NodeStore::new();
        let label = {
            let mut cx = BuildCx::new(&mut store);
            cx.leaf(LeafStyle {
                size: Sizing::fixed(80.0, 20.0),
                style: BoxStyle::NONE,
            })
            .id()
        };
        store.set_content_payload(
            label,
            Content::Text {
                ink: None,
                glyphs: vec![GlyphInstanceData {
                    rect: Rect::new(2.0, 3.0, 8.0, 10.0),
                    uv: Rect::new(0.0, 0.0, 1.0, 1.0),
                }],
                atlas: TextureId::new(7),
                lane: viso_render::GlyphLane::CoverageA8,
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: INK,
                natural: Vec2 { x: 80.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                width_use: crate::WidthUse::Ignored,
                min_content: 80.0,
                max_content: 80.0,
            },
        );
        store.set_text_shadows(label, shadows);
        assert_eq!(store.text_shadows(label), shadows);
        store.layout(label, Rect::new(10.0, 20.0, 80.0, 20.0), &mut Vec::new());
        let mut out = Vec::new();
        crate::paint::paint_tree(&store, label, &mut out);
        (out, store.world(label))
    }

    fn runs(out: &[Primitive]) -> Vec<(Rgba, f32)> {
        out.iter()
            .filter_map(|p| match p {
                Primitive::GlyphRun(r) => Some((r.color, r.glyphs[0].rect.x)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_sharp_outer_shadow_is_a_moved_copy_under_the_text() {
        let shadow = TextShadow {
            color: SHADE,
            offset: Vec2::new(1.0, 2.0),
            blur: 0.0,
            inset: false,
        };
        let (out, world) = painted(&[shadow]);
        assert!(
            !out.iter().any(|p| matches!(p, Primitive::Layer(_))),
            "{out:?}"
        );
        assert_eq!(runs(&out), [(SHADE, world.x + 3.0), (INK, world.x + 2.0)]);
    }

    #[test]
    fn a_soft_shadow_blurs_in_a_layer_and_an_inset_one_cuts_inside_the_glyphs() {
        let outer = TextShadow {
            color: SHADE,
            offset: Vec2::new(0.0, 2.0),
            blur: 4.0,
            inset: false,
        };
        let inner = TextShadow {
            inset: true,
            ..outer
        };
        let (out, world) = painted(&[outer, inner]);
        let layers: Vec<(f32, f32)> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Layer(l) => Some((l.blur_sigma, l.opacity)),
                _ => None,
            })
            .collect();
        // The outer blur, then the inset's isolated group and its blurred cut.
        assert_eq!(layers, [(2.0, 1.0), (0.0, 1.0), (2.0, 1.0)]);
        assert_eq!(
            runs(&out),
            [
                (SHADE, world.x + 2.0),
                (INK, world.x + 2.0),
                (SHADE, world.x + 2.0),
                (OPAQUE, world.x + 2.0),
            ]
        );
        let at = out.iter().position(|p| matches!(p, Primitive::Isolate));
        assert!(at.is_some_and(|at| matches!(out[at - 1], Primitive::Layer(_))));
        assert!(
            out.iter()
                .any(|p| matches!(p, Primitive::Blend(Blend::DstOut)))
        );
    }
}
