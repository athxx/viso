//! The built-in shaders on the CPU: each decodes its instance, says which
//! target pixels it can touch, and shades the rows of a clip rect.
//!
//! A built-in's fragment math is restated here operation for operation from
//! its shader IR (SDFs, AA ramps, sampling conventions), so the raster matches
//! a device to within rounding. A draw's `shift` moves where it lands, not
//! what it computes: every pixel is shaded at its center minus the shift, as
//! the vertex stage moves the quad and the fragment sees unshifted
//! coordinates.

use super::TILE;
use super::sample::{View, sample, texture_bindings};
use crate::instance::AttrFormat;
use crate::resource::{BuiltinShader, SamplerDesc};
use crate::{DrawCommand, TextureId};

use AttrFormat::{Float1 as F1, Float2 as F2, Float4 as F4, Uint1 as U1};

/// The most fields a built-in reads.
pub(super) const MAX_FIELDS: usize = 16;

/// The fields `builtin` reads, in the order its decoder indexes them.
pub(super) fn schema(builtin: BuiltinShader) -> &'static [(&'static str, AttrFormat)] {
    const RECT: [(&str, AttrFormat); 2] = [("rect_pos", F2), ("rect_size", F2)];
    match builtin {
        BuiltinShader::Quad => &[
            RECT[0],
            RECT[1],
            ("color", F4),
            ("radius", F1),
            ("border_width", F1),
            ("border_color", F4),
        ],
        BuiltinShader::AnalyticRRect => &[
            RECT[0],
            RECT[1],
            ("color", F4),
            ("radius", F4),
            ("border_width", F1),
            ("border_color", F4),
        ],
        BuiltinShader::AnalyticEllipse | BuiltinShader::AnalyticCapsule => &[
            RECT[0],
            RECT[1],
            ("color", F4),
            ("border_width", F1),
            ("border_color", F4),
        ],
        BuiltinShader::AnalyticLine => &[
            ("p0", F2),
            ("p1", F2),
            ("width", F1),
            ("color", F4),
            ("cap", U1),
            ("border_width", F1),
            ("border_color", F4),
        ],
        BuiltinShader::AnalyticShadow => &[
            RECT[0],
            RECT[1],
            ("color", F4),
            ("radius", F4),
            ("offset", F2),
            ("sigma", F1),
            ("spread", F1),
            ("shape", U1),
            ("inner", U1),
        ],
        BuiltinShader::Image | BuiltinShader::GlyphRun | BuiltinShader::Mtsdf => &[
            RECT[0],
            RECT[1],
            ("uv_pos", F2),
            ("uv_size", F2),
            ("color", F4),
        ],
        BuiltinShader::Blur => &[
            RECT[0],
            RECT[1],
            ("uv_pos", F2),
            ("uv_size", F2),
            ("dir", F2),
            ("sigma", F1),
            ("radius", F1),
        ],
        BuiltinShader::ColorTransform => &[
            RECT[0],
            RECT[1],
            ("uv_pos", F2),
            ("uv_size", F2),
            ("row0", F4),
            ("row1", F4),
            ("row2", F4),
            ("row3", F4),
            ("offset", F4),
            ("gamma", F1),
        ],
        BuiltinShader::Material => &[
            RECT[0],
            RECT[1],
            ("uv_pos", F2),
            ("uv_size", F2),
            ("row0", F4),
            ("row1", F4),
            ("row2", F4),
            ("row3", F4),
            ("offset", F4),
            ("radius", F4),
            ("gamma", F1),
            ("noise", F1),
            ("opacity", F1),
        ],
        BuiltinShader::AdvancedBlend => &[
            RECT[0],
            RECT[1],
            ("uv_pos", F2),
            ("uv_size", F2),
            ("dst_uv_pos", F2),
            ("dst_uv_size", F2),
            ("mode", U1),
            ("opacity", F1),
        ],
        BuiltinShader::Gradient => &[
            RECT[0],
            RECT[1],
            ("kind", U1),
            ("extend", U1),
            ("p0", F2),
            ("p1", F2),
            ("lut_v", F1),
            ("use_lut", U1),
            ("color0", F4),
            ("color1", F4),
        ],
        BuiltinShader::Path | BuiltinShader::Mesh => &[("pos", F2), ("color", F4), ("edge", F1)],
        BuiltinShader::Layer => &[],
    }
}

/// One instance's (or vertex's) bytes and where its fields sit.
#[derive(Clone, Copy)]
pub(super) struct Inst<'a> {
    pub bytes: &'a [u8],
    pub offsets: &'a [u16; MAX_FIELDS],
}

impl Inst<'_> {
    #[inline]
    fn word(&self, field: usize, i: usize) -> [u8; 4] {
        let at = usize::from(self.offsets[field]) + i * 4;
        self.bytes[at..at + 4].try_into().expect("four bytes")
    }

    #[inline]
    pub(super) fn f1(&self, field: usize) -> f32 {
        f32::from_le_bytes(self.word(field, 0))
    }

    #[inline]
    pub(super) fn u1(&self, field: usize) -> u32 {
        u32::from_le_bytes(self.word(field, 0))
    }

    #[inline]
    pub(super) fn f2(&self, field: usize) -> [f32; 2] {
        std::array::from_fn(|i| f32::from_le_bytes(self.word(field, i)))
    }

    #[inline]
    pub(super) fn f4(&self, field: usize) -> [f32; 4] {
        std::array::from_fn(|i| f32::from_le_bytes(self.word(field, i)))
    }
}

/// Where a draw writes its rows.
pub(super) trait Rows {
    /// `src` (premultiplied) composited source-over at `(x0.., y)`.
    fn blend(&mut self, y: u32, x0: u32, src: &[[f32; 4]]);
    /// The same, through the target's ordered dither when it has one.
    fn blend_dithered(&mut self, y: u32, x0: u32, src: &[[f32; 4]]);
    /// `src` written as is.
    fn replace(&mut self, y: u32, x0: u32, src: &[[f32; 4]]);
    /// The pixels at `(x0.., y)`, premultiplied.
    fn load(&self, y: u32, x0: u32, out: &mut [[f32; 4]]);
}

