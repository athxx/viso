//! Portable color-glyph rasterization: turn one color glyph into a
//! premultiplied RGBA8 bitmap, the ColorRgba8 representation's raster product,
//! from the face bytes alone.
//!
//! A host whose platform raster cannot draw a face's color glyphs (Linux,
//! Windows and Android faces the platform did not open, or any application
//! face) still draws them here. Two sources are read, in the order a
//! renderer prefers them:
//!
//! - `COLR` (v0 layers and v1 paint graphs): painted in software at the
//!   requested size — solid fills, linear / radial / sweep gradients with every
//!   extend mode, clip boxes, transforms, and every composite mode.
//! - Bitmap strikes (`sbix`, `CBDT`): the nearest strike is decoded (PNG or
//!   premultiplied BGRA) and resampled to the requested size, so a glyph always
//!   comes back at exactly `pixels_per_em`.
//!
//! Font space is y-up in design units; the bitmap is y-down, top row first.
//! [`ColorGlyph::origin_px`] is the bitmap's bottom-left corner relative to the
//! pen origin in y-up pixels.

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use ttf_parser::colr::{
    ClipBox, CompositeMode, GradientExtend, LinearGradient, Paint, Painter, RadialGradient,
    SweepGradient,
};
use ttf_parser::{Face, GlyphId, OutlineBuilder, RasterImageFormat, RgbaColor, Transform};

use crate::system_fonts::ColorGlyph;

/// The largest bitmap edge, in pixels, a color glyph is painted at. A font
/// whose paint graph reaches far past its em (or a corrupt one) is refused
/// rather than allocating an unbounded canvas.
const MAX_EDGE: u32 = 2048;

/// Rasterize one color glyph of a face at `pixels_per_em`.
///
/// `sfnt` and `index` are the face bytes and face index; `glyph` is the glyph
/// index. `COLR` is preferred over bitmap strikes. `None` when the face does not
/// parse, the glyph has no color representation, or it paints nothing.
pub fn rasterize_color(
    sfnt: &[u8],
    index: u32,
    glyph: u16,
    pixels_per_em: u16,
) -> Option<ColorGlyph> {
    let face = Face::parse(sfnt, index).ok()?;
    let pixels_per_em = pixels_per_em.max(1);
    let gid = GlyphId(glyph);
    if face.is_color_glyph(gid)
        && let Some(painted) = paint_colr(&face, gid, pixels_per_em)
    {
        return Some(painted);
    }
    strike(&face, gid, pixels_per_em)
}

/// Whether the face carries any color-glyph table this module reads.
pub fn has_color_tables(face: &Face<'_>) -> bool {
    let tables = face.tables();
    tables.colr.is_some() || tables.sbix.is_some() || tables.cbdt.is_some()
}

// ---------------------------------------------------------------------------
// COLR

fn paint_colr(face: &Face<'_>, gid: GlyphId, pixels_per_em: u16) -> Option<ColorGlyph> {
    let scale = f32::from(pixels_per_em) / f32::from(face.units_per_em().max(1));
    let foreground = RgbaColor::new(0, 0, 0, 255);

    // Pass 1: the device-pixel (y-up) bounds of everything the graph outlines.
    let mut bounds = BoundsPass {
        face,
        transforms: vec![Transform::new_scale(scale, scale)],
        ink: Extent::EMPTY,
        clip: Extent::EMPTY,
    };
    face.paint_color_glyph(gid, 0, foreground, &mut bounds)?;
    let extent = if bounds.ink.is_empty() {
        bounds.clip
    } else {
        bounds.ink
    };
    if extent.is_empty() {
        return None;
    }
    // One pixel of margin keeps every edge the rasterizer accumulates inside
    // its rows.
    let left = extent.x0.floor() - 1.0;
    let bottom = extent.y0.floor() - 1.0;
    let right = extent.x1.ceil() + 1.0;
    let top = extent.y1.ceil() + 1.0;
    let width = (right - left) as u32;
    let height = (top - bottom) as u32;
    if width == 0 || height == 0 || width > MAX_EDGE || height > MAX_EDGE {
        return None;
    }

    // Pass 2: paint into a canvas whose base transform maps design units to
    // y-down canvas pixels.
    let base = Transform::new(scale, 0.0, 0.0, -scale, -left, top);
    let mut canvas = Canvas {
        face,
        width: width as usize,
        height: height as usize,
        transforms: vec![base],
        outline: None,
        clips: Vec::new(),
        layers: vec![(
            vec![[0.0; 4]; (width * height) as usize],
            CompositeMode::SourceOver,
        )],
    };
    face.paint_color_glyph(gid, 0, foreground, &mut canvas)?;
    let pixels = canvas.layers.swap_remove(0).0;
    finish(&pixels, width, height, [left, bottom], pixels_per_em)
}

