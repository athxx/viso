//! The CPU raster's 3D scene pass: the scene programs run on the CPU
//! against a depth buffer.
//!
//! Each instance's triangles are placed in the world, projected, clipped to
//! the near and far planes, culled by winding and rasterized at pixel centres
//! (top-left fill rule). Depth is interpolated linearly in screen space, as a
//! GPU does; the varyings perspective-correctly, and their derivatives from
//! the neighbouring pixel centres, as a GPU's quad does. Each fragment that
//! passes the depth test is shaded by the program's expression in
//! [`crate::scene`], the one the GPU programs compute.

use super::pixels::{Fmt, Px};
use super::sample::{View, sample};
use super::{Pipeline, Resources, Texture};
use crate::backend::{DrawCommand, Geometry, IndexFormat};
use crate::resource::{
    Binding, BlendMode, BuiltinShader, CullMode, DepthState, FilterMode, SamplerDesc,
};
use crate::scene::{
    FrameUniforms, MaterialUniforms, MeshFragment, SAMPLER_SLOTS, SceneInstance, SceneProgram,
    SceneTextures, SceneVertex, TEXTURE_SAMPLERS, TEXTURE_SLOTS, UNIFORM_SLOTS, normalize,
    shade_mesh,
};

/// The varyings a vertex carries to its fragments.
#[derive(Debug, Clone, Copy, Default)]
struct Varyings {
    world: [f32; 3],
    normal: [f32; 3],
    tangent: [f32; 4],
    uv: [f32; 2],
}

impl Varyings {
    const LANES: usize = 12;

    fn lanes(&self) -> [f32; Self::LANES] {
        let (w, n, t, uv) = (self.world, self.normal, self.tangent, self.uv);
        [
            w[0], w[1], w[2], n[0], n[1], n[2], t[0], t[1], t[2], t[3], uv[0], uv[1],
        ]
    }

    fn of(l: [f32; Self::LANES]) -> Varyings {
        Varyings {
            world: [l[0], l[1], l[2]],
            normal: [l[3], l[4], l[5]],
            tangent: [l[6], l[7], l[8], l[9]],
            uv: [l[10], l[11]],
        }
    }
}

/// A vertex in clip space with what the fragment interpolates.
#[derive(Debug, Clone, Copy)]
struct Clip {
    pos: [f32; 4],
    vary: Varyings,
}

impl Clip {
    fn lerp(a: &Clip, b: &Clip, t: f32) -> Clip {
        let (la, lb) = (a.vary.lanes(), b.vary.lanes());
        Clip {
            pos: core::array::from_fn(|i| a.pos[i] + (b.pos[i] - a.pos[i]) * t),
            vary: Varyings::of(core::array::from_fn(|i| la[i] + (lb[i] - la[i]) * t)),
        }
    }
}

/// The pixels a scene pass draws into.
pub(super) struct SceneTarget<'a, P> {
    pub px: &'a mut [P],
    pub depth: &'a mut [f32],
    pub width: u32,
    pub height: u32,
    pub fmt: Fmt,
}

