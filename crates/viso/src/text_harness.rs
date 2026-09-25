//! A headless drive of the wired text runtime, for the benches.
//!
//! The text benches of `viso-text` gate the paragraph and scheduling
//! primitives in isolation; this drives the path an app's frames take — the
//! shaper, its worker thread, the per-frame commit budget, glyph residency
//! over a headless raster backend — and reads back the counters the runtime
//! keeps, so a gate can assert what a frame did rather than what a primitive
//! could do.

use std::time::Duration;

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle};
use viso_platform::Instant;
use viso_render::Rgba;
use viso_text::inspect::TextInspection;
use viso_text::paragraph::LineLayout;
use viso_text::{GlyphImageKind, MemoryClass, TextOffset, TextPosition};
use viso_ui::{Content, TextRequest};

use crate::text_content::{self, ParagraphSlot, TextShaper};

const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// How long [`TextHarness::settle`] waits on the worker before it gives up.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);

/// The main thread's per-frame commit budget at a display refresh of one frame
/// per `interval`.
pub fn text_commit_budget(interval: Duration) -> Duration {
    text_content::text_commit_budget(interval)
}

/// One line's structure as plain data: its source range and width bits, and
/// each run's source range, inline extent bits, and glyph ids.
pub type LineShape = (
    (usize, usize),
    u32,
    Vec<((usize, usize), (u32, u32), Vec<u16>)>,
);

/// What one paragraph drew this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextDraw {
    /// Outline glyph instances.
    pub glyphs: usize,
    /// Color-bitmap glyph instances.
    pub color_glyphs: usize,
    /// The run's intrinsic size in physical pixels.
    pub natural: [f32; 2],
}

/// The runtime's counters as of now. The per-frame ones (`reshapes` through
/// `admission_failures`) reset at [`TextHarness::end_frame`]; the rest are
/// cumulative or current.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextStats {
    pub reshapes: u64,
    pub relinebreaks: u64,
    pub shaped_runs: u64,
    pub main_shaped_runs: u64,
    pub commits: u64,
    pub deferred: u64,
    pub rasters: u64,
    pub atlas_upload_bytes: u64,
    pub evictions: u64,
    pub admission_failures: u64,
    pub face_hits: u64,
    pub face_misses: u64,
    pub face_evictions: u64,
    pub face_recency_updates: u64,
    pub face_resident: u64,
    pub span_hits: u64,
    pub span_misses: u64,
    pub span_evictions: u64,
    pub coverage_resident: u64,
    pub color_resident: u64,
    pub coverage_evictions: u64,
    pub upload_bytes_total: u64,
    /// System font queries: the primary face's resolve plus fallback queries.
    pub system_queries: u64,
    pub fallback_plan_hits: u64,
    pub fallback_plan_misses: u64,
    /// Face parses coverage sets have cost, on the main thread and the worker.
    pub coverage_parses: u64,
}

/// The text runtime over a headless raster backend, driven frame by frame.
pub struct TextHarness {
    shaper: TextShaper,
    gpu: HeadlessRaster,
    updated: Vec<ParagraphSlot>,
}

impl TextHarness {
    /// The runtime as an app starts it: system fonts, nothing bundled.
    pub fn system() -> Self {
        Self::over(TextShaper::new())
    }

    /// The runtime with `font` bundled as the primary face.
    pub fn with_font(font: &[u8]) -> Self {
        let mut shaper = TextShaper::new();
        shaper.load_font(font, 0).expect("font parses");
        Self::over(shaper)
    }

    /// The runtime with `font` bundled, over atlas planes of `plane × plane`
    /// texels cut into `page × page` pages — small enough that page turnover
    /// is reachable in a few frames.
    pub fn with_font_and_atlas(font: &[u8], plane: u32, page: u32) -> Self {
        let mut shaper =
            TextShaper::with_atlas_geometry(plane, page, MemoryClass::for_target().text_budgets());
        shaper.load_font(font, 0).expect("font parses");
        Self::over(shaper)
    }

    fn over(shaper: TextShaper) -> Self {
        let mut gpu = HeadlessRaster::new();
        let _ = gpu.create_surface(RawWindowHandle::Headless, 128, 128);
        Self {
            shaper,
            gpu,
            updated: Vec::new(),
        }
    }

    /// Draw `text` as `paragraph` this frame, wrapped to `wrap` when given:
    /// its last good layout, with a new one queued when `text` differs.
    pub fn draw(
        &mut self,
        paragraph: u32,
        text: &str,
        font_size: f32,
        wrap: Option<f32>,
    ) -> TextDraw {
        let request = request(text, font_size, wrap.is_some());
        let content = self
            .shaper
            .shape(&mut self.gpu, slot(paragraph), &request, 1.0, wrap);
        let Content::Text {
            glyphs,
            color_glyphs,
            natural,
            ..
        } = content
        else {
            unreachable!("the shaper produces text content");
        };
        TextDraw {
            glyphs: glyphs.len(),
            color_glyphs: color_glyphs.len(),
            natural: [natural.x, natural.y],
        }
    }

