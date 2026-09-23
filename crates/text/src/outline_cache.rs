//! Retained vector outlines for glyphs promoted to [`OutlineVector`] at extreme
//! scale.
//!
//! At extreme sustained zoom a distance field can no longer represent a glyph
//! crisply, so the glyph is promoted to a retained tessellated outline that the
//! steady state reuses rather than re-tessellating each frame. This holds the
//! outline geometry keyed by face and glyph; `viso-render` owns the vertex
//! buffers.
//!
//! # Flattened once, in em units
//!
//! An outline is flattened into closed polygon contours once, in em units
//! relative to the pen origin, at a tolerance fine enough for every scale the
//! lane serves — so a zoom changes only the transform the contours are drawn
//! with, never the contours. Fill triangulation of those contours, and the
//! vertex buffers it produces, belong to `viso-render`, keyed by the same
//! (face, glyph) identity this cache reports as dropped when an entry dies.
//!
//! # Its own residency pool
//!
//! Every retained outline is admitted into the vector pool of
//! [`GlyphResidency`], which carries its own byte budget ([`OUTLINE_POOL`]) and
//! page-age CLOCK eviction, independent of the coverage and distance-field
//! pools (§13.6, §13.9). The cache is the owner of that pool's contents: it
//! drains the vector pool's reclaims itself, drops exactly the outlines on the
//! reclaimed pages, and a later request for one re-tessellates it once.
//!
//! [`OutlineVector`]: crate::glyph_representation::GlyphImageKind::OutlineVector

use std::collections::HashMap;

use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::FontFaceId;
use crate::glyph_cache::{Admission, GlyphKey, GlyphResidency, PoolBudget, Reclaimed};
use crate::glyph_representation::GlyphImageKind;
use crate::progressive::FontRevision;

/// Chord tolerance of the flattening, in em. A quarter-pixel error holds up to
/// 2048 px-per-em; past that the error stays this fraction of an em, far below
/// what a glyph that large can show.
const FLATTEN_TOLERANCE: f32 = 1.0 / 8192.0;

/// Upper bound on the segments one curve flattens into, so a pathological
/// control polygon cannot blow up the contour.
const FLATTEN_MAX: u32 = 256;

/// The vector pool's budget for retained outlines: 16 pages of 64 KiB.
/// Independent of every other pool; tuned by benchmark, not ABI.
pub const OUTLINE_POOL: PoolBudget = PoolBudget::new(16, 64 * 1024);

/// What to build an outline for.
#[derive(Debug, Clone, Copy)]
pub struct OutlineRequest<'a> {
    /// Owned face bytes.
    pub sfnt: &'a [u8],
    /// Face index within the font file.
    pub index: u32,
    /// The face's runtime identity.
    pub face: FontFaceId,
    /// Glyph index within the face.
    pub glyph: u16,
    /// The face's revision; an outline built from an older revision is rebuilt.
    pub revision: FontRevision,
}

/// A retained flattened outline for one glyph: closed polygon contours and the
/// ink extent, in em units, y-up, relative to the pen origin.
#[derive(Debug, Clone, PartialEq)]
pub struct OutlineGlyph {
    face: FontFaceId,
    glyph: u16,
    revision: FontRevision,
    points: Vec<[f32; 2]>,
    /// Exclusive end of each contour in `points`.
    ends: Vec<u32>,
    extent: [f32; 4],
}

impl OutlineGlyph {
    /// The face this outline was built from.
    pub fn face(&self) -> FontFaceId {
        self.face
    }

    /// The glyph index within that face.
    pub fn glyph(&self) -> u16 {
        self.glyph
    }

    /// The face revision at build time.
    pub fn revision(&self) -> FontRevision {
        self.revision
    }

    /// The closed contours, each a polygon whose last point joins its first.
    /// Filled with the nonzero rule, as the font's outline is.
    pub fn contours(&self) -> impl Iterator<Item = &[[f32; 2]]> + '_ {
        let mut start = 0;
        self.ends.iter().map(move |&end| {
            let contour = &self.points[start..end as usize];
            start = end as usize;
            contour
        })
    }

    /// Ink extent `[x_min, y_min, x_max, y_max]`, in em units. All zero for a
    /// glyph with no ink.
    pub fn extent(&self) -> [f32; 4] {
        self.extent
    }

    /// Whether the glyph had no ink (a space, or a contourless `.notdef`).
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// Bytes the retained geometry occupies, charged to the vector pool.
    pub fn byte_len(&self) -> usize {
        self.points.len() * size_of::<[f32; 2]>() + self.ends.len() * size_of::<u32>()
    }
}

