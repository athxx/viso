//! The scene's screen programs as the CPU raster runs them: each covers
//! the target with one triangle and computes these expressions per
//! fragment, as every backend's program does, in the same order.

use super::{
    AoUniforms, FrameUniforms, SceneTextures, SkyUniforms, add, cross, dot, length, normalize,
    scale, sub, tonemap, unpack_depth,
};

/// The night sky's color at the horizon and overhead, linear RGB.
const NIGHT_HORIZON: [f32; 3] = [0.0035, 0.0055, 0.011];
const NIGHT_ZENITH: [f32; 3] = [0.001, 0.0018, 0.0045];
/// The twilight's color across the sky and in its glow around the set sun.
const DUSK_SKY: [f32; 3] = [0.01, 0.018, 0.045];
const DUSK_GLOW: [f32; 3] = [0.1, 0.032, 0.006];

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The unit direction a fragment at clip-space `ndc` looks along: from the
/// point it covers on the near plane toward the one twice as far in depth,
/// unprojected by `inv_view_proj`, so a perspective and an orthographic
/// camera alike.
pub fn view_ray(inv_view_proj: &[f32; 16], ndc: [f32; 2]) -> [f32; 3] {
    let m = inv_view_proj;
    let at = |z: f32| {
        let p: [f32; 4] =
            core::array::from_fn(|i| m[i] * ndc[0] + m[4 + i] * ndc[1] + m[8 + i] * z + m[12 + i]);
        [p[0] / p[3], p[1] / p[3], p[2] / p[3]]
    };
    normalize(sub(at(0.5), at(1.0)))
}

/// The radiance, linear RGB, of the sky `sky` describes toward unit
/// direction `d` (`y` up): the Preetham model's day sky, faded toward a dark
/// night dome as the sun sets, with a twilight glow around where it went
/// down, and — when `disc` — the sun's disc, its rim smoothed.
pub fn sky_radiance(sky: &SkyUniforms, d: [f32; 3], disc: bool) -> [f32; 3] {
    let altitude = d[1];
    let cos_theta = altitude.max(0.01);
    let model = [sky.sun_model[0], sky.sun_model[1], sky.sun_model[2]];
    let cos_gamma = dot(d, model).clamp(-1.0, 1.0);
    let gamma = cos_gamma.acos();
    let perez = |c: [f32; 4], e: f32| {
        (1.0 + c[0] * (c[1] / cos_theta).exp())
            * (1.0 + c[2] * (c[3] * gamma).exp() + e * cos_gamma * cos_gamma)
    };
    let big_y = sky.zenith[0] * perez(sky.perez_y, sky.perez_e[0]) * sky.perez_e[3];
    let cx = sky.zenith[1] * perez(sky.perez_x, sky.perez_e[1]);
    let cy = (sky.zenith[2] * perez(sky.perez_yc, sky.perez_e[2])).max(0.0001);
    let below = (-altitude * 3.0).clamp(0.0, 1.0);
    // xyY to XYZ to linear sRGB.
    let big_x = cx * (big_y / cy);
    let big_z = (1.0 - cx - cy) * (big_y / cy);
    let day_weight = (1.0 - below * sky.zenith[3]) * sky.blend[0];
    let day = [
        (3.2406 * big_x - 1.5372 * big_y - 0.4986 * big_z).max(0.0) * day_weight,
        (-0.9689 * big_x + 1.8758 * big_y + 0.0415 * big_z).max(0.0) * day_weight,
        (0.0557 * big_x - 0.204 * big_y + 1.057 * big_z).max(0.0) * day_weight,
    ];
    let height = altitude.max(0.0).sqrt();
    let night_weight = (1.0 - sky.blend[0]) * (1.0 - 0.55 * below);
    let sun = [sky.sun[0], sky.sun[1], sky.sun[2]];
    let facing = dot(d, sun);
    let sun_gamma = facing.clamp(-1.0, 1.0).acos();
    let glow = (-altitude.abs() * 7.0).exp() * (-sun_gamma * 3.5).exp();
    let mut c: [f32; 3] = core::array::from_fn(|i| {
        let night = NIGHT_HORIZON[i] + (NIGHT_ZENITH[i] - NIGHT_HORIZON[i]) * height;
        let dusk = (DUSK_SKY[i] * (0.35 + 0.65 * height) + DUSK_GLOW[i] * glow) * sky.blend[1];
        day[i] + night * night_weight + dusk
    });
    if disc && sky.sun[3] > 0.0 && facing > 0.0 {
        // The angle from the sun's centre, exact at the small angles of
        // its disc where an arc cosine is not.
        let angle = length(cross(d, sun));
        let r = sky.sun[3];
        let inside = 1.0 - smoothstep(r * 0.85, r * 1.15, angle);
        for (ci, s) in c.iter_mut().zip(sky.sun_radiance) {
            *ci += s * inside;
        }
    }
    c
}

