//! The scene programs' fragment math as the CPU raster runs it: every
//! backend's program computes the same expressions, in the same order.

use core::f32::consts::PI;

use super::{
    Alpha, ENV_HEIGHT, ENV_LEVELS, ENV_WIDTH, FrameUniforms, MATERIAL_DOUBLE_SIDED,
    MATERIAL_NORMAL_MAP, MaterialUniforms, SHADOW_CASCADES, Shading, add, cross, dot, length,
    normalize, scale, sub,
};

/// The textures a fragment samples, by texture slot: each at a texture
/// coordinate, with the coordinate's screen derivatives choosing its detail.
pub trait SceneTextures {
    fn sample(&self, slot: usize, uv: [f32; 2], dx: [f32; 2], dy: [f32; 2]) -> [f32; 4];
}

/// What a mesh fragment reads: its interpolated varyings and their screen
/// derivatives.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeshFragment {
    pub world: [f32; 3],
    /// The interpolated normal, not normalized.
    pub normal: [f32; 3],
    pub tangent: [f32; 4],
    pub uv: [f32; 2],
    /// The instance's tint, premultiplied.
    pub color: [f32; 4],
    /// `uv`'s change one pixel right and one pixel down.
    pub uv_dx: [f32; 2],
    pub uv_dy: [f32; 2],
    /// The unit normal's change one pixel right and one pixel down.
    pub normal_dx: [f32; 3],
    pub normal_dy: [f32; 3],
    /// Where on the target it is, in pixels from the top left: a pixel's
    /// centre is at `+ 0.5`.
    pub pixel: [f32; 2],
}

/// The irradiance over π at unit normal `n` of an environment whose nine
/// spherical harmonic terms are `sh`.
#[inline]
pub fn sh_irradiance(sh: &[[f32; 4]; 9], n: [f32; 3]) -> [f32; 3] {
    let [x, y, z] = n;
    let basis = [
        1.0,
        y,
        z,
        x,
        x * y,
        y * z,
        3.0 * z * z - 1.0,
        x * z,
        x * x - y * y,
    ];
    core::array::from_fn(|c| {
        let mut e = 0.0;
        for (term, b) in sh.iter().zip(basis) {
            e += term[c] * b;
        }
        e.max(0.0)
    })
}

/// Where unit direction `d` falls on an equirectangular map: `u` around
/// `y` from `-z` (its centre), `v` from `+y` (its top) to `-y`.
#[inline]
pub fn env_direction_uv(d: [f32; 3]) -> [f32; 2] {
    [
        d[0].atan2(-d[2]) * (0.5 / PI) + 0.5,
        d[1].clamp(-1.0, 1.0).acos() / PI,
    ]
}

/// The environment atlas coordinate of `uv` on the map of roughness level
/// `level`, kept half a texel inside it so filtering never reads another
/// level.
#[inline]
pub fn env_atlas_uv(uv: [f32; 2], level: f32) -> [f32; 2] {
    let s = (-level).exp2();
    let w = ENV_WIDTH as f32 * s;
    let h = ENV_WIDTH as f32 * 0.5 * s;
    let x = (uv[0] * w).clamp(0.5, w - 0.5);
    let y = ENV_WIDTH as f32 - ENV_WIDTH as f32 * s + (uv[1] * h).clamp(0.5, h - 0.5);
    [x / ENV_WIDTH as f32, y / ENV_HEIGHT as f32]
}

/// The scale and bias of `f0` an environment's reflection is lit by at
/// roughness `roughness` seen at `nv`, by an analytic fit of the split-sum
/// integral.
#[inline]
pub fn env_brdf(roughness: f32, nv: f32) -> [f32; 2] {
    let r = [
        1.0 - roughness,
        -0.0275 * roughness + 0.0425,
        -0.572 * roughness + 1.04,
        0.022 * roughness - 0.04,
    ];
    let a = (r[0] * r[0]).min((-9.28 * nv).exp2()) * r[0] + r[1];
    [-1.04 * a + r[2], 1.04 * a + r[3]]
}