/// What a scene draw's bind group binds, by slot.
struct Bound<'a> {
    uniforms: [Option<&'a [u8]>; UNIFORM_SLOTS],
    textures: [Option<&'a Texture>; TEXTURE_SLOTS],
    samplers: [SamplerDesc; SAMPLER_SLOTS],
}

impl<'a> Bound<'a> {
    fn of(res: &'a Resources, cmd: &DrawCommand) -> Option<Bound<'a>> {
        let group = res.bind_groups.get(cmd.bind_group?.into())?;
        let mut bound = Bound {
            uniforms: [None; UNIFORM_SLOTS],
            textures: [None; TEXTURE_SLOTS],
            samplers: [SamplerDesc::LINEAR_CLAMP; SAMPLER_SLOTS],
        };
        let (mut u, mut t, mut s) = (0, 0, 0);
        for binding in group {
            match *binding {
                Binding::Uniform(id) => {
                    if let Some(slot) = bound.uniforms.get_mut(u) {
                        *slot = Some(&res.buffer(id)[..]);
                    }
                    u += 1;
                }
                Binding::Texture(id) => {
                    if let Some(slot) = bound.textures.get_mut(t) {
                        *slot = res.textures.get(id.into());
                    }
                    t += 1;
                }
                Binding::Sampler(id) => {
                    if let (Some(slot), Some(desc)) =
                        (bound.samplers.get_mut(s), res.samplers.get(id.into()))
                    {
                        *slot = *desc;
                    }
                    s += 1;
                }
            }
        }
        Some(bound)
    }

    fn frame(&self) -> Option<FrameUniforms> {
        let bytes = self.uniforms[0]?;
        (bytes.len() >= FrameUniforms::SIZE).then(|| FrameUniforms::read_from(bytes))
    }

    fn material(&self) -> Option<MaterialUniforms> {
        let bytes = self.uniforms[1]?;
        (bytes.len() >= MaterialUniforms::SIZE).then(|| MaterialUniforms::read_from(bytes))
    }
}

impl SceneTextures for Bound<'_> {
    fn sample(&self, slot: usize, uv: [f32; 2], dx: [f32; 2], dy: [f32; 2]) -> [f32; 4] {
        let Some(texture) = self.textures.get(slot).copied().flatten() else {
            return [0.0; 4];
        };
        let sampler = &self.samplers[TEXTURE_SAMPLERS[slot] as usize];
        let base = View::whole(texture.view());
        if sampler.filter != FilterMode::MipmapLinear || texture.levels() == 1 {
            return sample(&base, sampler, uv[0], uv[1]);
        }
        // The level whose texels the pixel's footprint spans one of.
        let (w, h) = (texture.width as f32, texture.height as f32);
        let len = |d: [f32; 2]| ((d[0] * w).powi(2) + (d[1] * h).powi(2)).sqrt();
        let rho = len(dx).max(len(dy));
        let last = (texture.levels() - 1) as f32;
        let lod = if rho > 1.0 { rho.log2().min(last) } else { 0.0 };
        let k0 = lod.floor();
        let at = |k: f32| {
            let view = View::whole(texture.level(k as usize));
            sample(&view, sampler, uv[0], uv[1])
        };
        let a = at(k0);
        let t = lod - k0;
        if t <= 0.0 {
            return a;
        }
        let b = at((k0 + 1.0).min(last));
        core::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
    }
}

/// Draws `commands`, the scene draws of a pass, into `target`.
pub(super) fn run<P: Px>(
    res: &Resources,
    target: &mut SceneTarget<'_, P>,
    commands: &[DrawCommand],
) {
    for cmd in commands {
        let Geometry::InstancedMesh {
            vertex_buffer,
            index_buffer,
            index_format,
            index_offset,
            index_count,
            count,
        } = cmd.geometry
        else {
            continue;
        };
        let pipe = res
            .pipelines
            .get(cmd.pipeline.into())
            .expect("pipeline handle does not resolve");
        if pipe.builtin != BuiltinShader::Scene {
            continue;
        }
        let Some(program) = SceneProgram::of_variant(pipe.variant) else {
            continue;
        };
        let Some(bound) = Bound::of(res, cmd) else {
            continue;
        };
        let (Some(frame), Some(material)) = (bound.frame(), bound.material()) else {
            continue;
        };
        let draw = Draw {
            pipe,
            frame: &frame,
            material: &material,
            bound: &bound,
            program,
            scissor: scissor(cmd, target.width, target.height),
        };
        let vertices = &res.buffer(vertex_buffer)[..];
        let indices = &res.buffer(index_buffer)[..];
        let instances = &res.buffer(cmd.instance_buffer)[..];
        for n in 0..count as usize {
            let at = cmd.instance_offset + n * SceneInstance::SIZE;
            let Some(bytes) = instances.get(at..at + SceneInstance::SIZE) else {
                break;
            };
            let instance = SceneInstance::read_from(bytes);
            for t in 0..index_count / 3 {
                let corner = |k: u32| {
                    let i = read_index(indices, index_format, index_offset + t * 3 + k) as usize;
                    let at = i * SceneVertex::SIZE;
                    let v = SceneVertex::read_from(&vertices[at..at + SceneVertex::SIZE]);
                    let world = instance.place(v.position);
                    Clip {
                        pos: frame.clip(world),
                        vary: Varyings {
                            world,
                            normal: instance.turn_normal(v.normal),
                            tangent: instance.turn_tangent(v.tangent),
                            uv: v.uv,
                        },
                    }
                };
                let tri = [corner(0), corner(1), corner(2)];
                draw_triangle(target, &draw, instance.color, &tri);
            }
        }
    }
}

/// One scene draw's state.
struct Draw<'a> {
    pipe: &'a Pipeline,
    frame: &'a FrameUniforms,
    material: &'a MaterialUniforms,
    bound: &'a Bound<'a>,
    program: SceneProgram,
    scissor: [u32; 4],
}

