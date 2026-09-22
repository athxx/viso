//! Multi-channel signed distance field generation for promoted glyphs.
//!
//! When a glyph is promoted under sustained transform (see
//! [`crate::glyph_representation`]), it is rendered once into a multi-channel
//! signed distance field so it stays sharp across a range of scales and
//! rotations without re-rasterizing per frame. The three color channels carry
//! *per-edge-group pseudo distances* — that is what reconstructs a sharp corner
//! instead of rounding it (§13.2) — and the alpha channel carries the glyph's
//! **true** signed distance, which drives anti-aliasing and, at generation time,
//! the sign-clash error correction that removes multi-channel interpolation
//! artifacts.
//!
//! The field is produced from the outline, not from a bitmap: corner detection
//! and edge coloring need the curve topology, and a grayscale pre-pass would
//! have already lost the corners this representation exists to keep. `ttf-parser`
//! supplies the outline; the distance math is this module's.
//!
//! The data is plain linear numbers, never color: a field texture must be
//! created with a non-sRGB format and sampled linearly, or every distance is
//! silently gamma-warped (§13.2).
//!
//! Per §13.5 a field is only valid inside a *quality window* around the
//! resolution bucket it was generated at. A request outside the window takes the
//! next bucket, or — past the top of the ladder — hands off to the retained
//! outline lane; one field is never stretched without bound.
//!
//! Viso 1.0 deliberately has **no plain single-channel SDF lane**: exact A8
//! coverage ([`crate::raster_a8`]) serves fixed-size text better than a
//! single-channel field, and anything that needs to scale gets the
//! corner-preserving multi-channel field instead.

use ttf_parser::{Face, GlyphId, OutlineBuilder};

use crate::FontFaceId;
use crate::progressive::FontRevision;

/// Width of the encoded signed-distance span, in field texels: a stored `0`
/// means `-DISTANCE_RANGE / 2` texels (outside), `255` means `+DISTANCE_RANGE /
/// 2` (inside), and `0.5` is the edge. Baked into the sampling shader, so it is
/// part of the field's ABI rather than a per-glyph parameter.
pub const DISTANCE_RANGE: f32 = 4.0;

/// Texels of margin around the glyph's ink box, on every side. Must be at least
/// `DISTANCE_RANGE / 2` or the outside half of the field would be clipped at the
/// border and the edge would harden into the texture's edge.
pub const FIELD_PAD: u32 = 3;

/// Revision of the generator and its error correction (§13.5). A field carries
/// it so a cached field from an older generator is recognizably stale rather
/// than silently mixed with new ones.
pub const GENERATOR_REVISION: u32 = 1;

/// The resolution ladder: the px-per-em a field is ever generated at. Doubling
/// steps, so the quality windows overlap and one step never leaves a gap.
pub const BUCKETS: [f32; 4] = [16.0, 32.0, 64.0, 128.0];

/// Lower edge of a bucket's quality window, as a factor of its px-per-em.
/// Minifying a field below this loses thin features faster than the true
/// distance channel can hide.
const WINDOW_MIN: f32 = 0.7;

/// Upper edge of a bucket's quality window, as a factor of its px-per-em. Past
/// this the texel grid of the field itself becomes visible on a curve.
const WINDOW_MAX: f32 = 2.25;

/// Cell edge of the segment-lookup grid, in field texels. Twice the distance
/// range, so searching a cell and its eight neighbours is guaranteed to find
/// every segment within `2 * DISTANCE_RANGE` texels of a sample — four times the
/// radius at which the encoding saturates.
const CELL: f32 = 2.0 * DISTANCE_RANGE;

/// Flattening step for curve segments, in field texels. The sagitta a chord of
/// this length leaves on a curve of glyph-scale radius is well under the
/// quantization step of the encoding.
const FLATTEN_STEP: f32 = 0.5;

/// Upper bound on the segments one curve flattens into, so a pathological
/// control polygon cannot blow up the segment list.
const FLATTEN_MAX: u32 = 96;

/// `sin` of msdfgen's default angle threshold: a join turning more than this
/// (about 8°) is a corner and splits the edge coloring.
const CORNER_CROSS: f32 = 0.141_120_01;

/// Which resolution to serve a request at (§13.5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MtsdfPlan {
    /// Generate (or reuse) the field at this bucket's px-per-em. The request is
    /// inside that bucket's quality window.
    Bucket(f32),
    /// Past the top of the ladder: no field can serve this scale, so the caller
    /// hands off to the retained outline lane ([`crate::outline_cache`], §13.6)
    /// rather than magnifying the largest field further.
    Outline,
}

/// Which bucket serves `px_per_em`, or [`MtsdfPlan::Outline`] past the ladder.
///
/// The smallest bucket whose window reaches the request wins, so a field is
/// never larger than the scale needs. A request under the smallest bucket's
/// window still takes the smallest bucket: minifying a field is safe, and the
/// promotion decision keeps small fixed-size text on the A8 coverage lane
/// anyway.
pub fn plan(px_per_em: f32) -> MtsdfPlan {
    for bucket in BUCKETS {
        if px_per_em <= bucket * WINDOW_MAX {
            return MtsdfPlan::Bucket(bucket);
        }
    }
    MtsdfPlan::Outline
}

/// What to generate a field for.
#[derive(Debug, Clone, Copy)]
pub struct MtsdfRequest<'a> {
    /// Owned face bytes.
    pub sfnt: &'a [u8],
    /// Face index within the font file.
    pub index: u32,
    /// The face's runtime identity, recorded on the field (§13.5).
    pub face: FontFaceId,
    /// Glyph index within the face.
    pub glyph: u16,
    /// The face's revision, recorded so a field outlives its face only as long
    /// as the face is unchanged (§13.5).
    pub revision: FontRevision,
    /// Bucket px-per-em from [`plan`]. A caller may pass any positive value; the
    /// recorded window is derived from it either way.
    pub px_per_em: f32,
}

