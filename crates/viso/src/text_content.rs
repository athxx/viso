//! Facade-owned text preparation: resolves faces, shapes retained requests, and
//! uploads raster products into renderer-owned representation pools.

use std::cell::Cell;
use std::collections::HashMap;

use viso_gpu::{GpuBackend, TextureDesc};
use viso_render::{
    AtlasAlloc, ColorAlloc, ColorAtlas, GlyphAtlas, GlyphInstanceData, Rect, TextureId,
};
use viso_text::fallback::{FallbackPlan, FallbackPlanKey, FallbackStyle, FontFallback};
use viso_text::font_manifest::{AssetRef, FontManifest};
use viso_text::system_fonts::ColorGlyph;
use viso_text::{
    Admission, BaseDirection, BidiInfo, ColorGlyphRasterizer, Coverage, CoverageBitmap, Direction,
    FontFaceId, FontRequest, FontResolver, FontRole, GlyphImageKind, GlyphKey, GlyphResidency,
    LineBreaker, PoolBudget, Reclaimed, Resolved, Segmenter, ShapedRun, Shaper, inspect_face,
    rasterize_coverage,
};
use viso_ui::{Content, TextRequest, Vec2};

use crate::system_fonts::{CoreTextColorRaster, CoreTextProvider, LiveFontRegistry};

const ATLAS_SIZE: u32 = 1024;
/// Page edge length: the atlas plane is cut into `256 × 256` pages, and the page
/// is the unit of residency and of eviction (§13.9). Sixteen pages per plane is
/// enough granularity that reclaiming the coldest one costs a small fraction of
/// the working set, and few enough that the CLOCK sweep is trivially cheap.
const ATLAS_PAGE: u32 = 256;
/// Bytes per texel of the color plane, so an RGBA page is budgeted for the four
/// times the bytes an A8 page of the same edge length holds.
const COLOR_BYTES_PER_TEXEL: usize = 4;
/// How many times one glyph may be re-aimed at a different page before it is
/// given up on for this frame.
///
/// Residency accounts bytes; the packer places rectangles, so fragmentation can
/// defeat a page the byte budget said would fit. Each refusal seals that page and
/// reclaims elsewhere, so the retry always makes progress; the bound only keeps a
/// pathological glyph from walking the whole plane in one frame.
const PLACEMENT_RETRIES: u32 = 4;

#[derive(Debug, Default)]
pub(crate) struct TextCounters {
    reshapes: Cell<u64>,
    relinebreaks: Cell<u64>,
    rasters: Cell<u64>,
    atlas_upload_bytes: Cell<u64>,
    /// Pages reclaimed this frame, across all pools (§25 eviction visibility).
    evictions: Cell<u64>,
    /// Admissions the packer refused this frame, forcing a re-aim.
    admission_failures: Cell<u64>,
}

impl TextCounters {
    pub(crate) fn reshapes(&self) -> u64 {
        self.reshapes.get()
    }

    pub(crate) fn relinebreaks(&self) -> u64 {
        self.relinebreaks.get()
    }

    pub(crate) fn rasters(&self) -> u64 {
        self.rasters.get()
    }

    pub(crate) fn atlas_upload_bytes(&self) -> u64 {
        self.atlas_upload_bytes.get()
    }

    pub(crate) fn evictions(&self) -> u64 {
        self.evictions.get()
    }

    pub(crate) fn admission_failures(&self) -> u64 {
        self.admission_failures.get()
    }

    fn record_shape(&self, wrapped: bool) {
        self.reshapes.set(self.reshapes.get() + 1);
        if wrapped {
            self.relinebreaks.set(self.relinebreaks.get() + 1);
        }
    }

    fn record_raster(&self) {
        self.rasters.set(self.rasters.get() + 1);
    }

    fn record_upload(&self, bytes: usize) {
        self.atlas_upload_bytes
            .set(self.atlas_upload_bytes.get() + bytes as u64);
    }

    fn record_eviction(&self) {
        self.evictions.set(self.evictions.get() + 1);
    }

    fn record_admission_failure(&self) {
        self.admission_failures
            .set(self.admission_failures.get() + 1);
    }

    fn reset(&self) {
        self.reshapes.set(0);
        self.relinebreaks.set(0);
        self.rasters.set(0);
        self.atlas_upload_bytes.set(0);
        self.evictions.set(0);
        self.admission_failures.set(0);
    }
}

#[derive(Debug, Clone, Copy)]
struct PositionedGlyph {
    face: FontFaceId,
    glyph: u16,
    cluster: usize,
    origin: [f32; 2],
}

#[derive(Debug, Clone)]
struct PreparedLayout {
    glyphs: Vec<PositionedGlyph>,
    natural: Vec2,
    baseline: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LayoutKey {
    face: FontFaceId,
    text: String,
    size_bits: u32,
    width_bits: Option<u32>,
}

/// Where one glyph's pixels live, as the pixel owner records it.
///
/// `page` is what makes residency and pixels one model: a cache hit touches that
/// page for the CLOCK sweep without hashing a key, and a reclaim of that page
/// drops exactly the placements that pointed into it.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Placement {
    uv: Rect,
    bearing: [f32; 2],
    size: [u32; 2],
    page: usize,
}

pub(crate) struct TextShaper {
    resolver: FontResolver,
    fallback: FontFallback,
    /// Coverage sets for resolver-owned faces, so per-cluster face selection is a
    /// set lookup instead of a face parse per grapheme.
    face_coverage: Coverage,
    shaper: Shaper,
    manifest: FontManifest,
    primary: Option<FontFaceId>,
    next_asset: u32,
    layouts: HashMap<LayoutKey, PreparedLayout>,
    coverage_uv: HashMap<GlyphKey, Placement>,
    color_uv: HashMap<GlyphKey, Placement>,
    coverage_atlas: Option<GlyphAtlas>,
    color_atlas: Option<ColorAtlas>,
    /// Which glyph is resident on which page, and which page is coldest. The
    /// metadata half of the atlas lives in `viso-text`; the atlases above own the
    /// pixels of the pages it names (§13.9, §13.10).
    residency: GlyphResidency,
    /// Scratch for draining reclaimed pages; reused so a reclaim allocates
    /// nothing after warm-up and a steady-state frame drains an empty vec.
    reclaims: Vec<Reclaimed>,
    /// Atlas plane geometry in texels: edge length and page edge length. Fixed
    /// for the process; the pool budgets above are derived from it, so the
    /// metadata layer's "page full" and the packer's agree by construction.
    atlas_size: u32,
    atlas_page: u32,
    provider: CoreTextProvider,
    color_raster: CoreTextColorRaster,
    counters: TextCounters,
}