/// What a draw reads besides its instance: the resources its bind group
/// names, how far its root moved, and the window of the deferred layer it
/// composites, if any, standing in for that layer's texture.
pub(super) struct Cx<'a> {
    pub res: &'a super::Resources,
    pub cmd: &'a DrawCommand,
    pub shift: [f32; 2],
    pub layer: Option<(TextureId, View<'a>)>,
}

/// The target pixels `[x0, y0, x1, y1)` a draw may touch inside a
/// `width × height` target.
pub(super) type PxRect = [u32; 4];

/// The pixels a shape covers, padded by `pad` on every side.
fn padded(min: [f32; 2], max: [f32; 2], pad: [f32; 2], shift: [f32; 2]) -> [f32; 4] {
    [
        (min[0] - pad[0] + shift[0]).floor(),
        (min[1] - pad[1] + shift[1]).floor(),
        (max[0] + pad[0] + shift[0]).ceil(),
        (max[1] + pad[1] + shift[1]).ceil(),
    ]
}

/// The pixels whose centers fall inside `[pos, pos + size)` (the top-left
/// rule a device applies to an axis-aligned quad).
fn centers(pos: [f32; 2], size: [f32; 2], shift: [f32; 2]) -> [f32; 4] {
    let x = pos[0] + shift[0];
    let y = pos[1] + shift[1];
    [
        (x - 0.5).ceil(),
        (y - 0.5).ceil(),
        (x + size[0] - 0.5).ceil(),
        (y + size[1] - 0.5).ceil(),
    ]
}

/// `r` cut to `[0, w) × [0, h)`, or `None` when empty.
pub(super) fn to_pixels(r: [f32; 4], w: u32, h: u32) -> Option<PxRect> {
    let x0 = r[0].clamp(0.0, w as f32) as u32;
    let y0 = r[1].clamp(0.0, h as f32) as u32;
    let x1 = r[2].clamp(0.0, w as f32) as u32;
    let y1 = r[3].clamp(0.0, h as f32) as u32;
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1, y1])
}

/// The pixels instance `i` of `builtin` can touch, before clipping.
pub(super) fn bounds(builtin: BuiltinShader, i: &Inst<'_>, shift: [f32; 2]) -> Option<[f32; 4]> {
    let rect = |i: &Inst<'_>| {
        let (pos, size) = (i.f2(0), i.f2(1));
        (pos, [pos[0] + size[0], pos[1] + size[1]], size)
    };
    Some(match builtin {
        BuiltinShader::Quad
        | BuiltinShader::AnalyticRRect
        | BuiltinShader::AnalyticEllipse
        | BuiltinShader::AnalyticCapsule
        | BuiltinShader::Gradient
        | BuiltinShader::Material => {
            let (min, max, _) = rect(i);
            padded(min, max, [1.0, 1.0], shift)
        }
        BuiltinShader::AnalyticLine => {
            let (p0, p1, hw, cap) = (i.f2(0), i.f2(1), i.f1(2) * 0.5, i.u1(4));
            let margin = hw + if cap == 0 { 0.0 } else { hw } + 1.0;
            padded(
                [p0[0].min(p1[0]), p0[1].min(p1[1])],
                [p0[0].max(p1[0]), p0[1].max(p1[1])],
                [margin, margin],
                shift,
            )
        }
        BuiltinShader::AnalyticShadow => {
            let (min, max, _) = rect(i);
            let (offset, sigma, spread) = (i.f2(4), i.f1(5), i.f1(6));
            let reach = 3.0 * sigma + spread.max(0.0) + 1.0;
            padded(
                min,
                max,
                [reach + offset[0].abs(), reach + offset[1].abs()],
                shift,
            )
        }
        BuiltinShader::Image
        | BuiltinShader::GlyphRun
        | BuiltinShader::Mtsdf
        | BuiltinShader::Blur
        | BuiltinShader::ColorTransform
        | BuiltinShader::AdvancedBlend => {
            let (pos, _, size) = rect(i);
            if size[0] <= 0.0 || size[1] <= 0.0 {
                return None;
            }
            centers(pos, size, shift)
        }
        BuiltinShader::Path | BuiltinShader::Mesh | BuiltinShader::Layer => return None,
    })
}

/// Whether `builtin` reads its texture 0 only through [`source_region`]'s
/// mapping, so a layer it composites can be evaluated for one clip at a time.
pub(super) fn composites_by_region(builtin: BuiltinShader) -> bool {
    matches!(
        builtin,
        BuiltinShader::Image | BuiltinShader::ColorTransform
    )
}

/// The texels of a `width × height` texture 0 that instance `i` of `builtin`
/// fetches while shading `clip` through a clamp-to-edge sampler, one texel of
/// margin wide on every side; `None` when `builtin` does not
/// [composite by region](composites_by_region).
pub(super) fn source_region(
    builtin: BuiltinShader,
    i: &Inst<'_>,
    shift: [f32; 2],
    clip: PxRect,
    [width, height]: [u32; 2],
) -> Option<PxRect> {
    if !composites_by_region(builtin) || width == 0 || height == 0 {
        return None;
    }
    let map = UvMap::of(i, 2, 3);
    let lo = map.at([
        clip[0] as f32 + 0.5 - shift[0],
        clip[1] as f32 + 0.5 - shift[1],
    ]);
    let hi = map.at([
        clip[2] as f32 - 0.5 - shift[0],
        clip[3] as f32 - 0.5 - shift[1],
    ]);
    // A bilinear fetch reads `floor(t)` and the texel after it, a nearest one
    // `round(t)`, at `t = u·n − ½`; one more texel each way absorbs rounding.
    let span = |a: f32, b: f32, n: u32| {
        let t0 = a.min(b) * n as f32 - 0.5;
        let t1 = a.max(b) * n as f32 - 0.5;
        if !(t0.is_finite() && t1.is_finite()) {
            return (0, n);
        }
        let first = (t0.floor() - 1.0).clamp(0.0, (n - 1) as f32) as u32;
        let last = (t1.floor() + 2.0).clamp(0.0, (n - 1) as f32) as u32;
        (first, last + 1)
    };
    let (x0, x1) = span(lo[0], hi[0], width);
    let (y0, y1) = span(lo[1], hi[1], height);
    Some([x0, y0, x1, y1])
}