    /// Commit the worker's results within `budget`, as a frame does; the
    /// number of paragraphs whose drawn layout changed.
    pub fn commit(&mut self, budget: Duration) -> usize {
        if !self.shaper.has_pending_work() {
            return 0;
        }
        self.shaper.pump(&mut self.gpu, budget, &mut self.updated);
        let changed = self.updated.len();
        self.updated.clear();
        changed
    }

    /// Commit everything until the worker owes nothing.
    pub fn settle(&mut self) {
        let start = Instant::now();
        while self.shaper.has_pending_work() {
            self.shaper
                .pump(&mut self.gpu, Duration::MAX, &mut self.updated);
            assert!(
                start.elapsed() < SETTLE_DEADLINE,
                "the text worker did not settle"
            );
            std::thread::yield_now();
        }
        self.updated.clear();
    }

    /// Whether the worker owes results or anything waits to be committed.
    pub fn pending(&self) -> bool {
        self.shaper.has_pending_work()
    }

    /// Close the frame, as the runtime does.
    pub fn end_frame(&mut self) {
        self.shaper.end_frame();
    }

    /// Drop every paragraph `live` rejects.
    pub fn retain(&mut self, mut live: impl FnMut(u32) -> bool) {
        self.shaper.retain_paragraphs(|at| live(at.index()));
    }

    /// The caret box `[x, y, w, h]` at byte `offset` of `paragraph`, read from
    /// the lines it draws.
    pub fn caret(
        &self,
        paragraph: u32,
        text: &str,
        font_size: f32,
        offset: usize,
    ) -> Option<[f32; 4]> {
        let request = request(text, font_size, false);
        let position = TextPosition::downstream(TextOffset(offset));
        let rect = self.shaper.caret(slot(paragraph), &request, position)?;
        Some([rect.x, rect.y, rect.w, rect.h])
    }

    /// The cumulative shape invocations of `paragraph`'s layout.
    pub fn shape_calls(&self, paragraph: u32) -> u64 {
        self.shaper
            .drawn_layout(slot(paragraph))
            .map_or(0, |(calls, _)| calls)
    }

    /// The lines `paragraph` draws.
    pub fn line_structure(&self, paragraph: u32) -> Vec<LineShape> {
        self.shaper
            .drawn_layout(slot(paragraph))
            .map_or_else(Vec::new, |(_, lines)| structure(lines))
    }

    /// Why each fallback chose its face, why each cache missed, and where each
    /// resident glyph lives; empty unless the `inspector` feature is on.
    pub fn inspect(&self) -> TextInspection {
        self.shaper.inspect()
    }

    pub fn stats(&self) -> TextStats {
        let counters = self.shaper.counters();
        let faces = self.shaper.face_cache();
        let spans = self.shaper.shaping_cache();
        let residency = self.shaper.residency();
        let fallback = self.shaper.fallback();
        TextStats {
            reshapes: counters.reshapes(),
            relinebreaks: counters.relinebreaks(),
            shaped_runs: counters.shaped_runs(),
            main_shaped_runs: counters.main_shaped_runs(),
            commits: counters.commits(),
            deferred: counters.deferred(),
            rasters: counters.rasters(),
            atlas_upload_bytes: counters.atlas_upload_bytes(),
            evictions: counters.evictions(),
            admission_failures: counters.admission_failures(),
            face_hits: faces.hits(),
            face_misses: faces.misses(),
            face_evictions: faces.evictions(),
            face_recency_updates: faces.recency_updates(),
            face_resident: faces.len() as u64,
            span_hits: spans.hits(),
            span_misses: spans.misses(),
            span_evictions: spans.evictions(),
            coverage_resident: residency.pool_resident_glyphs(GlyphImageKind::MaskA8) as u64,
            color_resident: residency.pool_resident_glyphs(GlyphImageKind::ColorRgba8) as u64,
            coverage_evictions: residency.pool_evictions(GlyphImageKind::MaskA8),
            upload_bytes_total: residency.upload_bytes_total(),
            system_queries: self.shaper.primary_queries() + fallback.system_fallback_query_count(),
            fallback_plan_hits: fallback.fallback_plan_hit(),
            fallback_plan_misses: fallback.fallback_plan_miss(),
            coverage_parses: fallback.coverage_face_parses() + self.shaper.layout_coverage_parses(),
        }
    }
}

fn slot(paragraph: u32) -> ParagraphSlot {
    ParagraphSlot::from_key(u64::from(paragraph))
}

fn request(text: &str, font_size: f32, soft_wrap: bool) -> TextRequest {
    TextRequest {
        text: text.into(),
        font_size,
        color: WHITE,
        soft_wrap,
        locale: None,
    }
}

fn structure(lines: &[LineLayout]) -> Vec<LineShape> {
    lines
        .iter()
        .map(|line| {
            let runs = line
                .runs
                .iter()
                .map(|run| {
                    (
                        (run.logical_range.0.0, run.logical_range.1.0),
                        (
                            run.visual_inline_range.0.to_bits(),
                            run.visual_inline_range.1.to_bits(),
                        ),
                        run.glyphs.iter().map(|g| g.glyph_id).collect(),
                    )
                })
                .collect();
            (
                (line.logical_range.0.0, line.logical_range.1.0),
                line.width.to_bits(),
                runs,
            )
        })
        .collect()
}