#[derive(Debug)]
struct Entry {
    outline: OutlineGlyph,
    /// The vector-pool page the outline is resident on.
    page: usize,
}

/// The retained outline cache: (face, glyph) to a flattened outline, bounded
/// by the vector pool's budget.
#[derive(Debug, Default)]
pub struct OutlineCache {
    entries: HashMap<(FontFaceId, u16), Entry>,
    /// Keys whose retained geometry died since the last drain, so the owner of
    /// the vertex buffers frees exactly those.
    dropped: Vec<(FontFaceId, u16)>,
    reclaims: Vec<Reclaimed>,
    tessellations: u64,
}

impl OutlineCache {
    /// Get the retained outline for a glyph, building and admitting it only on
    /// a miss or a revision change. A hit touches its page for the frame's
    /// CLOCK fold and does nothing else. `None` when the bytes do not parse, or
    /// the face is CFF2 (whose bare default master is the wrong shape — the same
    /// refusal as the coverage rasterizer).
    pub fn get_or_build(
        &mut self,
        residency: &mut GlyphResidency,
        req: OutlineRequest<'_>,
    ) -> Option<&OutlineGlyph> {
        let key = (req.face, req.glyph);
        match self.entries.get(&key) {
            Some(entry) if entry.outline.revision == req.revision => {
                residency.touch_page(GlyphImageKind::OutlineVector, entry.page);
            }
            stale => {
                if stale.is_some() {
                    self.entries.remove(&key);
                    self.dropped.push(key);
                }
                let outline = build(&req)?;
                self.tessellations += 1;
                // A shed elsewhere may have left reclaims queued; settle them
                // before admission can reuse the pages they name.
                self.sync(residency);
                let admission = residency.get_or_admit(
                    GlyphKey {
                        face: req.face,
                        glyph: req.glyph,
                        kind: GlyphImageKind::OutlineVector,
                        bucket: 0,
                    },
                    outline.byte_len(),
                );
                let page = match admission {
                    Admission::Cached { page, .. } | Admission::Admitted { page, .. } => page,
                };
                // Admission may have reclaimed pages: drop what lived there
                // before the new outline takes its place.
                self.sync(residency);
                self.entries.insert(key, Entry { outline, page });
            }
        }
        self.entries.get(&key).map(|entry| &entry.outline)
    }

    /// Shed the vector pool down to `pressure_bytes`, dropping the outlines on
    /// every reclaimed page. Returns how many pages were reclaimed.
    pub fn shed(&mut self, residency: &mut GlyphResidency, pressure_bytes: usize) -> u64 {
        let pages = residency.shed_pool_to_pressure(GlyphImageKind::OutlineVector, pressure_bytes);
        self.sync(residency);
        pages
    }

    /// Move the keys whose retained geometry died since the last drain into
    /// `out`. Empty in steady state.
    pub fn take_dropped(&mut self, out: &mut Vec<(FontFaceId, u16)>) {
        out.append(&mut self.dropped);
    }

    /// Outlines built since creation: one per miss or revision change, never
    /// one per draw.
    pub fn tessellations(&self) -> u64 {
        self.tessellations
    }

    /// Outlines currently retained.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no outline is retained.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of retained geometry.
    pub fn bytes(&self) -> usize {
        self.entries
            .values()
            .map(|entry| entry.outline.byte_len())
            .sum()
    }

    /// Drop the outlines on every vector-pool page reclaimed since the last
    /// drain. Reclaims of the other pools stay queued for their owners.
    fn sync(&mut self, residency: &mut GlyphResidency) {
        residency.take_pool_reclaims(GlyphImageKind::OutlineVector, &mut self.reclaims);
        let dropped = &mut self.dropped;
        for reclaimed in self.reclaims.drain(..) {
            self.entries.retain(|key, entry| {
                let keep = entry.page != reclaimed.page;
                if !keep {
                    dropped.push(*key);
                }
                keep
            });
        }
    }
}

fn build(req: &OutlineRequest<'_>) -> Option<OutlineGlyph> {
    flatten(req, FLATTEN_TOLERANCE)
}