/// Shades instance `i` of `builtin` into the rows of `clip`.
pub(super) fn draw(
    builtin: BuiltinShader,
    cx: &Cx<'_>,
    i: &Inst<'_>,
    out: &mut dyn Rows,
    clip: PxRect,
) {
    match builtin {
        BuiltinShader::Quad => {
            let (pos, size) = (i.f2(0), i.f2(1));
            let half = [size[0] * 0.5, size[1] * 0.5];
            let center = [pos[0] + half[0], pos[1] + half[1]];
            // IQ box SDF radius: doubled then clamped to the smaller half-extent.
            let k = (2.0 * i.f1(3)).min(half[0].min(half[1]));
            let paint = Paint::of(i.f4(2), i.f1(4), i.f4(5));
            shade(cx, out, clip, |p| paint.at(box_sdf(p, center, half, k)));
        }
        BuiltinShader::AnalyticRRect => {
            let (pos, size, radii) = (i.f2(0), i.f2(1), i.f4(3));
            let half = [size[0] * 0.5, size[1] * 0.5];
            let center = [pos[0] + half[0], pos[1] + half[1]];
            let paint = Paint::of(i.f4(2), i.f1(4), i.f4(5));
            shade(cx, out, clip, |p| {
                paint.at(rrect_sdf(p, center, half, radii))
            });
        }
        BuiltinShader::AnalyticEllipse => {
            let (pos, size) = (i.f2(0), i.f2(1));
            let radii = [size[0] * 0.5, size[1] * 0.5];
            let center = [pos[0] + radii[0], pos[1] + radii[1]];
            let paint = Paint::of(i.f4(2), i.f1(3), i.f4(4));
            shade(cx, out, clip, |p| paint.at(ellipse_sdf(p, center, radii)));
        }
        BuiltinShader::AnalyticCapsule => {
            let (pos, size) = (i.f2(0), i.f2(1));
            let half = [size[0] * 0.5, size[1] * 0.5];
            let center = [pos[0] + half[0], pos[1] + half[1]];
            let paint = Paint::of(i.f4(2), i.f1(3), i.f4(4));
            shade(cx, out, clip, |p| paint.at(capsule_sdf(p, center, half)));
        }
        BuiltinShader::AnalyticLine => {
            let (p0, p1, hw, cap) = (i.f2(0), i.f2(1), i.f1(2) * 0.5, i.u1(4));
            let paint = Paint::of(i.f4(3), i.f1(5), i.f4(6));
            let ext = if cap == 1 { hw } else { 0.0 };
            shade(cx, out, clip, |p| {
                paint.at(if cap == 2 {
                    segment_sdf(p, p0, p1, hw)
                } else {
                    capped_segment_sdf(p, p0, p1, hw, ext)
                })
            });
        }
        BuiltinShader::AnalyticShadow => analytic_shadow(cx, i, out, clip),
        BuiltinShader::Image => {
            let Some((tex, samp)) = texture_bindings(cx, 0) else {
                return;
            };
            let map = UvMap::of(i, 2, 3);
            let tint = i.f4(4);
            let tint = [
                tint[0] * tint[3],
                tint[1] * tint[3],
                tint[2] * tint[3],
                tint[3],
            ];
            shade(cx, out, clip, |p| {
                let [u, v] = map.at(p);
                let t = sample(&tex, &samp, u, v);
                std::array::from_fn(|c| t[c] * tint[c])
            });
        }
        BuiltinShader::GlyphRun => {
            let Some((tex, samp)) = texture_bindings(cx, 0) else {
                return;
            };
            let map = UvMap::of(i, 2, 3);
            let color = i.f4(4);
            // R8 coverage is replicated into every channel, so any is it.
            shade(cx, out, clip, |p| {
                let [u, v] = map.at(p);
                let a = color[3] * sample(&tex, &samp, u, v)[0];
                [color[0] * a, color[1] * a, color[2] * a, a]
            });
        }
        BuiltinShader::Mtsdf => {
            let Some((tex, samp)) = texture_bindings(cx, 0) else {
                return;
            };
            let map = UvMap::of(i, 2, 3);
            let color = i.f4(4);
            // Screen-space width of the stored distance range: the quad's device
            // pixels per uv unit, over the atlas texels per uv unit.
            let span = i.f2(1)[0] / i.f2(3)[0].max(1e-6);
            let px_range = MTSDF_DISTANCE_RANGE * span / (tex.width as f32).max(1.0);
            shade(cx, out, clip, |p| {
                let [u, v] = map.at(p);
                let s = sample(&tex, &samp, u, v);
                let md = s[0].min(s[1]).max(s[2].min(s[0].max(s[1])));
                let sd = if (md > 0.5) != (s[3] > 0.5) { s[3] } else { md };
                let a = color[3] * ((sd - 0.5) * px_range + 0.5).clamp(0.0, 1.0);
                [color[0] * a, color[1] * a, color[2] * a, a]
            });
        }
        BuiltinShader::Blur => blur(cx, i, out, clip),
        BuiltinShader::ColorTransform => {
            let Some((tex, samp)) = texture_bindings(cx, 0) else {
                return;
            };
            let map = UvMap::of(i, 2, 3);
            let matrix = Matrix::of(i, 4, 8, 9);
            shade(cx, out, clip, |p| {
                let [u, v] = map.at(p);
                let dst = matrix.apply(sample(&tex, &samp, u, v));
                if dst[3] <= 0.0 {
                    return [0.0; 4];
                }
                [dst[0] * dst[3], dst[1] * dst[3], dst[2] * dst[3], dst[3]]
            });
        }
        BuiltinShader::Material => material(cx, i, out, clip),
        BuiltinShader::AdvancedBlend => advanced_blend(cx, i, out, clip),
        BuiltinShader::Gradient => gradient(cx, i, out, clip),
        BuiltinShader::Path | BuiltinShader::Mesh | BuiltinShader::Layer => {}
    }
}

/// Distance range an MTSDF glyph field encodes, in source texels: the
/// generator's `viso_text::mtsdf::DISTANCE_RANGE`, which this crate sits below
/// and restates (pinned by `MTSDF_MSL`, which bakes the same literal).
const MTSDF_DISTANCE_RANGE: f32 = 4.0;