/// An MTSDF raster for one glyph at one resolution bucket: the metadata the
/// residency pool keys on. The pixel bytes are handed to `viso-render` for
/// upload; this crate holds identity and placement, not GPU memory.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MtsdfGlyph {
    /// The face this field was generated from.
    pub face: FontFaceId,
    /// The glyph index within that face.
    pub glyph: u16,
    /// The face revision at generation time.
    pub revision: FontRevision,
    /// The generator/error-correction revision at generation time.
    pub generator_revision: u32,
    /// Source texel resolution: px-per-em the field was generated at.
    pub px_per_em: f32,
    /// Field width in texels, including [`FIELD_PAD`] on both sides.
    pub width: u32,
    /// Field height in texels, including [`FIELD_PAD`] on both sides.
    pub height: u32,
    /// Left edge of the field relative to the pen origin, in field texels
    /// (the ink box's `x_min` less the pad).
    pub left: f32,
    /// Top edge of the field relative to the pen origin, in field texels, y-up
    /// (the ink box's `y_max` plus the pad).
    pub top: f32,
    /// The encoded signed-distance span in field texels — [`DISTANCE_RANGE`],
    /// recorded per field so a future ladder can vary it without reinterpreting
    /// old entries.
    pub distance_range: f32,
    /// Validated minimum effective px-per-em: below this the field is out of
    /// window and the caller re-[`plan`]s.
    pub min_px_per_em: f32,
    /// Validated maximum effective px-per-em.
    pub max_px_per_em: f32,
}

impl MtsdfGlyph {
    /// Bytes of RGBA8 field data this glyph occupies in a pool page.
    pub fn byte_len(&self) -> usize {
        (self.width as usize) * (self.height as usize) * 4
    }

    /// Whether the glyph had no ink (a space, or a contourless `.notdef`).
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Whether this field may serve `px_per_em` (§13.5).
    pub fn in_window(&self, px_per_em: f32) -> bool {
        px_per_em >= self.min_px_per_em && px_per_em <= self.max_px_per_em
    }

    /// Field texels per device pixel when drawn at `px_per_em` — the factor the
    /// sampling lane turns into its screen-space distance range.
    pub fn scale_for(&self, px_per_em: f32) -> f32 {
        px_per_em / self.px_per_em
    }
}

/// One flattened outline segment in field-texel space, tagged with the edge
/// colors it contributes to.
#[derive(Debug, Clone, Copy)]
struct Seg {
    a: V,
    b: V,
    /// Channel mask: bit 0 red, bit 1 green, bit 2 blue.
    color: u8,
}

/// One outline curve in field-texel space, before flattening.
#[derive(Debug, Clone, Copy)]
enum Curve {
    Line(V, V),
    Quad(V, V, V),
    Cubic(V, V, V, V),
}

impl Curve {
    /// Tangent direction leaving the start point.
    fn start_dir(self) -> V {
        match self {
            Curve::Line(a, b) => b.sub(a),
            Curve::Quad(a, c, b) => first_nonzero(c.sub(a), b.sub(a)),
            Curve::Cubic(a, c0, c1, b) => {
                first_nonzero(c0.sub(a), first_nonzero(c1.sub(a), b.sub(a)))
            }
        }
    }

    /// Tangent direction entering the end point.
    fn end_dir(self) -> V {
        match self {
            Curve::Line(a, b) => b.sub(a),
            Curve::Quad(a, c, b) => first_nonzero(b.sub(c), b.sub(a)),
            Curve::Cubic(a, c0, c1, b) => {
                first_nonzero(b.sub(c1), first_nonzero(b.sub(c0), b.sub(a)))
            }
        }
    }
}

/// The MTSDF generator: one reusable set of scratch buffers, so generating a
/// second glyph allocates nothing.
#[derive(Debug, Default)]
pub struct MtsdfGenerator {
    curves: Vec<Curve>,
    /// `(start, end)` ranges into `curves`, one per closed contour.
    contours: Vec<(usize, usize)>,
    /// Channel mask per curve, parallel to `curves`.
    curve_colors: Vec<u8>,
    /// Indices into `curves` whose *incoming* join is a corner.
    corners: Vec<usize>,
    segs: Vec<Seg>,
    /// CSR offsets of the segment-lookup grid, `cols * rows + 1` entries.
    cell_start: Vec<u32>,
    /// Per-cell write cursors while filling `cell_items`.
    cell_cursor: Vec<u32>,
    /// Segment indices per grid cell, in CSR order.
    cell_items: Vec<u32>,
    /// One scanline's outline crossings: `(x, winding direction)`.
    crossings: Vec<(f32, i32)>,
}

impl MtsdfGenerator {
    /// Bytes of scratch this generator currently holds. Used to assert that a
    /// warmed-up generator stops allocating.
    pub fn scratch_bytes(&self) -> usize {
        self.curves.capacity() * size_of::<Curve>()
            + self.contours.capacity() * size_of::<(usize, usize)>()
            + self.curve_colors.capacity()
            + self.corners.capacity() * size_of::<usize>()
            + self.segs.capacity() * size_of::<Seg>()
            + self.cell_start.capacity() * 4
            + self.cell_cursor.capacity() * 4
            + self.cell_items.capacity() * 4
            + self.crossings.capacity() * size_of::<(f32, i32)>()
    }