/// The color of the sky fragment at clip-space `ndc`: the sky along its
/// view ray, sun included, exposed by the frame's exposure and tone mapped,
/// opaque.
pub fn shade_sky(frame: &FrameUniforms, sky: &SkyUniforms, ndc: [f32; 2]) -> [f32; 4] {
    let d = view_ray(&sky.inv_view_proj, ndc);
    let r = sky_radiance(sky, d, true);
    let c = tonemap(r.map(|v| v * frame.camera[3]));
    [
        c[0].clamp(0.0, 1.0),
        c[1].clamp(0.0, 1.0),
        c[2].clamp(0.0, 1.0),
        1.0,
    ]
}

/// How many hemisphere taps an occlusion texel takes.
const AO_TAPS: u32 = 12;

/// The target coordinate of clip-space `ndc`, `y` down.
#[inline]
fn uv_of(ndc: [f32; 2]) -> [f32; 2] {
    [ndc[0] * 0.5 + 0.5, 0.5 - ndc[1] * 0.5]
}

/// The clip-space point of target coordinate `uv`.
#[inline]
fn ndc_of(uv: [f32; 2]) -> [f32; 2] {
    [uv[0] * 2.0 - 1.0, 1.0 - uv[1] * 2.0]
}

/// The world point at clip-space `ndc` and depth `z`.
fn unproject(m: &[f32; 16], ndc: [f32; 2], z: f32) -> [f32; 3] {
    let p: [f32; 4] =
        core::array::from_fn(|i| m[i] * ndc[0] + m[4 + i] * ndc[1] + m[8 + i] * z + m[12 + i]);
    [p[0] / p[3], p[1] / p[3], p[2] / p[3]]
}

/// The view's clip depth at target coordinate `uv`, from its packed
/// `1 - z` in texture slot 6; `0.0` where nothing was drawn.
fn view_depth(tex: &dyn SceneTextures, uv: [f32; 2]) -> f32 {
    1.0 - unpack_depth(tex.sample(6, uv, [0.0; 2], [0.0; 2]))
}

/// Occlusion `ao` with distance `d` from the camera packed into a texel:
/// the occlusion in red, `log2(1 + d) / 16` in sixteen bits of green and
/// blue. `d` of `0.0` is no surface.
fn pack_occlusion(ao: f32, d: f32) -> [f32; 4] {
    let e = ((1.0 + d).log2() / 16.0).clamp(0.0, 1.0);
    let v = (e * 65535.0).floor();
    let hi = (v / 256.0).floor();
    let lo = v - hi * 256.0;
    [ao, hi / 255.0, lo / 255.0, 1.0]
}

/// The distance from the camera an occlusion texel holds; `0.0` for none.
fn occlusion_distance(t: [f32; 4]) -> f32 {
    let v = (t[1] * 255.0 + 0.5).floor() * 256.0 + (t[2] * 255.0 + 0.5).floor();
    (v / 65535.0 * 16.0).exp2() - 1.0
}

