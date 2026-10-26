//! The daylight sky: the Preetham analytic model for one sun, drawn behind
//! a scene's meshes and lighting it as its environment.
//!
//! Everything that depends only on the sun and the haze — the five Perez
//! coefficients of the luminance and of each chromaticity, the zenith's
//! color and the normalization toward it — is computed here once per sky;
//! a fragment evaluates only the Perez functions along its view ray
//! ([`viso_gpu::scene::sky_radiance`]). The model holds for a sun above the
//! horizon; below it, its sun is held a little above, and the day sky fades
//! into a dark night dome through a twilight glow.

use core::f32::consts::{FRAC_PI_2, PI};

use viso_gpu::scene::{SkyUniforms, sky_radiance};
use viso_math::{Vec3, vec3};

use super::{Environment, Lighting};

/// The lowest sun the model is evaluated for, in radians above the horizon:
/// its normalization diverges below.
const MODEL_ELEVATION: f32 = 0.035;
/// The sun's angular radius, in radians.
const SUN_RADIUS: f32 = 0.004_65;
/// The sun disc's radiance over the direct light's: far brighter than
/// anything tone mapping keeps apart.
const SUN_DISC: f32 = 4000.0;
/// The model's luminance, in kcd/m², to the scene's linear light: a clear
/// noon sky lights a scene about as its default gradient does.
const DAYLIGHT_SCALE: f32 = 0.025;
/// How much the sky below the horizon is dimmed at the nadir.
const BELOW_DIM: f32 = 0.65;

/// A daylight sky: where the sun is and how hazy the air.
///
/// Set it as a frame's [`sky`](super::SceneFrame::sky) to draw it behind
/// the meshes; a frame that names no [`Environment`] of its own is then
/// lit by [`Environment::from_sky`] of it. Light the frame by the sun too
/// with [`Lighting::sunlit`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sky {
    /// The direction toward the sun, `y` up; its length does not matter.
    pub sun: Vec3,
    /// The haze in the air, from `1.7` (the clearest) to `10.0` (hazy).
    pub turbidity: f32,
    /// What the sky's light is scaled by.
    pub strength: f32,
    /// Whether the sun's disc shows.
    pub sun_disc: bool,
}

impl Default for Sky {
    fn default() -> Sky {
        Sky {
            sun: Lighting::default().direction,
            turbidity: 2.5,
            strength: 1.0,
            sun_disc: true,
        }
    }
}

/// One Perez channel's five coefficients at turbidity `t`: each linear in
/// `t`, from the published tables.
fn perez(t: f32, rows: [[f32; 2]; 5]) -> [f32; 5] {
    rows.map(|[a, b]| a * t + b)
}

const PEREZ_Y: [[f32; 2]; 5] = [
    [0.1787, -1.4630],
    [-0.3554, 0.4275],
    [-0.0227, 5.3251],
    [0.1206, -2.5771],
    [-0.0670, 0.3703],
];
const PEREZ_X: [[f32; 2]; 5] = [
    [-0.0193, -0.2592],
    [-0.0665, 0.0008],
    [-0.0004, 0.2125],
    [-0.0641, -0.8989],
    [-0.0033, 0.0452],
];
const PEREZ_YC: [[f32; 2]; 5] = [
    [-0.0167, -0.2608],
    [-0.0950, 0.0092],
    [-0.0079, 0.2102],
    [-0.0441, -1.6537],
    [-0.0109, 0.0529],
];

/// A zenith chromaticity: cubic in the sun's zenith angle `ths`, quadratic
/// in turbidity `t`, from the published matrices.
fn zenith_chroma(t: f32, ths: f32, m: [[f32; 4]; 3]) -> f32 {
    let row = |r: [f32; 4]| ((r[0] * ths + r[1]) * ths + r[2]) * ths + r[3];
    t * t * row(m[0]) + t * row(m[1]) + row(m[2])
}

const ZENITH_X: [[f32; 4]; 3] = [
    [0.00166, -0.00375, 0.00209, 0.0],
    [-0.02903, 0.06377, -0.03202, 0.00394],
    [0.11693, -0.21196, 0.06052, 0.25886],
];
const ZENITH_YC: [[f32; 4]; 3] = [
    [0.00275, -0.00610, 0.00317, 0.0],
    [-0.04214, 0.08970, -0.04153, 0.00516],
    [0.15346, -0.26756, 0.06670, 0.26688],
];