/// A y-up device-pixel box.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Extent {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Extent {
    const EMPTY: Self = Self {
        x0: f32::INFINITY,
        y0: f32::INFINITY,
        x1: f32::NEG_INFINITY,
        y1: f32::NEG_INFINITY,
    };

    fn is_empty(&self) -> bool {
        !(self.x0 < self.x1 && self.y0 < self.y1)
    }

    fn add(&mut self, x: f32, y: f32) {
        if x.is_finite() && y.is_finite() {
            self.x0 = self.x0.min(x);
            self.y0 = self.y0.min(y);
            self.x1 = self.x1.max(x);
            self.y1 = self.y1.max(y);
        }
    }
}

fn apply(t: &Transform, x: f32, y: f32) -> (f32, f32) {
    (t.a * x + t.c * y + t.e, t.b * x + t.d * y + t.f)
}

fn invert(t: &Transform) -> Option<Transform> {
    let det = t.a * t.d - t.b * t.c;
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let inv = 1.0 / det;
    let a = t.d * inv;
    let b = -t.b * inv;
    let c = -t.c * inv;
    let d = t.a * inv;
    Some(Transform::new(
        a,
        b,
        c,
        d,
        -(a * t.e + c * t.f),
        -(b * t.e + d * t.f),
    ))
}

/// Accumulates the transformed control points of every outline the graph
/// draws (and, for a graph that outlines nothing, its clip boxes).
struct BoundsPass<'f, 'a> {
    face: &'f Face<'a>,
    transforms: Vec<Transform>,
    ink: Extent,
    clip: Extent,
}

impl BoundsPass<'_, '_> {
    fn current(&self) -> Transform {
        self.transforms.last().copied().unwrap_or_default()
    }
}

struct ExtentSink<'e> {
    transform: Transform,
    extent: &'e mut Extent,
}

impl ExtentSink<'_> {
    fn add(&mut self, x: f32, y: f32) {
        let (x, y) = apply(&self.transform, x, y);
        self.extent.add(x, y);
    }
}

impl OutlineBuilder for ExtentSink<'_> {
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

impl<'a> Painter<'a> for BoundsPass<'_, 'a> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        let mut sink = ExtentSink {
            transform: self.current(),
            extent: &mut self.ink,
        };
        self.face.outline_glyph(glyph_id, &mut sink);
    }
    fn paint(&mut self, _: Paint<'a>) {}
    fn push_clip(&mut self) {}
    fn push_clip_box(&mut self, clipbox: ClipBox) {
        let t = self.current();
        for (x, y) in corners(&clipbox) {
            let (x, y) = apply(&t, x, y);
            self.clip.add(x, y);
        }
    }
    fn pop_clip(&mut self) {}
    fn push_layer(&mut self, _: CompositeMode) {}
    fn pop_layer(&mut self) {}
    fn push_transform(&mut self, transform: Transform) {
        let next = Transform::combine(self.current(), transform);
        self.transforms.push(next);
    }
    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }
}

fn corners(clip: &ClipBox) -> [(f32, f32); 4] {
    [
        (clip.x_min, clip.y_min),
        (clip.x_max, clip.y_min),
        (clip.x_max, clip.y_max),
        (clip.x_min, clip.y_max),
    ]
}

/// A coverage mask over the canvas, one `0.0..=1.0` sample per pixel.
type Mask = Vec<f32>;

/// Premultiplied linear RGBA, one sample per pixel.
type Layer = Vec<[f32; 4]>;