/// The device-pixel AA factor every analytic fragment computes at unit scale:
/// one pixel step is one unit of the sample position, so
/// `1 / length((1, 0), (0, 1))`.
const AA: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Runs `f` at the center of every pixel of `clip` (minus the shift) and
/// composites the rows.
#[inline]
fn shade(cx: &Cx<'_>, out: &mut dyn Rows, clip: PxRect, mut f: impl FnMut([f32; 2]) -> [f32; 4]) {
    let mut row = [[0.0f32; 4]; TILE as usize];
    let n = (clip[2] - clip[0]) as usize;
    for y in clip[1]..clip[3] {
        let py = y as f32 + 0.5 - cx.shift[1];
        for (k, px) in row[..n].iter_mut().enumerate() {
            *px = f([(clip[0] + k as u32) as f32 + 0.5 - cx.shift[0], py]);
        }
        out.blend(y, clip[0], &row[..n]);
    }
}

/// A fill with an optional border composited over it, both straight colors.
struct Paint {
    fill: [f32; 4],
    border_width: f32,
    border: [f32; 4],
}

impl Paint {
    fn of(fill: [f32; 4], border_width: f32, border: [f32; 4]) -> Paint {
        Paint {
            fill,
            border_width,
            border,
        }
    }

    /// The premultiplied color at signed distance `d`: the fill's coverage
    /// ramp, then the border band over it.
    #[inline]
    fn at(&self, d: f32) -> [f32; 4] {
        let f = self.fill;
        let cov = (-d * AA).clamp(0.0, 1.0);
        let fa = f[3] * cov;
        let mut src = [f[0] * fa, f[1] * fa, f[2] * fa, fa];
        if self.border_width > 0.0 {
            let bcov = (-(d.abs() - self.border_width * 0.5) * AA).clamp(0.0, 1.0);
            if bcov > 0.0 {
                let b = self.border;
                let ba = b[3] * bcov;
                let inv = 1.0 - ba;
                src = [
                    b[0] * ba + src[0] * inv,
                    b[1] * ba + src[1] * inv,
                    b[2] * ba + src[2] * inv,
                    ba + src[3] * inv,
                ];
            }
        }
        src
    }
}

/// A destination rect mapped onto a uv sub-rect.
struct UvMap {
    pos: [f32; 2],
    size: [f32; 2],
    uv_pos: [f32; 2],
    uv_size: [f32; 2],
}

impl UvMap {
    fn of(i: &Inst<'_>, uv_pos: usize, uv_size: usize) -> UvMap {
        UvMap {
            pos: i.f2(0),
            size: i.f2(1),
            uv_pos: i.f2(uv_pos),
            uv_size: i.f2(uv_size),
        }
    }

    #[inline]
    fn at(&self, p: [f32; 2]) -> [f32; 2] {
        let fx = (p[0] - self.pos[0]) / self.size[0];
        let fy = (p[1] - self.pos[1]) / self.size[1];
        [
            self.uv_pos[0] + fx * self.uv_size[0],
            self.uv_pos[1] + fy * self.uv_size[1],
        ]
    }
}

/// A color matrix over straight RGBA, its constant column and an RGB gamma.
struct Matrix {
    rows: [[f32; 4]; 4],
    offset: [f32; 4],
    gamma: f32,
}

impl Matrix {
    fn of(i: &Inst<'_>, rows: usize, offset: usize, gamma: usize) -> Matrix {
        Matrix {
            rows: std::array::from_fn(|r| i.f4(rows + r)),
            offset: i.f4(offset),
            gamma: i.f1(gamma),
        }
    }

    /// `texel` (premultiplied) unpremultiplied, transformed, clamped and
    /// gamma-corrected, as straight RGBA. A transparent texel carries no color
    /// and enters as zero.
    #[inline]
    fn apply(&self, texel: [f32; 4]) -> [f32; 4] {
        let a = texel[3];
        let straight = if a > 0.0 {
            [texel[0] / a, texel[1] / a, texel[2] / a, a]
        } else {
            [0.0; 4]
        };
        let mut dst: [f32; 4] = std::array::from_fn(|c| {
            let row = self.rows[c];
            (row[0] * straight[0]
                + row[1] * straight[1]
                + row[2] * straight[2]
                + row[3] * straight[3]
                + self.offset[c])
                .clamp(0.0, 1.0)
        });
        if self.gamma != 1.0 {
            for channel in dst.iter_mut().take(3) {
                *channel = channel.powf(self.gamma);
            }
        }
        dst
    }
}

/// A soft drop shadow: the shape's SDF through a Gaussian integral, sampled
/// in shape-centered space shifted by `-offset`.
fn analytic_shadow(cx: &Cx<'_>, i: &Inst<'_>, out: &mut dyn Rows, clip: PxRect) {
    let (pos, size, fill, radii) = (i.f2(0), i.f2(1), i.f4(2), i.f4(3));
    let (offset, sigma, spread, shape, inner) = (i.f2(4), i.f1(5), i.f1(6), i.u1(7), i.u1(8));
    let half = [size[0] * 0.5, size[1] * 0.5];
    let center = [pos[0] + half[0], pos[1] + half[1]];
    let half_ext = [(half[0] + spread).max(0.0), (half[1] + spread).max(0.0)];
    shade(cx, out, clip, |p| {
        let p = [p[0] - center[0] - offset[0], p[1] - center[1] - offset[1]];
        let d = shadow_sdf(shape, p, half_ext, radii);
        // `1.4142135` (not `SQRT_2`) is byte-exact with the emitted literal.
        #[allow(clippy::approx_constant)]
        let cov = if sigma <= 0.01 {
            (-d * AA).clamp(0.0, 1.0)
        } else if inner != 0 {
            let soft = 0.5 * (1.0 + erf_approx(d / (1.4142135 * sigma)));
            let inside = 0.5 * (1.0 - erf_approx(d / (1.4142135 * sigma)));
            (soft * inside).clamp(0.0, 1.0)
        } else {
            (0.5 * (1.0 - erf_approx(d / (1.4142135 * sigma)))).clamp(0.0, 1.0)
        };
        let fa = fill[3] * cov;
        [fill[0] * fa, fill[1] * fa, fill[2] * fa, fa]
    });
}