fn scissor(cmd: &DrawCommand, w: u32, h: u32) -> [u32; 4] {
    match cmd.scissor {
        Some((x, y, sw, sh)) => [x.min(w), y.min(h), (x + sw).min(w), (y + sh).min(h)],
        None => [0, 0, w, h],
    }
}

fn read_index(indices: &[u8], format: IndexFormat, at: u32) -> u32 {
    let at = at as usize;
    match format {
        IndexFormat::U16 => u16::from_le_bytes([indices[at * 2], indices[at * 2 + 1]]).into(),
        IndexFormat::U32 => u32::from_le_bytes([
            indices[at * 4],
            indices[at * 4 + 1],
            indices[at * 4 + 2],
            indices[at * 4 + 3],
        ]),
    }
}

/// Clips `tri` to `0 <= z <= w` (the far and the near plane of a reversed-Z
/// projection) and rasterizes what remains.
fn draw_triangle<P: Px>(
    target: &mut SceneTarget<'_, P>,
    draw: &Draw<'_>,
    color: [f32; 4],
    tri: &[Clip; 3],
) {
    let mut poly: Vec<Clip> = tri.to_vec();
    for plane in [
        |c: &Clip| c.pos[3] - c.pos[2], // near: z <= w
        |c: &Clip| c.pos[2],            // far: z >= 0
    ] {
        poly = clip_polygon(&poly, plane);
        if poly.len() < 3 {
            return;
        }
    }
    for k in 1..poly.len() - 1 {
        raster(target, draw, color, [&poly[0], &poly[k], &poly[k + 1]]);
    }
}

/// Sutherland–Hodgman against the half-space `distance >= 0`.
fn clip_polygon(poly: &[Clip], distance: fn(&Clip) -> f32) -> Vec<Clip> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for i in 0..poly.len() {
        let (a, b) = (&poly[i], &poly[(i + 1) % poly.len()]);
        let (da, db) = (distance(a), distance(b));
        if da >= 0.0 {
            out.push(*a);
        }
        if (da >= 0.0) != (db >= 0.0) {
            out.push(Clip::lerp(a, b, da / (da - db)));
        }
    }
    out
}