    /// Generate the multi-channel distance field for one glyph, writing
    /// `width * height * 4` bytes of RGBA8 into `out`.
    ///
    /// `out` is cleared first and reused, so a caller that keeps one buffer pays
    /// no per-glyph allocation. Returns `None` when the bytes do not parse as a
    /// usable face, or when the face is CFF2 — a variable outline format whose
    /// bare default master is the wrong shape (the same refusal as
    /// [`crate::raster_a8::rasterize_coverage`], for the same reason). A glyph
    /// with no ink yields an empty field, which is `Some`.
    pub fn generate(&mut self, req: MtsdfRequest<'_>, out: &mut Vec<u8>) -> Option<MtsdfGlyph> {
        out.clear();
        let face = Face::parse(req.sfnt, req.index).ok()?;
        if face.tables().cff2.is_some() {
            return None;
        }
        let scale = req.px_per_em / face.units_per_em() as f32;
        let meta = |width: u32, height: u32, left: f32, top: f32| MtsdfGlyph {
            face: req.face,
            glyph: req.glyph,
            revision: req.revision,
            generator_revision: GENERATOR_REVISION,
            px_per_em: req.px_per_em,
            width,
            height,
            left,
            top,
            distance_range: DISTANCE_RANGE,
            min_px_per_em: req.px_per_em * WINDOW_MIN,
            max_px_per_em: req.px_per_em * WINDOW_MAX,
        };

        let mut bounds = BoundsSink::default();
        let inked = face
            .outline_glyph(GlyphId(req.glyph), &mut bounds)
            .and(bounds.bounds);
        let Some((x_min, _, x_max, y_max)) = inked else {
            return Some(meta(0, 0, 0.0, 0.0));
        };
        let pad = FIELD_PAD as f32;
        let ink_w = ((x_max - x_min) * scale).ceil().max(0.0);
        let ink_h = bounds
            .bounds
            .map(|(_, y0, _, y1)| ((y1 - y0) * scale).ceil().max(0.0))
            .unwrap_or(0.0);
        if ink_w == 0.0 || ink_h == 0.0 {
            return Some(meta(0, 0, x_min * scale - pad, y_max * scale + pad));
        }
        let width = ink_w as u32 + 2 * FIELD_PAD;
        let height = ink_h as u32 + 2 * FIELD_PAD;

        self.build_outline(&face, req.glyph, scale, x_min, y_max)?;
        self.color_edges();
        self.flatten();
        if self.segs.is_empty() {
            return Some(meta(0, 0, x_min * scale - pad, y_max * scale + pad));
        }
        self.build_grid(width, height);
        self.encode(width, height, out);

        Some(meta(
            width,
            height,
            x_min * scale - pad,
            y_max * scale + pad,
        ))
    }

    /// Walk the outline into `curves`/`contours`, mapping design units to the
    /// padded, y-down field.
    fn build_outline(
        &mut self,
        face: &Face<'_>,
        glyph: u16,
        scale: f32,
        x_min: f32,
        y_max: f32,
    ) -> Option<()> {
        self.curves.clear();
        self.contours.clear();
        let mut sink = CurveSink {
            curves: &mut self.curves,
            contours: &mut self.contours,
            scale,
            x_min,
            y_max,
            pad: FIELD_PAD as f32,
            last: V::ZERO,
            start: V::ZERO,
            open: None,
        };
        face.outline_glyph(GlyphId(glyph), &mut sink)?;
        sink.finish_contour();
        Some(())
    }

    /// Assign each curve its channel mask — msdfgen's simple edge coloring: the
    /// contour is cut at its corners and consecutive corner-to-corner splines
    /// take different pairs of channels, so at a corner two channels change sign
    /// independently and their median reconstructs the wedge instead of rounding
    /// it.
    fn color_edges(&mut self) {
        self.curve_colors.clear();
        self.curve_colors.resize(self.curves.len(), WHITE);
        let contours = core::mem::take(&mut self.contours);
        let mut seed: u64 = 0;
        for &(start, end) in &contours {
            let n = end - start;
            if n == 0 {
                continue;
            }
            self.corners.clear();
            for i in 0..n {
                let prev = self.curves[start + (i + n - 1) % n].end_dir().norm();
                let cur = self.curves[start + i].start_dir().norm();
                if prev.dot(cur) <= 0.0 || prev.cross(cur).abs() > CORNER_CROSS {
                    self.corners.push(i);
                }
            }
            match self.corners.len() {
                // A smooth closed contour (an 'o', a bowl) has no corner to
                // preserve: one channel group is the whole shape, and the field
                // degenerates to the true distance in all three channels.
                0 => {
                    for i in 0..n {
                        self.curve_colors[start + i] = WHITE;
                    }
                }
                // A teardrop — one corner, e.g. a comma. Splitting it in three
                // keeps the corner between two differing groups; the middle
                // third stays white so the smooth side has no seam.
                1 => {
                    let mut first = WHITE;
                    switch_color(&mut first, &mut seed, BLACK);
                    let mut third = first;
                    switch_color(&mut third, &mut seed, BLACK);
                    let colors = [first, WHITE, third];
                    let corner = self.corners[0];
                    for i in 0..n {
                        let at = start + (corner + i) % n;
                        self.curve_colors[at] = colors[(1 + trichotomy(i, n)) as usize];
                    }
                }
                _ => {
                    let mut color = WHITE;
                    switch_color(&mut color, &mut seed, BLACK);
                    let initial = color;
                    let corner = self.corners[0];
                    let last = self.corners.len() - 1;
                    let mut spline = 0;
                    for i in 0..n {
                        let local = (corner + i) % n;
                        if spline < last && self.corners[spline + 1] == local {
                            spline += 1;
                            let banned = if spline == last { initial } else { BLACK };
                            switch_color(&mut color, &mut seed, banned);
                        }
                        self.curve_colors[start + local] = color;
                    }
                }
            }
        }
        self.contours = contours;
    }

