//! Facade-owned text shaping: turns a node's unshaped `TextRequest` into a
//! shaped `Content::Text` payload the UI store can measure and paint.
//!
//! `viso-ui` cannot shape text — the architecture DAG forbids a `viso-ui →
//! viso-text` edge, so the UI holds no font stack. The facade legally owns one
//! (`viso-text` is an allowed facade dependency) and does the shaping here, then
//! hands the finished [`viso_ui::Content`] back to the node store. This is the
//! single seam where a font stack meets the retained tree.
//!
//! The glyph SDF atlas is a persistent GPU texture: created once on the first
//! shape and grown incrementally by re-uploading only the region
//! [`TextSystem::take_atlas_dirty`] reports after each shape. New glyphs pack
//! into the same atlas across the whole session, so steady-state text (a fixed
//! label set) uploads the atlas once and never again.

use viso_gpu::{GpuBackend, TextureDesc, TextureFormat};
use viso_render::{GlyphInstanceData, Rect, TextureId};
use viso_text::{FontId, TextSystem};
use viso_ui::{Content, TextRequest, Vec2};

/// The embedded UI font: the same DejaVu Sans subset the renderer's test scene
/// uses, kept in-tree so text renders deterministically with no system-font
/// dependency. A real font stack (system fonts, fallback chains) lands later;
/// this is the single default face for the first widget slice.
const UI_FONT: &[u8] = include_bytes!("../fixtures/DejaVuSans-subset.ttf");

/// Owns the facade's font stack and glyph atlas, shaping [`TextRequest`]s into
/// [`Content::Text`] payloads. One per app; created on launch.
pub(crate) struct TextShaper {
    text: TextSystem,
    font: FontId,
    /// The persistent glyph atlas texture, created lazily on the first shape
    /// (once a backend exists to allocate it). `None` until then.
    atlas: Option<TextureId>,
}

impl TextShaper {
    /// Build the shaper, loading the embedded UI font. Panics if the embedded
    /// font fails to parse — an in-tree asset, so a parse failure is a build
    /// bug, not a runtime condition.
    pub(crate) fn new() -> Self {
        let mut text = TextSystem::new();
        let font = text.load_font(UI_FONT, 0).expect("embedded UI font parses");
        Self {
            text,
            font,
            atlas: None,
        }
    }

    /// Shape one request into a [`Content::Text`], uploading any newly-packed
    /// glyphs to the atlas texture via `backend`. `dpi_factor` is the surface's
    /// device-pixel density (glyphs rasterize at that density).
    ///
    /// Glyph positions are node-local (origin at `(0, 0)`); the paint step
    /// shifts them to the node's world origin. `natural` is the run's bounding
    /// extent, which the measure pass reads for a `Fit` axis.
    pub(crate) fn shape<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        request: &TextRequest,
        dpi_factor: f32,
    ) -> Content {
        let quads = self
            .text
            .prepare(self.font, &request.text, request.font_size, dpi_factor);

        // Ensure the atlas texture exists, then upload whatever region this
        // shape newly rasterized. The atlas is single-channel R8 SDF coverage.
        let size = self.text.atlas_size();
        let atlas = *self.atlas.get_or_insert_with(|| {
            backend.create_texture(&TextureDesc {
                width: size,
                height: size,
                format: TextureFormat::R8Unorm,
                render_target: false,
                label: "ui-glyph-atlas",
            })
        });
        if let Some(d) = self.text.take_atlas_dirty() {
            // Upload the dirty rows at full width: the atlas is stored row-major
            // with stride `size`, so uploading whole rows (x = 0, width = size)
            // for the dirty band `[d.y, d.y + d.h)` matches the texture's stride
            // and stays bounded to the rows this shape actually touched.
            let row = size as usize;
            let pixels = self.text.atlas_pixels();
            let band = &pixels[d.y as usize * row..(d.y + d.h) as usize * row];
            backend.write_texture(atlas, 0, d.y, size, d.h, band);
        }

        // Map each quad to a node-local glyph instance and track the run extent.
        let mut natural = Vec2::ZERO;
        let glyphs: Vec<GlyphInstanceData> = quads
            .iter()
            .map(|q| {
                let rect = Rect {
                    x: q.rect_px[0],
                    y: q.rect_px[1],
                    w: q.rect_px[2],
                    h: q.rect_px[3],
                };
                natural.x = natural.x.max(rect.x + rect.w);
                natural.y = natural.y.max(rect.y + rect.h);
                GlyphInstanceData {
                    rect,
                    uv: Rect {
                        x: q.uv[0],
                        y: q.uv[1],
                        w: q.uv[2] - q.uv[0],
                        h: q.uv[3] - q.uv[1],
                    },
                    px_range: q.px_range,
                }
            })
            .collect();

        Content::Text {
            glyphs,
            atlas,
            color: request.color,
            natural,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_gpu::{HeadlessRaster, RawWindowHandle};
    use viso_render::Rgba;

    const WHITE: Rgba = Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    #[test]
    fn shapes_request_into_text_content_with_natural_extent() {
        let mut gpu = HeadlessRaster::new();
        // A surface exists in the real flow; the shaper only needs the backend
        // for texture allocation, so create one to keep the backend consistent.
        let _ = gpu.create_surface(RawWindowHandle::Headless, 64, 64);

        let mut shaper = TextShaper::new();
        let content = shaper.shape(
            &mut gpu,
            &TextRequest {
                text: "Viso".to_string(),
                font_size: 22.0,
                color: WHITE,
            },
            1.0,
        );

        match content {
            Content::Text {
                glyphs, natural, ..
            } => {
                assert!(!glyphs.is_empty(), "a visible run shapes some glyphs");
                assert!(
                    natural.x > 0.0 && natural.y > 0.0,
                    "the run has a positive natural extent, got {natural:?}"
                );
            }
            _ => panic!("a text request shapes into Content::Text"),
        }
    }

    #[test]
    fn reuses_one_atlas_across_shapes() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 64, 64);
        let mut shaper = TextShaper::new();

        let a = shaper.shape(
            &mut gpu,
            &TextRequest {
                text: "Vi".to_string(),
                font_size: 18.0,
                color: WHITE,
            },
            1.0,
        );
        let b = shaper.shape(
            &mut gpu,
            &TextRequest {
                text: "so".to_string(),
                font_size: 18.0,
                color: WHITE,
            },
            1.0,
        );
        let (Content::Text { atlas: at_a, .. }, Content::Text { atlas: at_b, .. }) = (a, b) else {
            panic!("both shape into text");
        };
        assert_eq!(at_a, at_b, "the atlas texture is created once and reused");
    }
}
