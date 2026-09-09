//! Facade-owned text shaping: turns a node's unshaped `TextRequest` into a
//! shaped `Content::Text` payload the UI store can measure and paint.
//!
//! `viso-ui` cannot shape text — the architecture DAG forbids a `viso-ui →
//! viso-text` edge, so the UI holds no font stack. The facade legally owns one
//! (`viso-text` is an allowed facade dependency) and does the shaping here, then
//! hands the finished [`viso_ui::Content`] back to the node store. This is the
//! single seam where a font stack meets the retained tree.
//!
//! Two persistent GPU textures back the run: an R8 SDF atlas for outline glyphs
//! and an RGBA8 color atlas for bitmap-emoji strikes. Each is created once on the
//! first shape that needs it and grown incrementally by re-uploading only the
//! region the text system reports dirty after each shape. New glyphs pack into
//! the same atlas across the whole session, so steady-state text (a fixed label
//! set) uploads each atlas once and never again. A pure-text run never allocates
//! or touches the color atlas.

use viso_gpu::{GpuBackend, TextureDesc, TextureFormat};
use viso_render::{GlyphInstanceData, Rect, TextureId};
use viso_text::{FontId, GlyphKind, SystemFallback, TextCounters, TextSystem};
use viso_ui::{Content, TextRequest, Vec2};

use crate::system_fonts::{CoreTextColorRaster, CoreTextProvider};

/// Owns the facade's font stack and glyph atlas, shaping [`TextRequest`]s into
/// [`Content::Text`] payloads. One per app; created on launch.
///
/// No font is bundled: the chain starts empty and the platform
/// [`CoreTextProvider`] pulls a covering system face into it on the first shape
/// (a system-default UI face, plus per-script / emoji fallbacks on demand).
/// Users may also load their own faces up front, which sit ahead of the system
/// fallbacks. On platforms with no provider (wasm), the chain stays empty and a
/// run shapes to no glyphs rather than panicking.
pub(crate) struct TextShaper {
    text: TextSystem,
    /// The primary face id once the chain has one — the request's font. `None`
    /// until a face is loaded (a user face, or a system face the provider
    /// resolves on the first shape). A `None` primary shapes to no glyphs.
    font: Option<FontId>,
    /// The persistent R8 SDF glyph atlas texture, created lazily on the first
    /// shape (once a backend exists to allocate it). `None` until then.
    atlas: Option<TextureId>,
    /// The persistent RGBA8 color atlas texture for bitmap-emoji glyphs, created
    /// lazily on the first shape that packs a color glyph. `None` until then —
    /// a pure-text session never allocates it.
    color_atlas: Option<TextureId>,
    /// The platform system-font provider (CoreText on macOS, a no-op elsewhere):
    /// consulted when a run has characters the loaded chain cannot render, to
    /// pull a covering system face into the fallback chain.
    provider: CoreTextProvider,
    /// The platform color-emoji rasterizer (CoreText on macOS, a no-op
    /// elsewhere): draws a system emoji face's glyphs — whose strikes were
    /// stripped at load — into premultiplied RGBA for the color atlas.
    color_raster: CoreTextColorRaster,
    /// Negative cache for system-font resolution — records which scripts / emoji
    /// have already been queried so an uncoverable run does not re-ask the OS
    /// every time it is (re)shaped.
    fallback: SystemFallback,
}

impl TextShaper {
    /// Build the shaper with an empty font chain. No face is bundled; the first
    /// shape resolves a system face through the provider (or, on wasm, shapes to
    /// nothing).
    pub(crate) fn new() -> Self {
        Self {
            text: TextSystem::new(),
            font: None,
            atlas: None,
            color_atlas: None,
            provider: CoreTextProvider::new(),
            color_raster: CoreTextColorRaster::new(),
            fallback: SystemFallback::new(),
        }
    }