    /// Flatten every colored curve into `segs`.
    fn flatten(&mut self) {
        self.segs.clear();
        for (curve, &color) in self.curves.iter().zip(self.curve_colors.iter()) {
            match *curve {
                Curve::Line(a, b) => push_seg(&mut self.segs, a, b, color),
                Curve::Quad(a, c, b) => {
                    let n = steps(a.sub(c).len() + c.sub(b).len());
                    let mut prev = a;
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let p = quad_at(a, c, b, t);
                        push_seg(&mut self.segs, prev, p, color);
                        prev = p;
                    }
                }
                Curve::Cubic(a, c0, c1, b) => {
                    let n = steps(a.sub(c0).len() + c0.sub(c1).len() + c1.sub(b).len());
                    let mut prev = a;
                    for i in 1..=n {
                        let t = i as f32 / n as f32;
                        let p = cubic_at(a, c0, c1, b, t);
                        push_seg(&mut self.segs, prev, p, color);
                        prev = p;
                    }
                }
            }
        }
    }

    /// Bucket segments into a uniform grid so a sample tests the handful of
    /// segments that can be nearest rather than the whole outline.
    fn build_grid(&mut self, width: u32, height: u32) {
        let cols = grid_dim(width);
        let rows = grid_dim(height);
        self.cell_start.clear();
        self.cell_start.resize(cols * rows + 1, 0);
        for seg in &self.segs {
            for cell in cells_of(seg, cols, rows) {
                self.cell_start[cell + 1] += 1;
            }
        }
        for i in 0..cols * rows {
            self.cell_start[i + 1] += self.cell_start[i];
        }
        let total = self.cell_start[cols * rows] as usize;
        self.cell_cursor.clear();
        self.cell_cursor
            .extend_from_slice(&self.cell_start[..cols * rows]);
        self.cell_items.clear();
        self.cell_items.resize(total, 0);
        for (i, seg) in self.segs.iter().enumerate() {
            for cell in cells_of(seg, cols, rows) {
                let at = self.cell_cursor[cell] as usize;
                self.cell_items[at] = i as u32;
                self.cell_cursor[cell] += 1;
            }
        }
    }

    /// Sample the field and write the encoded RGBA8 texels.
    fn encode(&mut self, width: u32, height: u32, out: &mut Vec<u8>) {
        let cols = grid_dim(width);
        let rows = grid_dim(height);
        // Font outlines wind consistently, but which winding means "inside"
        // depends on the format and on the y-flip into image space. The total
        // signed area settles it once for the whole glyph, so the per-channel
        // pseudo distances all agree on the sign of the interior.
        let orient = if signed_area(&self.segs) >= 0.0 {
            1.0
        } else {
            -1.0
        };
        out.reserve((width as usize) * (height as usize) * 4);
        for py in 0..height {
            let y = py as f32 + 0.5;
            self.scan_row(y);
            let mut crossing = 0usize;
            let mut winding = 0i32;
            let cy = (y / CELL) as usize;
            for px in 0..width {
                let p = V {
                    x: px as f32 + 0.5,
                    y,
                };
                while crossing < self.crossings.len() && self.crossings[crossing].0 <= p.x {
                    winding += self.crossings[crossing].1;
                    crossing += 1;
                }
                let inside = if winding != 0 { 1.0 } else { -1.0 };

                let mut best = [Cand::NONE; 3];
                let mut nearest = f32::INFINITY;
                let cx = (p.x / CELL) as usize;
                for gy in cy.saturating_sub(1)..(cy + 2).min(rows) {
                    for gx in cx.saturating_sub(1)..(cx + 2).min(cols) {
                        let cell = gy * cols + gx;
                        let lo = self.cell_start[cell] as usize;
                        let hi = self.cell_start[cell + 1] as usize;
                        for &index in &self.cell_items[lo..hi] {
                            let seg = self.segs[index as usize];
                            let cand = measure(seg, p);
                            if cand.dist < nearest {
                                nearest = cand.dist;
                            }
                            for (channel, slot) in best.iter_mut().enumerate() {
                                if seg.color & (1 << channel) != 0 && cand.closer_than(*slot) {
                                    *slot = cand;
                                }
                            }
                        }
                    }
                }

                // A channel with no edge in reach saturates: beyond twice the
                // distance range the encoding clamps anyway, and the winding
                // number is the authority on which way it clamps.
                let mut value = [0.0f32; 4];
                for (channel, slot) in best.iter().enumerate() {
                    value[channel] = if slot.dist.is_finite() {
                        orient * slot.pseudo
                    } else {
                        inside * DISTANCE_RANGE
                    };
                }
                value[3] = if nearest.is_finite() {
                    inside * nearest
                } else {
                    inside * DISTANCE_RANGE
                };

                // Error correction (§13.5): the multi-channel median and the true
                // distance may legitimately differ in magnitude — that is the
                // corner being reconstructed — but never in sign. Where they do,
                // the median is an interpolation artifact and the true distance
                // wins.
                let median = median3(value[0], value[1], value[2]);
                if (median >= 0.0) != (value[3] >= 0.0) {
                    value[0] = value[3];
                    value[1] = value[3];
                    value[2] = value[3];
                }
                out.extend_from_slice(&[
                    encode(value[0]),
                    encode(value[1]),
                    encode(value[2]),
                    encode(value[3]),
                ]);
            }
        }
    }

    /// Collect the outline crossings of scanline `y`, sorted by x, so insideness
    /// along the row is one forward walk instead of a point-in-polygon test per
    /// texel.
    fn scan_row(&mut self, y: f32) {
        self.crossings.clear();
        for seg in &self.segs {
            let (lo, hi) = (seg.a.y.min(seg.b.y), seg.a.y.max(seg.b.y));
            if y < lo || y >= hi {
                continue;
            }
            let t = (y - seg.a.y) / (seg.b.y - seg.a.y);
            let x = seg.a.x + t * (seg.b.x - seg.a.x);
            self.crossings
                .push((x, if seg.b.y > seg.a.y { 1 } else { -1 }));
        }
        self.crossings.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    }
}

/// The nearest-edge candidate for one channel: msdfgen's `SignedDistance` plus
/// the pseudo distance to carry forward once the edge is selected.
#[derive(Debug, Clone, Copy)]
struct Cand {
    /// True (clamped-to-segment) distance, unsigned.
    dist: f32,
    /// Orthogonality tie-break: `0` when the sample projects onto the segment's
    /// interior, `|cos|` of the angle at an endpoint otherwise. A smaller value
    /// wins a tie, which is what keeps two segments meeting at a point from
    /// fighting over the sample.
    ortho: f32,
    /// Signed perpendicular distance to the segment's *infinite* line — the
    /// pseudo distance, which is what the channel stores so an edge group
    /// continues past its endpoints.
    pseudo: f32,
}

