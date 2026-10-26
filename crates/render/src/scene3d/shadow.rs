//! The directional light's cascaded shadow: the cascades fitted to the part
//! of the view that holds receivers, and the transform each one's casters
//! are drawn into its tile of the shadow atlas by.
//!
//! Each cascade covers the bounding sphere of a slice of the view frustum,
//! so its texels keep their size as the camera turns; its centre snaps to
//! whole texels across the light, so its texels keep their place as the
//! camera moves; and its depth reaches back toward the light to every
//! caster, so one outside the slice still shadows into it. The slices'
//! distances step along a fixed geometric ladder rather than following the
//! receivers continuously, so the texel size changes in rare steps.

use viso_gpu::scene::SHADOW_CASCADES;
use viso_math::{Mat4, Vec3, vec3};

use super::{Camera, Projection};

/// How the directional light casts shadows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shadows {
    /// How much of the light's direct share a shadow takes away: `1.0` all
    /// of it, `0.0` none — and then no shadow is drawn at all.
    pub strength: f32,
    /// How far from the camera shadows reach at most. The cascades cover
    /// the part of the view that holds something to shadow, within this.
    pub distance: f32,
    /// The edge, in texels, of each cascade at most. A view a few hundred
    /// pixels large uses fewer, and the device's largest texture caps it.
    pub resolution: u32,
}

impl Default for Shadows {
    fn default() -> Shadows {
        Shadows {
            strength: 1.0,
            distance: 200.0,
            resolution: 1024,
        }
    }
}

impl Shadows {
    /// No shadows.
    pub const NONE: Shadows = Shadows {
        strength: 0.0,
        distance: 0.0,
        resolution: 0,
    };

    /// The cascade edge, in texels, of a view `used` pixels large on a
    /// device whose textures are at most `max_size` texels wide: the
    /// smallest power of two at least the view's larger side, within
    /// `256..=resolution`.
    pub(crate) fn tile(&self, used: [u32; 2], max_size: u32) -> u32 {
        let want = used[0].max(used[1]).max(1).next_power_of_two();
        want.clamp(256, self.resolution.max(256))
            .min(max_size / SHADOW_CASCADES as u32)
            .max(1)
    }
}

/// One cascade: the world as its tile sees it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Cascade {
    /// Rows taking a world point to the tile's x and y in `[-1, 1]` (y up)
    /// and its depth from the light in `[0, 1]`.
    pub rows: [[f32; 4]; 3],
    /// The world size of one texel.
    pub texel: f32,
    /// The depth a receiver is moved toward the light by before it is
    /// compared, in depth units.
    pub bias: f32,
}

impl Cascade {
    /// World point `p` in the cascade's space.
    #[inline]
    pub(crate) fn project(&self, p: [f32; 3]) -> [f32; 3] {
        self.rows
            .map(|r| r[0] * p[0] + r[1] * p[1] + r[2] * p[2] + r[3])
    }

    /// Whether a box centred at `centre` with half extents `half` can cast
    /// a shadow into the tile: it overlaps the tile across the light and
    /// lies no farther from the light than the cascade's far end.
    #[inline]
    pub(crate) fn reaches(&self, centre: [f32; 3], half: [f32; 3]) -> bool {
        let q = self.project(centre);
        let reach =
            |r: &[f32; 4]| r[0].abs() * half[0] + r[1].abs() * half[1] + r[2].abs() * half[2];
        q[0].abs() - reach(&self.rows[0]) <= 1.0
            && q[1].abs() - reach(&self.rows[1]) <= 1.0
            && q[2] - reach(&self.rows[2]) <= 1.0
    }

    /// The world-to-clip transform that draws the cascade into tile
    /// `index` of an atlas of [`SHADOW_CASCADES`] tiles side by side, with
    /// reversed depth: nearer the light is nearer.
    pub(crate) fn clip(&self, index: usize) -> Mat4 {
        let n = SHADOW_CASCADES as f32;
        let [rx, ry, rz] = self.rows;
        let x = [
            rx[0] / n,
            rx[1] / n,
            rx[2] / n,
            rx[3] / n + (2.0 * index as f32 + 1.0) / n - 1.0,
        ];
        let z = [-rz[0], -rz[1], -rz[2], 1.0 - rz[3]];
        let w = [0.0, 0.0, 0.0, 1.0];
        let rows = [x, ry, z, w];
        Mat4 {
            v: core::array::from_fn(|i| rows[i % 4][i / 4]),
        }
    }
}