/// A Perez function `c` at view zenith cosine `cos_theta` and sun angle
/// `gamma`.
fn perez_f(c: &[f32; 5], cos_theta: f32, gamma: f32) -> f32 {
    (1.0 + c[0] * (c[1] / cos_theta.max(0.01)).exp())
        * (1.0 + c[2] * (c[3] * gamma).exp() + c[4] * gamma.cos() * gamma.cos())
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The air's optical depth toward the sun at elevation `el` (radians),
/// relative to straight up: the Kasten–Young air mass.
fn air_mass(el: f32) -> f32 {
    let deg = el.to_degrees().max(0.0);
    1.0 / (el.max(0.0).sin() + 0.50572 * (deg + 6.07995).powf(-1.6364))
}

impl Sky {
    /// The unit direction toward the sun.
    fn sun_dir(&self) -> Vec3 {
        if self.sun.length() > 1e-6 {
            self.sun.normalize()
        } else {
            vec3(0.0, 1.0, 0.0)
        }
    }

    /// The color sunlight keeps through the air at the sun's elevation, over
    /// what it keeps with the sun overhead, and faded out as it sets: what a
    /// scene's light is tinted by under this sky. Reddens toward the
    /// horizon, black once the sun is below it.
    pub fn sunlight(&self) -> [f32; 3] {
        let el = self.sun_dir().y.clamp(-1.0, 1.0).asin();
        let t = self.turbidity.clamp(1.7, 10.0);
        // Rayleigh scattering, strongest in blue, and the haze's, grey.
        let rayleigh = [0.045, 0.1, 0.24];
        let haze = 0.025 * (t - 1.0);
        let extra = air_mass(el) - 1.0;
        let up = smoothstep(-SUN_RADIUS * 2.0, 0.06, el);
        rayleigh.map(|r| (-(r + haze) * extra).exp() * up)
    }

    /// Its radiance toward `direction` (`y` up), linear RGB, without the
    /// sun's disc: what its [`Environment`] holds.
    pub fn radiance(&self, direction: Vec3) -> [f32; 3] {
        let d = if direction.length() > 1e-6 {
            direction.normalize()
        } else {
            vec3(0.0, 1.0, 0.0)
        };
        sky_radiance(&self.uniforms(&[0.0; 16]), d.to_array(), false)
    }

    /// The uniform block a [`SceneProgram::Sky`] draw of it binds, for a
    /// view whose clip-to-world transform is `inv_view_proj` (column-major):
    /// what the lane writes for a frame's sky, for drawing it on a backend
    /// directly.
    ///
    /// [`SceneProgram::Sky`]: viso_gpu::scene::SceneProgram::Sky
    pub fn uniforms(&self, inv_view_proj: &[f32; 16]) -> SkyUniforms {
        let sun = self.sun_dir();
        let t = self.turbidity.clamp(1.7, 10.0);
        let el = sun.y.clamp(-1.0, 1.0).asin();
        let el_model = el.max(MODEL_ELEVATION);
        let ths = FRAC_PI_2 - el_model;
        let (py, px, pc) = (perez(t, PEREZ_Y), perez(t, PEREZ_X), perez(t, PEREZ_YC));
        // The zenith's luminance (kcd/m²) and chromaticities.
        let chi = (4.0 / 9.0 - t / 120.0) * (PI - 2.0 * ths);
        let zen_y = ((4.0453 * t - 4.971) * chi.tan() - 0.2155 * t + 2.4192).max(0.001);
        let zen_x = zenith_chroma(t, ths, ZENITH_X);
        let zen_yc = zenith_chroma(t, ths, ZENITH_YC);
        let toward_zenith = |c: &[f32; 5]| perez_f(c, 1.0, ths);
        // The model's sun: the true one's bearing at no less than the
        // model's lowest elevation.
        let flat = vec3(sun.x, 0.0, sun.z);
        let bearing = if flat.length() > 1e-5 {
            flat.normalize()
        } else {
            vec3(1.0, 0.0, 0.0)
        };
        let model = bearing * el_model.cos() + vec3(0.0, el_model.sin(), 0.0);
        let deg = el.to_degrees();
        let day = smoothstep(-12.0, 0.0, deg);
        let twilight = smoothstep(-12.0, -3.0, deg) * (1.0 - smoothstep(2.0, 8.0, deg));
        let visible = self.sun_disc && el > -SUN_RADIUS;
        let light = self.sunlight();
        let quad = |c: [f32; 5]| [c[0], c[1], c[2], c[3]];
        SkyUniforms {
            inv_view_proj: *inv_view_proj,
            perez_y: quad(py),
            perez_x: quad(px),
            perez_yc: quad(pc),
            perez_e: [py[4], px[4], pc[4], DAYLIGHT_SCALE * self.strength.max(0.0)],
            zenith: [
                zen_y / toward_zenith(&py),
                zen_x / toward_zenith(&px),
                zen_yc / toward_zenith(&pc),
                BELOW_DIM,
            ],
            sun_model: [model.x, model.y, model.z, 0.0],
            sun: [sun.x, sun.y, sun.z, if visible { SUN_RADIUS } else { 0.0 }],
            sun_radiance: [
                light[0] * SUN_DISC,
                light[1] * SUN_DISC,
                light[2] * SUN_DISC,
                0.0,
            ],
            blend: [day, twilight, 0.0, 0.0],
        }
    }

    /// A key that tells skies apart.
    pub(crate) fn key(&self) -> [u32; 6] {
        [
            self.sun.x.to_bits(),
            self.sun.y.to_bits(),
            self.sun.z.to_bits(),
            self.turbidity.to_bits(),
            self.strength.to_bits(),
            u32::from(self.sun_disc),
        ]
    }
}

impl Environment {
    /// The environment of `sky`, its sun's disc left out — the scene's
    /// light carries the sun: its radiance over an equirectangular map
    /// 128 × 64 texels large, prefiltered on the calling thread (a few
    /// milliseconds).
    pub fn from_sky(sky: &Sky) -> Environment {
        let (w, h) = (128u32, 64u32);
        let uniforms = sky.uniforms(&[0.0; 16]);
        let mut radiance = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            let theta = (y as f32 + 0.5) / h as f32 * PI;
            for x in 0..w {
                let phi = ((x as f32 + 0.5) / w as f32 - 0.5) * 2.0 * PI;
                let s = theta.sin();
                let d = [phi.sin() * s, theta.cos(), -phi.cos() * s];
                radiance.push(sky_radiance(&uniforms, d, false));
            }
        }
        Environment::from_equirect(w, h, &radiance)
    }
}