impl Cand {
    const NONE: Cand = Cand {
        dist: f32::INFINITY,
        ortho: f32::INFINITY,
        pseudo: 0.0,
    };

    fn closer_than(self, other: Cand) -> bool {
        self.dist < other.dist || (self.dist == other.dist && self.ortho < other.ortho)
    }
}

/// Distance of `p` to one segment, as a channel candidate.
fn measure(seg: Seg, p: V) -> Cand {
    let ab = seg.b.sub(seg.a);
    let ap = p.sub(seg.a);
    let len2 = ab.dot(ab);
    if len2 == 0.0 {
        return Cand {
            dist: ap.len(),
            ortho: f32::INFINITY,
            pseudo: 0.0,
        };
    }
    let len = len2.sqrt();
    let t = ap.dot(ab) / len2;
    let pseudo = ab.cross(ap) / len;
    if t > 0.0 && t < 1.0 {
        return Cand {
            dist: pseudo.abs(),
            ortho: 0.0,
            pseudo,
        };
    }
    let end = if t > 0.5 { seg.b } else { seg.a };
    let eq = p.sub(end);
    let dist = eq.len();
    Cand {
        dist,
        ortho: (ab.scale(1.0 / len).dot(eq.norm())).abs(),
        pseudo,
    }
}

/// Twice the signed area of the closed polylines, in field texels. Positive when
/// the winding is positive in the field's y-down space.
fn signed_area(segs: &[Seg]) -> f32 {
    segs.iter()
        .map(|s| s.a.x * s.b.y - s.b.x * s.a.y)
        .sum::<f32>()
}

/// Encode a signed distance in field texels to its stored byte.
fn encode(distance: f32) -> u8 {
    let unit = (distance / DISTANCE_RANGE + 0.5).clamp(0.0, 1.0);
    (unit * 255.0).round() as u8
}

/// Median of three — the multi-channel field's shape reconstruction.
fn median3(a: f32, b: f32, c: f32) -> f32 {
    a.max(b).min(a.min(b).max(c))
}

/// Grid cells along one axis of a `texels`-wide field.
fn grid_dim(texels: u32) -> usize {
    ((texels as f32 / CELL).ceil() as usize).max(1)
}

/// The grid cells a segment's bounding box touches.
fn cells_of(seg: &Seg, cols: usize, rows: usize) -> impl Iterator<Item = usize> {
    let cell = |v: f32, n: usize| ((v / CELL).floor().max(0.0) as usize).min(n - 1);
    let x0 = cell(seg.a.x.min(seg.b.x), cols);
    let x1 = cell(seg.a.x.max(seg.b.x), cols);
    let y0 = cell(seg.a.y.min(seg.b.y), rows);
    let y1 = cell(seg.a.y.max(seg.b.y), rows);
    (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| y * cols + x))
}

/// Push one flattened segment, dropping degenerate ones.
fn push_seg(segs: &mut Vec<Seg>, a: V, b: V, color: u8) {
    if a != b {
        segs.push(Seg { a, b, color });
    }
}

/// Segments one curve of this control-polygon length flattens into.
fn steps(length: f32) -> u32 {
    ((length / FLATTEN_STEP).ceil() as u32).clamp(1, FLATTEN_MAX)
}

fn quad_at(a: V, c: V, b: V, t: f32) -> V {
    let s = 1.0 - t;
    a.scale(s * s).add(c.scale(2.0 * s * t)).add(b.scale(t * t))
}

fn cubic_at(a: V, c0: V, c1: V, b: V, t: f32) -> V {
    let s = 1.0 - t;
    a.scale(s * s * s)
        .add(c0.scale(3.0 * s * s * t))
        .add(c1.scale(3.0 * s * t * t))
        .add(b.scale(t * t * t))
}

/// The first of two directions that is not degenerate.
fn first_nonzero(a: V, b: V) -> V {
    if a.x != 0.0 || a.y != 0.0 { a } else { b }
}

const BLACK: u8 = 0;
const RED: u8 = 1;
const GREEN: u8 = 2;
const YELLOW: u8 = 3;
const BLUE: u8 = 4;
const MAGENTA: u8 = 5;
const CYAN: u8 = 6;
const WHITE: u8 = 7;

/// Advance to the next edge color, never reusing `banned`'s single channel.
/// msdfgen's `switchColor`: a cheap deterministic rotation through the two-channel
/// colors, which is all the coloring needs — neighbouring splines only have to
/// differ, not follow a particular order.
fn switch_color(color: &mut u8, seed: &mut u64, banned: u8) {
    let combined = *color & banned;
    if combined == RED || combined == GREEN || combined == BLUE {
        *color = combined ^ WHITE;
        return;
    }
    if *color == BLACK || *color == WHITE {
        *color = [CYAN, MAGENTA, YELLOW][(*seed % 3) as usize];
        *seed /= 3;
        return;
    }
    let shifted = (*color as u32) << (1 + (*seed & 1));
    *color = ((shifted | shifted >> 3) & WHITE as u32) as u8;
    *seed >>= 1;
}

/// Which third of `n` position `i` falls in, as `-1`, `0`, `1` — msdfgen's
/// symmetrical trichotomy, used to cut a one-corner contour into three.
fn trichotomy(i: usize, n: usize) -> i32 {
    if n <= 1 {
        return 0;
    }
    (3.0 + 2.875 * i as f32 / (n - 1) as f32 - 1.4375 + 0.5) as i32 - 3
}

/// A point in field-texel space.
#[derive(Debug, Clone, Copy, PartialEq)]
struct V {
    x: f32,
    y: f32,
}

impl V {
    const ZERO: V = V { x: 0.0, y: 0.0 };

    fn add(self, o: V) -> V {
        V {
            x: self.x + o.x,
            y: self.y + o.y,
        }
    }