impl TextShaper {
    pub(crate) fn new() -> Self {
        Self::with_atlas_geometry(ATLAS_SIZE, ATLAS_PAGE)
    }

    /// A shaper over atlas planes of `size × size` texels cut into
    /// `page × page` pages, with each pool budgeted to match.
    fn with_atlas_geometry(size: u32, page: u32) -> Self {
        // One live-font registry, shared between the provider (which records the
        // handles CoreText resolves) and the color/coverage raster (which
        // rasterizes through them). See `system_fonts::LiveFontRegistry`.
        let live = LiveFontRegistry::new();
        let per_axis = (size / page.max(1)).max(1) as usize;
        let pages = per_axis * per_axis;
        let page_bytes = (page as usize) * (page as usize);
        Self {
            resolver: FontResolver::new(),
            fallback: FontFallback::new(1 << 30),
            face_coverage: Coverage::new(),
            shaper: Shaper::new(),
            manifest: FontManifest::default(),
            primary: None,
            next_asset: 0,
            layouts: HashMap::new(),
            coverage_uv: HashMap::new(),
            color_uv: HashMap::new(),
            coverage_atlas: None,
            color_atlas: None,
            // Each pool is budgeted from the geometry of the plane that holds it,
            // so "this page is full" means the same thing to the metadata layer
            // and to the packer. The MTSDF and vector pools keep their defaults
            // until their planes exist; their budgets are independent either way.
            residency: GlyphResidency::with_pool_budgets(
                PoolBudget::new(pages, page_bytes),
                PoolBudget::default(),
                PoolBudget::new(pages, page_bytes * COLOR_BYTES_PER_TEXEL),
                PoolBudget::default(),
            ),
            reclaims: Vec::new(),
            atlas_size: size,
            atlas_page: page,
            provider: CoreTextProvider::new(live.clone()),
            color_raster: CoreTextColorRaster::new(live),
            counters: TextCounters::default(),
        }
    }

    pub(crate) fn load_font(
        &mut self,
        bytes: impl Into<Box<[u8]>>,
        index: u32,
    ) -> Option<FontFaceId> {
        let bytes = bytes.into().into_vec();
        inspect_face(&bytes, index)?;
        let asset = AssetRef(self.next_asset);
        self.next_asset = self.next_asset.wrapping_add(1);
        let face = self.resolver.register_app_face(asset, index, bytes);
        self.primary.get_or_insert(face);
        Some(face)
    }

    pub(crate) fn counters(&self) -> &TextCounters {
        &self.counters
    }

    /// Residency itself, for the per-pool counters (resident glyphs, pages, and
    /// upload bytes) — read straight from the owner rather than mirrored.
    pub(crate) fn residency(&self) -> &GlyphResidency {
        &self.residency
    }

    /// Close the frame: fold the pages drawn this frame into page recency once
    /// (not once per glyph draw) and zero the per-frame counters.
    pub(crate) fn end_frame(&mut self) {
        self.residency.advance_epoch();
        self.counters.reset();
    }