/// Flatten a glyph's outline with a chord error under `tolerance` em.
fn flatten(req: &OutlineRequest<'_>, tolerance: f32) -> Option<OutlineGlyph> {
    let face = Face::parse(req.sfnt, req.index).ok()?;
    if face.tables().cff2.is_some() {
        return None;
    }
    let upem = f32::from(face.units_per_em());
    let mut sink = Flatten {
        tolerance: tolerance * upem,
        inv_upem: 1.0 / upem,
        points: Vec::new(),
        ends: Vec::new(),
        start: 0,
        last: [0.0; 2],
    };
    // A glyph with no outline (a space) is an empty outline, not a failure.
    let _ = face.outline_glyph(GlyphId(req.glyph), &mut sink);
    sink.finish_contour();
    let mut extent = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for &[x, y] in &sink.points {
        extent = [
            extent[0].min(x),
            extent[1].min(y),
            extent[2].max(x),
            extent[3].max(y),
        ];
    }
    if sink.points.is_empty() {
        extent = [0.0; 4];
    }
    Some(OutlineGlyph {
        face: req.face,
        glyph: req.glyph,
        revision: req.revision,
        points: sink.points,
        ends: sink.ends,
        extent,
    })
}

/// Flattens a font outline into em-unit polygons.
struct Flatten {
    /// Chord tolerance in font units.
    tolerance: f32,
    inv_upem: f32,
    points: Vec<[f32; 2]>,
    ends: Vec<u32>,
    /// Index in `points` where the open contour starts.
    start: usize,
    /// Last on-curve point, in font units.
    last: [f32; 2],
}

impl Flatten {
    fn push(&mut self, x: f32, y: f32) {
        self.last = [x, y];
        self.points.push([x * self.inv_upem, y * self.inv_upem]);
    }

    /// Close the open contour; one with fewer than three points has no area and
    /// is dropped.
    fn finish_contour(&mut self) {
        let mut len = self.points.len() - self.start;
        if len > 1 && self.points[self.start] == self.points[self.points.len() - 1] {
            self.points.pop();
            len -= 1;
        }
        if len < 3 {
            self.points.truncate(self.start);
        } else {
            self.ends.push(self.points.len() as u32);
        }
        self.start = self.points.len();
    }

    /// Segments for a curve whose second-difference magnitude is `bend`, so the
    /// chord error `bend / (8 n²)` of the curve's second derivative stays under
    /// the tolerance.
    fn segments(&self, bend: f32) -> u32 {
        ((bend / (8.0 * self.tolerance)).sqrt().ceil() as u32).clamp(1, FLATTEN_MAX)
    }
}

