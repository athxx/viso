//! Exact-coverage A8 rasterization: turn one glyph outline into an 8-bit
//! coverage bitmap, the MaskA8 representation's raster product.
//!
//! This is the default text lane. Stable UI, CJK, and editor text render as
//! exact coverage — the sharpest edges and smallest footprint, with the
//! simplest fragment shader — rather than being fed through an SDF. The outline
//! comes from `ttf-parser`; the analytic coverage accumulation comes from
//! `ab_glyph_rasterizer`. The result is a CPU-side bitmap with no caching, no
//! atlas, and no residency knowledge: producing pixels and owning where they
//! live are separate lifecycles, and the latter belongs to the caller and to
//! `viso-render`.
//!
//! Font outlines are y-up in design units; the rasterizer's image space is
//! y-down. The sink scales design units to device pixels, translates the glyph
//! bounding box to the image origin, and flips y as it walks the outline.

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use ttf_parser::{Face, GlyphId, OutlineBuilder};

/// A CPU-side A8 coverage bitmap: one glyph rasterized at a device size.
///
/// Coverage is row-major, one byte per pixel, `0..=255`. The bitmap owns no GPU
/// memory; the caller uploads it into an atlas page and records residency.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageBitmap {
    /// Bitmap width in device pixels.
    pub width: u32,
    /// Bitmap height in device pixels.
    pub height: u32,
    /// Left edge of the bitmap relative to the pen origin, in device pixels
    /// (the glyph bounding box's `x_min`). Render uses it to place the quad.
    pub left: f32,
    /// Top edge of the bitmap relative to the pen origin, in device pixels
    /// (the glyph bounding box's `y_max`, y-up). Render uses it to place the
    /// quad.
    pub top: f32,
    /// Coverage samples, `width * height` bytes, row-major, top row first.
    pub coverage: Vec<u8>,
}

impl CoverageBitmap {
    /// The upload candidate size in bytes: one byte per covered pixel.
    pub fn byte_len(&self) -> usize {
        self.coverage.len()
    }

    /// Whether the glyph rasterized to no pixels (a space or empty outline).
    pub fn is_empty(&self) -> bool {
        self.coverage.is_empty()
    }
}

/// Walks a glyph outline into the rasterizer, mapping design units to the
/// image's device-pixel, y-down space.
struct OutlineSink<'a> {
    rasterizer: &'a mut Rasterizer,
    /// Design units to device pixels: `px_per_em / units_per_em`.
    scale: f32,
    /// Bounding box origin in design units, subtracted so the image starts at
    /// pixel (0, 0). `x_min` is the left edge; `y_max` is the top edge, which
    /// maps to image row 0 as the outline is flipped y-up -> y-down.
    x_min: f32,
    y_max: f32,
    /// The subpath's current point and start, kept so `close` draws the closing
    /// edge back to the start.
    last: Point,
    start: Point,
}

impl OutlineSink<'_> {
    /// Map a design-unit (y-up) point to the y-down image point. The box's
    /// `y_max` maps to image row 0, and design y decreases downward.
    fn map(&self, x: f32, y: f32) -> Point {
        let px = (x - self.x_min) * self.scale;
        let py = (self.y_max - y) * self.scale;
        point(px, py)
    }
}

impl OutlineBuilder for OutlineSink<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.last = p;
        self.start = p;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.map(x, y);
        self.rasterizer.draw_line(self.last, p);
        self.last = p;
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let c = self.map(x1, y1);
        let p = self.map(x, y);
        self.rasterizer.draw_quad(self.last, c, p);
        self.last = p;
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let c0 = self.map(x1, y1);
        let c1 = self.map(x2, y2);
        let p = self.map(x, y);
        self.rasterizer.draw_cubic(self.last, c0, c1, p);
        self.last = p;
    }

    fn close(&mut self) {
        self.rasterizer.draw_line(self.last, self.start);
        self.last = self.start;
    }
}