/// The tap window of a sub-rect, inset half a texel so the outer texel is
/// held rather than blended with the pooled target's padding beyond it.
fn inset_window(uv_pos: [f32; 2], uv_size: [f32; 2], tex: &View<'_>) -> ([f32; 2], [f32; 2]) {
    let half = [0.5 / tex.width as f32, 0.5 / tex.height as f32];
    let lo = [uv_pos[0] + half[0], uv_pos[1] + half[1]];
    let hi = [
        (uv_pos[0] + uv_size[0] - half[0]).max(lo[0]),
        (uv_pos[1] + uv_size[1] - half[1]).max(lo[1]),
    ];
    (lo, hi)
}

/// One separable Gaussian pass: `2r + 1` taps along `dir`, each weighted by
/// `exp(-i² / 2σ²)`, normalized.
fn blur(cx: &Cx<'_>, i: &Inst<'_>, out: &mut dyn Rows, clip: PxRect) {
    let Some((tex, samp)) = texture_bindings(cx, 0) else {
        return;
    };
    let map = UvMap::of(i, 2, 3);
    let (dir, sigma, radius) = (i.f2(4), i.f1(5), i.f1(6));
    let r = if sigma > 0.0 {
        radius.max(0.0).round() as i32
    } else {
        0
    };
    let two_sigma_sq = 2.0 * sigma * sigma;
    let (lo, hi) = inset_window(map.uv_pos, map.uv_size, &tex);
    shade(cx, out, clip, |p| {
        let [u, v] = map.at(p);
        let mut acc = [0.0f32; 4];
        let mut wsum = 0.0f32;
        for k in -r..=r {
            let fk = k as f32;
            let w = if two_sigma_sq > 0.0 {
                (-(fk * fk) / two_sigma_sq).exp()
            } else {
                1.0
            };
            let su = (u + fk * dir[0]).clamp(lo[0], hi[0]);
            let sv = (v + fk * dir[1]).clamp(lo[1], hi[1]);
            let t = sample(&tex, &samp, su, sv);
            for c in 0..4 {
                acc[c] += w * t[c];
            }
            wsum += w;
        }
        if wsum <= 0.0 {
            return [0.0; 4];
        }
        acc.map(|c| c / wsum)
    });
}

/// A frosted material: the blurred backdrop tinted on straight RGBA, grain
/// from an integer hash of the pixel, then the surface's rounded-rect mask.
fn material(cx: &Cx<'_>, i: &Inst<'_>, out: &mut dyn Rows, clip: PxRect) {
    let Some((tex, samp)) = texture_bindings(cx, 0) else {
        return;
    };
    let map = UvMap::of(i, 2, 3);
    let matrix = Matrix::of(i, 4, 8, 10);
    let (radius, noise, opacity) = (i.f4(9), i.f1(11), i.f1(12));
    if map.size[0] <= 0.0 || map.size[1] <= 0.0 {
        return;
    }
    let center = [
        map.pos[0] + map.size[0] * 0.5,
        map.pos[1] + map.size[1] * 0.5,
    ];
    let half = [map.size[0] * 0.5, map.size[1] * 0.5];
    let (lo, hi) = inset_window(map.uv_pos, map.uv_size, &tex);
    shade(cx, out, clip, |p| {
        // The AA ramp is one unit per pixel: what the fragment's `aa_factor`
        // evaluates to on this pixel-space quad.
        let cov = (-rrect_sdf(p, center, half, radius)).clamp(0.0, 1.0);
        if cov <= 0.0 {
            return [0.0; 4];
        }
        let [u, v] = map.at(p);
        let mut dst = matrix.apply(sample(
            &tex,
            &samp,
            u.clamp(lo[0], hi[0]),
            v.clamp(lo[1], hi[1]),
        ));
        if noise > 0.0 {
            let g = grain(p);
            for channel in dst.iter_mut().take(3) {
                *channel = (*channel + noise * g).clamp(0.0, 1.0);
            }
        }
        let oa = dst[3] * cov * opacity;
        if oa <= 0.0 {
            return [0.0; 4];
        }
        [dst[0] * oa, dst[1] * oa, dst[2] * oa, oa]
    });
}

/// An isolated advanced-blend composite: the layer (texture 0) and the
/// destination snapshot (texture 1) blended in the fragment and written, not
/// blended again.
fn advanced_blend(cx: &Cx<'_>, i: &Inst<'_>, out: &mut dyn Rows, clip: PxRect) {
    let (Some((src, samp)), Some((dst, _))) = (texture_bindings(cx, 0), texture_bindings(cx, 1))
    else {
        return;
    };
    let layer = UvMap::of(i, 2, 3);
    let snapshot = UvMap::of(i, 4, 5);
    let (mode, opacity) = (i.u1(6), i.f1(7));
    let mut row = [[0.0f32; 4]; TILE as usize];
    let n = (clip[2] - clip[0]) as usize;
    for y in clip[1]..clip[3] {
        let py = y as f32 + 0.5 - cx.shift[1];
        for (k, px) in row[..n].iter_mut().enumerate() {
            let p = [(clip[0] + k as u32) as f32 + 0.5 - cx.shift[0], py];
            let [u, v] = layer.at(p);
            let s = sample(&src, &samp, u, v).map(|c| c * opacity);
            let [u, v] = snapshot.at(p);
            *px = blend_composite(mode, s, sample(&dst, &samp, u, v));
        }
        out.replace(y, clip[0], &row[..n]);
    }
}