    pub(crate) fn shape<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        request: &TextRequest,
        dpi_factor: f32,
        max_width: Option<f32>,
    ) -> Content {
        let Some(face) = self.resolve_primary() else {
            return empty_content(request, max_width);
        };
        let wrap_width = request.soft_wrap.then_some(max_width).flatten();
        let key = LayoutKey {
            face,
            text: request.text.clone(),
            size_bits: request.font_size.to_bits(),
            width_bits: wrap_width.map(f32::to_bits),
        };
        let layout = if let Some(layout) = self.layouts.get(&key) {
            layout.clone()
        } else {
            let layout = self.prepare_layout(face, &request.text, request.font_size, wrap_width);
            self.counters.record_shape(wrap_width.is_some());
            self.layouts.insert(key, layout.clone());
            layout
        };

        let mut glyphs = Vec::with_capacity(layout.glyphs.len());
        let mut color_glyphs = Vec::new();
        for glyph in layout.glyphs {
            let ppem = (request.font_size * dpi_factor)
                .round()
                .clamp(1.0, u16::MAX as f32) as u16;
            let is_emoji = request.text[glyph.cluster..]
                .chars()
                .next()
                .is_some_and(is_emoji);

            if is_emoji
                && let Some(color) =
                    self.color_raster
                        .rasterize_color_glyph(glyph.face, glyph.glyph, ppem)
            {
                let key = GlyphKey {
                    face: glyph.face,
                    glyph: glyph.glyph,
                    bucket: ppem,
                    kind: GlyphImageKind::ColorRgba8,
                };
                let packed = match self.color_uv.get(&key) {
                    Some(&placement) => {
                        self.residency
                            .touch_page(GlyphImageKind::ColorRgba8, placement.page);
                        Some(placement)
                    }
                    None => self.admit_color(backend, key, &color),
                };
                if let Some(Placement {
                    uv, bearing, size, ..
                }) = packed
                {
                    let inv = 1.0 / dpi_factor;
                    // `origin_px` is the ink bbox's *bottom-left* in y-up strike
                    // px: `bearing[1]` is the bottom of the ink above the baseline
                    // (negative for a glyph that descends). The quad's top edge is
                    // `bearing[1] + size.height` px above the baseline, so in
                    // y-down screen space it sits at `baseline - (bottom + h)` —
                    // the same "top of ink above baseline" the coverage path gets
                    // directly from `top = y_max`.
                    color_glyphs.push(GlyphInstanceData {
                        rect: Rect {
                            x: glyph.origin[0] + bearing[0] * inv,
                            y: glyph.origin[1] - (bearing[1] + size[1] as f32) * inv,
                            w: size[0] as f32 * inv,
                            h: size[1] as f32 * inv,
                        },
                        uv,
                    });
                    continue;
                }
            }

            let key = GlyphKey {
                face: glyph.face,
                glyph: glyph.glyph,
                bucket: ppem,
                kind: GlyphImageKind::MaskA8,
            };
            let packed = if let Some(&cached) = self.coverage_uv.get(&key) {
                self.residency
                    .touch_page(GlyphImageKind::MaskA8, cached.page);
                cached
            } else {
                // Parser-outlined coverage first; if the face carries no
                // parser-readable outline (Apple proprietary `hvgl`, e.g.
                // PingFang → empty bitmap), recover it as grayscale A8 through
                // CoreText, the same path emoji uses for color.
                let bitmap = match self.rasterize(glyph.face, glyph.glyph, ppem as f32) {
                    Some(b) if !b.is_empty() => b,
                    _ => {
                        let cb = self.color_raster.rasterize_coverage_glyph(
                            glyph.face,
                            glyph.glyph,
                            ppem,
                        );
                        let Some(b) = cb else {
                            continue;
                        };
                        if b.is_empty() {
                            continue;
                        }
                        b
                    }
                };
                let Some(placement) = self.admit_coverage(backend, key, &bitmap) else {
                    continue;
                };
                placement
            };
            let Placement {
                uv, bearing, size, ..
            } = packed;
            let inv = 1.0 / dpi_factor;
            glyphs.push(GlyphInstanceData {
                rect: Rect {
                    x: glyph.origin[0] + bearing[0] * inv,
                    y: glyph.origin[1] - bearing[1] * inv,
                    w: size[0] as f32 * inv,
                    h: size[1] as f32 * inv,
                },
                uv,
            });
        }

        let atlas = self
            .coverage_atlas
            .as_ref()
            .map_or(TextureId::new(0), GlyphAtlas::texture);
        self.upload_dirty(backend);
        Content::Text {
            glyphs,
            atlas,
            color_glyphs,
            color_atlas: self.color_atlas.as_ref().map(ColorAtlas::texture),
            color: request.color,
            natural: layout.natural,
            baseline: layout.baseline,
            shaped_at_width: wrap_width,
            soft_wrap: request.soft_wrap,
        }
    }

    fn resolve_primary(&mut self) -> Option<FontFaceId> {
        if self.primary.is_none() {
            let request = FontRequest::role(FontRole::Ui);
            if let Resolved::Face(face) =
                self.resolver
                    .resolve(&request, &self.manifest, &self.provider, "")
            {
                self.primary = Some(face);
                self.register_color_face(face);
            }
        }
        self.primary
    }

    fn register_color_face(&self, face: FontFaceId) {
        if let Some(metrics) = self.resolver.face_metrics(face)
            && let Some(name) = metrics.postscript_name
        {
            self.color_raster
                .register_face(face, &name, metrics.glyph_count);
        }
    }

    fn face_bytes(&self, face: FontFaceId) -> Option<(&[u8], u32)> {
        self.resolver
            .face_bytes(face)
            .or_else(|| self.fallback.face_bytes(face))
    }

    fn shape_span(
        &mut self,
        base: FontFaceId,
        text: &str,
        source_start: usize,
        direction: Direction,
    ) -> Vec<(ShapedRun, usize)> {
        let Some(run) = self.shape_registered(base, text, direction) else {
            return Vec::new();
        };
        if !run.has_coverage_miss() {
            return vec![(run, source_start)];
        }
        self.shape_clusters(base, text, source_start, direction)
    }

    fn shape_clusters(
        &mut self,
        base: FontFaceId,
        text: &str,
        source_start: usize,
        direction: Direction,
    ) -> Vec<(ShapedRun, usize)> {
        let boundaries: Vec<usize> = Segmenter::new(text)
            .grapheme_boundaries()
            .map(|offset| offset.0)
            .collect();
        let mut groups: Vec<(FontFaceId, usize, usize)> = Vec::new();
        for pair in boundaries.windows(2) {
            let cluster = &text[pair[0]..pair[1]];
            let face = if self.face_covers(base, cluster) {
                base
            } else {
                self.resolve_fallback(base, cluster).unwrap_or(base)
            };
            if let Some(last) = groups.last_mut()
                && last.0 == face
            {
                last.2 = pair[1];
            } else {
                groups.push((face, pair[0], pair[1]));
            }
        }
        let mut out = Vec::with_capacity(groups.len());
        for (face, start, end) in groups {
            self.push_shaped(
                &mut out,
                face,
                &text[start..end],
                source_start + start,
                direction,
            );
        }
        out
    }

    fn push_shaped(
        &mut self,
        out: &mut Vec<(ShapedRun, usize)>,
        face: FontFaceId,
        text: &str,
        source_start: usize,
        direction: Direction,
    ) {
        if let Some(run) = self.shape_registered(face, text, direction) {
            out.push((run, source_start));
        }
    }

    /// Whether the face has a glyph for every scalar in `text` — the candidate
    /// filter that picks a cluster's face before shaping.
    ///
    /// The answer comes from the face's coverage set, built once per face, so a
    /// paragraph of many clusters does not re-parse a face per cluster. It is a
    /// filter, not the verdict: the shaped run's coverage-miss flag is still what
    /// decides whether the choice held.
    fn face_covers(&mut self, face: FontFaceId, text: &str) -> bool {
        let Some((bytes, index)) = self
            .resolver
            .face_bytes(face)
            .or_else(|| self.fallback.face_bytes(face))
        else {
            return false;
        };
        self.face_coverage.face_covers(face, bytes, index, text)
    }

    fn shape_registered(
        &mut self,
        face: FontFaceId,
        text: &str,
        direction: Direction,
    ) -> Option<ShapedRun> {
        let resolver = &self.resolver;
        let fallback = &self.fallback;
        let shaper = &mut self.shaper;
        let (bytes, index) = resolver
            .face_bytes(face)
            .or_else(|| fallback.face_bytes(face))?;
        shaper.shape_run(face, bytes, index, text, direction)
    }

    fn rasterize(
        &self,
        face: FontFaceId,
        glyph: u16,
        pixels_per_em: f32,
    ) -> Option<viso_text::CoverageBitmap> {
        let (bytes, index) = self.face_bytes(face)?;
        rasterize_coverage(bytes, index, glyph, pixels_per_em)
    }

    fn resolve_fallback(&mut self, base: FontFaceId, text: &str) -> Option<FontFaceId> {
        let key = FallbackPlanKey {
            base,
            script: FontFallback::run_script(text),
            locale: String::new(),
            style: FallbackStyle::default(),
            source_revision: 0,
        };
        match self.fallback.plan_run(&key, text, &self.provider) {
            FallbackPlan::Mapped { face, .. } => {
                self.register_fallback_color_face(face);
                Some(face)
            }
            FallbackPlan::Unresolved => None,
        }
    }

    /// Register a resolved fallback face with the CoreText raster so its color /
    /// proprietary-outline (`hvgl`) glyphs can be re-opened by name.
    ///
    /// The glyph count comes from the reassembled sfnt (`maxp` parses fine), but
    /// the re-open name must be the *platform*-reported PostScript name: an
    /// AppleColorEmoji / PingFang sfnt reassembled from CoreText tables keeps only
    /// Macintosh-platform `name` records, and `ttf-parser` returns `None` for the
    /// PostScript name of those — so reading it from the bytes leaves the color
    /// face unregistered and every emoji falls through to the monochrome A8 path
    /// (a tofu-like blob). Take the name the provider captured from CoreText.
    fn register_fallback_color_face(&self, face: FontFaceId) {
        let Some((bytes, index)) = self.fallback.face_bytes(face) else {
            return;
        };
        let Some(metrics) = inspect_face(bytes, index) else {
            return;
        };
        if let Some(name) = self.fallback.face_postscript_name(face) {
            self.color_raster
                .register_face(face, name, metrics.glyph_count);
        }
    }

    fn prepare_layout(
        &mut self,
        base: FontFaceId,
        text: &str,
        font_size: f32,
        max_width: Option<f32>,
    ) -> PreparedLayout {
        let rows = self.rows(base, text, font_size, max_width);
        let metrics = self
            .resolver
            .face_metrics(base)
            .unwrap_or_else(default_metrics);
        let baseline = metrics.ascender_em * font_size;
        let line_height = metrics.line_height_em.max(1.0) * font_size;
        let mut positioned = Vec::new();
        let mut natural = Vec2::ZERO;

        for (row_index, (start, end)) in rows.into_iter().enumerate() {
            let bidi = BidiInfo::resolve(&text[start..end], BaseDirection::Auto);
            let mut pen_x = 0.0;
            for directional in bidi.direction_runs() {
                let run_start = start + directional.start.0;
                let run_end = start + directional.end.0;
                for (run, source) in self.shape_span(
                    base,
                    &text[run_start..run_end],
                    run_start,
                    directional.direction,
                ) {
                    for glyph in run.glyphs {
                        positioned.push(PositionedGlyph {
                            face: run.face,
                            glyph: glyph.glyph_id,
                            cluster: source + glyph.cluster as usize,
                            origin: [
                                pen_x + glyph.x_offset * font_size,
                                baseline + row_index as f32 * line_height
                                    - glyph.y_offset * font_size,
                            ],
                        });
                        pen_x += glyph.x_advance * font_size;
                    }
                }
            }
            natural.x = natural.x.max(pen_x);
            natural.y = (row_index as f32 + 1.0) * line_height;
        }
        PreparedLayout {
            glyphs: positioned,
            natural,
            baseline,
        }
    }

    fn rows(
        &mut self,
        base: FontFaceId,
        text: &str,
        font_size: f32,
        max_width: Option<f32>,
    ) -> Vec<(usize, usize)> {
        let Some(limit) = max_width.filter(|width| *width > 0.0) else {
            return hard_rows(text);
        };
        let breaker = LineBreaker::new();
        let mut rows = Vec::new();
        for (hard_start, hard_end) in hard_rows(text) {
            if hard_start == hard_end {
                rows.push((hard_start, hard_end));
                continue;
            }
            let line = &text[hard_start..hard_end];
            let mut row_start = 0;
            let mut last_fit = None;
            for (offset, _) in breaker.break_opportunities(line) {
                let end = offset.0;
                let width = self.measure(base, &line[row_start..end], font_size);
                if width <= limit || last_fit.is_none() {
                    last_fit = Some(end);
                    continue;
                }
                let chosen = last_fit.unwrap_or(end);
                rows.push((hard_start + row_start, hard_start + chosen));
                row_start = chosen;
                last_fit = Some(end);
            }
            if row_start < line.len() {
                rows.push((hard_start + row_start, hard_end));
            }
        }
        rows
    }

    fn measure(&mut self, base: FontFaceId, text: &str, font_size: f32) -> f32 {
        let bidi = BidiInfo::resolve(text, BaseDirection::Auto);
        bidi.direction_runs()
            .into_iter()
            .flat_map(|run| {
                self.shape_span(
                    base,
                    &text[run.start.0..run.end.0],
                    run.start.0,
                    run.direction,
                )
            })
            .map(|(run, _)| run.width_ems * font_size)
            .sum()
    }

    /// Admit a freshly rasterized coverage bitmap and pack its pixels onto the
    /// page residency chose, re-aiming if the packer refuses that page.
    ///
    /// `None` when the glyph cannot be placed at all — larger than a page, empty,
    /// or it exhausted its re-aims this frame. Nothing is cleared on the way:
    /// filling the pool reclaims the coldest page and re-admits into it.
    fn admit_coverage<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        key: GlyphKey,
        bitmap: &CoverageBitmap,
    ) -> Option<Placement> {
        let bytes = bitmap.coverage.len();
        for _ in 0..PLACEMENT_RETRIES {
            let (page, fresh) = self.page_for(key, bytes);
            let atlas = ensure_coverage_atlas(
                &mut self.coverage_atlas,
                backend,
                self.atlas_size,
                self.atlas_page,
            );
            match atlas.alloc_in_page(page, bitmap) {
                AtlasAlloc::Placed(uv) => {
                    let placement = Placement {
                        uv,
                        bearing: [bitmap.left, bitmap.top],
                        size: [bitmap.width, bitmap.height],
                        page,
                    };
                    self.coverage_uv.insert(key, placement);
                    self.counters.record_raster();
                    return Some(placement);
                }
                AtlasAlloc::PageFull if fresh => self.revoke(key, page, bytes),
                AtlasAlloc::PageFull | AtlasAlloc::Empty | AtlasAlloc::TooLarge => return None,
            }
        }
        None
    }

    /// The color-glyph twin of [`Self::admit_coverage`], against the RGBA pool and
    /// the color plane.
    fn admit_color<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        key: GlyphKey,
        color: &ColorGlyph,
    ) -> Option<Placement> {
        let bytes = color.rgba.len();
        for _ in 0..PLACEMENT_RETRIES {
            let (page, fresh) = self.page_for(key, bytes);
            let atlas = ensure_color_atlas(
                &mut self.color_atlas,
                backend,
                self.atlas_size,
                self.atlas_page,
            );
            match atlas.alloc_in_page(page, color) {
                ColorAlloc::Placed(uv) => {
                    let placement = Placement {
                        uv,
                        bearing: color.origin_px,
                        size: [color.width, color.height],
                        page,
                    };
                    self.color_uv.insert(key, placement);
                    self.counters.record_raster();
                    return Some(placement);
                }
                ColorAlloc::PageFull if fresh => self.revoke(key, page, bytes),
                ColorAlloc::PageFull | ColorAlloc::Empty | ColorAlloc::TooLarge => return None,
            }
        }
        None
    }

    /// Ask residency which page a glyph belongs on, then reopen every page the
    /// decision reclaimed before anything is packed into it.
    ///
    /// The flag says whether this call created the admission; only then may a
    /// refused placement be revoked (revoking a placement someone else owns would
    /// corrupt the byte accounting).
    fn page_for(&mut self, key: GlyphKey, bytes: usize) -> (usize, bool) {
        let admission = self.residency.get_or_admit(key, bytes);
        self.drain_reclaims();
        match admission {
            Admission::Admitted { page, .. } => (page, true),
            Admission::Cached { page, .. } => (page, false),
        }
    }

    /// Apply the reclaims residency has reported: reopen each named page for
    /// packing and drop the placements that pointed into it — that page and
    /// nothing else. Every other page keeps its pixels and its UVs, so no reclaim
    /// can cascade into a cache-wide clear.
    fn drain_reclaims(&mut self) {
        let mut reclaims = std::mem::take(&mut self.reclaims);
        self.residency.take_reclaims(&mut reclaims);
        for reclaimed in reclaims.drain(..) {
            self.counters.record_eviction();
            match reclaimed.kind {
                GlyphImageKind::MaskA8 => {
                    if let Some(atlas) = self.coverage_atlas.as_mut() {
                        atlas.reset_page(reclaimed.page);
                    }
                    self.coverage_uv
                        .retain(|_, placement| placement.page != reclaimed.page);
                }
                GlyphImageKind::ColorRgba8 => {
                    if let Some(atlas) = self.color_atlas.as_mut() {
                        atlas.reset_page(reclaimed.page);
                    }
                    self.color_uv
                        .retain(|_, placement| placement.page != reclaimed.page);
                }
                // The MTSDF and vector pools hold no plane yet, so they own no
                // pixels to reopen.
                GlyphImageKind::ScalableMtsdf
                | GlyphImageKind::OutlineVector
                | GlyphImageKind::ColorVector => {}
            }
        }
        self.reclaims = reclaims;
    }

    /// Undo an admission the packer refused, counting the failure.
    fn revoke(&mut self, key: GlyphKey, page: usize, bytes: usize) {
        self.residency.revoke(key, page, bytes);
        self.counters.record_admission_failure();
    }

    fn upload_dirty<B: GpuBackend>(&mut self, backend: &mut B) {
        if let Some(atlas) = self.coverage_atlas.as_mut()
            && let Some((x, y, width, height, bytes)) = atlas.take_dirty()
        {
            self.counters.record_upload(bytes.len());
            backend.write_texture(atlas.texture(), x, y, width, height, &bytes);
        }
        if let Some(atlas) = self.color_atlas.as_mut()
            && let Some((x, y, width, height, bytes)) = atlas.take_dirty()
        {
            self.counters.record_upload(bytes.len());
            backend.write_texture(atlas.texture(), x, y, width, height, &bytes);
        }
    }
}

