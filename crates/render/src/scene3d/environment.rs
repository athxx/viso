//! The light a scene's surroundings shed on it: its radiance prefiltered
//! for every roughness, and its diffuse irradiance as spherical harmonics.

use core::f32::consts::PI;

use viso_gpu::f32_to_f16;
use viso_gpu::scene::{ENV_HEIGHT, ENV_LEVELS, ENV_WIDTH, env_direction_uv};

/// How many directions a texel of a rough level averages.
const SAMPLES: u32 = 48;

/// An environment a scene is lit by: the radiance arriving from every
/// direction, prefiltered for every roughness a surface reflects it at, and
/// the diffuse light it sheds.
///
/// Build one from an equirectangular radiance map with
/// [`from_equirect`](Self::from_equirect), or from the colors of the sky and
/// the ground with [`gradient`](Self::gradient), and set it as a frame's
/// [`environment`](super::SceneFrame::environment). Building one runs on
/// the calling thread and costs a few milliseconds; the renderer uploads it
/// once for every frame that keeps it.
#[derive(Clone, PartialEq)]
pub struct Environment {
    /// The atlas, `ENV_WIDTH × ENV_HEIGHT` RGBA half floats.
    pub(crate) texels: Vec<u16>,
    /// The diffuse irradiance over π, as the frame's uniforms hold it.
    pub(crate) sh: [[f32; 4]; 9],
}

impl core::fmt::Debug for Environment {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Environment").field("sh", &self.sh).finish()
    }
}

/// An equirectangular radiance map: `u` around `y` from `-z`, `v` from
/// `+y` down to `-y`, linear RGB.
struct Equirect {
    width: usize,
    height: usize,
    texels: Vec<[f32; 3]>,
}

impl Equirect {
    /// The radiance toward `uv`, bilinear, wrapping around and clamping at
    /// the poles.
    fn sample(&self, uv: [f32; 2]) -> [f32; 3] {
        let (w, h) = (self.width as f32, self.height as f32);
        let x = uv[0] * w - 0.5;
        let y = (uv[1] * h - 0.5).clamp(0.0, h - 1.0);
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |xi: i32, yi: i32| {
            let xi = xi.rem_euclid(self.width as i32) as usize;
            let yi = (yi.max(0) as usize).min(self.height - 1);
            self.texels[yi * self.width + xi]
        };
        let (x0, y0) = (x0 as i32, y0 as i32);
        let (a, b, c, d) = (
            at(x0, y0),
            at(x0 + 1, y0),
            at(x0, y0 + 1),
            at(x0 + 1, y0 + 1),
        );
        core::array::from_fn(|i| {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            top + (bottom - top) * fy
        })
    }

    /// Half its size each way, each texel the average of four.
    fn half(&self) -> Equirect {
        let (w, h) = ((self.width / 2).max(1), (self.height / 2).max(1));
        let mut texels = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0.0f32; 3];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (2 * x + dx).min(self.width - 1);
                    let sy = (2 * y + dy).min(self.height - 1);
                    let t = self.texels[sy * self.width + sx];
                    for i in 0..3 {
                        sum[i] += t[i] * 0.25;
                    }
                }
                texels.push(sum);
            }
        }
        Equirect {
            width: w,
            height: h,
            texels,
        }
    }
}

/// The unit direction toward `uv` of an equirectangular map.
fn direction(uv: [f32; 2]) -> [f32; 3] {
    let phi = (uv[0] - 0.5) * 2.0 * PI;
    let theta = uv[1] * PI;
    let s = theta.sin();
    [phi.sin() * s, theta.cos(), -phi.cos() * s]
}

fn radical_inverse(mut bits: u32) -> f32 {
    bits = bits.reverse_bits();
    bits as f32 * 2.328_306_4e-10
}