/// The software painter: a transform stack, the outline most recently set, a
/// clip-mask stack, and a layer stack each popped with its composite mode.
struct Canvas<'f, 'a> {
    face: &'f Face<'a>,
    width: usize,
    height: usize,
    transforms: Vec<Transform>,
    /// The outline `paint` fills in a v0 graph. A v1 graph turns it into a clip
    /// (`push_clip`) before painting, which consumes it.
    outline: Option<Mask>,
    clips: Vec<Mask>,
    layers: Vec<(Layer, CompositeMode)>,
}

/// Walks a path into the rasterizer through an affine transform to canvas px.
struct MaskSink<'r> {
    rasterizer: &'r mut Rasterizer,
    transform: Transform,
    last: Point,
    start: Point,
}

impl MaskSink<'_> {
    fn map(&self, x: f32, y: f32) -> Point {
        let (x, y) = apply(&self.transform, x, y);
        point(x, y)
    }
}

impl OutlineBuilder for MaskSink<'_> {
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

impl Canvas<'_, '_> {
    fn current(&self) -> Transform {
        self.transforms.last().copied().unwrap_or_default()
    }

    fn rasterize(&self, draw: impl FnOnce(&mut MaskSink<'_>)) -> Mask {
        let mut rasterizer = Rasterizer::new(self.width, self.height);
        let mut sink = MaskSink {
            rasterizer: &mut rasterizer,
            transform: self.current(),
            last: point(0.0, 0.0),
            start: point(0.0, 0.0),
        };
        draw(&mut sink);
        let mut mask = vec![0.0; self.width * self.height];
        rasterizer.for_each_pixel(|i, v| {
            if let Some(m) = mask.get_mut(i) {
                *m = v.abs().min(1.0);
            }
        });
        mask
    }

    /// Push `mask` intersected with the current clip.
    fn push_mask(&mut self, mut mask: Mask) {
        if let Some(top) = self.clips.last() {
            for (m, c) in mask.iter_mut().zip(top) {
                *m *= c;
            }
        }
        self.clips.push(mask);
    }

    /// Source-over `color_at(x, y)` onto the top layer wherever the current
    /// outline and clip cover.
    fn fill(&mut self, mut color_at: impl FnMut(f32, f32) -> Option<[f32; 4]>) {
        let outline = self.outline.take();
        let clip = self.clips.last();
        let width = self.width;
        let Some((layer, _)) = self.layers.last_mut() else {
            return;
        };
        for (i, dst) in layer.iter_mut().enumerate() {
            let mut cover = 1.0;
            if let Some(mask) = &outline {
                cover *= mask[i];
            }
            if let Some(mask) = clip {
                cover *= mask[i];
            }
            if cover <= 0.0 {
                continue;
            }
            let x = (i % width) as f32 + 0.5;
            let y = (i / width) as f32 + 0.5;
            let Some(src) = color_at(x, y) else {
                continue;
            };
            let sa = src[3] * cover;
            for c in 0..4 {
                dst[c] = src[c] * cover + dst[c] * (1.0 - sa);
            }
        }
    }

    /// Fill with a gradient evaluated in the gradient's own (design-unit) space:
    /// each canvas pixel center is mapped back through the current transform.
    fn fill_gradient(&mut self, stops: ColorLine, sample: impl Fn(f32, f32) -> Option<f32>) {
        if stops.stops.is_empty() {
            return;
        }
        let Some(inverse) = invert(&self.current()) else {
            return;
        };
        self.fill(|x, y| {
            let (gx, gy) = apply(&inverse, x, y);
            sample(gx, gy).map(|t| stops.at(t))
        });
    }
}

impl<'a> Painter<'a> for Canvas<'_, 'a> {
    fn outline_glyph(&mut self, glyph_id: GlyphId) {
        let face = self.face;
        let mask = self.rasterize(|sink| {
            face.outline_glyph(glyph_id, sink);
        });
        self.outline = Some(mask);
    }

    fn paint(&mut self, paint: Paint<'a>) {
        match paint {
            Paint::Solid(color) => {
                let color = premultiply(color);
                self.fill(|_, _| Some(color));
            }
            Paint::LinearGradient(gradient) => self.paint_linear(&gradient),
            Paint::RadialGradient(gradient) => self.paint_radial(&gradient),
            Paint::SweepGradient(gradient) => self.paint_sweep(&gradient),
        }
    }

    fn push_clip(&mut self) {
        let mask = self
            .outline
            .take()
            .unwrap_or_else(|| vec![1.0; self.width * self.height]);
        self.push_mask(mask);
    }

