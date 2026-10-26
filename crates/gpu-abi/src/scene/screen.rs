//! The scene's screen programs as the CPU raster runs them: each covers
//! the target with one triangle and computes these expressions per
//! fragment, as every backend's program does, in the same order.

use super::{FrameUniforms, SkyUniforms, cross, dot, length, normalize, sub, tonemap};

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
