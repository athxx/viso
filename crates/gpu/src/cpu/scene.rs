//! The CPU raster's 3D scene pass: the [`BuiltinShader::Scene`] program run
//! on the CPU against a depth buffer.
//!
//! Each instance's triangles are placed in the world, projected, clipped to
//! the near and far planes, culled by winding and rasterized at pixel centres
//! (top-left fill rule). Depth is interpolated linearly in screen space, as a
//! GPU does; the world position and normal perspective-correctly. Each
//! fragment that passes the depth test is shaded by
//! [`SceneUniforms::shade`], the expression the GPU programs compute.

use super::pixels::{Fmt, Px};
use super::{Pipeline, Resources};
use crate::backend::{DrawCommand, Geometry, IndexFormat};
use crate::resource::{Binding, BlendMode, BuiltinShader, CullMode, DepthState};
use crate::scene::{SceneInstance, SceneUniforms, SceneVertex, normalize};

/// A vertex in clip space with what the fragment interpolates.
#[derive(Debug, Clone, Copy)]
struct Clip {
    pos: [f32; 4],
    world: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
}

impl Clip {
    fn lerp(a: &Clip, b: &Clip, t: f32) -> Clip {
        let mix = |x: f32, y: f32| x + (y - x) * t;
        Clip {
            pos: core::array::from_fn(|i| mix(a.pos[i], b.pos[i])),
            world: core::array::from_fn(|i| mix(a.world[i], b.world[i])),
            normal: core::array::from_fn(|i| mix(a.normal[i], b.normal[i])),
            uv: core::array::from_fn(|i| mix(a.uv[i], b.uv[i])),
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
        let Some(uniforms) = uniforms(res, cmd) else {
            continue;
        };
        let vertices = &res.buffer(vertex_buffer)[..];
        let indices = &res.buffer(index_buffer)[..];
        let instances = &res.buffer(cmd.instance_buffer)[..];
        let scissor = scissor(cmd, target.width, target.height);
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
                        pos: uniforms.clip(world),
                        world,
                        normal: instance.turn_normal(v.normal),
                        uv: v.uv,
                    }
                };
                let tri = [corner(0), corner(1), corner(2)];
                draw_triangle(target, pipe, &uniforms, instance.color, &tri, scissor);
            }
        }
    }
}

/// The uniform block the draw's bind group holds.
fn uniforms(res: &Resources, cmd: &DrawCommand) -> Option<SceneUniforms> {
    let group = res.bind_groups.get(cmd.bind_group?.into())?;
    group.iter().find_map(|binding| match binding {
        Binding::Uniform(buffer) => {
            let bytes = res.buffer(*buffer);
            (bytes.len() >= SceneUniforms::SIZE).then(|| SceneUniforms::read_from(bytes))
        }
        _ => None,
    })
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
    pipe: &Pipeline,
    uniforms: &SceneUniforms,
    color: [f32; 4],
    tri: &[Clip; 3],
    scissor: [u32; 4],
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
        raster(
            target,
            pipe,
            uniforms,
            color,
            [&poly[0], &poly[k], &poly[k + 1]],
            scissor,
        );
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
fn raster<P: Px>(
    target: &mut SceneTarget<'_, P>,
    pipe: &Pipeline,
    uniforms: &SceneUniforms,
    color: [f32; 4],
    v: [&Clip; 3],
    scissor: [u32; 4],
) {
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
    if pipe.cull == CullMode::Back && area > 0.0 {
        return;
    }
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
    let depth_state = pipe.depth;
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
            let at = y as usize * target.width as usize + x as usize;
            if let Some(DepthState { compare, write, .. }) = depth_state {
                if !compare.passes(z, target.depth[at]) {
                    continue;
                }
                if write {
                    target.depth[at] = z;
                }
            }
            // Perspective-correct weights.
            let pw = [
                b[0] * screen[0][3],
                b[1] * screen[1][3],
                b[2] * screen[2][3],
            ];
            let sum = pw[0] + pw[1] + pw[2];
            let mix = |f: fn(&Clip) -> [f32; 3]| -> [f32; 3] {
                core::array::from_fn(|i| {
                    (pw[0] * f(v[0])[i] + pw[1] * f(v[1])[i] + pw[2] * f(v[2])[i]) / sum
                })
            };
            let world = mix(|c| c.world);
            let normal = normalize(mix(|c| c.normal));
            let uv: [f32; 2] = core::array::from_fn(|i| {
                (pw[0] * v[0].uv[i] + pw[1] * v[1].uv[i] + pw[2] * v[2].uv[i]) / sum
            });
            let shaded = uniforms.shade(world, normal, uv, color);
            let px = &mut target.px[at..at + 1];
            match pipe.blend {
                BlendMode::Replace => P::replace(px, &[shaded], target.fmt),
                BlendMode::PremultipliedOver => P::blend(px, &[shaded], target.fmt, None),
            }
        }
    }
}