    fn push_clip_box(&mut self, clipbox: ClipBox) {
        let [p0, p1, p2, p3] = corners(&clipbox);
        let mask = self.rasterize(|sink| {
            sink.move_to(p0.0, p0.1);
            sink.line_to(p1.0, p1.1);
            sink.line_to(p2.0, p2.1);
            sink.line_to(p3.0, p3.1);
            sink.close();
        });
        self.push_mask(mask);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn push_layer(&mut self, mode: CompositeMode) {
        self.layers
            .push((vec![[0.0; 4]; self.width * self.height], mode));
    }

    fn pop_layer(&mut self) {
        if self.layers.len() < 2 {
            return;
        }
        let Some((source, mode)) = self.layers.pop() else {
            return;
        };
        if let Some((backdrop, _)) = self.layers.last_mut() {
            for (d, s) in backdrop.iter_mut().zip(&source) {
                *d = composite(mode, *s, *d);
            }
        }
    }

    fn push_transform(&mut self, transform: Transform) {
        let next = Transform::combine(self.current(), transform);
        self.transforms.push(next);
    }

    fn pop_transform(&mut self) {
        if self.transforms.len() > 1 {
            self.transforms.pop();
        }
    }
}

impl Canvas<'_, '_> {
    fn paint_linear(&mut self, gradient: &LinearGradient<'_>) {
        let stops = ColorLine::new(gradient.stops(0, &[]), gradient.extend);
        let (x0, y0) = (gradient.x0, gradient.y0);
        // The gradient runs from p0 toward p1 projected onto the normal of
        // p0→p2 (COLRv1 PaintLinearGradient).
        let (dx, dy) = (gradient.x2 - x0, gradient.y2 - y0);
        let (px, py) = (gradient.x1 - x0, gradient.y1 - y0);
        let (vx, vy) = if dx * dx + dy * dy > 1e-12 {
            let (nx, ny) = (dy, -dx);
            let k = (px * nx + py * ny) / (nx * nx + ny * ny);
            (nx * k, ny * k)
        } else {
            (px, py)
        };
        let len2 = vx * vx + vy * vy;
        if len2 <= 1e-12 {
            return;
        }
        self.fill_gradient(stops, |x, y| Some(((x - x0) * vx + (y - y0) * vy) / len2));
    }

    fn paint_radial(&mut self, gradient: &RadialGradient<'_>) {
        let stops = ColorLine::new(gradient.stops(0, &[]), gradient.extend);
        let (c0x, c0y, r0) = (gradient.x0, gradient.y0, gradient.r0);
        let (cdx, cdy) = (gradient.x1 - c0x, gradient.y1 - c0y);
        let dr = gradient.r1 - r0;
        let a = cdx * cdx + cdy * cdy - dr * dr;
        // Two-point conical: the largest t whose circle passes through the
        // point with a non-negative radius.
        self.fill_gradient(stops, move |x, y| {
            let (pdx, pdy) = (x - c0x, y - c0y);
            let b = pdx * cdx + pdy * cdy + r0 * dr;
            let c = pdx * pdx + pdy * pdy - r0 * r0;
            let valid = |t: f32| r0 + t * dr >= 0.0;
            if a.abs() < 1e-6 {
                if b.abs() < 1e-12 {
                    return None;
                }
                let t = c / (2.0 * b);
                return valid(t).then_some(t);
            }
            let disc = b * b - a * c;
            if disc < 0.0 {
                return None;
            }
            let root = disc.sqrt();
            let (t1, t2) = ((b + root) / a, (b - root) / a);
            let (hi, lo) = if t1 > t2 { (t1, t2) } else { (t2, t1) };
            if valid(hi) {
                Some(hi)
            } else if valid(lo) {
                Some(lo)
            } else {
                None
            }
        });
    }