/// A gradient over an axis-aligned rect: the parameter by kind, the extend
/// wrap, then a LUT row or the inline two-stop lerp, both premultiplied,
/// times the rect's edge coverage; dithered on a target that bands.
fn gradient(cx: &Cx<'_>, i: &Inst<'_>, out: &mut dyn Rows, clip: PxRect) {
    let (pos, size) = (i.f2(0), i.f2(1));
    if size[0] <= 0.0 || size[1] <= 0.0 {
        return;
    }
    let (kind, extend, g0, g1, lut_v, use_lut) =
        (i.u1(2), i.u1(3), i.f2(4), i.f2(5), i.f1(6), i.u1(7));
    let (color0, color1) = (i.f4(8), i.f4(9));
    let lut = if use_lut != 0 {
        let Some(lut) = texture_bindings(cx, 0) else {
            return;
        };
        Some(lut)
    } else {
        None
    };
    let center = [pos[0] + size[0] * 0.5, pos[1] + size[1] * 0.5];
    let half = [size[0] * 0.5, size[1] * 0.5];
    let mut row = [[0.0f32; 4]; TILE as usize];
    let n = (clip[2] - clip[0]) as usize;
    for y in clip[1]..clip[3] {
        let py = y as f32 + 0.5 - cx.shift[1];
        for (k, px) in row[..n].iter_mut().enumerate() {
            let p = [(clip[0] + k as u32) as f32 + 0.5 - cx.shift[0], py];
            let q = [
                (p[0] - center[0]).abs() - half[0],
                (p[1] - center[1]).abs() - half[1],
            ];
            let m = [q[0].max(0.0), q[1].max(0.0)];
            let d = (m[0] * m[0] + m[1] * m[1]).sqrt() + q[0].max(q[1]).min(0.0);
            let cov = (-d * AA).clamp(0.0, 1.0);
            if cov <= 0.0 {
                *px = [0.0; 4];
                continue;
            }
            let u = gradient_extend(extend, gradient_t(kind, p, g0, g1));
            let g = match &lut {
                Some((tex, samp)) => sample(tex, samp, u, lut_v),
                None => std::array::from_fn(|c| color0[c] + (color1[c] - color0[c]) * u),
            };
            *px = g.map(|c| c * cov);
        }
        out.blend_dithered(y, clip[0], &row[..n]);
    }
}

/// One triangle of an indexed mesh: its corners' positions (shifted),
/// straight colors and AA edge weights.
#[derive(Debug, Clone, Copy)]
pub(super) struct Triangle {
    pub pos: [[f32; 2]; 3],
    pub color: [[f32; 4]; 3],
    pub edge: [f32; 3],
}

impl Triangle {
    pub(super) fn bounds(&self) -> [f32; 4] {
        let [a, b, c] = self.pos;
        [
            a[0].min(b[0]).min(c[0]).floor(),
            a[1].min(b[1]).min(c[1]).floor(),
            a[0].max(b[0]).max(c[0]).ceil(),
            a[1].max(b[1]).max(c[1]).ceil(),
        ]
    }

    /// Scan-fills the triangle over `clip` at pixel centers under the
    /// top-left rule, color and edge weight interpolated barycentrically, the
    /// weight scaling alpha.
    pub(super) fn draw(&self, out: &mut dyn Rows, clip: PxRect) {
        let [p0, p1, p2] = self.pos;
        let area = edge_fn(p0, p1, p2);
        if area == 0.0 {
            return;
        }
        let inv_area = 1.0 / area;
        // A sample exactly on an edge belongs to the triangle only on a left or
        // horizontal top edge, so abutting triangles never double-cover.
        let owns = [
            owns_boundary(p1, p2, area),
            owns_boundary(p2, p0, area),
            owns_boundary(p0, p1, area),
        ];
        let inside = |w: f32, owns: bool| w > 0.0 || (w == 0.0 && owns);
        let [c0, c1, c2] = self.color;
        let [e0, e1, e2] = self.edge;
        let mut row = [[0.0f32; 4]; TILE as usize];
        for y in clip[1]..clip[3] {
            let (mut first, mut last) = (u32::MAX, 0);
            for x in clip[0]..clip[2] {
                let p = [x as f32 + 0.5, y as f32 + 0.5];
                let w0 = edge_fn(p1, p2, p) * inv_area;
                let w1 = edge_fn(p2, p0, p) * inv_area;
                let w2 = edge_fn(p0, p1, p) * inv_area;
                let k = (x - clip[0]) as usize;
                row[k] = [0.0; 4];
                if !(inside(w0, owns[0]) && inside(w1, owns[1]) && inside(w2, owns[2])) {
                    continue;
                }
                let cov = (w0 * e0 + w1 * e1 + w2 * e2).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let col: [f32; 4] = std::array::from_fn(|c| w0 * c0[c] + w1 * c1[c] + w2 * c2[c]);
                let a = col[3] * cov;
                if a <= 0.0 {
                    continue;
                }
                row[k] = [col[0] * a, col[1] * a, col[2] * a, a];
                first = first.min(x);
                last = x + 1;
            }
            if first < last {
                let from = (first - clip[0]) as usize;
                let to = (last - clip[0]) as usize;
                out.blend(y, first, &row[from..to]);
            }
        }
    }
}

/// The raw gradient parameter before extend wrapping. Linear: the sample
/// projected onto `p0 → p1`, `[0, 1]` at the endpoints. Radial: distance from
/// `p0` over the radius `g1.x`. Sweep: the angle around `p0` past the start
/// angle `g1.x`, as a fraction of a turn.
fn gradient_t(kind: u32, p: [f32; 2], g0: [f32; 2], g1: [f32; 2]) -> f32 {
    if kind == 1 {
        let r = g1[0].max(1e-6);
        let dx = p[0] - g0[0];
        let dy = p[1] - g0[1];
        return (dx * dx + dy * dy).sqrt() / r;
    }
    if kind == 2 {
        let ang = (p[1] - g0[1]).atan2(p[0] - g0[0]) - g1[0];
        let turn = ang * (1.0 / (2.0 * std::f32::consts::PI));
        return turn - turn.floor();
    }
    let axis = [g1[0] - g0[0], g1[1] - g0[1]];
    let len2 = (axis[0] * axis[0] + axis[1] * axis[1]).max(1e-12);
    ((p[0] - g0[0]) * axis[0] + (p[1] - g0[1]) * axis[1]) / len2
}

/// The extend mode applied to a raw parameter: 0 clamp, 1 repeat, 2 mirror
/// (a triangle wave of period 2).
fn gradient_extend(mode: u32, t: f32) -> f32 {
    if mode == 1 {
        return t - t.floor();
    }
    if mode == 2 {
        let u = t - 2.0 * (t * 0.5).floor();
        return if u > 1.0 { 2.0 - u } else { u };
    }
    t.clamp(0.0, 1.0)
}