impl Environment {
    /// The environment of an equirectangular radiance map `width × height`
    /// texels large: `radiance` row by row from the top, linear RGB. Its
    /// centre column looks along `-z`, its top row up `+y`.
    ///
    /// # Panics
    ///
    /// If `radiance` holds fewer than `width × height` texels.
    pub fn from_equirect(width: u32, height: u32, radiance: &[[f32; 3]]) -> Environment {
        let (w, h) = (width.max(1) as usize, height.max(1) as usize);
        assert!(
            radiance.len() >= w * h,
            "an equirect map {w}×{h} texels large"
        );
        prefilter(Equirect {
            width: w,
            height: h,
            texels: radiance[..w * h].to_vec(),
        })
    }

    /// A sky that brightens from `horizon` to `sky` overhead, over ground
    /// that darkens from `horizon` to `ground` beneath: the environment of a
    /// frame that names none, from its lighting's sky and ground colors.
    pub fn gradient(sky: [f32; 3], horizon: [f32; 3], ground: [f32; 3]) -> Environment {
        let (w, h) = (64usize, 32usize);
        let mut texels = Vec::with_capacity(w * h);
        for y in 0..h {
            let v = (y as f32 + 0.5) / h as f32;
            let up = (v * PI).cos();
            let (to, t) = if up >= 0.0 {
                (sky, up.sqrt())
            } else {
                (ground, (-up).sqrt())
            };
            let c: [f32; 3] = core::array::from_fn(|i| horizon[i] + (to[i] - horizon[i]) * t);
            texels.extend(core::iter::repeat_n(c, w));
        }
        prefilter(Equirect {
            width: w,
            height: h,
            texels,
        })
    }

    /// The prefiltered atlas the programs sample:
    /// [`ENV_WIDTH`](viso_gpu::scene::ENV_WIDTH) ×
    /// [`ENV_HEIGHT`](viso_gpu::scene::ENV_HEIGHT) RGBA half floats, row by
    /// row from the top.
    pub fn atlas(&self) -> &[u16] {
        &self.texels
    }

    /// Its diffuse irradiance over π as the nine spherical harmonic terms a
    /// frame's uniforms hold.
    pub fn harmonics(&self) -> &[[f32; 4]; 9] {
        &self.sh
    }

    /// Its diffuse irradiance over π at unit normal `n`.
    pub fn irradiance(&self, n: [f32; 3]) -> [f32; 3] {
        viso_gpu::scene::sh_irradiance(&self.sh, n)
    }
}