    fn paint_sweep(&mut self, gradient: &SweepGradient<'_>) {
        let stops = ColorLine::new(gradient.stops(0, &[]), gradient.extend);
        let (cx, cy) = (gradient.center_x, gradient.center_y);
        // Biased F2DOT14 angles: `(value + 1.0) * 180°`, counter-clockwise in
        // y-up design space.
        let start = (gradient.start_angle + 1.0) * 180.0;
        let end = (gradient.end_angle + 1.0) * 180.0;
        let span = end - start;
        if span.abs() < 1e-6 {
            return;
        }
        self.fill_gradient(stops, move |x, y| {
            let angle = (y - cy).atan2(x - cx).to_degrees().rem_euclid(360.0);
            Some((angle - start) / span)
        });
    }
}

/// A gradient's stops, premultiplied and sorted, with its extend mode.
struct ColorLine {
    stops: Vec<(f32, [f32; 4])>,
    extend: GradientExtend,
}

impl ColorLine {
    fn new(
        stops: impl Iterator<Item = ttf_parser::colr::ColorStop>,
        extend: GradientExtend,
    ) -> Self {
        let mut stops: Vec<(f32, [f32; 4])> = stops
            .map(|stop| (stop.stop_offset, premultiply(stop.color)))
            .collect();
        stops.sort_by(|l, r| l.0.total_cmp(&r.0));
        Self { stops, extend }
    }

    fn at(&self, t: f32) -> [f32; 4] {
        let (Some(first), Some(last)) = (self.stops.first(), self.stops.last()) else {
            return [0.0; 4];
        };
        let span = last.0 - first.0;
        let t = if span <= 1e-6 || !t.is_finite() {
            t
        } else {
            match self.extend {
                GradientExtend::Pad => t,
                GradientExtend::Repeat => first.0 + (t - first.0).rem_euclid(span),
                GradientExtend::Reflect => {
                    let u = (t - first.0).rem_euclid(2.0 * span);
                    first.0 + if u > span { 2.0 * span - u } else { u }
                }
            }
        };
        if t.is_nan() || t <= first.0 {
            return first.1;
        }
        if t >= last.0 {
            return last.1;
        }
        for pair in self.stops.windows(2) {
            let (o0, c0) = pair[0];
            let (o1, c1) = pair[1];
            if t <= o1 {
                let w = if o1 - o0 > 1e-9 {
                    (t - o0) / (o1 - o0)
                } else {
                    1.0
                };
                return std::array::from_fn(|c| c0[c] + (c1[c] - c0[c]) * w);
            }
        }
        last.1
    }
}

fn premultiply(color: RgbaColor) -> [f32; 4] {
    let a = f32::from(color.alpha) / 255.0;
    [
        f32::from(color.red) / 255.0 * a,
        f32::from(color.green) / 255.0 * a,
        f32::from(color.blue) / 255.0 * a,
        a,
    ]
}

/// Composite premultiplied `s` onto premultiplied `d` with `mode` (the
/// Porter-Duff operators and the W3C separable / non-separable blend modes).
fn composite(mode: CompositeMode, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    use CompositeMode as M;
    let (sa, da) = (s[3], d[3]);
    let porter_duff =
        |fs: f32, fd: f32| -> [f32; 4] { std::array::from_fn(|c| s[c] * fs + d[c] * fd) };
    match mode {
        M::Clear => [0.0; 4],
        M::Source => s,
        M::Destination => d,
        M::SourceOver => porter_duff(1.0, 1.0 - sa),
        M::DestinationOver => porter_duff(1.0 - da, 1.0),
        M::SourceIn => porter_duff(da, 0.0),
        M::DestinationIn => porter_duff(0.0, sa),
        M::SourceOut => porter_duff(1.0 - da, 0.0),
        M::DestinationOut => porter_duff(0.0, 1.0 - sa),
        M::SourceAtop => porter_duff(da, 1.0 - sa),
        M::DestinationAtop => porter_duff(1.0 - da, sa),
        M::Xor => porter_duff(1.0 - da, 1.0 - sa),
        M::Plus => std::array::from_fn(|c| (s[c] + d[c]).min(1.0)),
        _ => {
            let unpremul = |p: [f32; 4]| -> [f32; 3] {
                if p[3] > 0.0 {
                    [p[0] / p[3], p[1] / p[3], p[2] / p[3]]
                } else {
                    [0.0; 3]
                }
            };
            let (cs, cb) = (unpremul(s), unpremul(d));
            let mixed = blend(mode, cs, cb);
            let alpha = sa + da - sa * da;
            let mut out = [0.0; 4];
            for c in 0..3 {
                out[c] = s[c] * (1.0 - da) + d[c] * (1.0 - sa) + sa * da * mixed[c];
            }
            out[3] = alpha;
            out
        }
    }
}

/// The blend function `B(Cs, Cb)` on unpremultiplied colors.
fn blend(mode: CompositeMode, cs: [f32; 3], cb: [f32; 3]) -> [f32; 3] {
    use CompositeMode as M;
    let separable =
        |f: fn(f32, f32) -> f32| -> [f32; 3] { std::array::from_fn(|c| f(cs[c], cb[c])) };
    match mode {
        M::Multiply => separable(|s, b| s * b),
        M::Screen => separable(screen),
        M::Overlay => separable(|s, b| hard_light(b, s)),
        M::Darken => separable(f32::min),
        M::Lighten => separable(f32::max),
        M::ColorDodge => separable(|s, b| {
            if b <= 0.0 {
                0.0
            } else if s >= 1.0 {
                1.0
            } else {
                (b / (1.0 - s)).min(1.0)
            }
        }),
        M::ColorBurn => separable(|s, b| {
            if b >= 1.0 {
                1.0
            } else if s <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - b) / s).min(1.0)
            }
        }),
        M::HardLight => separable(hard_light),
        M::SoftLight => separable(|s, b| {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 {
                    ((16.0 * b - 12.0) * b + 4.0) * b
                } else {
                    b.sqrt()
                };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }),
        M::Difference => separable(|s, b| (b - s).abs()),
        M::Exclusion => separable(|s, b| b + s - 2.0 * b * s),
        M::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        M::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        M::Color => set_lum(cs, lum(cb)),
        M::Luminosity => set_lum(cb, lum(cs)),
        _ => cs,
    }
}

