//! Glyph rasterization: outline → coverage → single-channel SDF.
//!
//! The pipeline is: flatten the glyph outline to a coverage bitmap with
//! `ab_glyph_rasterizer`, then convert coverage to a signed distance field with
//! `sdfer`'s ESDT. The result is stored one byte per texel (R8) in the atlas
//! and decoded back to coverage in the shader.
//!
//! ## SDF encoding (must match the shader decode)
//!
//! `sdfer` encodes a signed distance `d` (in raster pixels, positive outside
//! the glyph) as `stored = 1 - (d / radius + cutoff)`. So the zero-distance
//! edge lands at `stored = 1 - cutoff = `[`SDF_EDGE`], and one raster pixel of
//! distance spans `1 / radius` in stored units. The shader recovers coverage
//! with `clamp((stored - SDF_EDGE) * px_range + 0.5, 0, 1)`, where `px_range`
//! is [`SDF_RADIUS`] scaled by the on-screen/raster size ratio (1.0 here, since
//! Phase 2 rasterizes at display density).

use crate::font::{Command, FontFace};
use ab_glyph_rasterizer::{Rasterizer, point};

/// ESDT padding, in texels, around the coverage bitmap. Gives the distance
/// transform room to spread outside the glyph.
pub const SDF_PAD: usize = 4;
/// ESDT distance radius, in raster pixels. Distances beyond this saturate.
pub const SDF_RADIUS: f32 = 8.0;
/// ESDT cutoff — the fraction of `radius` reserved below the edge.
pub const SDF_CUTOFF: f32 = 0.25;
/// Stored value at the glyph edge (`d = 0`): `1 - cutoff`.
pub const SDF_EDGE: f32 = 1.0 - SDF_CUTOFF;

/// A rasterized glyph: a single-channel SDF bitmap plus placement metadata.
pub struct RasterGlyph {
    /// SDF width in texels (includes `2 * SDF_PAD`).
    pub width: u32,
    /// SDF height in texels (includes `2 * SDF_PAD`).
    pub height: u32,
    /// R8 SDF bytes, row-major, `width * height` long.
    pub sdf: Vec<u8>,
    /// Offset from the pen origin (baseline point) to the **top-left of the SDF
    /// bitmap**, in pixels. Includes the padding shift.
    pub bearing_px: [f32; 2],
    /// Coverage-ramp width for the shader, in stored-units⁻¹ (see module docs).
    pub px_range: f32,
}

/// Rasterize `glyph_id` at `dpx_per_em` raster density. Returns `None` for
/// glyphs with no outline (whitespace).
pub fn rasterize_glyph(face: &FontFace, glyph_id: u16, dpx_per_em: f32) -> Option<RasterGlyph> {
    let cmds = face.outline(glyph_id)?;
    if cmds.is_empty() {
        return None;
    }
    let upem = face.units_per_em;
    let scale = dpx_per_em / upem;

    // Outline bounds in raster pixels (Y-up font space scaled by `scale`).
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in &cmds {
        let pts: &[(f32, f32)] = match c {
            Command::MoveTo { x, y } | Command::LineTo { x, y } => &[(*x, *y)],
            Command::QuadTo { cx, cy, x, y } => &[(*cx, *cy), (*x, *y)],
            Command::CurveTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => &[(*c1x, *c1y), (*c2x, *c2y), (*x, *y)],
            Command::Close => &[],
        };
        for &(px, py) in pts {
            min_x = min_x.min(px * scale);
            min_y = min_y.min(py * scale);
            max_x = max_x.max(px * scale);
            max_y = max_y.max(py * scale);
        }
    }
    if !(max_x > min_x && max_y > min_y) {
        return None;
    }

    // Coverage bitmap size (ceil the fractional extent, pad by 1px each side for
    // the AA fringe), before the SDF padding is added.
    let cov_w = (max_x - min_x).ceil() as usize + 2;
    let cov_h = (max_y - min_y).ceil() as usize + 2;

    // Map a font-space point (Y-up) into coverage-bitmap space (Y-down), with a
    // 1px inset so the outline never touches the bitmap edge.
    let tx = |x: f32| x * scale - min_x + 1.0;
    // Flip Y: font-space top (max_y) maps to bitmap row 0.
    let ty = |y: f32| max_y - y * scale + 1.0;

    let mut rasterizer = Rasterizer::new(cov_w, cov_h);
    flatten(&cmds, &tx, &ty, &mut rasterizer);

    // Coverage → padded Unorm8 image for ESDT.
    let pad = SDF_PAD;
    let img_w = cov_w + 2 * pad;
    let img_h = cov_h + 2 * pad;
    let mut glyph_img: sdfer::Image2d<sdfer::Unorm8> = sdfer::Image2d::new(img_w, img_h);
    rasterizer.for_each_pixel_2d(|x, y, coverage| {
        glyph_img[(x as usize + pad, y as usize + pad)] = sdfer::Unorm8::encode(coverage);
    });

    let params = sdfer::esdt::Params {
        pad: 0,
        radius: SDF_RADIUS,
        cutoff: SDF_CUTOFF,
        solidify: true,
        preprocess: false,
    };
    let (sdf_img, _bufs) = sdfer::esdt::glyph_to_sdf(&mut glyph_img, params, None);

    let width = sdf_img.width() as u32;
    let height = sdf_img.height() as u32;
    let mut sdf = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        for x in 0..width as usize {
            sdf.push(sdf_img[(x, y)].to_bits());
        }
    }

    // Bitmap top-left relative to the pen origin. Coverage col 0 corresponds to
    // font x = (min_x - 1) in raster px; the SDF image adds `pad` more on the
    // left/top. The pen origin is the baseline point (font 0,0).
    let bearing_x = min_x - 1.0 - pad as f32;
    // In Y-down pixel space the SDF top is `pad` texels above coverage row 0,
    // which sits at font y = max_y + 1 (measuring downward from the baseline).
    let bearing_y = -(max_y + 1.0 + pad as f32);

    Some(RasterGlyph {
        width,
        height,
        sdf,
        bearing_px: [bearing_x, bearing_y],
        px_range: SDF_RADIUS,
    })
}

/// Flatten outline commands into the rasterizer, mapping each point through the
/// `tx`/`ty` coordinate transforms.
fn flatten(
    cmds: &[Command],
    tx: &impl Fn(f32) -> f32,
    ty: &impl Fn(f32) -> f32,
    r: &mut Rasterizer,
) {
    let map = |x: f32, y: f32| point(tx(x), ty(y));
    let mut start = point(0.0, 0.0);
    let mut cur = point(0.0, 0.0);
    for c in cmds {
        match *c {
            Command::MoveTo { x, y } => {
                start = map(x, y);
                cur = start;
            }
            Command::LineTo { x, y } => {
                let p = map(x, y);
                r.draw_line(cur, p);
                cur = p;
            }
            Command::QuadTo { cx, cy, x, y } => {
                let c1 = map(cx, cy);
                let p = map(x, y);
                r.draw_quad(cur, c1, p);
                cur = p;
            }
            Command::CurveTo {
                c1x,
                c1y,
                c2x,
                c2y,
                x,
                y,
            } => {
                let c1 = map(c1x, c1y);
                let c2 = map(c2x, c2y);
                let p = map(x, y);
                r.draw_cubic(cur, c1, c2, p);
                cur = p;
            }
            Command::Close => {
                r.draw_line(cur, start);
                cur = start;
            }
        }
    }
    // Ensure the final contour is closed for correct winding coverage.
    if cur != start {
        r.draw_line(cur, start);
    }
}