/// The ambient occlusion of the view's texel at clip-space `ndc`, packed
/// with its distance from the camera (red the share of the hemisphere
/// open, `1.0` unoccluded).
///
/// The surface there is unprojected from the view's depth; its normal comes
/// from the neighbouring texels' points, each axis from the side nearer in
/// depth so a silhouette lends it no false slope. Twelve taps spiral over
/// the hemisphere about the normal, lifted off the tangent plane and
/// denser near the point, turned per texel by a fixed noise; a tap is
/// occluded by the surface its texel shows when that stands in front of
/// it by more than the bias — so surfaces closer together than the bias
/// never occlude each other — fading out where the surface is more than
/// a radius farther or nearer than the point, a different object.
pub fn shade_ao(
    frame: &FrameUniforms,
    ao: &AoUniforms,
    ndc: [f32; 2],
    tex: &dyn SceneTextures,
) -> [f32; 4] {
    let inv = &ao.inv_view_proj;
    let eye = [frame.camera[0], frame.camera[1], frame.camera[2]];
    let uv = uv_of(ndc);
    let z = view_depth(tex, uv);
    if z <= 0.0 {
        return pack_occlusion(1.0, 0.0);
    }
    let p = unproject(inv, ndc, z);
    let d = length(sub(p, eye));
    // A neighbour's point and distance; a distance of 0 where it is empty.
    let near = |du: f32, dv: f32| {
        let at = [uv[0] + du, uv[1] + dv];
        let z = view_depth(tex, at);
        if z <= 0.0 {
            ([0.0; 3], 0.0)
        } else {
            let q = unproject(inv, ndc_of(at), z);
            (q, length(sub(q, eye)))
        }
    };
    let (tx, ty) = (ao.texel[0], ao.texel[1]);
    let (pl, dl) = near(-tx, 0.0);
    let (pr, dr) = near(tx, 0.0);
    let (pu, du) = near(0.0, -ty);
    let (pd, dd) = near(0.0, ty);
    let ddx = if dr <= 0.0 || (dl > 0.0 && (dl - d).abs() < (dr - d).abs()) {
        sub(p, pl)
    } else {
        sub(pr, p)
    };
    let ddy = if dd <= 0.0 || (du > 0.0 && (du - d).abs() < (dd - d).abs()) {
        sub(p, pu)
    } else {
        sub(pd, p)
    };
    let to_eye = sub(eye, p);
    let mut n = cross(ddx, ddy);
    if length(n) < 1e-12 {
        n = to_eye;
    }
    n = normalize(n);
    if dot(n, to_eye) < 0.0 {
        n = scale(n, -1.0);
    }
    let up = if n[1].abs() > 0.9 {
        [1.0, 0.0, 0.0]
    } else {
        [0.0, 1.0, 0.0]
    };
    let t = normalize(cross(up, n));
    let b = cross(n, t);
    // Interleaved gradient noise of the texel: fixed, so it never crawls.
    let px = (uv[0] * ao.texel[2]).floor();
    let py = (uv[1] * ao.texel[3]).floor();
    let inner = 0.06711056 * px + 0.00583715 * py;
    let outer = 52.982_92 * (inner - inner.floor());
    let rot = (outer - outer.floor()) * core::f32::consts::TAU;
    let radius = ao.params[0];
    let bias = ao.params[1] + d * ao.params[2];
    let mut occ = 0.0f32;
    for k in 0..AO_TAPS {
        let kf = k as f32;
        let fi = (kf + 0.5) / AO_TAPS as f32;
        let ang = kf * 2.3999632 + rot;
        let zt = 0.18 + 0.74 * fi;
        let r = (1.0 - zt * zt).max(0.0).sqrt();
        let reach = (0.2 + 0.8 * fi * fi) * radius;
        let dir = add(
            add(scale(t, ang.cos() * r), scale(b, ang.sin() * r)),
            scale(n, zt),
        );
        let s = add(p, scale(dir, reach));
        let c = frame.clip(s);
        if c[3] > 0.0 {
            let su = [c[0] / c[3] * 0.5 + 0.5, 0.5 - c[1] / c[3] * 0.5];
            if su[0] > 0.0 && su[0] < 1.0 && su[1] > 0.0 && su[1] < 1.0 {
                let sz = view_depth(tex, su);
                if sz > 0.0 {
                    let q = unproject(inv, ndc_of(su), sz);
                    let sd = length(sub(q, eye));
                    let ahead = length(sub(s, eye)) - sd;
                    let range = 1.0 - smoothstep(radius, radius * 2.0, (d - sd).abs());
                    occ += smoothstep(bias, bias * 2.0, ahead) * range;
                }
            }
        }
    }
    pack_occlusion((1.0 - occ / AO_TAPS as f32).clamp(0.0, 1.0), d)
}

/// One axis of the occlusion blur at clip-space `ndc`: nine taps of the
/// occlusion in texture slot 6 along the step, Gaussian over the texels
/// and over the difference in distance (relative to the distance), so it
/// never averages across a silhouette or a step; its distance kept.
pub fn shade_ao_blur(ao: &AoUniforms, ndc: [f32; 2], tex: &dyn SceneTextures) -> [f32; 4] {
    let uv = uv_of(ndc);
    let c = tex.sample(6, uv, [0.0; 2], [0.0; 2]);
    let d = occlusion_distance(c);
    if d <= 0.0 {
        return [c[0], c[1], c[2], 1.0];
    }
    let sigma = 0.03 * d + 0.02;
    let mut sum = 0.0f32;
    let mut weight = 0.0f32;
    for k in -4..=4 {
        let kf = k as f32;
        let at = [uv[0] + ao.step[0] * kf, uv[1] + ao.step[1] * kf];
        let s = tex.sample(6, at, [0.0; 2], [0.0; 2]);
        let sd = occlusion_distance(s);
        let dz = (sd - d) / sigma;
        let w = if sd > 0.0 {
            (-kf * kf * 0.1033).exp() * (-dz * dz).exp()
        } else {
            0.0
        };
        sum += s[0] * w;
        weight += w;
    }
    [sum / weight.max(0.0001), c[1], c[2], 1.0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_occlusion_texel_keeps_its_distance() {
        for d in [0.05f32, 1.0, 7.5, 120.0, 3000.0] {
            let t = pack_occlusion(0.5, d);
            let back = occlusion_distance(t.map(|v| (v * 255.0).round() / 255.0));
            assert!((back - d).abs() <= d * 0.002 + 1e-3, "{d}: {back}");
        }
        assert_eq!(occlusion_distance(pack_occlusion(1.0, 0.0)), 0.0);
    }
}