impl Lighting {
    /// The default lighting, lit by `sky`'s sun: from where the sun is, in
    /// the color its light keeps through the air.
    pub fn sunlit(sky: &Sky) -> Lighting {
        Lighting {
            direction: sky.sun_dir(),
            color: sky.sunlight(),
            ..Lighting::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: [f32; 3]) -> f32 {
        0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    }

    fn sky_at(elevation_deg: f32) -> Sky {
        let e = elevation_deg.to_radians();
        Sky {
            sun: vec3(e.cos(), e.sin(), 0.0),
            ..Sky::default()
        }
    }

    #[test]
    fn a_noon_sky_is_blue_overhead_and_brighter_at_the_horizon() {
        let sky = sky_at(60.0);
        let up = sky.radiance(vec3(0.0, 1.0, 0.0));
        assert!(up[2] > up[0], "the zenith leans blue: {up:?}");
        let horizon = sky.radiance(vec3(0.0, 0.05, -1.0));
        assert!(luminance(horizon) > luminance(up), "{horizon:?} {up:?}");
        // About as bright as the default gradient's sky.
        assert!((0.05..1.0).contains(&luminance(up)), "{up:?}");
    }

    #[test]
    fn the_sky_glows_around_the_sun() {
        let sky = sky_at(25.0);
        let s = sky.sun_dir();
        let near = sky.radiance(s + vec3(0.0, 0.15, 0.0));
        let away = sky.radiance(vec3(-s.x, s.y, -s.z));
        assert!(luminance(near) > luminance(away));
    }

    #[test]
    fn sunset_reddens_the_sun_and_its_horizon() {
        let sky = sky_at(3.0);
        let light = sky.sunlight();
        assert!(light[0] > light[2] * 1.5, "{light:?}");
        let noon = sky_at(80.0).sunlight();
        assert!(noon.iter().all(|&c| c > 0.85), "{noon:?}");
        let horizon = sky.radiance(vec3(1.0, 0.05, 0.0));
        assert!(horizon[0] > horizon[2], "{horizon:?}");
    }

    #[test]
    fn night_is_dark_and_unlit() {
        let sky = sky_at(-20.0);
        assert_eq!(sky.sunlight(), [0.0; 3]);
        let up = sky.radiance(vec3(0.0, 1.0, 0.0));
        assert!(luminance(up) < 0.01, "{up:?}");
        assert!(luminance(sky_at(45.0).radiance(vec3(0.0, 1.0, 0.0))) > luminance(up) * 20.0);
    }

    #[test]
    fn every_elevation_gives_finite_light() {
        for deg in -90..=90 {
            let sky = sky_at(deg as f32);
            for d in [
                vec3(0.3, 0.4, -0.5),
                vec3(0.0, -1.0, 0.0),
                vec3(1.0, 0.0, 0.0),
            ] {
                let c = sky.radiance(d);
                assert!(c.iter().all(|v| v.is_finite() && *v >= 0.0), "{deg}: {c:?}");
            }
        }
    }

    #[test]
    fn its_environment_lights_an_upward_face_by_the_sky() {
        let sky = sky_at(50.0);
        let env = Environment::from_sky(&sky);
        let up = env.irradiance([0.0, 1.0, 0.0]);
        let down = env.irradiance([0.0, -1.0, 0.0]);
        assert!(up[2] > down[2] && up[2] > 0.05, "{up:?} {down:?}");
    }
}