/// `d` rounded along a ladder of four steps per doubling: up when `up`,
/// else down.
fn ladder(d: f32, up: bool) -> f32 {
    let steps = d.max(1e-6).log2() * 4.0;
    let k = if up { steps.ceil() } else { steps.floor() };
    (k / 4.0).exp2()
}

/// The camera's frustum slice between view depths `d0` and `d1` as the
/// centre and radius of a sphere around its eight corners.
fn slice_sphere(camera: &Camera, aspect: f32, d0: f32, d1: f32) -> (Vec3, f32) {
    let (right, up, forward) = camera.basis();
    let half = |d: f32| match camera.projection {
        Projection::Perspective { fov_y } => {
            let h = d * (fov_y * 0.5).tan();
            (h * aspect, h)
        }
        Projection::Orthographic { height } => (height * 0.5 * aspect, height * 0.5),
    };
    let mut corners = [Vec3::ZERO; 8];
    for (k, d) in [d0, d1].into_iter().enumerate() {
        let (w, h) = half(d);
        let mid = camera.eye + forward * d;
        for (j, (sx, sy)) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)]
            .into_iter()
            .enumerate()
        {
            corners[k * 4 + j] = mid + right * (w * sx) + up * (h * sy);
        }
    }
    let centre = corners.iter().fold(Vec3::ZERO, |a, &c| a + c) * 0.125;
    let radius = corners
        .iter()
        .map(|&c| (c - centre).length())
        .fold(0.0f32, f32::max);
    (centre, radius.max(1e-4))
}