    fn sub(self, o: V) -> V {
        V {
            x: self.x - o.x,
            y: self.y - o.y,
        }
    }

    fn scale(self, k: f32) -> V {
        V {
            x: self.x * k,
            y: self.y * k,
        }
    }

    fn dot(self, o: V) -> f32 {
        self.x * o.x + self.y * o.y
    }

    fn cross(self, o: V) -> f32 {
        self.x * o.y - self.y * o.x
    }

    fn len(self) -> f32 {
        self.dot(self).sqrt()
    }

    fn norm(self) -> V {
        let len = self.len();
        if len == 0.0 {
            V::ZERO
        } else {
            self.scale(1.0 / len)
        }
    }
}

/// Walks a glyph outline into curves, mapping design units to the padded,
/// y-down field.
struct CurveSink<'a> {
    curves: &'a mut Vec<Curve>,
    contours: &'a mut Vec<(usize, usize)>,
    /// Design units to field texels.
    scale: f32,
    x_min: f32,
    y_max: f32,
    pad: f32,
    last: V,
    start: V,
    /// Index in `curves` where the open contour began.
    open: Option<usize>,
}

impl CurveSink<'_> {
    fn map(&self, x: f32, y: f32) -> V {
        V {
            x: (x - self.x_min) * self.scale + self.pad,
            y: (self.y_max - y) * self.scale + self.pad,
        }
    }

    /// Close the open contour, adding the implicit closing edge if the outline
    /// left one out, and record its curve range.
    fn finish_contour(&mut self) {
        let Some(from) = self.open.take() else { return };
        if self.last != self.start {
            self.curves.push(Curve::Line(self.last, self.start));
        }
        if self.curves.len() > from {
            self.contours.push((from, self.curves.len()));
        }
    }
}

impl OutlineBuilder for CurveSink<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish_contour();
        let p = self.map(x, y);
        self.last = p;
        self.start = p;
        self.open = Some(self.curves.len());
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.curves.push(Curve::Line(self.last, p));
        self.last = p;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let c = self.map(x1, y1);
        let p = self.map(x, y);
        self.curves.push(Curve::Quad(self.last, c, p));
        self.last = p;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let c0 = self.map(x1, y1);
        let c1 = self.map(x2, y2);
        let p = self.map(x, y);
        self.curves.push(Curve::Cubic(self.last, c0, c1, p));
        self.last = p;
    }

    fn close(&mut self) {
        self.finish_contour();
        self.last = self.start;
    }
}

/// A first-pass sink accumulating only the design-unit bounding box, so the
/// field size is known before the outline is walked for real.
#[derive(Default)]
struct BoundsSink {
    bounds: Option<(f32, f32, f32, f32)>,
}

impl BoundsSink {
    fn add(&mut self, x: f32, y: f32) {
        match &mut self.bounds {
            Some((x_min, y_min, x_max, y_max)) => {
                *x_min = x_min.min(x);
                *y_min = y_min.min(y);
                *x_max = x_max.max(x);
                *y_max = y_max.max(y);
            }
            none => *none = Some((x, y, x, y)),
        }
    }
}