    /// Load a user face from raw sfnt bytes, registering it in the chain. If the
    /// chain was empty, this face becomes the primary — so a user-loaded face
    /// sits ahead of any system fallback resolved later. Returns `None` if the
    /// bytes do not parse as an sfnt face.
    ///
    /// This is the internal seam the facade's public user-font API and the tests
    /// build on; it does not fetch anything, only registers already-in-hand bytes.
    /// The driver drives it from `WindowState::open` for each face the app
    /// registered through [`AppCx::load_font`](viso_ui::context::AppCx::load_font).
    pub(crate) fn load_font(&mut self, bytes: impl Into<Box<[u8]>>, index: u32) -> Option<FontId> {
        let id = self.text.load_font(bytes, index)?;
        self.font.get_or_insert(id);
        Some(id)
    }

    /// The text subsystem's per-frame work counters (reshapes, re-linebreaks,
    /// fresh rasters, atlas-upload bytes). The driver reads these into the
    /// `VISO_FRAME_TRACE` dump and zeroes them at the frame boundary via
    /// [`reset_counters`](Self::reset_counters).
    pub(crate) fn counters(&self) -> &TextCounters {
        self.text.counters()
    }

    /// Zero the text counters at a frame boundary so each count reflects one frame.
    pub(crate) fn reset_counters(&self) {
        self.text.reset_counters();
    }