/// `source` prefiltered into the atlas, with its irradiance.
fn prefilter(source: Equirect) -> Environment {
    // The source and its chain of halves, for sampling a direction at the
    // detail a sample's solid angle covers.
    let mut chain = vec![source];
    while chain.last().is_some_and(|m| m.width > 4) {
        let next = chain.last().expect("non-empty").half();
        chain.push(next);
    }
    let base_texel = 4.0 * PI / (chain[0].width * chain[0].height) as f32;
    let fetch = |dir: [f32; 3], lod: f32| -> [f32; 3] {
        let uv = env_direction_uv(dir);
        let lod = lod.clamp(0.0, (chain.len() - 1) as f32);
        let k = lod.floor() as usize;
        let a = chain[k].sample(uv);
        if k + 1 >= chain.len() {
            return a;
        }
        let b = chain[k + 1].sample(uv);
        let t = lod - k as f32;
        core::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
    };
    let mut texels = vec![0u16; (ENV_WIDTH * ENV_HEIGHT * 4) as usize];
    for level in 0..ENV_LEVELS {
        let (lw, lh) = (
            (ENV_WIDTH >> level) as usize,
            (ENV_WIDTH >> (level + 1)) as usize,
        );
        let row0 = (ENV_WIDTH - (ENV_WIDTH >> level)) as usize;
        let roughness = level as f32 / (ENV_LEVELS - 1) as f32;
        let alpha = roughness * roughness;
        let a2 = alpha * alpha;
        // The level's sample directions about +z, each with its weight's
        // source detail: GGX-distributed half vectors reflected about it.
        let samples: Vec<([f32; 3], f32)> = if level == 0 {
            Vec::new()
        } else {
            (0..SAMPLES)
                .filter_map(|i| {
                    let (u, v) = (i as f32 / SAMPLES as f32, radical_inverse(i));
                    let phi = 2.0 * PI * u;
                    let cos_t = ((1.0 - v) / (1.0 + (a2 - 1.0) * v)).sqrt();
                    let sin_t = (1.0 - cos_t * cos_t).max(0.0).sqrt();
                    let hv = [sin_t * phi.cos(), sin_t * phi.sin(), cos_t];
                    // With the view along the normal, L = 2 (N·H) H - N.
                    let l = [
                        2.0 * cos_t * hv[0],
                        2.0 * cos_t * hv[1],
                        2.0 * cos_t * cos_t - 1.0,
                    ];
                    if l[2] <= 0.0 {
                        return None;
                    }
                    let d = cos_t * cos_t * (a2 - 1.0) + 1.0;
                    let pdf = a2 / (PI * d * d) / 4.0;
                    let solid = 1.0 / (SAMPLES as f32 * pdf);
                    let lod = 0.5 * (solid / base_texel).log2() + 1.0;
                    Some((l, lod))
                })
                .collect()
        };
        for y in 0..lh {
            for x in 0..lw {
                let uv = [(x as f32 + 0.5) / lw as f32, (y as f32 + 0.5) / lh as f32];
                let n = direction(uv);
                let c = if level == 0 {
                    fetch(n, 0.0)
                } else {
                    // A frame about the normal.
                    let up = if n[1].abs() < 0.999 {
                        [0.0, 1.0, 0.0]
                    } else {
                        [1.0, 0.0, 0.0]
                    };
                    let t = normalize(cross(up, n));
                    let b = cross(n, t);
                    let mut sum = [0.0f32; 3];
                    let mut weight = 0.0f32;
                    for &(l, lod) in &samples {
                        let d: [f32; 3] =
                            core::array::from_fn(|i| t[i] * l[0] + b[i] * l[1] + n[i] * l[2]);
                        let c = fetch(d, lod);
                        for i in 0..3 {
                            sum[i] += c[i] * l[2];
                        }
                        weight += l[2];
                    }
                    sum.map(|s| s / weight.max(1e-6))
                };
                let at = ((row0 + y) * ENV_WIDTH as usize + x) * 4;
                for i in 0..3 {
                    texels[at + i] = f32_to_f16(c[i].max(0.0));
                }
                texels[at + 3] = f32_to_f16(1.0);
            }
        }
    }
    Environment {
        texels,
        sh: irradiance_sh(
            chain
                .iter()
                .find(|m| m.width <= 128)
                .unwrap_or(&chain[chain.len() - 1]),
        ),
    }
}