/// Twice the signed area of `(a, b, c)`.
pub(super) fn edge_fn(a: [f32; 2], b: [f32; 2], c: [f32; 2]) -> f32 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Whether edge `a → b` of a triangle of signed doubled area `area` is a top
/// or left edge (y down): its inward normal points right, or straight down.
pub(super) fn owns_boundary(a: [f32; 2], b: [f32; 2], area: f32) -> bool {
    let s = area.signum();
    let nx = -(b[1] - a[1]) * s;
    let ny = (b[0] - a[0]) * s;
    nx > 0.0 || (nx == 0.0 && ny > 0.0)
}

/// Signed distance to a rounded box (IQ), negative inside; `k` is the
/// already doubled-and-clamped corner radius.
fn box_sdf(p: [f32; 2], center: [f32; 2], half: [f32; 2], k: f32) -> f32 {
    let q = [
        (p[0] - center[0]).abs() - (half[0] - k),
        (p[1] - center[1]).abs() - (half[1] - k),
    ];
    let mx = [q[0].max(0.0), q[1].max(0.0)];
    (mx[0] * mx[0] + mx[1] * mx[1]).sqrt() + q[0].max(q[1]).min(0.0) - k
}

/// Signed distance to a rounded box with a radius per corner (left-top,
/// right-top, right-bottom, left-bottom), picked by the sample's quadrant.
fn rrect_sdf(p: [f32; 2], center: [f32; 2], half: [f32; 2], radii: [f32; 4]) -> f32 {
    let d = [p[0] - center[0], p[1] - center[1]];
    let r = if d[0] < 0.0 {
        if d[1] < 0.0 { radii[0] } else { radii[3] }
    } else if d[1] < 0.0 {
        radii[1]
    } else {
        radii[2]
    };
    box_sdf(p, center, half, (2.0 * r).min(half[0].min(half[1])))
}

/// Grain in `[-0.5, 0.5]` from an integer hash of the pixel: wrapping
/// multiplies and shifts, bit-identical to the shader's, and never time.
fn grain(p: [f32; 2]) -> f32 {
    let x = p[0].floor() as i32 as u32;
    let y = p[1].floor() as i32 as u32;
    let mut h = x.wrapping_mul(0x9E37_79B9) ^ y.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h & 0x00FF_FFFF) as f32 / 16_777_215.0 - 0.5
}

/// Signed distance to an axis-aligned ellipse: normalized by the radii,
/// offset by the unit circle, scaled back by the smaller radius.
fn ellipse_sdf(p: [f32; 2], center: [f32; 2], radii: [f32; 2]) -> f32 {
    let n = [(p[0] - center[0]) / radii[0], (p[1] - center[1]) / radii[1]];
    ((n[0] * n[0] + n[1] * n[1]).sqrt() - 1.0) * radii[0].min(radii[1])
}

/// Signed distance to a capsule: a rounded box whose radius is the smaller
/// half-extent.
fn capsule_sdf(p: [f32; 2], center: [f32; 2], half: [f32; 2]) -> f32 {
    box_sdf(p, center, half, half[0].min(half[1]))
}

/// The shadow family's SDF in shape-centered space: 1 ellipse (radii guarded
/// at `1e-4`), 2 capsule, otherwise the per-corner rounded box.
fn shadow_sdf(shape: u32, p: [f32; 2], half_ext: [f32; 2], radii: [f32; 4]) -> f32 {
    match shape {
        1 => {
            let r = [half_ext[0].max(1e-4), half_ext[1].max(1e-4)];
            let n = [p[0] / r[0], p[1] / r[1]];
            ((n[0] * n[0] + n[1] * n[1]).sqrt() - 1.0) * r[0].min(r[1])
        }
        2 => capsule_sdf(p, [0.0, 0.0], half_ext),
        _ => rrect_sdf(p, [0.0, 0.0], half_ext, radii),
    }
}

/// Abramowitz & Stegun 7.1.26 (max error ~1.5e-7), byte-exact against the
/// shader's `erf_approx`; `erf(0) == 0` exactly.
#[allow(clippy::excessive_precision)]
fn erf_approx(x: f32) -> f32 {
    let s = if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    };
    let ax = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * ax);
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
            + 0.254829592)
            * t
            * (-ax * ax).exp();
    s * y
}