    /// Shape one request into a [`Content::Text`], uploading any newly-packed
    /// glyphs to the atlas texture via `backend`. `dpi_factor` is the surface's
    /// device-pixel density (glyphs rasterize at that density).
    ///
    /// Glyph positions are node-local (origin at `(0, 0)`); the paint step
    /// shifts them to the node's world origin. `natural` is the run's bounding
    /// extent, which the measure pass reads for a `Fit` axis.
    /// `max_width` is the box width (physical px) the layout pass assigned this
    /// run, or `None` to shape unconstrained (single-line natural). It is only
    /// ever `Some` on a reflow pass for a wrap-eligible leaf (DL1 constraint
    /// downflow); the first shape of any run passes `None`. The produced
    /// `Content::Text` records it as `shaped_at_width` so the layout pass can
    /// tell whether a later assigned width still matches.
    pub(crate) fn shape<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        request: &TextRequest,
        dpi_factor: f32,
        max_width: Option<f32>,
    ) -> Content {
        // Seed the primary face from the system if the chain is still empty (no
        // user face loaded): the first shape pulls the platform default UI face.
        // On a platform with no provider (wasm) this stays `None` and the run
        // shapes to no glyphs — an empty text content, not a panic.
        let font = self.font.or_else(|| {
            let id = self.text.resolve_primary(&self.provider);
            self.font = id;
            id
        });
        let Some(font) = font else {
            return Content::Text {
                glyphs: Vec::new(),
                atlas: self.atlas.unwrap_or(TextureId(0)),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: request.color,
                natural: Vec2::ZERO,
                baseline: 0.0,
                shaped_at_width: max_width,
                soft_wrap: request.soft_wrap,
            };
        };

        // Before laying out, extend the fallback chain with system faces for any
        // script / emoji the loaded chain cannot render, so the run resolves
        // against the grown chain rather than boxing uncovered characters. The
        // negative cache makes an uncoverable (or already-resolved) run a no-op,
        // so steady-state reshapes of the same text pay only the shape, not an OS
        // query. `prepare` below reshapes from scratch, picking up new faces.
        self.text
            .resolve_missing(font, &request.text, &self.provider, &mut self.fallback);

        // Soft wrap only when the run opted in *and* the layout pass handed down
        // a constraining width (DL1 constraint downflow). A run that did not opt
        // in, or that was shaped unconstrained (`max_width == None`, the first
        // shape), stays single-line — `prepare` with `None` produces the natural
        // one-line extent the measure pass reads for a `Fit` axis.
        let wrap_width = if request.soft_wrap { max_width } else { None };
        let quads = self.text.prepare(
            font,
            &request.text,
            request.font_size,
            wrap_width,
            dpi_factor,
            Some(&self.color_raster),
        );

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
            self.text.counters().record_atlas_upload(band.len() as u64);
            backend.write_texture(atlas, 0, d.y, size, d.h, band);
        }

        // Route each quad to its atlas run by kind: SDF outlines decode through
        // the coverage ramp against `atlas`; color-bitmap glyphs sample RGBA
        // directly against the color atlas and paint as image quads. Both runs
        // share the node-local coordinate space and feed the one run extent.
        let mut natural = Vec2::ZERO;
        let mut glyphs: Vec<GlyphInstanceData> = Vec::with_capacity(quads.len());
        let mut color_glyphs: Vec<GlyphInstanceData> = Vec::new();
        for q in &quads {
            let rect = Rect {
                x: q.rect_px[0],
                y: q.rect_px[1],
                w: q.rect_px[2],
                h: q.rect_px[3],
            };
            natural.x = natural.x.max(rect.x + rect.w);
            natural.y = natural.y.max(rect.y + rect.h);
            let instance = GlyphInstanceData {
                rect,
                uv: Rect {
                    x: q.uv[0],
                    y: q.uv[1],
                    w: q.uv[2] - q.uv[0],
                    h: q.uv[3] - q.uv[1],
                },
                px_range: q.px_range,
            };
            match q.kind {
                GlyphKind::Sdf => glyphs.push(instance),
                GlyphKind::Color => color_glyphs.push(instance),
            }
        }

        // Only if the run packed color glyphs: ensure the RGBA color atlas
        // texture exists and upload its newly-rasterized band. Its buffer is
        // row-major RGBA with stride `size * 4` bytes, so a dirty band spans
        // `[d.y, d.y + d.h)` rows at that wider stride.
        let color_atlas = if color_glyphs.is_empty() {
            None
        } else {
            let csize = self.text.color_atlas_size();
            let ctex = *self.color_atlas.get_or_insert_with(|| {
                backend.create_texture(&TextureDesc {
                    width: csize,
                    height: csize,
                    format: TextureFormat::Rgba8Unorm,
                    render_target: false,
                    label: "ui-glyph-color-atlas",
                })
            });
            if let Some(d) = self.text.take_color_atlas_dirty() {
                let row = csize as usize * 4;
                let pixels = self.text.color_atlas_pixels();
                let band = &pixels[d.y as usize * row..(d.y + d.h) as usize * row];
                self.text.counters().record_atlas_upload(band.len() as u64);
                backend.write_texture(ctex, 0, d.y, csize, d.h, band);
            }
            Some(ctex)
        };

        // The first-line baseline in the same logical-pixel space as `natural`
        // and the glyph rects, so a grid cell can align this run on its baseline.
        let baseline = self.text.first_baseline(font, request.font_size);

        Content::Text {
            glyphs,
            atlas,
            color_glyphs,
            color_atlas,
            color: request.color,
            natural,
            baseline,
            // The width this run actually wrapped at: `None` when it stayed
            // single-line (not opted in, or shaped unconstrained). The layout
            // pass reflows only when its assigned width disagrees with this.
            shaped_at_width: wrap_width,
            soft_wrap: request.soft_wrap,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_gpu::{HeadlessRaster, RawWindowHandle};
    use viso_render::Rgba;

    /// A DejaVu Sans subset used only to give the tests a deterministic face:
    /// production bundles no font (the chain is seeded from the system), so the
    /// tests inject this through the same [`TextShaper::load_font`] seam a user
    /// font uses, making it the primary. It is a test fixture, not a default.
    const TEST_FONT: &[u8] = include_bytes!("../fixtures/DejaVuSans-subset.ttf");

    /// A shaper with the test fixture loaded as its primary face — the setup
    /// every shaping test shares now that no face is bundled.
    fn shaper_with_test_font() -> TextShaper {
        let mut shaper = TextShaper::new();
        shaper
            .load_font(TEST_FONT, 0)
            .expect("test fixture font parses");
        shaper
    }

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

        let mut shaper = shaper_with_test_font();
        let content = shaper.shape(
            &mut gpu,
            &TextRequest {
                text: "Viso".to_string(),
                font_size: 22.0,
                color: WHITE,
                soft_wrap: false,
            },
            1.0,
            None,
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
        let mut shaper = shaper_with_test_font();

        let a = shaper.shape(
            &mut gpu,
            &TextRequest {
                text: "Vi".to_string(),
                font_size: 18.0,
                color: WHITE,
                soft_wrap: false,
            },
            1.0,
            None,
        );
        let b = shaper.shape(
            &mut gpu,
            &TextRequest {
                text: "so".to_string(),
                font_size: 18.0,
                color: WHITE,
                soft_wrap: false,
            },
            1.0,
            None,
        );
        let (Content::Text { atlas: at_a, .. }, Content::Text { atlas: at_b, .. }) = (a, b) else {
            panic!("both shape into text");
        };
        assert_eq!(at_a, at_b, "the atlas texture is created once and reused");
    }

    #[test]
    fn dpi_factor_preserves_logical_extent() {
        // Glyphs rasterize at the surface density, but positions/extents are in
        // logical pixels: shaping the same run at 1x and 2x lands the run in
        // essentially the same logical box (the higher-density SDF is decoded
        // back to logical space in `prepare`; only the final glyph's integer
        // bitmap extent quantizes differently per density, a sub-glyph delta).
        // This is the invariant the threaded real dpi relies on — a HiDPI window
        // lays text out at the same size, only crisper. The wrong behavior
        // (ignoring dpi and emitting the 2x bitmap into logical space) would
        // roughly double the extent, which this tolerance rejects.
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        let mut shaper = shaper_with_test_font();

        let request = TextRequest {
            text: "Viso".to_string(),
            font_size: 24.0,
            color: WHITE,
            soft_wrap: false,
        };
        let one = shaper.shape(&mut gpu, &request, 1.0, None);
        let two = shaper.shape(&mut gpu, &request, 2.0, None);

        let (Content::Text { natural: n1, .. }, Content::Text { natural: n2, .. }) = (one, two)
        else {
            panic!("both shape into text");
        };
        // Within a few logical pixels (glyph-extent quantization), not doubled.
        assert!(
            (n1.x - n2.x).abs() < 4.0 && (n1.y - n2.y).abs() < 4.0,
            "logical extent is dpi-invariant within quantization: {n1:?} vs {n2:?}"
        );
    }

    /// End-to-end proof of the Hello World greeting on a real macOS system: with
    /// no bundled font, an empty shaper resolves the mixed English / Chinese /
    /// Thai / emoji run entirely from system faces. English seeds the primary UI
    /// face, the CJK and Thai spans pull covering fallbacks, and the emoji
    /// rasterizes through CoreText into the color atlas. A pass means every
    /// script produced outline glyphs (a non-empty SDF run whose extent spans a
    /// wide multi-script line) and the emoji produced a color glyph on its own
    /// RGBA atlas — the exact pipeline the example drives.
    #[cfg(target_os = "macos")]
    #[test]
    fn hello_world_shapes_all_scripts_from_system_fonts() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 512, 128);

        // Empty chain: no fixture loaded, so the primary and every fallback come
        // from the system provider — exactly what the example relies on.
        let mut shaper = TextShaper::new();
        let content = shaper.shape(
            &mut gpu,
            &TextRequest {
                text: "Hello 世界 สวัสดี 🎉".to_string(),
                font_size: 48.0,
                color: WHITE,
                soft_wrap: false,
            },
            2.0,
            None,
        );

        match content {
            Content::Text {
                glyphs,
                color_glyphs,
                color_atlas,
                natural,
                ..
            } => {
                // Outline glyphs for the Latin, Han, and Thai spans. The run is a
                // wide single line, so its natural width dwarfs its height — a
                // coarse guard that the three scripts all shaped rather than one
                // covering face swallowing the rest as tofu.
                assert!(
                    !glyphs.is_empty(),
                    "the multi-script run shapes outline glyphs from system faces"
                );
                assert!(
                    natural.x > natural.y * 3.0,
                    "a mixed one-line run is far wider than tall, got {natural:?}"
                );
                // The emoji rasterized to color on its own RGBA atlas.
                assert!(
                    !color_glyphs.is_empty(),
                    "the emoji shapes into a color glyph"
                );
                assert!(
                    color_atlas.is_some(),
                    "a color glyph allocates the RGBA color atlas"
                );
            }
            _ => panic!("the greeting shapes into Content::Text"),
        }
    }

    /// The DL1 shaper contract: a `soft_wrap` run shaped at a narrow `max_width`
    /// wraps — its natural extent gets narrower and taller than the same run
    /// shaped unconstrained — and it records the width it was shaped at, which
    /// the layout-side reflow recorder reads to decide the run has settled.
    #[test]
    fn soft_wrap_run_shaped_at_a_narrow_width_wraps_and_records_it() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 256, 256);
        let mut shaper = shaper_with_test_font();

        // A run with several breakable spaces, so a narrow box has somewhere to
        // wrap. Same request both times — only the assigned width differs.
        let request = TextRequest {
            text: "wrap this paragraph onto several lines".to_string(),
            font_size: 20.0,
            color: WHITE,
            soft_wrap: true,
        };

        let wide = shaper.shape(&mut gpu, &request, 1.0, None);
        let Content::Text {
            natural: wide_n,
            shaped_at_width: wide_at,
            ..
        } = wide
        else {
            panic!("shapes into text");
        };
        assert_eq!(wide_at, None, "an unconstrained shape records no width");

        // Constrain to a fraction of the single-line width so it must wrap.
        let box_w = wide_n.x * 0.4;
        let narrow = shaper.shape(&mut gpu, &request, 1.0, Some(box_w));
        let Content::Text {
            natural: narrow_n,
            shaped_at_width: narrow_at,
            ..
        } = narrow
        else {
            panic!("shapes into text");
        };
        assert_eq!(
            narrow_at,
            Some(box_w),
            "a wrapped run records the width it was shaped at"
        );
        // The wrapped run is much narrower than the unconstrained single line.
        // It need not fit `box_w` exactly — a single word longer than the box
        // cannot be broken, so the run is only as narrow as its longest word —
        // but it must have collapsed well below the one-line width.
        assert!(
            narrow_n.x < wide_n.x * 0.75,
            "wrapping collapses the run well below its single-line width: {} !< {}",
            narrow_n.x,
            wide_n.x * 0.75
        );
        assert!(
            narrow_n.y > wide_n.y,
            "wrapping onto more lines makes the run taller: {} !> {}",
            narrow_n.y,
            wide_n.y
        );
    }

    /// A run that did not opt into wrapping ignores `max_width` entirely: it
    /// stays a single line and its natural extent is unchanged, so a
    /// width-constrained non-wrapping leaf clips rather than reflows.
    #[test]
    fn non_wrapping_run_ignores_the_assigned_width() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 256, 256);
        let mut shaper = shaper_with_test_font();

        let request = TextRequest {
            text: "single line stays single".to_string(),
            font_size: 20.0,
            color: WHITE,
            soft_wrap: false,
        };

        let unconstrained = shaper.shape(&mut gpu, &request, 1.0, None);
        let Content::Text { natural: n0, .. } = unconstrained else {
            panic!("shapes into text");
        };
        // Pass a box far narrower than the single line: a non-wrapping run must
        // ignore it and keep the same one-line extent.
        let constrained = shaper.shape(&mut gpu, &request, 1.0, Some(n0.x * 0.3));
        let Content::Text {
            natural: n1,
            shaped_at_width: at1,
            ..
        } = constrained
        else {
            panic!("shapes into text");
        };
        assert!(
            (n0.x - n1.x).abs() <= 0.5 && (n0.y - n1.y).abs() <= 0.5,
            "a non-wrapping run keeps its single-line extent: {n0:?} vs {n1:?}"
        );
        // A non-wrapping run is never width-constrained, so it records no shaped
        // width regardless of the box handed down — the recorder's `soft_wrap`
        // guard means it never reaches the width comparison anyway.
        assert_eq!(
            at1, None,
            "a non-wrapping run records no shaped-at width even when a box is passed"
        );
    }
}