fn ensure_coverage_atlas<'a, B: GpuBackend>(
    atlas: &'a mut Option<GlyphAtlas>,
    backend: &mut B,
    size: u32,
    page: u32,
) -> &'a mut GlyphAtlas {
    atlas.get_or_insert_with(|| {
        let texture = backend.create_texture(&TextureDesc {
            width: size,
            height: size,
            format: GlyphAtlas::FORMAT,
            render_target: false,
            label: "ui-glyph-coverage",
        });
        GlyphAtlas::new(size, page, texture)
    })
}

fn ensure_color_atlas<'a, B: GpuBackend>(
    atlas: &'a mut Option<ColorAtlas>,
    backend: &mut B,
    size: u32,
    page: u32,
) -> &'a mut ColorAtlas {
    atlas.get_or_insert_with(|| {
        let texture = backend.create_texture(&TextureDesc {
            width: size,
            height: size,
            format: ColorAtlas::FORMAT,
            render_target: false,
            label: "ui-glyph-color",
        });
        ColorAtlas::new(size, page, texture)
    })
}

fn hard_rows(text: &str) -> Vec<(usize, usize)> {
    let mut rows = Vec::new();
    let mut start = 0;
    for (offset, ch) in text.char_indices() {
        if ch == '\n' {
            rows.push((start, offset));
            start = offset + ch.len_utf8();
        }
    }
    rows.push((start, text.len()));
    rows
}