/// The cascades of a view through `camera`, `aspect` wide per unit of
/// height, lit from unit direction `light` (toward the light), with
/// receivers between view depths `near` and `far` and every caster inside
/// the world box `casters` (its least and greatest corners), each cascade
/// `resolution` texels across.
///
/// The slices split the depth range between a uniform and a logarithmic
/// split, so the nearest cascade is the sharpest without wasting itself on
/// the first stretch in front of the camera.
pub(crate) fn fit(
    camera: &Camera,
    aspect: f32,
    light: [f32; 3],
    (near, far): (f32, f32),
    casters: ([f32; 3], [f32; 3]),
    resolution: u32,
) -> [Cascade; SHADOW_CASCADES] {
    let near = ladder(near.max(camera.near.max(1e-4)), false);
    let far = ladder(far.max(near * 1.01), true);
    let n = SHADOW_CASCADES as f32;
    let split = |i: usize| {
        let t = i as f32 / n;
        let log = near * (far / near).powf(t);
        let uniform = near + (far - near) * t;
        0.75 * log + 0.25 * uniform
    };
    // The light's frame: `fwd` the way its light travels.
    let fwd = vec3(-light[0], -light[1], -light[2]).normalize();
    let hint = if fwd.y.abs() > 0.99 {
        vec3(1.0, 0.0, 0.0)
    } else {
        vec3(0.0, 1.0, 0.0)
    };
    let right = hint.cross(fwd).normalize();
    let up = fwd.cross(right);
    // The casters' nearest reach toward the light.
    let (lo, hi) = casters;
    let mut caster_near = f32::MAX;
    for i in 0..8 {
        let p = vec3(
            if i & 1 == 0 { lo[0] } else { hi[0] },
            if i & 2 == 0 { lo[1] } else { hi[1] },
            if i & 4 == 0 { lo[2] } else { hi[2] },
        );
        caster_near = caster_near.min(fwd.dot(p));
    }
    let res = resolution.max(1) as f32;
    core::array::from_fn(|i| {
        let (centre, radius) = slice_sphere(camera, aspect, split(i), split(i + 1));
        // A texel of margin so the filter never reads past the slice, and
        // the extent rounded up so it steps rather than drifts.
        let extent = ladder(radius * (1.0 + 4.0 / res), true);
        let texel = 2.0 * extent / res;
        let snap = |v: f32| (v / texel).floor() * texel;
        let (cr, cu, cf) = (
            snap(right.dot(centre)),
            snap(up.dot(centre)),
            fwd.dot(centre),
        );
        let margin = texel * 2.0;
        let z0 = (cf - radius).min(caster_near) - margin;
        let z1 = cf + radius + margin;
        let depth = (z1 - z0).max(1e-4);
        Cascade {
            rows: [
                [
                    right.x / extent,
                    right.y / extent,
                    right.z / extent,
                    -cr / extent,
                ],
                [up.x / extent, up.y / extent, up.z / extent, -cu / extent],
                [fwd.x / depth, fwd.y / depth, fwd.z / depth, -z0 / depth],
            ],
            texel,
            bias: texel / depth,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn camera(eye: Vec3) -> Camera {
        Camera {
            eye,
            target: eye + vec3(0.0, -0.3, -1.0),
            ..Camera::default()
        }
    }

    fn light() -> [f32; 3] {
        let l = vec3(0.4, 0.8, 0.3).normalize();
        [l.x, l.y, l.z]
    }

    const WORLD: ([f32; 3], [f32; 3]) = ([-60.0, 0.0, -60.0], [60.0, 20.0, 60.0]);

    #[test]
    fn every_slice_lies_inside_its_cascade() {
        let cam = camera(vec3(3.0, 5.0, 10.0));
        let cascades = fit(&cam, 1.5, light(), (1.0, 50.0), WORLD, 1024);
        let (right, up, forward) = cam.basis();
        let (near, far) = (ladder(1.0, false), ladder(50.0, true));
        let n = SHADOW_CASCADES as f32;
        for (i, c) in cascades.iter().enumerate() {
            let split = |i: usize| {
                let t = i as f32 / n;
                0.75 * near * (far / near).powf(t) + 0.25 * (near + (far - near) * t)
            };
            for d in [split(i), split(i + 1)] {
                let h = d * (25f32.to_radians()).tan();
                for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    let p = cam.eye + forward * d + right * (h * 1.5 * sx) + up * (h * sy);
                    let q = c.project(p.to_array());
                    assert!(q[0].abs() <= 1.0 && q[1].abs() <= 1.0, "cascade {i}: {q:?}");
                    assert!((0.0..=1.0).contains(&q[2]), "cascade {i}: {q:?}");
                }
            }
        }
        // Nearer cascades are sharper.
        assert!(cascades[0].texel < cascades[1].texel && cascades[1].texel < cascades[2].texel);
    }

    #[test]
    fn a_caster_toward_the_light_is_in_depth_range() {
        let cam = camera(vec3(0.0, 2.0, 0.0));
        let cascades = fit(&cam, 1.0, light(), (0.5, 20.0), WORLD, 1024);
        // The world's corner nearest the light.
        let corner = [60.0, 20.0, 60.0];
        for c in &cascades {
            assert!(c.project(corner)[2] >= 0.0);
        }
    }

    #[test]
    fn moving_the_camera_moves_the_texel_grid_by_whole_texels() {
        let probe = [1.2, 0.3, -4.5];
        let at = |offset: Vec3| {
            fit(
                &camera(vec3(0.0, 4.0, 6.0) + offset),
                1.5,
                light(),
                (1.0, 40.0),
                WORLD,
                1024,
            )
        };
        let a = at(Vec3::ZERO);
        for offset in [
            vec3(0.001, 0.0, 0.002),
            vec3(0.4, 0.0, -0.7),
            vec3(-1.3, 0.2, 2.9),
        ] {
            let b = at(offset);
            for (ca, cb) in a.iter().zip(&b) {
                assert_eq!(ca.texel, cb.texel);
                let (pa, pb) = (ca.project(probe), cb.project(probe));
                for k in 0..2 {
                    let texels = (pa[k] - pb[k]) * 0.5 * 1024.0;
                    assert!((texels - texels.round()).abs() < 0.02, "{texels}");
                }
            }
        }
    }

    #[test]
    fn the_clip_transform_puts_a_cascade_in_its_tile() {
        let cam = camera(vec3(0.0, 4.0, 6.0));
        let cascades = fit(&cam, 1.0, light(), (1.0, 30.0), WORLD, 512);
        let c = cascades[1];
        let centre = cam.target;
        let q = c.project(centre.to_array());
        let clip = c.clip(1).transform_point(centre);
        // Tile 1 of three spans clip x from -1/3 to 1/3.
        assert!((clip.x - q[0] / 3.0).abs() < 1e-4, "{clip:?} {q:?}");
        assert!((clip.y - q[1]).abs() < 1e-4);
        assert!((clip.z - (1.0 - q[2])).abs() < 1e-4);
    }

    #[test]
    fn a_small_view_uses_a_small_tile() {
        let s = Shadows::default();
        assert_eq!(s.tile([200, 120], 16384), 256);
        assert_eq!(s.tile([700, 400], 16384), 1024);
        assert_eq!(s.tile([4000, 2000], 16384), 1024);
        assert_eq!(s.tile([4000, 2000], 2048), 682);
    }
}