impl OutlineBuilder for BoundsSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.add(x1, y1);
        self.add(x, y);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.add(x1, y1);
        self.add(x2, y2);
        self.add(x, y);
    }

    fn close(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster_a8::rasterize_coverage;

    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    /// 'A' — two diagonals and a crossbar, so every corner is sharp.
    fn glyph_a() -> u16 {
        glyph_of('A')
    }

    /// 'o' — all curve, no corner: the smooth counter-case.
    fn glyph_o() -> u16 {
        glyph_of('o')
    }

    fn glyph_of(ch: char) -> u16 {
        Face::parse(DEJAVU, 0)
            .expect("fixture parses")
            .glyph_index(ch)
            .expect("fixture covers the character")
            .0
    }

    fn request(glyph: u16, px_per_em: f32) -> MtsdfRequest<'static> {
        MtsdfRequest {
            sfnt: DEJAVU,
            index: 0,
            face: FontFaceId(7),
            glyph,
            revision: FontRevision(3),
            px_per_em,
        }
    }

    /// Bilinear sample of one channel of a field, at continuous texel
    /// coordinates whose integer+0.5 values land on texel centers.
    fn sample(field: &[u8], meta: &MtsdfGlyph, channel: usize, u: f32, v: f32) -> f32 {
        let at = |x: i32, y: i32| {
            let x = x.clamp(0, meta.width as i32 - 1) as usize;
            let y = y.clamp(0, meta.height as i32 - 1) as usize;
            field[(y * meta.width as usize + x) * 4 + channel] as f32 / 255.0
        };
        let (x, y) = (u - 0.5, v - 0.5);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as i32, y0 as i32);
        let top = at(x0, y0) * (1.0 - fx) + at(x0 + 1, y0) * fx;
        let bottom = at(x0, y0 + 1) * (1.0 - fx) + at(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }

    /// Reconstruct coverage from the field exactly the way the sampling lane
    /// does, over the pixel grid of a `rasterize_coverage` bitmap at
    /// `ratio * meta.px_per_em`. `channels` selects the multi-channel median or
    /// the true-distance channel alone.
    fn reconstruct(
        field: &[u8],
        meta: &MtsdfGlyph,
        ratio: f32,
        width: u32,
        height: u32,
        multi_channel: bool,
    ) -> Vec<f32> {
        let px_range = meta.distance_range * ratio;
        let mut out = Vec::with_capacity((width * height) as usize);
        for py in 0..height {
            for px in 0..width {
                let u = FIELD_PAD as f32 + (px as f32 + 0.5) / ratio;
                let v = FIELD_PAD as f32 + (py as f32 + 0.5) / ratio;
                let distance = if multi_channel {
                    median3(
                        sample(field, meta, 0, u, v),
                        sample(field, meta, 1, u, v),
                        sample(field, meta, 2, u, v),
                    )
                } else {
                    sample(field, meta, 3, u, v)
                };
                out.push(((distance - 0.5) * px_range + 0.5).clamp(0.0, 1.0));
            }
        }
        out
    }

    /// Mean absolute error of a field reconstruction against exact coverage at
    /// the same effective size.
    fn error_vs_coverage(glyph: u16, bucket: f32, ratio: f32, multi_channel: bool) -> f32 {
        let mut generator = MtsdfGenerator::default();
        let mut field = Vec::new();
        let meta = generator
            .generate(request(glyph, bucket), &mut field)
            .expect("fixture parses");
        let exact = rasterize_coverage(DEJAVU, 0, glyph, bucket * ratio).expect("fixture parses");
        let got = reconstruct(
            &field,
            &meta,
            ratio,
            exact.width,
            exact.height,
            multi_channel,
        );
        let sum: f32 = got
            .iter()
            .zip(exact.coverage.iter())
            .map(|(a, b)| (a - *b as f32 / 255.0).abs())
            .sum();
        sum / got.len() as f32
    }

    #[test]
    fn a_generated_field_reconstructs_coverage_at_the_bucket_scale() {
        // At 1:1 the field's own texels are the device pixels, so a distance
        // field reconstruction should land close to exact analytic coverage. The
        // residual is the edge band: a distance field antialiases by distance,
        // not by area, so a partially covered pixel differs by a few percent.
        for glyph in [glyph_a(), glyph_o()] {
            let error = error_vs_coverage(glyph, 32.0, 1.0, true);
            assert!(error < 0.03, "glyph {glyph} reconstruction error {error}");
        }
    }

    #[test]
    fn a_generated_field_reconstructs_coverage_at_the_window_edges() {
        // The quality window is the claim that one field serves a range of
        // scales (§13.5). Both edges must still reconstruct the glyph; if they
        // did not, the window would be a lie and the ladder would need more
        // buckets.
        for ratio in [WINDOW_MIN, WINDOW_MAX] {
            for glyph in [glyph_a(), glyph_o()] {
                let error = error_vs_coverage(glyph, 32.0, ratio, true);
                assert!(
                    error < 0.05,
                    "glyph {glyph} at {ratio}x window edge: error {error}"
                );
            }
        }
    }

    #[test]
    fn corners_survive_where_a_single_channel_rounds_them() {
        // This is what the multi-channel encoding buys, stated as a measurement:
        // magnified well past the bucket, the median of the three channels
        // reconstructs 'A's sharp apex and feet, while the same field's true
        // distance channel — a plain single-channel SDF — rounds them and drifts
        // further from exact coverage. Viso 1.0 ships no single-channel lane for
        // exactly this reason.
        let multi = error_vs_coverage(glyph_a(), 32.0, WINDOW_MAX, true);
        let single = error_vs_coverage(glyph_a(), 32.0, WINDOW_MAX, false);
        assert!(
            multi < single * 0.75,
            "multi-channel error {multi} should beat single-channel {single} at a corner"
        );
    }

    #[test]
    fn a_smooth_glyph_costs_the_multi_channel_encoding_nothing() {
        // A contour with no corner gets one white edge group, so all three
        // channels carry the true distance and the median degenerates to it.
        // Multi-channel is never worse than single-channel, only sometimes equal.
        let multi = error_vs_coverage(glyph_o(), 32.0, 1.0, true);
        let single = error_vs_coverage(glyph_o(), 32.0, 1.0, false);
        assert!((multi - single).abs() < 1e-6, "{multi} vs {single}");
    }

    #[test]
    fn generation_allocates_no_scratch_after_warm_up() {
        // Scratch reuse across generations is a hot-path contract (§28): the
        // first glyph sizes the buffers, later glyphs of the same bucket reuse
        // them. Warm up on the widest glyph so later ones fit.
        let mut generator = MtsdfGenerator::default();
        let mut field = Vec::new();
        for glyph in [glyph_a(), glyph_o()] {
            generator
                .generate(request(glyph, 64.0), &mut field)
                .expect("fixture parses");
        }
        let scratch = generator.scratch_bytes();
        let capacity = field.capacity();
        for glyph in [glyph_o(), glyph_a(), glyph_o()] {
            generator
                .generate(request(glyph, 64.0), &mut field)
                .expect("fixture parses");
            assert_eq!(generator.scratch_bytes(), scratch, "generator scratch grew");
            assert_eq!(capacity, field.capacity(), "output buffer reallocated");
        }
    }

    #[test]
    fn a_request_past_a_window_takes_the_next_bucket() {
        // Never stretch one field without bound (§13.5): each request lands on
        // the smallest bucket whose window reaches it, and stepping past a
        // window steps up a bucket rather than magnifying further.
        assert_eq!(plan(16.0), MtsdfPlan::Bucket(16.0));
        assert_eq!(plan(16.0 * WINDOW_MAX), MtsdfPlan::Bucket(16.0));
        assert_eq!(plan(16.0 * WINDOW_MAX + 0.1), MtsdfPlan::Bucket(32.0));
        // 64 px/em is still inside the 32 bucket's window, and the smaller field
        // is the cheaper one: the ladder never picks a bucket it does not need.
        assert_eq!(plan(64.0), MtsdfPlan::Bucket(32.0));
        assert_eq!(plan(32.0 * WINDOW_MAX + 0.1), MtsdfPlan::Bucket(64.0));
        // Under the smallest bucket a field only minifies, which is safe.
        assert_eq!(plan(4.0), MtsdfPlan::Bucket(16.0));
    }

    #[test]
    fn past_the_ladder_the_request_hands_off_to_the_outline() {
        // §13.6: past the top bucket's window there is no field to serve the
        // scale, so the caller falls back to the retained outline.
        let top = BUCKETS[BUCKETS.len() - 1];
        assert_eq!(plan(top * WINDOW_MAX), MtsdfPlan::Bucket(top));
        assert_eq!(plan(top * WINDOW_MAX + 0.1), MtsdfPlan::Outline);
        assert_eq!(plan(4096.0), MtsdfPlan::Outline);
    }

    #[test]
    fn the_planned_bucket_is_always_inside_the_field_window() {
        // `plan` and `MtsdfGlyph::in_window` must agree, or a field would be
        // admitted and then immediately rejected.
        let mut generator = MtsdfGenerator::default();
        let mut field = Vec::new();
        let mut px = 11.2;
        while px < BUCKETS[BUCKETS.len() - 1] * WINDOW_MAX {
            let MtsdfPlan::Bucket(bucket) = plan(px) else {
                panic!("{px} px/em should be servable by a bucket");
            };
            let meta = generator
                .generate(request(glyph_a(), bucket), &mut field)
                .expect("fixture parses");
            assert!(meta.in_window(px), "{px} px/em out of {bucket} window");
            px *= 1.07;
        }
    }

    #[test]
    fn field_geometry_carries_the_pad_and_the_source_resolution() {
        let mut generator = MtsdfGenerator::default();
        let mut field = Vec::new();
        let meta = generator
            .generate(request(glyph_a(), 32.0), &mut field)
            .expect("fixture parses");
        let ink = rasterize_coverage(DEJAVU, 0, glyph_a(), 32.0).expect("fixture parses");
        assert_eq!(meta.width, ink.width + 2 * FIELD_PAD);
        assert_eq!(meta.height, ink.height + 2 * FIELD_PAD);
        assert!((meta.left - (ink.left - FIELD_PAD as f32)).abs() < 1e-3);
        assert!((meta.top - (ink.top + FIELD_PAD as f32)).abs() < 1e-3);
        assert_eq!(field.len(), meta.byte_len());
        assert_eq!(meta.px_per_em, 32.0);
        assert_eq!(meta.distance_range, DISTANCE_RANGE);
        assert_eq!(meta.generator_revision, GENERATOR_REVISION);
        assert_eq!(meta.revision, FontRevision(3));
        assert_eq!(meta.face, FontFaceId(7));
        assert_eq!(meta.scale_for(64.0), 2.0);
        // The pad is wide enough that the field's border reads as fully outside:
        // otherwise the outside half of the distance ramp would be clipped and
        // the glyph edge would harden into the texture border. Only the median
        // and the true distance are claims about the shape — an individual
        // channel may saturate the other way where a distant edge group's line
        // extends past the corner, which is exactly what the median filters out.
        assert!(FIELD_PAD as f32 >= DISTANCE_RANGE / 2.0);
        for corner in [0, (meta.width as usize - 1) * 4] {
            let texel = &field[corner..corner + 4];
            let median = median3(texel[0] as f32, texel[1] as f32, texel[2] as f32);
            assert_eq!((median, texel[3]), (0.0, 0), "border texel {texel:?}");
        }
    }

    #[test]
    fn the_true_distance_channel_signs_the_interior() {
        // The alpha channel is a real signed distance: saturated inside the
        // stroke, zero well outside, and monotone across the edge between.
        let mut generator = MtsdfGenerator::default();
        let mut field = Vec::new();
        let meta = generator
            .generate(request(glyph_o(), 64.0), &mut field)
            .expect("fixture parses");
        let alpha = |x: u32, y: u32| field[((y * meta.width + x) as usize) * 4 + 3];
        let mid = meta.height / 2;
        let mut row: Vec<u8> = (0..meta.width).map(|x| alpha(x, mid)).collect();
        assert_eq!(row[0], 0, "outside the left sidebearing");
        assert_eq!(*row.iter().max().expect("non-empty row"), 255);
        // Across the left stem the distance rises then falls — never flat noise.
        row.truncate(meta.width as usize / 4);
        assert!(row.windows(2).any(|w| w[1] > w[0]));
    }

    #[test]
    fn an_empty_glyph_yields_an_empty_field() {
        let mut generator = MtsdfGenerator::default();
        let mut field = Vec::new();
        let meta = generator
            .generate(request(0, 64.0), &mut field)
            .expect("fixture parses");
        assert!(meta.is_empty());
        assert_eq!(meta.byte_len(), 0);
        assert!(field.is_empty());
    }

    #[test]
    fn bad_bytes_do_not_parse() {
        let mut generator = MtsdfGenerator::default();
        let mut field = vec![9u8; 4];
        let mut request = request(glyph_a(), 32.0);
        request.sfnt = b"not a font";
        assert!(generator.generate(request, &mut field).is_none());
        assert!(field.is_empty(), "the output buffer is cleared regardless");
    }

    #[test]
    fn edge_coloring_splits_at_corners() {
        // A cut contour must alternate colors at its corners: that is the whole
        // mechanism. Verified through the coloring itself rather than the pixels,
        // so a regression names its cause.
        let mut generator = MtsdfGenerator::default();
        let face = Face::parse(DEJAVU, 0).expect("fixture parses");
        generator
            .build_outline(
                &face,
                glyph_a(),
                32.0 / face.units_per_em() as f32,
                0.0,
                0.0,
            )
            .expect("glyph outlines");
        generator.color_edges();
        let colors = &generator.curve_colors;
        assert!(colors.len() > 2);
        assert!(colors.iter().all(|c| *c != BLACK));
        assert!(
            colors.iter().any(|c| *c != colors[0]),
            "'A' has corners, so its edges cannot all share one color"
        );
        // Two-channel colors, so every pair of neighbours shares exactly one
        // channel: that shared channel is what stays continuous across a corner.
        assert!(colors.iter().all(|c| c.count_ones() == 2 || *c == WHITE));
    }

    #[test]
    fn a_smooth_contour_keeps_one_edge_group() {
        let mut generator = MtsdfGenerator::default();
        let face = Face::parse(DEJAVU, 0).expect("fixture parses");
        generator
            .build_outline(
                &face,
                glyph_o(),
                32.0 / face.units_per_em() as f32,
                0.0,
                0.0,
            )
            .expect("glyph outlines");
        generator.color_edges();
        assert!(generator.curve_colors.iter().all(|c| *c == WHITE));
    }
}