/// The irradiance over π of `map` as nine spherical harmonic terms, each
/// scaled by its basis constant and the cosine lobe's band factor so the
/// irradiance is their polynomial at the normal.
fn irradiance_sh(map: &Equirect) -> [[f32; 4]; 9] {
    let mut l = [[0.0f32; 3]; 9];
    let mut total = 0.0f32;
    for y in 0..map.height {
        let v = (y as f32 + 0.5) / map.height as f32;
        let theta = v * PI;
        let solid = (2.0 * PI / map.width as f32) * (PI / map.height as f32) * theta.sin();
        for x in 0..map.width {
            let uv = [(x as f32 + 0.5) / map.width as f32, v];
            let [dx, dy, dz] = direction(uv);
            let c = map.texels[y * map.width + x];
            total += solid;
            let basis = [
                0.282_095,
                0.488_603 * dy,
                0.488_603 * dz,
                0.488_603 * dx,
                1.092_548 * dx * dy,
                1.092_548 * dy * dz,
                0.315_392 * (3.0 * dz * dz - 1.0),
                1.092_548 * dx * dz,
                0.546_274 * (dx * dx - dy * dy),
            ];
            for (term, b) in l.iter_mut().zip(basis) {
                for i in 0..3 {
                    term[i] += c[i] * b * solid;
                }
            }
        }
    }
    // The texels' solid angles sum to the sphere's; the cosine lobe's bands
    // over π, times each basis constant again, make the polynomial the
    // programs evaluate.
    let sphere = 4.0 * PI / total.max(1e-6);
    let band = [
        1.0,
        2.0 / 3.0,
        2.0 / 3.0,
        2.0 / 3.0,
        0.25,
        0.25,
        0.25,
        0.25,
        0.25,
    ];
    let constant = [
        0.282_095, 0.488_603, 0.488_603, 0.488_603, 1.092_548, 1.092_548, 0.315_392, 1.092_548,
        0.546_274,
    ];
    core::array::from_fn(|k| {
        let s = band[k] * constant[k] * sphere;
        [l[k][0] * s, l[k][1] * s, l[k][2] * s, 0.0]
    })
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(a: [f32; 3]) -> [f32; 3] {
    let len = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    if len > 0.0 { a.map(|v| v / len) } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_gpu::f16_to_f32;

    #[test]
    fn a_constant_environment_is_its_constant_everywhere() {
        let env = Environment::from_equirect(16, 8, &[[0.5, 0.25, 1.0]; 128]);
        for n in [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, -0.6, 0.8]] {
            let e = env.irradiance(n);
            for (a, b) in e.iter().zip([0.5, 0.25, 1.0]) {
                assert!((a - b).abs() < 0.02, "{n:?}: {e:?}");
            }
        }
        // Every level of the atlas holds it.
        for level in 0..ENV_LEVELS {
            let row = (ENV_WIDTH - (ENV_WIDTH >> level)) as usize;
            let at = (row * ENV_WIDTH as usize + 3) * 4;
            let r = f16_to_f32(env.texels[at]);
            assert!((r - 0.5).abs() < 0.01, "level {level}: {r}");
        }
    }

    #[test]
    fn a_gradient_lights_an_upward_face_by_its_sky() {
        let env = Environment::gradient([0.6, 0.7, 1.0], [0.4, 0.4, 0.4], [0.1, 0.1, 0.1]);
        let up = env.irradiance([0.0, 1.0, 0.0]);
        let down = env.irradiance([0.0, -1.0, 0.0]);
        assert!(up[2] > 0.7 && up[2] < 1.0, "{up:?}");
        assert!(down[0] < 0.25, "{down:?}");
    }

    #[test]
    fn a_rough_level_blurs_a_bright_spot() {
        // A bright patch straight ahead along -z.
        let (w, h) = (64usize, 32usize);
        let mut radiance = vec![[0.0f32; 3]; w * h];
        for y in 14..18 {
            for x in 30..34 {
                radiance[y * w + x] = [10.0; 3];
            }
        }
        let env = Environment::from_equirect(w as u32, h as u32, &radiance);
        let at = |level: u32, uv: [f32; 2]| {
            let (lw, lh) = (ENV_WIDTH >> level, ENV_WIDTH >> (level + 1));
            let row = ENV_WIDTH - (ENV_WIDTH >> level);
            let x = ((uv[0] * lw as f32) as u32).min(lw - 1);
            let y = row + ((uv[1] * lh as f32) as u32).min(lh - 1);
            f16_to_f32(env.texels[((y * ENV_WIDTH + x) * 4) as usize])
        };
        // Sharp at level 0, spread wider and dimmer at the roughest.
        assert!(at(0, [0.5, 0.5]) > 5.0);
        assert!(at(0, [0.6, 0.5]) < 0.1);
        assert!(at(ENV_LEVELS - 1, [0.5, 0.5]) < at(0, [0.5, 0.5]));
        assert!(at(ENV_LEVELS - 1, [0.6, 0.5]) > 0.01);
    }
}