fn screen(s: f32, b: f32) -> f32 {
    s + b - s * b
}

fn hard_light(s: f32, b: f32) -> f32 {
    if s <= 0.5 {
        b * 2.0 * s
    } else {
        screen(2.0 * s - 1.0, b)
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 && l - n > 1e-9 {
        out = out.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 && x - l > 1e-9 {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    out
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color(c.map(|v| v + d))
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut order = [0usize, 1, 2];
    order.sort_by(|&l, &r| c[l].total_cmp(&c[r]));
    let [lo, mid, hi] = order;
    let mut out = [0.0; 3];
    if c[hi] > c[lo] {
        out[mid] = (c[mid] - c[lo]) * s / (c[hi] - c[lo]);
        out[hi] = s;
    }
    out
}

// ---------------------------------------------------------------------------
// Bitmap strikes

fn strike(face: &Face<'_>, gid: GlyphId, pixels_per_em: u16) -> Option<ColorGlyph> {
    let image = face.glyph_raster_image(gid, pixels_per_em)?;
    let (width, height) = (u32::from(image.width), u32::from(image.height));
    if width == 0 || height == 0 {
        return None;
    }
    let rgba = match image.format {
        RasterImageFormat::PNG => decode_png(image.data, width, height)?,
        RasterImageFormat::BitmapPremulBgra32 => {
            let len = (width * height * 4) as usize;
            let data = image.data.get(..len)?;
            data.as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[2], p[1], p[0], p[3]])
                .collect()
        }
        _ => return None,
    };
    let scale = f32::from(pixels_per_em) / f32::from(image.pixels_per_em.max(1));
    let out_w = ((width as f32 * scale).round() as u32).clamp(1, MAX_EDGE);
    let out_h = ((height as f32 * scale).round() as u32).clamp(1, MAX_EDGE);
    let rgba = if out_w == width && out_h == height {
        rgba
    } else {
        resample(&rgba, width, height, out_w, out_h)
    };
    Some(ColorGlyph {
        width: out_w,
        height: out_h,
        rgba,
        pixels_per_em,
        // `sbix` origin offsets and `CBDT` (bearing_y - height) both name the
        // image's bottom-left in y-up strike pixels.
        origin_px: [f32::from(image.x) * scale, f32::from(image.y) * scale],
    })
}

/// Decode a strike's PNG to premultiplied RGBA8 of exactly `width × height`.
fn decode_png(data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    use zune_core::bytestream::ZCursor;
    use zune_core::options::DecoderOptions;
    let options = DecoderOptions::default()
        .png_set_add_alpha_channel(true)
        .png_set_strip_to_8bit(true);
    let mut decoder = zune_png::PngDecoder::new_with_options(ZCursor::new(data), options);
    let pixels = decoder.decode_raw().ok()?;
    let (w, h) = decoder.dimensions()?;
    if (w as u32, h as u32) != (width, height) || w == 0 || h == 0 {
        return None;
    }
    let channels = pixels.len() / (w * h);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for p in pixels.chunks_exact(channels.max(1)) {
        let (r, g, b, a) = match channels {
            4 => (p[0], p[1], p[2], p[3]),
            3 => (p[0], p[1], p[2], 255),
            2 => (p[0], p[0], p[0], p[1]),
            1 => (p[0], p[0], p[0], 255),
            _ => return None,
        };
        let premul = |v: u8| ((u16::from(v) * u16::from(a) + 127) / 255) as u8;
        rgba.extend_from_slice(&[premul(r), premul(g), premul(b), a]);
    }
    (rgba.len() == w * h * 4).then_some(rgba)
}

/// Per-output-sample source taps along one axis: an area average when
/// shrinking, a linear tent when enlarging.
fn taps(src: u32, dst: u32) -> Vec<Vec<(usize, f32)>> {
    let ratio = src as f32 / dst as f32;
    (0..dst)
        .map(|o| {
            let mut out = Vec::new();
            if ratio >= 1.0 {
                let lo = o as f32 * ratio;
                let hi = lo + ratio;
                let mut i = lo.floor() as u32;
                while (i as f32) < hi && i < src {
                    let w = (hi.min(i as f32 + 1.0) - lo.max(i as f32)).max(0.0);
                    if w > 0.0 {
                        out.push((i as usize, w / ratio));
                    }
                    i += 1;
                }
            } else {
                let center = (o as f32 + 0.5) * ratio - 0.5;
                let base = center.floor();
                let frac = center - base;
                let clamp = |i: f32| (i.max(0.0) as u32).min(src - 1) as usize;
                out.push((clamp(base), 1.0 - frac));
                out.push((clamp(base + 1.0), frac));
            }
            out
        })
        .collect()
}

/// Resample premultiplied RGBA8, separably.
fn resample(rgba: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let xs = taps(sw, dw);
    let ys = taps(sh, dh);
    let mut rows = vec![[0.0f32; 4]; (dw * sh) as usize];
    for y in 0..sh as usize {
        for (x, tap) in xs.iter().enumerate() {
            let mut acc = [0.0; 4];
            for &(i, w) in tap {
                let p = (y * sw as usize + i) * 4;
                for c in 0..4 {
                    acc[c] += f32::from(rgba[p + c]) * w;
                }
            }
            rows[y * dw as usize + x] = acc;
        }
    }
    let mut out = Vec::with_capacity((dw * dh * 4) as usize);
    for tap in &ys {
        for x in 0..dw as usize {
            let mut acc = [0.0; 4];
            for &(i, w) in tap {
                let p = rows[i * dw as usize + x];
                for c in 0..4 {
                    acc[c] += p[c] * w;
                }
            }
            let a = acc[3].round().clamp(0.0, 255.0);
            for channel in &acc[..3] {
                out.push(channel.round().clamp(0.0, a) as u8);
            }
            out.push(a as u8);
        }
    }
    out
}

/// Quantize the painted canvas, trimming fully transparent borders.
fn finish(
    pixels: &[[f32; 4]],
    width: u32,
    height: u32,
    bottom_left: [f32; 2],
    pixels_per_em: u16,
) -> Option<ColorGlyph> {
    let (w, h) = (width as usize, height as usize);
    let alpha = |x: usize, y: usize| pixels[y * w + x][3] * 255.0 >= 0.5;
    let rows: Vec<usize> = (0..h).filter(|&y| (0..w).any(|x| alpha(x, y))).collect();
    let (&top, &bottom) = (rows.first()?, rows.last()?);
    let left = (0..w).find(|&x| (top..=bottom).any(|y| alpha(x, y)))?;
    let right = (0..w)
        .rev()
        .find(|&x| (top..=bottom).any(|y| alpha(x, y)))?;
    let (out_w, out_h) = (right - left + 1, bottom - top + 1);
    let mut rgba = Vec::with_capacity(out_w * out_h * 4);
    for y in top..=bottom {
        for x in left..=right {
            let p = pixels[y * w + x];
            let a = p[3].clamp(0.0, 1.0);
            for channel in &p[..3] {
                rgba.push((channel.clamp(0.0, a) * 255.0).round() as u8);
            }
            rgba.push((a * 255.0).round() as u8);
        }
    }
    Some(ColorGlyph {
        width: out_w as u32,
        height: out_h as u32,
        rgba,
        pixels_per_em,
        origin_px: [
            bottom_left[0] + left as f32,
            bottom_left[1] + (h - 1 - bottom) as f32,
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porter_duff_source_over_matches_the_closed_form() {
        let s = [0.5, 0.0, 0.0, 0.5];
        let d = [0.0, 0.0, 1.0, 1.0];
        let out = composite(CompositeMode::SourceOver, s, d);
        assert_eq!(out, [0.5, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn separable_blend_over_opaque_backdrop_is_the_blend_function() {
        let s = [0.5, 0.5, 0.5, 1.0];
        let d = [0.5, 0.5, 0.5, 1.0];
        let out = composite(CompositeMode::Multiply, s, d);
        assert!((out[0] - 0.25).abs() < 1e-6 && (out[3] - 1.0).abs() < 1e-6);
        let out = composite(CompositeMode::Screen, s, d);
        assert!((out[0] - 0.75).abs() < 1e-6);
    }

    #[test]
    fn luminosity_keeps_the_backdrop_hue() {
        let red = [1.0, 0.0, 0.0];
        let gray = [0.5, 0.5, 0.5];
        let out = blend(CompositeMode::Luminosity, gray, red);
        assert!((lum(out) - 0.5).abs() < 1e-4);
        assert!(out[0] > out[1] && out[1] == out[2]);
    }

    #[test]
    fn color_line_extends_pad_repeat_and_reflect() {
        let line = |extend| ColorLine {
            stops: vec![(0.0, [0.0; 4]), (1.0, [1.0; 4])],
            extend,
        };
        assert_eq!(line(GradientExtend::Pad).at(1.5)[0], 1.0);
        assert!((line(GradientExtend::Repeat).at(1.25)[0] - 0.25).abs() < 1e-6);
        assert!((line(GradientExtend::Reflect).at(1.25)[0] - 0.75).abs() < 1e-6);
        assert!((line(GradientExtend::Pad).at(0.5)[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn inverse_undoes_the_transform() {
        let t = Transform::new(2.0, 0.5, -0.25, 3.0, 7.0, -4.0);
        let inv = invert(&t).expect("invertible");
        let (x, y) = apply(&t, 1.5, -2.0);
        let (x, y) = apply(&inv, x, y);
        assert!((x - 1.5).abs() < 1e-4 && (y + 2.0).abs() < 1e-4);
    }

    #[test]
    fn shrinking_resample_averages_and_stays_premultiplied() {
        // 2×1 → 1×1: opaque red next to transparent.
        let src = [255, 0, 0, 255, 0, 0, 0, 0];
        let out = resample(&src, 2, 1, 1, 1);
        assert_eq!(out, vec![128, 0, 0, 128]);
    }

    #[test]
    fn enlarging_resample_keeps_edge_texels() {
        let src = [10, 20, 30, 40];
        let out = resample(&src, 1, 1, 3, 2);
        assert_eq!(out.len(), 3 * 2 * 4);
        assert!(
            out.as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [10, 20, 30, 40])
        );
    }

    #[test]
    fn finish_trims_and_reports_the_bottom_left() {
        // 3×3 canvas, one opaque pixel at column 1, row 0 (top).
        let mut pixels = vec![[0.0; 4]; 9];
        pixels[1] = [1.0, 0.0, 0.0, 1.0];
        let glyph = finish(&pixels, 3, 3, [10.0, -5.0], 16).expect("ink");
        assert_eq!((glyph.width, glyph.height), (1, 1));
        assert_eq!(glyph.rgba, vec![255, 0, 0, 255]);
        // Row 0 of a 3-row canvas is 2 rows above its bottom.
        assert_eq!(glyph.origin_px, [11.0, -3.0]);
    }

    #[test]
    fn a_face_without_color_tables_paints_nothing() {
        assert!(rasterize_color(&[0u8; 12], 0, 1, 32).is_none());
    }
}