fn empty_content(request: &TextRequest, shaped_at_width: Option<f32>) -> Content {
    Content::Text {
        glyphs: Vec::new(),
        atlas: TextureId::new(0),
        color_glyphs: Vec::new(),
        color_atlas: None,
        color: request.color,
        natural: Vec2::ZERO,
        baseline: 0.0,
        shaped_at_width,
        soft_wrap: request.soft_wrap,
    }
}

fn default_metrics() -> viso_text::FaceMetrics {
    viso_text::FaceMetrics {
        ascender_em: 0.8,
        descender_em: -0.2,
        line_height_em: 1.2,
        units_per_em: 1000,
        glyph_count: 0,
        postscript_name: None,
    }
}

fn is_emoji(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0xFE0F | 0x200D
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_gpu::{HeadlessRaster, RawWindowHandle};
    use viso_render::Rgba;

    const TEST_FONT: &[u8] = include_bytes!("../fixtures/DejaVuSans-subset.ttf");
    const WHITE: Rgba = Rgba {
        r: 1.0,
        g: 1.0,
        b: 1.0,
        a: 1.0,
    };

    fn shaper() -> TextShaper {
        let mut shaper = TextShaper::new();
        shaper.load_font(TEST_FONT, 0).expect("fixture parses");
        shaper
    }

    /// A deliberately tiny plane for the residency tests: 96 × 96 texels cut into
    /// nine 32-texel pages. At the sizes used below one padded glyph fills a page,
    /// so every distinct glyph owns a page and page turnover is reachable within a
    /// handful of frames — the same code path a 1024 × 1024 plane reaches after a
    /// few thousand.
    const TINY_PLANE: u32 = 96;
    const TINY_PAGE: u32 = 32;
    /// Two ascenders: at ~30px each is padded past half a page on both axes, so no
    /// two of them ever share a page.
    const PAIR: &str = "bd";

    fn tiny_shaper() -> TextShaper {
        let mut shaper = TextShaper::with_atlas_geometry(TINY_PLANE, TINY_PAGE);
        shaper.load_font(TEST_FONT, 0).expect("fixture parses");
        shaper
    }

    fn headless() -> HeadlessRaster {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        gpu
    }

    fn request(text: &str, font_size: f32) -> TextRequest {
        TextRequest {
            text: text.into(),
            font_size,
            color: WHITE,
            soft_wrap: false,
        }
    }

    /// Draw the hot pair plus one never-before-seen resolution bucket per frame,
    /// closing every frame the way the runtime does. Eighteen glyphs are demanded
    /// of nine pages, so the pool must turn over. Returns the pages reclaimed.
    fn flood(shaper: &mut TextShaper, gpu: &mut HeadlessRaster) -> u64 {
        let mut evictions = 0;
        for size in [27.0, 28.0, 29.0, 31.0, 32.0, 33.0, 34.0] {
            shaper.shape(gpu, &request(PAIR, 30.0), 1.0, None);
            shaper.shape(gpu, &request(PAIR, size), 1.0, None);
            evictions += shaper.counters().evictions();
            shaper.end_frame();
        }
        evictions
    }

    #[test]
    fn filling_the_pool_reclaims_cold_pages_and_keeps_the_hot_glyphs() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        shaper.shape(&mut gpu, &request(PAIR, 30.0), 1.0, None);
        shaper.end_frame();

        let evicted = flood(&mut shaper, &mut gpu);
        assert!(evicted > 0, "nine pages must turn over under this load");
        assert_eq!(
            shaper.residency().pool_page_count(GlyphImageKind::MaskA8),
            9,
            "eviction reuses pages; it never grows the plane",
        );

        // The hot pair was drawn in every single frame, so CLOCK found its pages
        // referenced every time it passed them: they are still resident, and this
        // frame therefore rasterizes nothing and uploads nothing.
        let content = shaper.shape(&mut gpu, &request(PAIR, 30.0), 1.0, None);
        let Content::Text { glyphs, .. } = &content else {
            panic!("text content");
        };
        assert_eq!(glyphs.len(), 2);
        assert_eq!(
            shaper.counters().rasters(),
            0,
            "the hot glyphs must survive the flood",
        );
        assert_eq!(shaper.counters().atlas_upload_bytes(), 0);
    }

    #[test]
    fn a_reclaimed_glyph_readmits_without_a_whole_atlas_upload() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        // Drawn once, in the first frame, and never touched again: the coldest
        // page in the pool from that moment on.
        let probe = request("q", 26.0);
        shaper.shape(&mut gpu, &probe, 1.0, None);
        shaper.end_frame();
        assert!(flood(&mut shaper, &mut gpu) > 0);

        let content = shaper.shape(&mut gpu, &probe, 1.0, None);
        let Content::Text { glyphs, .. } = &content else {
            panic!("text content");
        };
        assert_eq!(
            glyphs.len(),
            1,
            "a reclaimed glyph re-admits, it is not dropped"
        );
        assert!(
            shaper.counters().rasters() > 0,
            "its page was reclaimed during the flood",
        );
        // Only the re-admitted glyph's own texels move. The generational wipe this
        // replaced re-uploaded the entire plane every time it fired.
        let uploaded = shaper.counters().atlas_upload_bytes();
        assert!(
            uploaded > 0 && uploaded < (TINY_PAGE * TINY_PAGE) as u64,
            "re-admission uploaded {uploaded} bytes",
        );
    }

    /// Pool budgets are independent: filling the RGBA pool reclaims color pages
    /// and nothing else. The A8 coverage pool keeps every glyph and every page —
    /// §13.10's `memory pressure 不引发全 Text cache 连锁清空`.
    ///
    /// macOS-only: only the CoreText provider yields real color-emoji bitmaps, so
    /// only there can the RGBA pool be filled at all.
    #[cfg(target_os = "macos")]
    #[test]
    fn filling_the_color_pool_reclaims_nothing_from_the_coverage_pool() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        let latin = request(PAIR, 30.0);
        shaper.shape(&mut gpu, &latin, 1.0, None);
        shaper.end_frame();
        let coverage_glyphs = shaper
            .residency()
            .pool_resident_glyphs(GlyphImageKind::MaskA8);
        let coverage_bytes = shaper
            .residency()
            .pool_resident_bytes(GlyphImageKind::MaskA8);
        assert!(coverage_glyphs > 0, "the coverage pool is warm");

        // One fresh emoji bucket per frame, never re-requested, until the nine-page
        // color pool turns over. The Latin pair is not drawn again at all.
        for size in 12..=26 {
            shaper.shape(&mut gpu, &request("🥟", size as f32), 1.0, None);
            shaper.end_frame();
        }
        let residency = shaper.residency();
        assert!(
            residency.pool_evictions(GlyphImageKind::ColorRgba8) > 0,
            "the color pool must have turned over",
        );
        assert_eq!(
            residency.pool_evictions(GlyphImageKind::MaskA8),
            0,
            "color pressure must not reclaim a coverage page",
        );
        assert_eq!(
            residency.pool_resident_glyphs(GlyphImageKind::MaskA8),
            coverage_glyphs,
        );
        assert_eq!(
            residency.pool_resident_bytes(GlyphImageKind::MaskA8),
            coverage_bytes,
        );

        // And the untouched coverage glyphs are still usable: no raster, no upload.
        shaper.shape(&mut gpu, &latin, 1.0, None);
        assert_eq!(shaper.counters().rasters(), 0);
        assert_eq!(shaper.counters().atlas_upload_bytes(), 0);
    }

    #[test]
    fn a_warm_working_set_admits_nothing_and_uploads_zero_bytes() {
        let mut gpu = headless();
        let mut shaper = tiny_shaper();
        let frame = ["Viso", "steady", "state"];
        for text in frame {
            shaper.shape(&mut gpu, &request(text, 18.0), 1.0, None);
        }
        shaper.end_frame();
        let residency = shaper.residency();
        let resident = residency.pool_resident_glyphs(GlyphImageKind::MaskA8);
        let pages = residency.pool_page_count(GlyphImageKind::MaskA8);
        let uploaded = residency.pool_upload_bytes(GlyphImageKind::MaskA8);
        assert!(
            resident > 0 && uploaded > 0,
            "the first frame admitted glyphs"
        );

        for text in frame {
            shaper.shape(&mut gpu, &request(text, 18.0), 1.0, None);
        }
        let counters = shaper.counters();
        assert_eq!(counters.reshapes(), 0);
        assert_eq!(counters.rasters(), 0);
        assert_eq!(counters.atlas_upload_bytes(), 0);
        assert_eq!(counters.evictions(), 0);
        assert_eq!(counters.admission_failures(), 0);
        let residency = shaper.residency();
        assert_eq!(
            residency.pool_resident_glyphs(GlyphImageKind::MaskA8),
            resident,
            "a warm frame admits nothing",
        );
        assert_eq!(residency.pool_page_count(GlyphImageKind::MaskA8), pages);
        assert_eq!(
            residency.pool_upload_bytes(GlyphImageKind::MaskA8),
            uploaded,
            "a warm frame uploads nothing",
        );
    }

    #[test]
    fn shapes_and_reuses_residency() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        let mut shaper = shaper();
        let request = TextRequest {
            text: "Viso".into(),
            font_size: 22.0,
            color: WHITE,
            soft_wrap: false,
        };
        let first = shaper.shape(&mut gpu, &request, 1.0, None);
        shaper.end_frame();
        let second = shaper.shape(&mut gpu, &request, 1.0, None);
        let Content::Text {
            glyphs,
            natural,
            atlas,
            ..
        } = &first
        else {
            panic!("text content");
        };
        assert!(!glyphs.is_empty());
        assert!(natural.x > 0.0 && natural.y > 0.0);
        let Content::Text {
            atlas: second_atlas,
            ..
        } = second
        else {
            panic!("text content");
        };
        assert_eq!(atlas, &second_atlas);
        assert_eq!(shaper.counters().reshapes(), 0);
        assert_eq!(shaper.counters().rasters(), 0);
        assert_eq!(shaper.counters().atlas_upload_bytes(), 0);
    }

    /// The multilingual sample must resolve CJK, Devanagari, and emoji through
    /// the CoreText system-font path and actually rasterize their glyphs — the
    /// live-`CTFont` registry regression guard. Apple's PingFang/`.SFNS`-fallback
    /// CJK and Devanagari faces carry proprietary `hvgl` outlines a generic parser
    /// cannot render, so they route through the CoreText grayscale-coverage raster;
    /// if the raster could not bind their live handle (the old `CTFontCreateWithName`
    /// path silently substituted a Latin fallback), those glyphs would vanish and
    /// the atlas would receive no coverage upload for them.
    ///
    /// macOS-only: it depends on the CoreText system-font provider resolving real
    /// system faces, which the non-macOS stub does not.
    #[cfg(target_os = "macos")]
    #[test]
    fn multilingual_sample_rasterizes_cjk_devanagari_emoji() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 1024, 256);
        // No app font loaded: every script falls through to the system provider,
        // exactly as the hello-world example does.
        let mut shaper = TextShaper::new();
        let request = TextRequest {
            text: "世界 नमस्ते 🥟".into(),
            font_size: 48.0,
            color: WHITE,
            soft_wrap: false,
        };
        let content = shaper.shape(&mut gpu, &request, 1.0, None);
        let Content::Text {
            glyphs,
            color_glyphs,
            baseline,
            ..
        } = &content
        else {
            panic!("text content");
        };
        let baseline = *baseline;

        // Every visible cluster placed a glyph: 2 Han + the नमस्ते cluster(s) +
        // the emoji. A dropped face would leave gaps; assert we got well more than
        // the two ASCII spaces could explain.
        assert!(
            glyphs.len() >= 4,
            "CJK/Devanagari/emoji glyphs must all place (got {})",
            glyphs.len()
        );

        // Baseline sanity: the emoji color quad must sit on the *same* baseline as
        // the CJK/Devanagari coverage glyphs, not float above or drop below it. An
        // emoji strike is roughly em-tall sitting on the baseline, so its top edge
        // is above the baseline and its bottom edge at/just below it. A sign error
        // in the color placement math (mistaking the ink-bbox bottom for its top)
        // would push the whole quad a full glyph height off — this guards it.
        let emoji = color_glyphs.first().expect("emoji placed a color glyph");
        let top = emoji.rect.y;
        let bottom = emoji.rect.y + emoji.rect.h;
        assert!(
            top < baseline,
            "emoji top ({top}) must be above the baseline ({baseline})",
        );
        assert!(
            bottom > baseline - emoji.rect.h,
            "emoji must rest on the baseline, not float a glyph-height above it",
        );
        // Its bottom must not sink far below the baseline (a small descent is fine).
        assert!(
            bottom <= baseline + emoji.rect.h * 0.5,
            "emoji bottom ({bottom}) sits too far below the baseline ({baseline})",
        );
        // The atlas received real coverage/color uploads — glyphs actually
        // rasterized rather than resolving to empty bitmaps.
        assert!(
            shaper.counters().atlas_upload_bytes() > 0,
            "system-font glyphs must upload atlas coverage",
        );

        // Each script in isolation must rasterize, so a passing aggregate above
        // cannot be one script (e.g. emoji) covering for a dropped face. CJK and
        // Devanagari place monochrome coverage glyphs; emoji places a *color*
        // glyph (its own atlas), so assert against the right vector per script.
        for (label, text, color) in [
            ("Han", "世界", false),
            ("Devanagari", "नमस्ते", false),
            ("emoji", "🥟", true),
        ] {
            let mut solo = TextShaper::new();
            let req = TextRequest {
                text: text.into(),
                font_size: 48.0,
                color: WHITE,
                soft_wrap: false,
            };
            let c = solo.shape(&mut gpu, &req, 1.0, None);
            let Content::Text {
                glyphs,
                color_glyphs,
                ..
            } = &c
            else {
                panic!("text content");
            };
            let placed = if color {
                color_glyphs.len()
            } else {
                glyphs.len()
            };
            assert!(
                placed > 0,
                "{label} must place at least one glyph on its own",
            );
            assert!(
                solo.counters().atlas_upload_bytes() > 0,
                "{label} must upload atlas coverage on its own",
            );
        }
    }

    /// The static `ttf-parser` coverage fast path must refuse a CFF2 (variable)
    /// outline so the pipeline falls through to the authoritative CoreText
    /// raster. This is the direct guard for the Devanagari-tofu regression: the
    /// macOS system fallback `.SFDevanagari-Regular` is a CFF2 face whose sfnt
    /// (reassembled from CoreText tables at a fixed instance) carries only the
    /// default master. Drawing that bare master with the generic parser produced
    /// the *wrong* shape — non-empty ink that the shape() loop happily uploaded,
    /// so the older "glyphs placed + atlas uploaded" assertions passed while the
    /// runtime showed tofu. Here we shape real Devanagari, and for every placed
    /// glyph whose face is CFF2 we require the static path to return `None`
    /// (refuse) while the live CoreText coverage path returns real ink. If the
    /// refusal regressed, the static path would return `Some` and this fails.
    ///
    /// macOS-only: depends on the CoreText provider resolving `.SFDevanagari`.
    #[cfg(target_os = "macos")]
    #[test]
    fn cff2_devanagari_routes_through_live_raster_not_static_parser() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 512, 128);
        let mut shaper = TextShaper::new();
        // Shape once so the CJK/Devanagari fallback face is resolved and its live
        // handle registered with the color raster.
        let request = TextRequest {
            text: "नमस्ते".into(),
            font_size: 48.0,
            color: WHITE,
            soft_wrap: false,
        };
        let _ = shaper.shape(&mut gpu, &request, 1.0, None);
        // Re-derive the per-glyph face/glyph placements (GlyphInstanceData in the
        // shaped Content carries only rect/uv, not face/glyph).
        let base = shaper.resolve_primary().expect("primary UI face resolves");
        let layout = shaper.prepare_layout(base, "नमस्ते", 48.0, None);
        let glyphs = &layout.glyphs;
        assert!(!glyphs.is_empty(), "Devanagari must place glyphs");

        // At least one placed glyph must come from a CFF2 face and be served by
        // the live raster, not the static parser — otherwise the guard is vacuous
        // (the platform resolved a non-CFF2 face and this regression can't recur).
        let mut saw_cff2 = false;
        for g in glyphs {
            let Some((bytes, index)) = shaper.face_bytes(g.face) else {
                continue;
            };
            let is_cff2 = ttf_parser::Face::parse(bytes, index)
                .map(|f| f.tables().cff2.is_some())
                .unwrap_or(false);
            if !is_cff2 {
                continue;
            }
            saw_cff2 = true;

            // The static fast path must refuse a CFF2 outline...
            assert!(
                shaper.rasterize(g.face, g.glyph, 48.0).is_none(),
                "static parser must refuse CFF2 glyph {} (variable outline)",
                g.glyph,
            );
            // ...and the authoritative live path must render real ink for it.
            let live = shaper
                .color_raster
                .rasterize_coverage_glyph(g.face, g.glyph, 48)
                .expect("live CoreText coverage for CFF2 glyph");
            assert!(
                live.coverage.iter().any(|&v| v > 0),
                "live raster must produce ink for CFF2 glyph {}",
                g.glyph,
            );
        }
        assert!(
            saw_cff2,
            "expected the macOS Devanagari fallback to be a CFF2 face",
        );
    }

    #[test]
    fn wrapping_reduces_width_and_increases_height() {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 256, 256);
        let mut shaper = shaper();
        let request = TextRequest {
            text: "wrap this paragraph onto several lines".into(),
            font_size: 20.0,
            color: WHITE,
            soft_wrap: true,
        };
        let wide = shaper.shape(&mut gpu, &request, 1.0, None);
        let Content::Text { natural: wide, .. } = wide else {
            panic!("text content");
        };
        let narrow = shaper.shape(&mut gpu, &request, 1.0, Some(wide.x * 0.4));
        let Content::Text {
            natural: narrow, ..
        } = narrow
        else {
            panic!("text content");
        };
        assert!(narrow.x < wide.x);
        assert!(narrow.y > wide.y);
    }
}