/// Linear light mapped into `[0, 1]`, hues kept: the Khronos PBR Neutral
/// curve, the identity on a base color lit by white light of intensity 1.
#[inline]
pub fn tonemap(c: [f32; 3]) -> [f32; 3] {
    let x = c[0].min(c[1].min(c[2]));
    let offset = if x < 0.08 { x - 6.25 * x * x } else { 0.04 };
    let d = [c[0] - offset, c[1] - offset, c[2] - offset];
    let peak = d[0].max(d[1].max(d[2]));
    if peak < 0.76 {
        return d;
    }
    let e = 0.24;
    let new_peak = 1.0 - e * e / (peak + e - 0.76);
    let d = scale(d, new_peak / peak);
    let g = 1.0 - 1.0 / (0.15 * (peak - new_peak) + 1.0);
    core::array::from_fn(|i| d[i] + (new_peak - d[i]) * g)
}

/// How much of a sprite covers `uv`: 1 at the centre of the unit square,
/// falling smoothly to 0 at the inscribed circle.
#[inline]
pub fn disc(uv: [f32; 2]) -> f32 {
    let (x, y) = (uv[0] * 2.0 - 1.0, uv[1] * 2.0 - 1.0);
    let t = (1.0 - (x * x + y * y).sqrt()).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn ggx(nh: f32, a2: f32) -> f32 {
    let d = nh * nh * (a2 - 1.0) + 1.0;
    a2 / (PI * d * d)
}

#[inline]
fn smith(nv: f32, nl: f32, a2: f32) -> f32 {
    let gv = nl * (nv * nv * (1.0 - a2) + a2).sqrt();
    let gl = nv * (nl * nl * (1.0 - a2) + a2).sqrt();
    0.5 / (gv + gl)
}

#[inline]
fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    core::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// The largest depth [`pack_depth`] stores, `2^24 - 1`.
const DEPTH_STEPS: f32 = 16_777_215.0;

/// Depth `z` in `[0, 1]` as the RGBA8 texel the shadow atlas stores: 24
/// bits, most significant byte in red, alpha one. Every channel is a whole
/// multiple of `1 / 255`, so an 8-bit target stores it exactly.
#[inline]
pub fn pack_depth(z: f32) -> [f32; 4] {
    let v = (z.clamp(0.0, 1.0) * DEPTH_STEPS).floor();
    let hi = (v * (1.0 / 65536.0)).floor();
    let rest = v - hi * 65536.0;
    let mid = (rest * (1.0 / 256.0)).floor();
    let lo = rest - mid * 256.0;
    [hi / 255.0, mid / 255.0, lo / 255.0, 1.0]
}

/// The depth a texel [`pack_depth`] made holds.
#[inline]
pub fn unpack_depth(t: [f32; 4]) -> f32 {
    let b = |c: f32| (c * 255.0 + 0.5).floor();
    (b(t[0]) * 65536.0 + b(t[1]) * 256.0 + b(t[2])) / DEPTH_STEPS
}

/// `p` in cascade `c`'s space: its x and y in `[-1, 1]` over the tile and
/// its depth from the light.
#[inline]
fn cascade_point(frame: &FrameUniforms, c: usize, p: [f32; 3]) -> [f32; 3] {
    let row = |k: usize| {
        let r = frame.shadow[c * 4 + k];
        r[0] * p[0] + r[1] * p[1] + r[2] * p[2] + r[3]
    };
    [row(0), row(1), row(2)]
}

/// Whether cascade point `q` lies inside its cascade.
#[inline]
fn in_cascade(q: [f32; 3]) -> bool {
    q[0].abs().max(q[1].abs()) < 1.0 && q[2] >= 0.0 && q[2] <= 1.0
}

/// The share of light reaching depth `reference` around tile coordinate
/// `uv` of cascade `c`: a 4×4 tent of depth comparisons, the 3×3 box
/// filter of the nearest texels interpolated across them so the penumbra
/// is continuous.
fn shadow_pcf(
    frame: &FrameUniforms,
    c: usize,
    uv: [f32; 2],
    reference: f32,
    tex: &dyn SceneTextures,
) -> f32 {
    let e = frame.shadow_params[1];
    let (tu, tv) = (uv[0] / e - 0.5, uv[1] / e - 0.5);
    let (fu, fv) = (tu - tu.floor(), tv - tv.floor());
    let (bu, bv) = ((tu.floor() + 0.5) * e, (tv.floor() + 0.5) * e);
    let weight = |i: usize, f: f32| match i {
        0 => 1.0 - f,
        3 => f,
        _ => 1.0,
    };
    let mut sum = 0.0;
    for j in 0..4 {
        let v = (bv + (j as f32 - 1.0) * e).clamp(0.5 * e, 1.0 - 0.5 * e);
        for i in 0..4 {
            let u = (bu + (i as f32 - 1.0) * e).clamp(0.5 * e, 1.0 - 0.5 * e);
            let at = [(u + c as f32) / SHADOW_CASCADES as f32, v];
            let depth = unpack_depth(tex.sample(6, at, [0.0; 2], [0.0; 2]));
            if reference <= depth {
                sum += weight(i, fu) * weight(j, fv);
            }
        }
    }
    (sum / 9.0).min(1.0)
}

/// How much of the directional light reaches world point `p` of a surface
/// whose unit geometric normal, turned toward the viewer, is `n`: `1.0`
/// lit, down to `1 - strength` in full shadow.
///
/// The point is looked up in the nearest cascade that holds it, nudged
/// along its normal and compared with a bias, both growing as the light
/// grazes the surface; within the outer tenth of a cascade it cross-fades
/// into the next, and past the last into full light.
pub fn shadow_visibility(
    frame: &FrameUniforms,
    p: [f32; 3],
    n: [f32; 3],
    tex: &dyn SceneTextures,
) -> f32 {
    let strength = frame.shadow_params[0];
    if strength <= 0.0 {
        return 1.0;
    }
    let l = [frame.light_dir[0], frame.light_dir[1], frame.light_dir[2]];
    let slope = 1.0 - dot(n, l).clamp(0.0, 1.0);
    let Some(first) = (0..SHADOW_CASCADES).find(|&c| in_cascade(cascade_point(frame, c, p))) else {
        return 1.0;
    };
    let mut visible = 0.0;
    let mut left = 1.0;
    for c in first..SHADOW_CASCADES {
        let extra = frame.shadow[c * 4 + 3];
        let q = cascade_point(frame, c, add(p, scale(n, extra[1] * 1.5 * slope)));
        if !in_cascade(q) {
            visible += left;
            break;
        }
        let reference = q[2] - extra[0] * (1.0 + 2.0 * slope);
        let lit = shadow_pcf(
            frame,
            c,
            [q[0] * 0.5 + 0.5, 0.5 - q[1] * 0.5],
            reference,
            tex,
        );
        let handed = ((q[0].abs().max(q[1].abs()) - 0.9) * 10.0).clamp(0.0, 1.0);
        visible += left * (1.0 - handed) * lit;
        left *= handed;
        if left <= 0.0 {
            break;
        }
        if c == SHADOW_CASCADES - 1 {
            visible += left;
        }
    }
    1.0 - strength * (1.0 - visible)
}

/// The shadow atlas texel of depth fragment `f` in `material` under
/// `frame` (its cascade's): the [`pack_depth`]ed depth from the light, or
/// `None` where a masked material, or an invisible instance, discards it.
pub fn shade_depth(
    frame: &FrameUniforms,
    material: &MaterialUniforms,
    f: &MeshFragment,
    tex: &dyn SceneTextures,
) -> Option<[f32; 4]> {
    let tint_a = f.color[3];
    if tint_a <= 0.0 {
        return None;
    }
    if material.alpha() == Alpha::Mask {
        let a = tex.sample(0, f.uv, f.uv_dx, f.uv_dy)[3] * material.base_color[3] * tint_a;
        if a < material.mode[2] {
            return None;
        }
    }
    let clip = frame.clip(f.world);
    Some(pack_depth(1.0 - clip[2] / clip[3]))
}

/// The color of mesh fragment `f` in `material` under `frame`, premultiplied
/// linear light in `[0, 1]`, or `None` where the fragment is discarded:
///
/// - lit: Lambert diffuse and a GGX specular lobe (Smith height-correlated
///   visibility, Schlick Fresnel) from the light; the environment's
///   spherical-harmonic irradiance and prefiltered reflection, occluded by
///   the occlusion map and the view's ambient occlusion; emission; then
///   exposure and tone mapping;
/// - unlit: the color as is;
/// - sprite: the color as is, covering a disc across `uv`;
///
/// then fog.
pub fn shade_mesh(
    frame: &FrameUniforms,
    material: &MaterialUniforms,
    f: &MeshFragment,
    tex: &dyn SceneTextures,
) -> Option<[f32; 4]> {
    let n_geom = normalize(f.normal);
    let sample = |slot: usize| tex.sample(slot, f.uv, f.uv_dx, f.uv_dy);
    let factor = material.base_color;
    let texel = sample(0);
    let mut base: [f32; 4] = core::array::from_fn(|i| texel[i] * factor[i]);
    let tint_a = f.color[3];
    if tint_a <= 0.0 {
        return None;
    }
    for (b, c) in base.iter_mut().zip(f.color).take(3) {
        *b *= c / tint_a;
    }
    base[3] *= tint_a;
    let mut a = tint_a;
    match material.alpha() {
        Alpha::Blend => a = base[3],
        Alpha::Mask if base[3] < material.mode[2] => return None,
        _ => {}
    }
    let camera = [frame.camera[0], frame.camera[1], frame.camera[2]];
    let to_eye = sub(camera, f.world);
    let distance = length(to_eye);
    let mut c = [base[0], base[1], base[2]];
    match material.shading() {
        Shading::Sprite => {
            a *= disc(f.uv);
            if a <= 0.0 {
                return None;
            }
        }
        Shading::Unlit => {}
        Shading::Lit => {
            let v = scale(to_eye, 1.0 / distance.max(1e-6));
            let bits = material.flags();
            let side = if bits & MATERIAL_DOUBLE_SIDED != 0 && dot(n_geom, v) < 0.0 {
                -1.0
            } else {
                1.0
            };
            let mut n = scale(n_geom, side);
            let params = material.params;
            if bits & MATERIAL_NORMAL_MAP != 0 {
                let m = sample(2);
                let tn = [
                    (m[0] * 2.0 - 1.0) * params[2],
                    (m[1] * 2.0 - 1.0) * params[2],
                    m[2] * 2.0 - 1.0,
                ];
                let t = scale([f.tangent[0], f.tangent[1], f.tangent[2]], side);
                let t = sub(t, scale(n, dot(n, t)));
                let tl = length(t);
                if tl > 1e-6 {
                    let t = scale(t, 1.0 / tl);
                    let b = scale(cross(n, t), f.tangent[3] * side);
                    n = normalize(add(add(scale(t, tn[0]), scale(b, tn[1])), scale(n, tn[2])));
                }
            }
            let mr = sample(1);
            let metallic = (params[0] * mr[2]).clamp(0.0, 1.0);
            let roughness = (params[1] * mr[1]).clamp(0.045, 1.0);
            // The occlusion map's, and the view's ambient occlusion where
            // this pixel is.
            let screen = tex.sample(
                7,
                [
                    f.pixel[0] * frame.occlusion[1],
                    f.pixel[1] * frame.occlusion[2],
                ],
                [0.0; 2],
                [0.0; 2],
            );
            let ambient_occlusion = 1.0 - frame.occlusion[0] * (1.0 - screen[0]);
            let occlusion = (1.0 + params[3] * (sample(3)[0] - 1.0)) * ambient_occlusion;
            let glow = sample(4);
            let emissive: [f32; 3] = core::array::from_fn(|i| glow[i] * material.emissive[i]);

            let f0 = mix3([0.04; 3], c, metallic);
            let albedo = scale(c, 1.0 - metallic);
            let alpha = roughness * roughness;
            let variance = 0.15 * (dot(f.normal_dx, f.normal_dx) + dot(f.normal_dy, f.normal_dy));
            let a2 = (alpha * alpha + (2.0 * variance).min(0.2)).clamp(0.0, 1.0);
            let nv = dot(n, v).max(0.0001);

            let l = [frame.light_dir[0], frame.light_dir[1], frame.light_dir[2]];
            let nl = dot(n, l).max(0.0);
            let mut color = [0.0f32; 3];
            if nl > 0.0 {
                let shadow = shadow_visibility(frame, f.world, scale(n_geom, side), tex);
                let h = normalize(add(l, v));
                let nh = dot(n, h).max(0.0);
                let vh = dot(v, h).max(0.0);
                let k = (1.0 - vh).powf(5.0);
                let spec = ggx(nh, a2) * smith(nv, nl, a2);
                let e = frame.light_dir[3] * nl * shadow;
                for i in 0..3 {
                    let fresnel = f0[i] + (1.0 - f0[i]) * k;
                    let brdf = (1.0 - fresnel) * albedo[i] + fresnel * (spec * PI);
                    color[i] = brdf * (frame.light_color[i] * e);
                }
            }

            let irradiance = sh_irradiance(&frame.sh, n);
            let r = sub(scale(n, 2.0 * dot(n, v)), v);
            let lod = roughness * (ENV_LEVELS - 1) as f32;
            let dir = env_direction_uv(r);
            let k0 = lod.floor();
            let k1 = (k0 + 1.0).min((ENV_LEVELS - 1) as f32);
            let e0 = tex.sample(5, env_atlas_uv(dir, k0), [0.0; 2], [0.0; 2]);
            let e1 = tex.sample(5, env_atlas_uv(dir, k1), [0.0; 2], [0.0; 2]);
            let prefiltered = mix3([e0[0], e0[1], e0[2]], [e1[0], e1[1], e1[2]], lod - k0);
            let ab = env_brdf(roughness, nv);
            let spec_occ = ((nv + occlusion).powf((-16.0 * roughness - 1.0).exp2()) - 1.0
                + occlusion)
                .clamp(0.0, 1.0);
            let ambient = frame.ambient;
            for i in 0..3 {
                let reflected = prefiltered[i] * (f0[i] * ab[0] + ab[1]);
                color[i] += albedo[i] * irradiance[i] * occlusion * ambient[0]
                    + reflected * (spec_occ * ambient[1])
                    + emissive[i];
            }
            c = tonemap(scale(color, frame.camera[3]));
        }
    }
    let fog = 1.0 - (-frame.fog[3] * distance).exp();
    let lit: [f32; 3] =
        core::array::from_fn(|i| (c[i] + (frame.fog[i] - c[i]) * fog).clamp(0.0, 1.0));
    Some([lit[0] * a, lit[1] * a, lit[2] * a, a])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every texture white and the environment black.
    struct Plain;

    impl SceneTextures for Plain {
        fn sample(&self, slot: usize, _: [f32; 2], _: [f32; 2], _: [f32; 2]) -> [f32; 4] {
            if slot == 5 {
                [0.0, 0.0, 0.0, 1.0]
            } else {
                [1.0; 4]
            }
        }
    }

    fn frame() -> FrameUniforms {
        FrameUniforms {
            camera: [0.0, 0.0, 5.0, 1.0],
            light_dir: [0.0, 1.0, 0.0, 1.0],
            light_color: [1.0; 4],
            ambient: [1.0, 1.0, 0.0, 0.0],
            ..FrameUniforms::default()
        }
    }

    fn material(shading: Shading) -> MaterialUniforms {
        MaterialUniforms {
            base_color: [0.5, 0.25, 0.75, 1.0],
            params: [0.0, 1.0, 1.0, 1.0],
            mode: [shading as u8 as f32, 0.0, 0.5, 0.0],
            ..MaterialUniforms::default()
        }
    }

    fn fragment(normal: [f32; 3], uv: [f32; 2]) -> MeshFragment {
        MeshFragment {
            normal,
            uv,
            color: [1.0; 4],
            tangent: [1.0, 0.0, 0.0, 1.0],
            ..MeshFragment::default()
        }
    }

    #[test]
    fn a_face_toward_the_light_is_brighter_than_one_away() {
        let m = material(Shading::Lit);
        let lit = shade_mesh(&frame(), &m, &fragment([0.0, 1.0, 0.0], [0.5; 2]), &Plain).unwrap();
        let dark = shade_mesh(&frame(), &m, &fragment([0.0, -1.0, 0.0], [0.5; 2]), &Plain).unwrap();
        assert!(lit[0] > 0.3 && dark[0] < 0.01, "{lit:?} {dark:?}");
        assert_eq!(lit[3], 1.0);
    }

    #[test]
    fn a_rough_white_surface_under_white_light_reflects_about_its_color() {
        let mut m = material(Shading::Lit);
        m.base_color = [0.5, 0.5, 0.5, 1.0];
        let f = fragment([0.0, 1.0, 0.0], [0.5; 2]);
        let mut fr = frame();
        fr.camera = [0.0, 5.0, 0.0, 1.0];
        let c = shade_mesh(&fr, &m, &f, &Plain).unwrap();
        // Diffuse 0.48, a little specular, through the neutral curve.
        assert!((c[0] - 0.5).abs() < 0.06, "{c:?}");
    }

    #[test]
    fn a_packed_depth_survives_an_8_bit_target() {
        for z in [0.0, 1.0, 0.5, 0.123_456_7, 0.999_999, 1.0e-7] {
            let texel = pack_depth(z).map(|c| (c * 255.0).round() / 255.0);
            assert!((unpack_depth(texel) - z).abs() < 1.0e-7, "{z}");
        }
        // The cleared atlas reads as farthest.
        assert_eq!(unpack_depth([1.0; 4]), 1.0);
    }

    /// One cascade over x and z in [-1, 1], looking down from depth 0 at
    /// y = 1 to depth 1 at y = -1, its atlas every texel at `occluder`
    /// depth.
    struct Atlas(f32);

    impl SceneTextures for Atlas {
        fn sample(&self, slot: usize, _: [f32; 2], _: [f32; 2], _: [f32; 2]) -> [f32; 4] {
            if slot == 6 {
                pack_depth(self.0)
            } else {
                [1.0; 4]
            }
        }
    }

    fn shadowed_frame() -> FrameUniforms {
        let mut f = frame();
        f.shadow_params = [1.0, 1.0 / 64.0, 0.0, 0.0];
        for c in 0..SHADOW_CASCADES {
            let k = (1 << c) as f32;
            f.shadow[c * 4] = [1.0 / k, 0.0, 0.0, 0.0];
            f.shadow[c * 4 + 1] = [0.0, 0.0, -1.0 / k, 0.0];
            f.shadow[c * 4 + 2] = [0.0, -0.5, 0.0, 0.5];
            f.shadow[c * 4 + 3] = [0.001, 2.0 * k / 64.0, 0.0, 0.0];
        }
        f
    }

    #[test]
    fn a_point_behind_an_occluder_is_in_shadow_and_one_before_it_is_lit() {
        let f = shadowed_frame();
        let up = [0.0, 1.0, 0.0];
        // An occluder at y = 0 (depth 0.5): the ground below is shadowed.
        assert_eq!(
            shadow_visibility(&f, [0.1, -0.5, 0.2], up, &Atlas(0.5)),
            0.0
        );
        assert_eq!(shadow_visibility(&f, [0.1, 0.5, 0.2], up, &Atlas(0.5)), 1.0);
        // The occluder itself does not shadow its own surface.
        assert_eq!(shadow_visibility(&f, [0.1, 0.0, 0.2], up, &Atlas(0.5)), 1.0);
        // Half the strength, half the darkness; past every cascade, lit.
        let mut half = f;
        half.shadow_params[0] = 0.5;
        assert_eq!(
            shadow_visibility(&half, [0.1, -0.5, 0.2], up, &Atlas(0.5)),
            0.5
        );
        assert_eq!(
            shadow_visibility(&f, [9.0, -0.5, 0.2], up, &Atlas(0.5)),
            1.0
        );
        // Near a cascade's rim the next one takes over, still shadowed.
        assert_eq!(
            shadow_visibility(&f, [0.97, -0.5, 0.2], up, &Atlas(0.5)),
            0.0
        );
    }

    #[test]
    fn a_shadow_darkens_only_the_direct_light() {
        let mut fr = shadowed_frame();
        fr.camera = [0.0, 5.0, 0.0, 1.0];
        let m = material(Shading::Lit);
        let mut f = fragment([0.0, 1.0, 0.0], [0.5; 2]);
        f.world = [0.1, -0.5, 0.2];
        let shadowed = shade_mesh(&fr, &m, &f, &Atlas(0.5)).unwrap();
        let lit = shade_mesh(&fr, &m, &f, &Atlas(1.0)).unwrap();
        assert!(shadowed[0] < lit[0] * 0.2, "{shadowed:?} {lit:?}");
    }

    #[test]
    fn a_depth_fragment_stores_its_distance_from_the_light_and_a_mask_cuts_it() {
        let mut fr = frame();
        // Clip z 0.75 at the origin: depth from the light 0.25.
        fr.view_proj = [
            1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.75, 1.0,
        ];
        let mut m = material(Shading::Lit);
        let f = fragment([0.0, 1.0, 0.0], [0.5; 2]);
        let t = shade_depth(&fr, &m, &f, &Plain).unwrap();
        assert!((unpack_depth(t) - 0.25).abs() < 1.0e-6);
        m.mode[1] = Alpha::Mask as u8 as f32;
        m.base_color[3] = 0.25;
        assert!(shade_depth(&fr, &m, &f, &Plain).is_none());
    }

    #[test]
    fn the_irradiance_of_a_constant_environment_is_that_constant() {
        let mut sh = [[0.0; 4]; 9];
        sh[0] = [0.25, 0.5, 1.0, 0.0];
        for n in [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]] {
            assert_eq!(sh_irradiance(&sh, n), [0.25, 0.5, 1.0]);
        }
    }

    #[test]
    fn the_neutral_curve_keeps_mid_tones_and_compresses_highlights() {
        let mid = tonemap([0.5, 0.3, 0.2]);
        assert!((mid[0] - 0.46).abs() < 0.01, "{mid:?}");
        let hot = tonemap([8.0, 4.0, 2.0]);
        assert!(hot.iter().all(|&c| c <= 1.0) && hot[0] > hot[1] && hot[1] > hot[2]);
    }

    #[test]
    fn each_atlas_level_stays_inside_its_own_rows() {
        for k in 0..ENV_LEVELS {
            let top = env_atlas_uv([0.0, 0.0], k as f32)[1] * ENV_HEIGHT as f32;
            let bottom = env_atlas_uv([1.0, 1.0], k as f32)[1] * ENV_HEIGHT as f32;
            let row = (ENV_WIDTH - (ENV_WIDTH >> k)) as f32;
            let height = (ENV_WIDTH >> (k + 1)) as f32;
            assert!(
                top >= row && bottom <= row + height,
                "level {k}: {top} {bottom}"
            );
        }
        assert_eq!(env_direction_uv([0.0, 0.0, -1.0]), [0.5, 0.5]);
        assert_eq!(env_direction_uv([0.0, 1.0, 0.0])[1], 0.0);
    }

    #[test]
    fn unlit_keeps_its_color_and_a_sprite_fades_to_its_rim() {
        let m = material(Shading::Unlit);
        let up = shade_mesh(&frame(), &m, &fragment([0.0, 1.0, 0.0], [0.5; 2]), &Plain).unwrap();
        for (a, b) in up.iter().zip(m.base_color) {
            assert!((a - b).abs() < 1e-5, "{up:?}");
        }
        let s = material(Shading::Sprite);
        let at = |uv| shade_mesh(&frame(), &s, &fragment([0.0; 3], uv), &Plain);
        assert!((at([0.5, 0.5]).unwrap()[3] - 1.0).abs() < 1e-6);
        assert!((at([0.75, 0.5]).unwrap()[3] - 0.5).abs() < 1e-6);
        assert_eq!(at([1.0, 0.5]), None);
    }

    #[test]
    fn a_masked_material_drops_what_falls_under_its_cutoff() {
        let mut m = material(Shading::Unlit);
        m.mode[1] = Alpha::Mask as u8 as f32;
        m.base_color[3] = 0.4;
        let f = fragment([0.0, 1.0, 0.0], [0.5; 2]);
        assert_eq!(shade_mesh(&frame(), &m, &f, &Plain), None);
        m.base_color[3] = 0.6;
        assert_eq!(shade_mesh(&frame(), &m, &f, &Plain).unwrap()[3], 1.0);
    }
}
