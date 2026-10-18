//! An SVG document rasterized on the CPU, drawn as the renderer draws an SVG
//! asset so a texture baked at build time looks like the scene drawn live.
//!
//! The document walks in paint order exactly as the renderer lowers it:
//! groups compose their transforms, group opacity multiplies into each
//! descendant's paint alpha, and each path paints its solid fill, then its
//! solid stroke. Gradient and pattern paints and images are skipped;
//! clip-paths, masks and filters are ignored. The document box stretches to
//! the image, a stroke widening by the stretch's geometric mean.
//!
//! Coverage is the analytic signed-area accumulation of
//! `ab_glyph_rasterizer`: a fill under its own fill rule, a stroke as the
//! nonzero union of its pieces, so a stroke crossing itself is no darker
//! where it overlaps. Each coverage composites source-over, premultiplied,
//! into an `f32` image rounded to bytes once at the end.

mod coverage;
mod flatten;
mod stroke;

use viso_math::{Affine2, LinearPremul, Srgb};
use viso_svg::tree::{self as svg, FillRule, Tree};

use coverage::{Coverage, Outline};
use flatten::Polylines;
use stroke::{Dash, Join, Pen, Stroker};

/// The largest distance, in output pixels, a flattened curve or arc strays
/// from the true one.
const TOLERANCE: f32 = 0.01;

/// `tree` drawn into a `width × height` image, its document box stretched to
/// fill it: premultiplied RGBA8, row by row from the top.
pub fn rasterize(tree: &Tree, width: u32, height: u32) -> Vec<[u8; 4]> {
    let stretch = |image: u32, document: f32| {
        if document > 0.0 {
            image as f32 / document
        } else {
            0.0
        }
    };
    let (sx, sy) = (stretch(width, tree.size.0), stretch(height, tree.size.1));
    let mut painter = Painter {
        width,
        height,
        pixels: vec![[0.0; 4]; width as usize * height as usize],
        device: Affine2::new(sx, 0.0, 0.0, sy, 0.0, 0.0),
        width_scale: (sx * sy).abs().sqrt(),
        lines: Polylines::default(),
        outline: Outline::default(),
        coverage: Coverage::default(),
        stroker: Stroker::default(),
    };
    if !painter.pixels.is_empty() {
        painter.group(&tree.root, &Affine2::IDENTITY, 1.0);
    }
    painter
        .pixels
        .iter()
        .map(|px| px.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8))
        .collect()
}

/// The image being drawn and the buffers each path reuses.
struct Painter {
    width: u32,
    height: u32,
    /// Premultiplied linear RGBA.
    pixels: Vec<[f32; 4]>,
    /// The document box onto the image.
    device: Affine2,
    /// What the stretch multiplies a stroke's width and dashes by.
    width_scale: f32,
    lines: Polylines,
    outline: Outline,
    coverage: Coverage,
    stroker: Stroker,
}

impl Painter {
    /// `group`'s subtree in paint order, under the accumulated transform
    /// `parent` and group opacity `opacity` above it.
    fn group(&mut self, group: &svg::Group, parent: &Affine2, opacity: f32) {
        let ts = parent.pre_concat(&group.transform);
        let opacity = opacity * group.opacity;
        for node in &group.children {
            match node {
                svg::Node::Group(g) => self.group(g, &ts, opacity),
                svg::Node::Path(p) => self.path(p, &ts, opacity),
                svg::Node::Image(_) => {}
            }
        }
    }

    /// `path`'s solid fill, then its solid stroke.
    fn path(&mut self, path: &svg::Path, ts: &Affine2, opacity: f32) {
        let fill = path
            .fill
            .as_ref()
            .and_then(|f| Some((solid_color(&f.paint, f.opacity * opacity)?, f.rule)));
        let stroke = path
            .stroke
            .as_ref()
            .and_then(|s| Some((solid_color(&s.paint, s.opacity * opacity)?, s)));
        if fill.is_none() && stroke.is_none() {
            return;
        }
        let device = self.device.pre_concat(ts);
        self.lines.flatten(&path.data, &device, TOLERANCE);
        if self.lines.is_empty() {
            return;
        }
        if let Some((color, rule)) = fill {
            self.outline.clear();
            for (points, _) in self.lines.iter() {
                self.outline.push(points);
            }
            self.paint(color, rule);
        }
        if let Some((color, stroke)) = stroke {
            let scale = if stroke.non_scaling {
                1.0
            } else {
                ts.mean_scale()
            } * self.width_scale;
            let pen = Pen {
                half_width: stroke.width * scale * 0.5,
                cap: stroke.cap,
                join: match stroke.join {
                    svg::LineJoin::Round => Join::Round,
                    svg::LineJoin::Bevel => Join::Bevel,
                    svg::LineJoin::Miter | svg::LineJoin::MiterClip => Join::Miter,
                },
                miter_limit: stroke.miter_limit,
                dash: stroke
                    .dasharray
                    .as_deref()
                    .map(|d| Dash::new(d, stroke.dashoffset, scale)),
                tolerance: TOLERANCE,
            };
            self.outline.clear();
            self.stroker.stroke(&self.lines, &pen, &mut self.outline);
            self.paint(color, FillRule::NonZero);
        }
    }

    /// The current outline's coverage under `rule`, painted in `color`
    /// source-over.
    fn paint(&mut self, color: LinearPremul, rule: FillRule) {
        if color.a <= 0.0 {
            return;
        }
        let Some((window, coverage)) =
            self.coverage
                .rasterize(&self.outline, rule, self.width, self.height)
        else {
            return;
        };
        let src = [color.r, color.g, color.b, color.a];
        let (x, w) = (window.x as usize, window.width as usize);
        for (row, cov_row) in coverage.chunks_exact(w).enumerate() {
            let start = (window.y as usize + row) * self.width as usize + x;
            for (dst, &cov) in self.pixels[start..start + w].iter_mut().zip(cov_row) {
                if cov > 0.0 {
                    let keep = 1.0 - color.a * cov;
                    for (d, s) in dst.iter_mut().zip(src) {
                        *d = s * cov + *d * keep;
                    }
                }
            }
        }
    }
}

/// A solid paint as premultiplied linear color, its alpha multiplied by
/// `opacity` and rounded to a byte first as the renderer does; `None` for a
/// gradient or pattern.
fn solid_color(paint: &svg::Paint, opacity: f32) -> Option<LinearPremul> {
    match paint {
        svg::Paint::Color(c) => {
            let alpha = (c.a as f32 * opacity).round().clamp(0.0, 255.0) as u8;
            Some(
                Srgb::from_u8(c.r, c.g, c.b, alpha)
                    .into_linear_straight()
                    .premultiply(),
            )
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