/// Signed distance to a segment of half-width `hw` (round caps).
fn segment_sdf(p: [f32; 2], a: [f32; 2], b: [f32; 2], hw: f32) -> f32 {
    let pa = [p[0] - a[0], p[1] - a[1]];
    let ba = [b[0] - a[0], b[1] - a[1]];
    let denom = ba[0] * ba[0] + ba[1] * ba[1];
    let h = if denom > 0.0 {
        ((pa[0] * ba[0] + pa[1] * ba[1]) / denom).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let dx = pa[0] - ba[0] * h;
    let dy = pa[1] - ba[1] * h;
    (dx * dx + dy * dy).sqrt() - hw
}

/// A butt (`cap_ext == 0`) or square (`cap_ext == hw`) capped segment: the
/// segment SDF cut by the two end half-planes.
fn capped_segment_sdf(p: [f32; 2], a: [f32; 2], b: [f32; 2], hw: f32, cap_ext: f32) -> f32 {
    let ba = [b[0] - a[0], b[1] - a[1]];
    let len = (ba[0] * ba[0] + ba[1] * ba[1]).sqrt();
    let dir = if len > 0.0 {
        [ba[0] / len, ba[1] / len]
    } else {
        [1.0, 0.0]
    };
    let t = (p[0] - a[0]) * dir[0] + (p[1] - a[1]) * dir[1];
    let d = segment_sdf(p, a, b, hw);
    d.max((-(t + cap_ext)).max(t - (len + cap_ext)))
}

/// The whole advanced-blend composite of one pixel, premultiplied in and out.
/// `mode` is the render crate's `Blend` discriminant: `0..=12` Porter-Duff and
/// `Plus`, `13..=23` the separable W3C modes, `24..=27` the HSL ones.
pub(super) fn blend_composite(mode: u32, s: [f32; 4], d: [f32; 4]) -> [f32; 4] {
    let (sa, da) = (s[3], d[3]);
    let out = if mode <= 12 {
        let (fa, fb) = porter_duff(mode, sa, da);
        std::array::from_fn(|c| fa * s[c] + fb * d[c])
    } else {
        // Defined on straight color with the union of coverages as alpha:
        //   co = as*(1-ab)*cs + as*ab*B(cb,cs) + (1-as)*ab*cb
        let straight = |c: [f32; 4], a: f32| {
            if a > 0.0 {
                [c[0] / a, c[1] / a, c[2] / a]
            } else {
                [0.0; 3]
            }
        };
        let cs = straight(s, sa);
        let cb = straight(d, da);
        let b = if mode <= 23 {
            std::array::from_fn(|c| blend_separable(mode, cb[c], cs[c]))
        } else {
            blend_nonseparable(mode, cb, cs)
        };
        let mut out = [0.0f32; 4];
        for c in 0..3 {
            out[c] = sa * (1.0 - da) * cs[c] + sa * da * b[c] + (1.0 - sa) * da * cb[c];
        }
        out[3] = sa + da - sa * da;
        out
    };
    out.map(|c| c.clamp(0.0, 1.0))
}

/// The Porter-Duff coverage pair `(fa, fb)` for modes `0..=12`; `Plus` (and
/// any other mode) is `(1, 1)` under the caller's clamp.
fn porter_duff(mode: u32, sa: f32, da: f32) -> (f32, f32) {
    match mode {
        0 => (0.0, 0.0),
        1 => (1.0, 0.0),
        2 => (0.0, 1.0),
        3 => (1.0, 1.0 - sa),
        4 => (1.0 - da, 1.0),
        5 => (da, 0.0),
        6 => (0.0, sa),
        7 => (1.0 - da, 0.0),
        8 => (0.0, 1.0 - sa),
        9 => (da, 1.0 - sa),
        10 => (1.0 - da, sa),
        11 => (1.0 - da, 1.0 - sa),
        _ => (1.0, 1.0),
    }
}

/// `B(cb, cs)` of a separable mode, one straight channel; the dodge/burn
/// guards are the W3C limits.
fn blend_separable(mode: u32, cb: f32, cs: f32) -> f32 {
    let screen = |cb: f32, cs: f32| cb + cs - cb * cs;
    let hard_light = |cb: f32, cs: f32| {
        if cs <= 0.5 {
            cb * 2.0 * cs
        } else {
            screen(cb, 2.0 * cs - 1.0)
        }
    };
    match mode {
        13 => cb * cs,
        14 => screen(cb, cs),
        15 => hard_light(cs, cb),
        16 => cb.min(cs),
        17 => cb.max(cs),
        18 => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        19 => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        20 => hard_light(cb, cs),
        21 => {
            let d = if cb <= 0.25 {
                ((16.0 * cb - 12.0) * cb + 4.0) * cb
            } else {
                cb.sqrt()
            };
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        22 => (cb - cs).abs(),
        _ => cb + cs - 2.0 * cb * cs,
    }
}

/// `B(cb, cs)` of a non-separable mode (`24..=27`), from the W3C model:
/// Rec.601 luminosity, and clipping that scales about the luminosity.
fn blend_nonseparable(mode: u32, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    let lum = |c: [f32; 3]| 0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2];
    let clip_color = |c: [f32; 3]| {
        let l = lum(c);
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        let mut c = c;
        if n < 0.0 {
            let k = l / (l - n).max(1e-6);
            for v in c.iter_mut() {
                *v = l + (*v - l) * k;
            }
        }
        if x > 1.0 {
            let k = (1.0 - l) / (x - l).max(1e-6);
            for v in c.iter_mut() {
                *v = l + (*v - l) * k;
            }
        }
        c
    };
    let set_lum = |c: [f32; 3], l: f32| {
        let d = l - lum(c);
        clip_color([c[0] + d, c[1] + d, c[2] + d])
    };
    let sat = |c: [f32; 3]| c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2]);
    let set_sat = |c: [f32; 3], s: f32| {
        let n = c[0].min(c[1]).min(c[2]);
        let x = c[0].max(c[1]).max(c[2]);
        if x > n {
            c.map(|v| (v - n) * s / (x - n))
        } else {
            [0.0; 3]
        }
    };
    match mode {
        24 => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        25 => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        26 => set_lum(cs, lum(cb)),
        _ => set_lum(cb, lum(cs)),
    }
}

/// The default sampler a draw without one samples through.
pub(super) const LINEAR_CLAMP: SamplerDesc = SamplerDesc {
    filter: crate::resource::FilterMode::Linear,
    address: crate::resource::AddressMode::ClampToEdge,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_and_top_edges_own_their_boundary_in_either_winding() {
        let (tl, tr, bl) = ([0.0, 0.0], [4.0, 0.0], [0.0, 4.0]);
        for flip in [false, true] {
            let (a, b, c) = if flip { (tl, bl, tr) } else { (tl, tr, bl) };
            let area = edge_fn(a, b, c);
            let edge = |p: [f32; 2], q: [f32; 2]| owns_boundary(p, q, area);
            assert!(edge(tl, tr) || edge(tr, tl));
            assert!(edge(tl, bl) || edge(bl, tl));
            let diag = if flip { edge(bl, tr) } else { edge(tr, bl) };
            assert!(!diag, "the bottom-right diagonal must not own its samples");
        }
    }

    #[test]
    fn only_pixels_whose_centers_are_inside_the_quad_are_covered() {
        let span = |lo: f32, extent: f32| {
            let r = centers([lo, 0.0], [extent, 1.0], [0.0, 0.0]);
            let px = to_pixels(r, 100, 100).map_or((0, 0), |p| (p[0], p[2]));
            px
        };
        assert_eq!(span(4.6, 6.0), (5, 11));
        assert_eq!(span(4.5, 6.0), (4, 10));
        assert_eq!(span(-3.0, 5.0), (0, 2));
        assert_eq!(span(98.0, 10.0), (98, 100));
        assert_eq!(span(4.1, 0.2), (0, 0), "no center inside: nothing");
    }
}