/// Rasterize one glyph of a face to exact A8 coverage at `px_per_em`.
///
/// `sfnt` and `index` are owned face bytes and face index; `glyph` is the glyph
/// index within the face; `px_per_em` is device pixels per em (logical size
/// times device scale). Returns `None` when the bytes do not parse as a usable
/// face. A glyph with no outline (a space, or `.notdef` with no contour)
/// rasterizes to a zero-area bitmap, which is `Some`, not a failure.
pub fn rasterize_coverage(
    sfnt: &[u8],
    index: u32,
    glyph: u16,
    px_per_em: f32,
) -> Option<CoverageBitmap> {
    let face = Face::parse(sfnt, index).ok()?;

    // CFF2 is a *variable* outline format: its charstrings carry only the default
    // master, and the true glyph shape is that master plus per-axis `blend`
    // deltas driven by the variation tables (`fvar`/`avar`/`CFF2` item-variation
    // store). A system face reassembled from CoreText tables at a fixed instance
    // has those deltas baked into the live handle, not into the sfnt bytes, and a
    // generic parser drawing the bare default master produces a *wrong* shape (a
    // different glyph, not the shaped one) — the Devanagari `.SFDevanagari`
    // regression, where the default master rendered as tofu-like ink. This static
    // fast path only faithfully serves non-variable outlines (`glyf`/`CFF`);
    // refuse a CFF2 face so the caller falls through to the authoritative
    // platform rasterizer, which honours the instance.
    if face.tables().cff2.is_some() {
        return None;
    }

    let upem = face.units_per_em() as f32;
    let scale = px_per_em / upem;

    // First pass over the outline yields the design-unit bounding box; a glyph
    // with no contour (space) has no box and rasterizes to zero area.
    let mut measure = BoundsSink::default();
    let Some(_) = face.outline_glyph(GlyphId(glyph), &mut measure) else {
        return Some(CoverageBitmap {
            width: 0,
            height: 0,
            left: 0.0,
            top: 0.0,
            coverage: Vec::new(),
        });
    };
    let Some((x_min, y_min, x_max, y_max)) = measure.bounds else {
        return Some(CoverageBitmap {
            width: 0,
            height: 0,
            left: 0.0,
            top: 0.0,
            coverage: Vec::new(),
        });
    };

    let width = ((x_max - x_min) * scale).ceil().max(0.0) as u32;
    let height = ((y_max - y_min) * scale).ceil().max(0.0) as u32;
    if width == 0 || height == 0 {
        return Some(CoverageBitmap {
            width: 0,
            height: 0,
            left: x_min * scale,
            top: y_max * scale,
            coverage: Vec::new(),
        });
    }

    let mut rasterizer = Rasterizer::new(width as usize, height as usize);
    let start = point(0.0, 0.0);
    let mut sink = OutlineSink {
        rasterizer: &mut rasterizer,
        scale,
        x_min,
        y_max,
        last: start,
        start,
    };
    // The bounding box is known to exist, so the outline walk succeeds again.
    face.outline_glyph(GlyphId(glyph), &mut sink)?;

    let mut coverage = vec![0u8; (width * height) as usize];
    rasterizer.for_each_pixel_2d(|x, y, cov| {
        coverage[(y * width + x) as usize] = (cov * 255.0).round().clamp(0.0, 255.0) as u8;
    });

    Some(CoverageBitmap {
        width,
        height,
        left: x_min * scale,
        top: y_max * scale,
        coverage,
    })
}

/// A first-pass sink that only accumulates the design-unit bounding box of an
/// outline, so the image size is known before the coverage walk.
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

    /// A subset of DejaVu Sans carrying the Latin letters the goldens use.
    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    #[test]
    fn covered_glyph_rasterizes_to_nonempty_coverage() {
        // 'A' is glyph 34 in the fixture. At 64 px/em it produces a bitmap with
        // both fully-covered interior pixels and empty corners, proving it is
        // exact coverage rather than an all-on or all-off fill.
        let bmp = rasterize_coverage(DEJAVU, 0, 34, 64.0).expect("fixture parses");
        assert!(bmp.width > 0 && bmp.height > 0);
        assert_eq!(bmp.byte_len(), (bmp.width * bmp.height) as usize);

        let max = *bmp.coverage.iter().max().unwrap();
        let min = *bmp.coverage.iter().min().unwrap();
        assert_eq!(max, 255, "a covered stroke has fully-opaque pixels");
        assert_eq!(min, 0, "the bounding box has empty pixels around the glyph");

        // The top-left corner of 'A' is outside the diagonal stroke, so empty.
        assert_eq!(bmp.coverage[0], 0);
    }

    #[test]
    fn coverage_bitmap_byte_len_matches_dims() {
        let bmp = rasterize_coverage(DEJAVU, 0, 55, 48.0).expect("fixture parses");
        assert_eq!(bmp.byte_len(), (bmp.width * bmp.height) as usize);
        assert_eq!(bmp.coverage.len(), bmp.byte_len());
    }

    #[test]
    fn empty_glyph_has_zero_area() {
        // Glyph 0 (.notdef) in this subset carries no contour, the empty-outline
        // case: it rasterizes to a zero-area bitmap rather than failing.
        let bmp = rasterize_coverage(DEJAVU, 0, 0, 64.0).expect("fixture parses");
        assert_eq!(bmp.width, 0);
        assert_eq!(bmp.height, 0);
        assert_eq!(bmp.byte_len(), 0);
        assert!(bmp.is_empty());
    }

    #[test]
    fn bad_bytes_do_not_parse() {
        assert!(rasterize_coverage(b"not a font", 0, 34, 64.0).is_none());
    }

    #[test]
    fn larger_px_per_em_yields_larger_bitmap() {
        // Exact coverage scales with device size: doubling px/em roughly doubles
        // each dimension (within rounding), confirming the scale is applied.
        let small = rasterize_coverage(DEJAVU, 0, 34, 32.0).expect("fixture parses");
        let large = rasterize_coverage(DEJAVU, 0, 34, 64.0).expect("fixture parses");
        assert!(large.width > small.width);
        assert!(large.height > small.height);
    }
}