impl OutlineBuilder for Flatten {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish_contour();
        self.push(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.push(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let [x0, y0] = self.last;
        // B'' = 2 (p0 - 2 p1 + p2).
        let bend = 2.0 * (x0 - 2.0 * x1 + x).hypot(y0 - 2.0 * y1 + y);
        let n = self.segments(bend);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.push(
                u * u * x0 + 2.0 * u * t * x1 + t * t * x,
                u * u * y0 + 2.0 * u * t * y1 + t * t * y,
            );
        }
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let [x0, y0] = self.last;
        // |B''| <= 6 max(|p0 - 2 p1 + p2|, |p1 - 2 p2 + p3|).
        let bend = 6.0
            * (x0 - 2.0 * x1 + x2)
                .hypot(y0 - 2.0 * y1 + y2)
                .max((x1 - 2.0 * x2 + x).hypot(y1 - 2.0 * y2 + y));
        let n = self.segments(bend);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.push(
                a * x0 + b * x1 + c * x2 + d * x,
                a * y0 + b * y1 + c * y2 + d * y,
            );
        }
    }

    fn close(&mut self) {
        self.finish_contour();
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ab_glyph_rasterizer::{Rasterizer, point};

    use super::*;
    use crate::glyph_representation::{
        Representation, RepresentationState, RunClass, TransformSample,
    };
    use crate::raster_a8::rasterize_coverage;

    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");
    const FACE: FontFaceId = FontFaceId(1);
    const REVISION: FontRevision = FontRevision(4);

    fn glyph(c: char) -> u16 {
        Face::parse(DEJAVU, 0)
            .expect("fixture parses")
            .glyph_index(c)
            .expect("fixture covers the character")
            .0
    }

    fn request(glyph: u16) -> OutlineRequest<'static> {
        OutlineRequest {
            sfnt: DEJAVU,
            index: 0,
            face: FACE,
            glyph,
            revision: REVISION,
        }
    }

    fn residency() -> GlyphResidency {
        GlyphResidency::with_pool_budgets(
            PoolBudget::default(),
            PoolBudget::default(),
            PoolBudget::default(),
            OUTLINE_POOL,
        )
    }

    /// Fill an outline's contours onto the A8 raster's grid for `px_per_em`.
    fn fill(
        outline: &OutlineGlyph,
        px_per_em: f32,
        width: u32,
        height: u32,
        left: f32,
        top: f32,
    ) -> Vec<u8> {
        let mut rasterizer = Rasterizer::new(width as usize, height as usize);
        let map = |[x, y]: [f32; 2]| point(x * px_per_em - left, top - y * px_per_em);
        for contour in outline.contours() {
            for (i, &from) in contour.iter().enumerate() {
                let to = contour[(i + 1) % contour.len()];
                rasterizer.draw_line(map(from), map(to));
            }
        }
        let mut coverage = vec![0u8; (width * height) as usize];
        rasterizer.for_each_pixel_2d(|x, y, cov| {
            coverage[(y * width + x) as usize] = (cov * 255.0).round().clamp(0.0, 255.0) as u8;
        });
        coverage
    }

    #[test]
    fn two_consecutive_frames_at_extreme_zoom_tessellate_once() {
        let glyph = glyph('A');
        let mut residency = residency();
        let mut cache = OutlineCache::default();
        let mut sample = TransformSample {
            now: Duration::ZERO,
            px_per_em: 64.0,
            rotated: false,
            world_space: false,
        };
        let mut state = RepresentationState::new(RunClass::Text, REVISION, &sample);
        let interval = Duration::from_micros(16_667);
        let mut outline_frames = 0;
        for frame in 0..120u32 {
            sample.now += interval;
            // A sustained zoom past the distance field's window, then held.
            sample.px_per_em = (64.0 * 1.06f32.powi(frame.min(40) as i32)).min(640.0);
            let resolution = state.resolve(&sample);
            if let Some(pending) = resolution.request
                && pending != resolution.draw
            {
                assert!(state.ready(pending, REVISION));
            }
            if resolution.draw == Representation::OUTLINE {
                outline_frames += 1;
                let outline = cache
                    .get_or_build(&mut residency, request(glyph))
                    .expect("outline builds");
                assert!(!outline.is_empty());
            }
            residency.advance_epoch();
        }
        assert!(outline_frames >= 2, "the zoom reached the outline lane");
        assert_eq!(cache.tessellations(), 1);
        assert_eq!(
            residency.pool_resident_glyphs(GlyphImageKind::OutlineVector),
            1
        );
    }

    /// Worst pixel difference, mean pixel difference, and relative ink error.
    fn compare(reference: &[u8], candidate: &[u8]) -> (u8, f64, f64) {
        let mut worst = 0u8;
        let mut total = 0u64;
        for (a, b) in reference.iter().zip(candidate) {
            worst = worst.max(a.abs_diff(*b));
            total += u64::from(a.abs_diff(*b));
        }
        let ink = |coverage: &[u8]| coverage.iter().map(|&v| f64::from(v)).sum::<f64>();
        let mean = total as f64 / reference.len() as f64;
        (worst, mean, (ink(candidate) / ink(reference) - 1.0).abs())
    }

    /// The A8 rasterizer flattens each quadratic with a chord error near
    /// 0.15 px, so against it an edge pixel on a tight curve may differ by that
    /// much coverage. Against the same outline flattened 64 times finer, the
    /// retained outline's own error shows: well under a tenth of a pixel.
    #[test]
    fn the_outline_fills_to_the_a8_raster_of_the_same_glyph() {
        let mut residency = residency();
        let mut cache = OutlineCache::default();
        for c in ['A', 'g', 'o', 'S'] {
            let glyph = glyph(c);
            let fine = flatten(&request(glyph), FLATTEN_TOLERANCE / 64.0).expect("outline builds");
            let outline = cache
                .get_or_build(&mut residency, request(glyph))
                .expect("outline builds");
            for px_per_em in [320.0, 512.0] {
                let exact =
                    rasterize_coverage(DEJAVU, 0, glyph, px_per_em).expect("coverage rasterizes");
                let grid = |outline| {
                    fill(
                        outline,
                        px_per_em,
                        exact.width,
                        exact.height,
                        exact.left,
                        exact.top,
                    )
                };
                let filled = grid(outline);
                let (worst, mean, ink) = compare(&exact.coverage, &filled);
                assert!(
                    worst <= 48,
                    "{c} at {px_per_em}: worst pixel off by {worst}"
                );
                assert!(mean < 1.0, "{c} at {px_per_em}: mean error {mean}");
                assert!(ink < 2e-3, "{c} at {px_per_em}: ink off by {ink}");

                let (worst, mean, ink) = compare(&grid(&fine), &filled);
                assert!(
                    worst <= 24,
                    "{c} at {px_per_em}: worst pixel off by {worst}"
                );
                assert!(mean < 0.25, "{c} at {px_per_em}: mean error {mean}");
                assert!(ink < 5e-4, "{c} at {px_per_em}: ink off by {ink}");
            }
        }
    }

    #[test]
    fn eviction_and_re_request_re_tessellate_exactly_once() {
        let glyph = glyph('A');
        let mut residency = residency();
        let mut cache = OutlineCache::default();
        cache
            .get_or_build(&mut residency, request(glyph))
            .expect("outline builds");
        cache
            .get_or_build(&mut residency, request(glyph))
            .expect("outline builds");
        assert_eq!(cache.tessellations(), 1);

        assert_eq!(cache.shed(&mut residency, 0), 1);
        assert!(cache.is_empty());
        let mut dropped = Vec::new();
        cache.take_dropped(&mut dropped);
        assert_eq!(dropped, [(FACE, glyph)]);

        for _ in 0..3 {
            cache
                .get_or_build(&mut residency, request(glyph))
                .expect("outline rebuilds");
        }
        assert_eq!(cache.tessellations(), 2);
        cache.take_dropped(&mut dropped);
        assert_eq!(dropped.len(), 1, "a steady hit drops nothing");
    }

    #[test]
    fn a_new_face_revision_rebuilds_once() {
        let glyph = glyph('A');
        let mut residency = residency();
        let mut cache = OutlineCache::default();
        cache
            .get_or_build(&mut residency, request(glyph))
            .expect("outline builds");
        let newer = OutlineRequest {
            revision: FontRevision(5),
            ..request(glyph)
        };
        for _ in 0..3 {
            let outline = cache
                .get_or_build(&mut residency, newer)
                .expect("outline rebuilds");
            assert_eq!(outline.revision(), FontRevision(5));
        }
        assert_eq!(cache.tessellations(), 2);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn the_cache_stays_within_the_vector_pool_budget() {
        let one = build(&request(glyph('A')))
            .expect("outline builds")
            .byte_len();
        let budget = PoolBudget::new(2, one * 2);
        let mut residency = GlyphResidency::with_pool_budgets(
            PoolBudget::default(),
            PoolBudget::default(),
            PoolBudget::default(),
            budget,
        );
        let mut cache = OutlineCache::default();
        let mut dropped = Vec::new();
        let glyphs = Face::parse(DEJAVU, 0)
            .expect("fixture parses")
            .number_of_glyphs();
        for glyph in 0..glyphs {
            if cache.get_or_build(&mut residency, request(glyph)).is_some() {
                residency.advance_epoch();
            }
            assert!(cache.bytes() <= residency.pool_resident_bytes(GlyphImageKind::OutlineVector));
            assert_eq!(
                cache.len(),
                residency.pool_resident_glyphs(GlyphImageKind::OutlineVector)
            );
        }
        cache.take_dropped(&mut dropped);
        assert!(!dropped.is_empty(), "the budget forced evictions");
        assert!(residency.pool_page_count(GlyphImageKind::OutlineVector) <= 2);
    }

    #[test]
    fn a_glyph_without_ink_is_an_empty_outline() {
        let mut residency = residency();
        let mut cache = OutlineCache::default();
        let outline = cache
            .get_or_build(&mut residency, request(glyph(' ')))
            .expect("space parses");
        assert!(outline.is_empty());
        assert_eq!(outline.extent(), [0.0; 4]);
        assert_eq!(outline.contours().count(), 0);
    }
}