/// A clip-space triangle, all of it inside the depth range, rasterized.
fn raster<P: Px>(target: &mut SceneTarget<'_, P>, draw: &Draw<'_>, color: [f32; 4], v: [&Clip; 3]) {
    let (w, h) = (target.width as f32, target.height as f32);
    // Screen position (y down), depth and 1/w of each corner.
    let screen = v.map(|c| {
        let inv = 1.0 / c.pos[3];
        [
            (c.pos[0] * inv * 0.5 + 0.5) * w,
            (0.5 - c.pos[1] * inv * 0.5) * h,
            c.pos[2] * inv,
            inv,
        ]
    });
    let edge = |a: [f32; 4], b: [f32; 4], p: [f32; 2]| {
        (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
    };
    let area = edge(screen[0], screen[1], [screen[2][0], screen[2][1]]);
    if area == 0.0 {
        return;
    }
    // Counter-clockwise in clip space (y up) is clockwise on screen (y
    // down), where its area is negative: a back face's is positive.
    if draw.pipe.cull == CullMode::Back && area > 0.0 {
        return;
    }
    let scissor = draw.scissor;
    let min_x = screen.iter().map(|s| s[0]).fold(f32::MAX, f32::min);
    let max_x = screen.iter().map(|s| s[0]).fold(f32::MIN, f32::max);
    let min_y = screen.iter().map(|s| s[1]).fold(f32::MAX, f32::min);
    let max_y = screen.iter().map(|s| s[1]).fold(f32::MIN, f32::max);
    let x0 = (min_x.floor().max(scissor[0] as f32)) as u32;
    let x1 = (max_x.ceil().min(scissor[2] as f32)) as u32;
    let y0 = (min_y.floor().max(scissor[1] as f32)) as u32;
    let y1 = (max_y.ceil().min(scissor[3] as f32)) as u32;
    // The top-left rule: a pixel centre on an edge belongs to the triangle
    // whose interior is right of or below the edge.
    let sign = area.signum();
    let owns = |a: [f32; 4], b: [f32; 4]| {
        let (dx, dy) = ((b[0] - a[0]) * sign, (b[1] - a[1]) * sign);
        dy < 0.0 || (dy == 0.0 && dx > 0.0)
    };
    let owned = [
        owns(screen[1], screen[2]),
        owns(screen[2], screen[0]),
        owns(screen[0], screen[1]),
    ];
    let lanes = v.map(|c| c.vary.lanes());
    // The varyings at screen point `p`, perspective-correct; outside the
    // triangle they extend it, as a GPU's helper pixels do.
    let at = |p: [f32; 2]| -> Varyings {
        let b = [
            edge(screen[1], screen[2], p) / area,
            edge(screen[2], screen[0], p) / area,
            edge(screen[0], screen[1], p) / area,
        ];
        let pw = [
            b[0] * screen[0][3],
            b[1] * screen[1][3],
            b[2] * screen[2][3],
        ];
        let sum = pw[0] + pw[1] + pw[2];
        Varyings::of(core::array::from_fn(|i| {
            (pw[0] * lanes[0][i] + pw[1] * lanes[1][i] + pw[2] * lanes[2][i]) / sum
        }))
    };
    let depth_state = draw.pipe.depth;
    for y in y0..y1 {
        for x in x0..x1 {
            let p = [x as f32 + 0.5, y as f32 + 0.5];
            let e = [
                edge(screen[1], screen[2], p) * sign,
                edge(screen[2], screen[0], p) * sign,
                edge(screen[0], screen[1], p) * sign,
            ];
            if e.iter()
                .zip(owned)
                .any(|(&e, own)| e < 0.0 || (e == 0.0 && !own))
            {
                continue;
            }
            let b = e.map(|e| e / (area * sign));
            let z = b[0] * screen[0][2] + b[1] * screen[1][2] + b[2] * screen[2][2];
            let index = y as usize * target.width as usize + x as usize;
            if let Some(DepthState { compare, .. }) = depth_state
                && !compare.passes(z, target.depth[index])
            {
                continue;
            }
            let here = at(p);
            let Some(shaded) = shade(
                draw,
                color,
                &here,
                &at([p[0] + 1.0, p[1]]),
                &at([p[0], p[1] + 1.0]),
            ) else {
                continue;
            };
            if let Some(DepthState { write: true, .. }) = depth_state {
                target.depth[index] = z;
            }
            let px = &mut target.px[index..index + 1];
            match draw.pipe.blend {
                BlendMode::Replace => P::replace(px, &[shaded], target.fmt),
                BlendMode::PremultipliedOver => P::blend(px, &[shaded], target.fmt, None),
            }
        }
    }
}

/// The program's color of the fragment with varyings `v`, its right and
/// lower neighbours' `right` and `down`; `None` where it discards.
fn shade(
    draw: &Draw<'_>,
    color: [f32; 4],
    v: &Varyings,
    right: &Varyings,
    down: &Varyings,
) -> Option<[f32; 4]> {
    let d2 = |a: [f32; 2], b: [f32; 2]| [b[0] - a[0], b[1] - a[1]];
    let d3 = |a: [f32; 3], b: [f32; 3]| [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    match draw.program {
        SceneProgram::Mesh => {
            let n = normalize(v.normal);
            let fragment = MeshFragment {
                world: v.world,
                normal: v.normal,
                tangent: v.tangent,
                uv: v.uv,
                color,
                uv_dx: d2(v.uv, right.uv),
                uv_dy: d2(v.uv, down.uv),
                normal_dx: d3(n, normalize(right.normal)),
                normal_dy: d3(n, normalize(down.normal)),
            };
            shade_mesh(draw.frame, draw.material, &fragment, draw.bound)
        }
    }
}
