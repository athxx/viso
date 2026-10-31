//! A program run on the CPU, and a small rasterizer that draws instanced
//! quads with it the way the GPU backends do.
//!
//! A program compiles once into registers and flat operations ([`code`])
//! that run on four lanes at once ([`exec`]): the fragment entry on a 2×2
//! quad of pixels, so `dpdx`, `dpdy` and `fwidth` take fine differences
//! across the quad and lanes outside the triangle run as helpers that write
//! nothing; the vertex entry on four vertices. A draw allocates nothing per
//! instance or pixel.
//!
//! Semantics follow the backends': `round` ties to even, float `%`
//! truncates, shift amounts wrap at the lane width, indices clamp to the last
//! lane or column, integer arithmetic wraps, integer division by zero gives
//! the dividend and remainder by zero gives zero.
//!
//! The rasterizer draws six vertices per instance as two triangles, maps clip
//! space to pixels with the y axis pointing down, samples pixel centers with a
//! top-left fill rule inside the draw's scissor, interpolates `F32` varyings
//! perspective-correct and integer ones from the first vertex, and blends into
//! a float target that rounds to 8 bits per channel after every write, as a
//! `Unorm8` attachment stores it (an extended-range target keeps the floats).
//!
//! [`CpuShader`] is a program as the headless raster runs it: the
//! [`CpuProgram`] its [`ProgramCode::Cpu`](viso_gpu_abi::ProgramCode::Cpu) carries,
//! binding the draw's uniform and instance bytes through the program's layout.
//! Its entries can run as Rust generated from it when the app built
//! ([`native`]) instead of interpreted, to the same bits.

mod code;
mod exec;
pub mod native;
#[doc(hidden)]
pub mod rt;

pub use native::NativeKernel;

use viso_gpu_abi::{
    AddressMode, BlendMode, CpuDraw, CpuProgram, CpuTarget, CpuTexture, FilterMode, SamplerDesc,
};

use self::code::{Code, Word};
use self::exec::{Cell, Machine, ZERO};
use super::{Program, Value};

/// What a draw binds besides its instances.
#[derive(Debug, Clone, Copy)]
pub struct Bindings<'a> {
    /// One value per uniform member.
    pub uniforms: &'a [Value],
    pub textures: &'a [CpuTexture<'a>],
    pub samplers: &'a [SamplerDesc],
}

/// A program run on the CPU by the headless raster.
#[derive(Debug, Clone)]
pub struct CpuShader {
    code: Code,
    /// Its entries compiled to Rust, when the app was built with them.
    native: Option<&'static NativeKernel>,
    /// The uniform block's words.
    uniforms: Vec<Word>,
    /// An instance record's words, and the bytes one takes.
    instance: Vec<Word>,
    stride: usize,
}

impl CpuShader {
    /// `program`, a validated program, compiled.
    pub fn new(program: Program) -> CpuShader {
        let interface = program.interface();
        let code = code::compile(&program);
        CpuShader {
            uniforms: code::words(
                &program,
                &program.uniforms,
                &code.uniforms,
                &interface.uniforms,
            ),
            instance: code::words(
                &program,
                &program.instance,
                &code.instance,
                &interface.instance,
            ),
            stride: interface.instance.size as usize,
            code,
            native: None,
        }
    }

    /// `program` run by `kernel`, the Rust generated from it
    /// ([`native::emit`]); interpreted when `kernel` was generated from
    /// another program.
    pub fn with_native(program: Program, kernel: &'static NativeKernel) -> CpuShader {
        let hash = native::program_hash(&program);
        let mut shader = CpuShader::new(program);
        if kernel.program == hash && kernel.regs == shader.code.regs {
            shader.native = Some(kernel);
        }
        shader
    }

    /// Whether its entries run as generated Rust.
    pub fn is_native(&self) -> bool {
        self.native.is_some()
    }
}

impl CpuProgram for CpuShader {
    fn draw(&self, target: &mut CpuTarget<'_>, draw: &CpuDraw<'_>, blend: BlendMode) {
        let mut machine = Machine::new(&self.code, draw.textures, draw.samplers, self.native);
        machine.bind_words(&self.uniforms, draw.uniforms);
        let count = draw
            .instances
            .len()
            .checked_div(self.stride)
            .map_or(draw.count as usize, |rows| rows.min(draw.count as usize));
        draw_into(target, &self.code, &mut machine, count, blend, |m, iid| {
            if self.stride > 0 {
                let at = iid * self.stride;
                m.bind_words(&self.instance, &draw.instances[at..at + self.stride]);
            }
        });
    }

    fn is_native(&self) -> bool {
        CpuShader::is_native(self)
    }
}

/// A float color target, premultiplied RGBA, row-major from the top.
#[derive(Debug, Clone, PartialEq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

impl Raster {
    /// A target cleared to `clear`.
    pub fn new(width: u32, height: u32, clear: [f32; 4]) -> Raster {
        Raster {
            width,
            height,
            pixels: vec![unorm8(clear); (width * height) as usize],
        }
    }

    /// The pixels as `Bgra8Unorm` bytes.
    pub fn to_bgra8(&self) -> Vec<u8> {
        let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8;
        self.pixels
            .iter()
            .flat_map(|p| [byte(p[2]), byte(p[1]), byte(p[0]), byte(p[3])])
            .collect()
    }

    /// Draws one quad per entry of `instances` (each the instance members'
    /// values) with `program`, a validated program.
    pub fn draw(
        &mut self,
        program: &Program,
        bindings: &Bindings<'_>,
        instances: &[Vec<Value>],
        blend: BlendMode,
    ) {
        let code = code::compile(program);
        let mut target = CpuTarget {
            pixels: &mut self.pixels,
            width: self.width,
            height: self.height,
            origin: [0, 0],
            stride: self.width,
            scissor: [0, 0, self.width, self.height],
            unorm8: true,
        };
        let mut machine = Machine::new(&code, bindings.textures, bindings.samplers, None);
        machine.bind_values(&code.uniforms, bindings.uniforms);
        draw_into(
            &mut target,
            &code,
            &mut machine,
            instances.len(),
            blend,
            |m, iid| m.bind_values(&code.instance, &instances[iid]),
        );
    }
}

/// Draws `count` quads with `code` into `target`, `instance` binding each.
fn draw_into<'a>(
    target: &mut CpuTarget<'_>,
    code: &'a Code,
    machine: &mut Machine<'a>,
    count: usize,
    blend: BlendMode,
    mut instance: impl FnMut(&mut Machine<'a>, usize),
) {
    let (Some(vertex), Some(fragment)) = (&code.vertex, &code.fragment) else {
        return;
    };
    let varyings = code.varyings.len();
    // Each corner's varyings, four components each.
    let mut corners = vec![0u32; 6 * varyings * 4];
    let mut points = [Point::default(); 6];
    let (width, height) = (target.width as f32, target.height as f32);
    let mut canvas = Canvas(target);
    for iid in 0..count {
        instance(machine, iid);
        for (first, lanes) in [(0, 4), (4, 2)] {
            let vids = std::array::from_fn(|l| (first + l) as u32);
            let clips = machine.vertex(vertex, vids, lanes, iid as u32);
            for (l, clip) in clips.iter().enumerate().take(lanes) {
                let vid = first + l;
                let w = clip[3];
                points[vid] = Point {
                    x: (clip[0] / w * 0.5 + 0.5) * width,
                    y: (0.5 - clip[1] / w * 0.5) * height,
                    z: clip[2] / w,
                    inv_w: 1.0 / w,
                };
                for v in 0..varyings {
                    let cell = machine.varying(v);
                    let at = (vid * varyings + v) * 4;
                    for c in 0..4 {
                        corners[at + c] = cell[c][l];
                    }
                }
            }
        }
        for t in 0..2 {
            let triangle = Triangle {
                points: [points[3 * t], points[3 * t + 1], points[3 * t + 2]],
                varyings: &corners[3 * t * varyings * 4..(3 * t + 3) * varyings * 4],
            };
            canvas.triangle(code, machine, fragment, &triangle, blend);
        }
    }
}

/// One triangle: its corners and their varyings, four components each.
struct Triangle<'c> {
    points: [Point; 3],
    varyings: &'c [u32],
}

impl Triangle<'_> {
    /// Component `c` of varying `v` at corner `k`.
    fn varying(&self, varyings: usize, k: usize, v: usize, c: usize) -> u32 {
        self.varyings[(k * varyings + v) * 4 + c]
    }
}

/// The target a draw rasterizes into.
struct Canvas<'t, 'a>(&'t mut CpuTarget<'a>);

impl Canvas<'_, '_> {
    fn triangle<'a>(
        &mut self,
        code: &'a Code,
        machine: &mut Machine<'a>,
        fragment: &'a code::CFunction,
        t: &Triangle<'_>,
        blend: BlendMode,
    ) {
        let [a, b, c] = t.points;
        let area = edge(a, b, c.x, c.y);
        if area == 0.0 || !area.is_finite() {
            return;
        }
        // Quads start on even pixels, so a lane outside the scissor runs as
        // a helper.
        let [sx0, sy0, sx1, sy1] = self.0.scissor.map(i64::from);
        let min_x = (a.x.min(b.x).min(c.x).floor() as i64).max(sx0) & !1;
        let min_y = (a.y.min(b.y).min(c.y).floor() as i64).max(sy0) & !1;
        let max_x = (a.x.max(b.x).max(c.x).ceil() as i64).min(sx1);
        let max_y = (a.y.max(b.y).max(c.y).ceil() as i64).min(sy1);
        // Each edge with the vertex opposite it; a pixel center on an edge is
        // inside only on a top or left edge.
        let edges = [(b, c), (c, a), (a, b)];
        let owns = |(p, q): (Point, Point)| {
            let (dx, dy) = if area > 0.0 {
                (q.x - p.x, q.y - p.y)
            } else {
                (p.x - q.x, p.y - q.y)
            };
            (dy == 0.0 && dx > 0.0) || dy < 0.0
        };
        let mut y = min_y;
        while y < max_y {
            let mut x = min_x;
            while x < max_x {
                let mut lanes = [Pixel::default(); 4];
                let mut any = false;
                for (k, lane) in lanes.iter_mut().enumerate() {
                    let (px, py) = (x + (k as i64 & 1), y + (k as i64 >> 1));
                    let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
                    let w = [
                        edge(b, c, cx, cy) / area,
                        edge(c, a, cx, cy) / area,
                        edge(a, b, cx, cy) / area,
                    ];
                    let inside = w
                        .iter()
                        .zip(edges)
                        .all(|(w, e)| *w > 0.0 || (*w == 0.0 && owns(e)));
                    let on_target = (sx0..sx1).contains(&px) && (sy0..sy1).contains(&py);
                    *lane = Pixel {
                        x: px,
                        y: py,
                        weights: w,
                        covered: inside && on_target,
                    };
                    any |= lane.covered;
                }
                if any {
                    self.quad(code, machine, fragment, t, &lanes, blend);
                }
                x += 2;
            }
            y += 2;
        }
    }

    fn quad<'a>(
        &mut self,
        code: &'a Code,
        machine: &mut Machine<'a>,
        fragment: &'a code::CFunction,
        t: &Triangle<'_>,
        lanes: &[Pixel; 4],
        blend: BlendMode,
    ) {
        let p = &t.points;
        let inv_w: [f32; 4] =
            std::array::from_fn(|l| (0..3).map(|k| lanes[l].weights[k] * p[k].inv_w).sum());
        let varyings = code.varyings.len();
        for (v, &(_, interpolates)) in code.varyings.iter().enumerate() {
            let n = usize::from(code.varying_lanes[v]);
            let mut cell: Cell = ZERO;
            for (c, component) in cell.iter_mut().enumerate().take(n) {
                if !interpolates {
                    *component = [t.varying(varyings, 0, v, c); 4];
                    continue;
                }
                // Perspective-correct: interpolate `a / w` and `1 / w`.
                *component = std::array::from_fn(|l| {
                    let sum: f32 = (0..3)
                        .map(|k| {
                            lanes[l].weights[k]
                                * p[k].inv_w
                                * f32::from_bits(t.varying(varyings, k, v, c))
                        })
                        .sum();
                    (sum / inv_w[l]).to_bits()
                });
            }
            machine.set_varying(v, cell);
        }
        let frag_coord: [[f32; 4]; 4] = std::array::from_fn(|l| {
            let px = &lanes[l];
            let z: f32 = (0..3).map(|k| px.weights[k] * p[k].z).sum();
            [px.x as f32 + 0.5, px.y as f32 + 0.5, z, inv_w[l]]
        });
        let colors = machine.fragment(fragment, frag_coord);
        for (px, color) in lanes.iter().zip(colors) {
            let (true, Some(src)) = (px.covered, color) else {
                continue;
            };
            let at = self.0.index(px.x as u32, px.y as u32);
            let dst = self.0.pixels[at];
            let out = match blend {
                BlendMode::Replace => src,
                BlendMode::PremultipliedOver => {
                    std::array::from_fn(|i| src[i] + dst[i] * (1.0 - src[3]))
                }
                BlendMode::DestinationOut => dst.map(|d| d * (1.0 - src[3])),
            };
            self.0.pixels[at] = if self.0.unorm8 { unorm8(out) } else { out };
        }
    }
}

/// `v` as a `Unorm8` attachment stores it.
fn unorm8(v: [f32; 4]) -> [f32; 4] {
    v.map(|c| (c.clamp(0.0, 1.0) * 255.0).round_ties_even() / 255.0)
}

#[derive(Debug, Clone, Copy, Default)]
struct Point {
    x: f32,
    y: f32,
    z: f32,
    inv_w: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct Pixel {
    x: i64,
    y: i64,
    weights: [f32; 3],
    covered: bool,
}

/// Twice the signed area of `a b (x, y)`.
fn edge(a: Point, b: Point, x: f32, y: f32) -> f32 {
    (b.x - a.x) * (y - a.y) - (b.y - a.y) * (x - a.x)
}

/// `texture` at `(u, v)` through `sampler`, at the base level.
fn sample(texture: &CpuTexture<'_>, sampler: &SamplerDesc, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (texture.width as i64, texture.height as i64);
    if w == 0 || h == 0 {
        return [0.0; 4];
    }
    let wrap = |i: i64, size: i64| -> i64 {
        match sampler.address {
            AddressMode::ClampToEdge => i.clamp(0, size - 1),
            AddressMode::Repeat => i.rem_euclid(size),
            AddressMode::Mirror => {
                let period = i.rem_euclid(2 * size);
                if period < size {
                    period
                } else {
                    2 * size - 1 - period
                }
            }
        }
    };
    let texel = |x: i64, y: i64| texture.texels.get((wrap(y, h) * w + wrap(x, w)) as usize);
    let (x, y) = (u * w as f32, v * h as f32);
    if sampler.filter == FilterMode::Nearest {
        return texel(x.floor() as i64, y.floor() as i64);
    }
    let (x, y) = (x - 0.5, y - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = (x - x0, y - y0);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (a, b, c, d) = (
        texel(x0, y0),
        texel(x0 + 1, y0),
        texel(x0, y0 + 1),
        texel(x0 + 1, y0 + 1),
    );
    std::array::from_fn(|i| {
        let top = a[i] + (b[i] - a[i]) * fx;
        let bottom = c[i] + (d[i] - c[i]) * fx;
        top + (bottom - top) * fy
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::program::{
        BinaryOp, Binding, Block, Builtin, Expr, ExprKind, Function, Intrinsic, Local, Span, Stage,
        Stmt, Ty,
    };
    use viso_gpu_abi::CpuDraw;

    #[test]
    fn a_quad_covers_each_pixel_once() {
        let binding = |name: &str, ty| Binding {
            name: name.into(),
            ty,
            span: Span::default(),
        };
        let expr = Expr::new;
        let unit = expr(
            Ty::VEC2,
            ExprKind::Intrinsic(
                Intrinsic::QuadVertex,
                vec![expr(Ty::U32, ExprKind::Local(0))],
            ),
        );
        // Pixel position `(1.5, 1.5) + unit * (5, 3)`: every edge runs through
        // pixel centers.
        let position = expr(
            Ty::VEC2,
            ExprKind::Binary(
                BinaryOp::Add,
                Box::new(expr(
                    Ty::VEC2,
                    ExprKind::Construct(vec![expr(Ty::F32, ExprKind::F32(1.5))]),
                )),
                Box::new(expr(
                    Ty::VEC2,
                    ExprKind::Binary(
                        BinaryOp::Mul,
                        Box::new(unit),
                        Box::new(expr(Ty::VEC2, ExprKind::Uniform(0))),
                    ),
                )),
            ),
        );
        let clip = expr(
            Ty::VEC4,
            ExprKind::Intrinsic(
                Intrinsic::ToClip,
                vec![position, expr(Ty::VEC2, ExprKind::Uniform(1))],
            ),
        );
        let half = expr(
            Ty::VEC4,
            ExprKind::Construct(vec![expr(Ty::F32, ExprKind::F32(0.5))]),
        );
        let program = Program {
            name: "Cover".into(),
            uniforms: vec![binding("size", Ty::VEC2), binding("viewport", Ty::VEC2)],
            vertex: Some(Function {
                name: "vertex".into(),
                stage: Some(Stage::Vertex),
                params: 1,
                builtins: vec![Builtin::VertexId],
                ret: Ty::VertexOutput,
                locals: vec![Local {
                    name: "vertex_id".into(),
                    ty: Ty::U32,
                    mutable: false,
                }],
                body: Block(vec![Stmt::Return(Some(expr(
                    Ty::VertexOutput,
                    ExprKind::Construct(vec![clip]),
                )))]),
                span: Span::default(),
            }),
            fragment: Some(Function {
                name: "fragment".into(),
                stage: Some(Stage::Fragment),
                params: 0,
                builtins: Vec::new(),
                ret: Ty::VEC4,
                locals: Vec::new(),
                body: Block(vec![Stmt::Return(Some(half))]),
                span: Span::default(),
            }),
            ..Program::default()
        };
        program.validate().expect("valid");
        let uniforms = [Value::floats(&[5.0, 3.0]), Value::floats(&[8.0, 8.0])];
        let bindings = Bindings {
            uniforms: &uniforms,
            textures: &[],
            samplers: &[],
        };
        let mut raster = Raster::new(8, 8, [0.0; 4]);
        raster.draw(
            &program,
            &bindings,
            &[Vec::new()],
            BlendMode::PremultipliedOver,
        );
        // The left and top edges own the centers on them, the right and bottom
        // ones do not: x 1..=5 by y 1..=3, and the shared diagonal blends each
        // pixel once (twice would give 0.75).
        let half = (0.5f32 * 255.0).round_ties_even() / 255.0;
        let covered: Vec<(u32, u32)> = (0..64)
            .filter(|i| raster.pixels[*i as usize] != [0.0; 4])
            .map(|i| (i % 8, i / 8))
            .collect();
        let expected: Vec<(u32, u32)> =
            (1..=3).flat_map(|y| (1..=5).map(move |x| (x, y))).collect();
        assert_eq!(covered, expected);
        assert!(
            covered
                .iter()
                .all(|(x, y)| raster.pixels[(y * 8 + x) as usize] == [half; 4])
        );

        // As the headless raster runs it: from the encoded uniform bytes,
        // inside a scissor, into a target that keeps the floats.
        let interface = program.interface();
        let bytes = interface
            .uniforms
            .encode(&program.uniforms, &uniforms)
            .expect("encodes");
        let mut pixels = vec![[0.0; 4]; 64];
        let mut target = CpuTarget {
            pixels: &mut pixels,
            width: 8,
            height: 8,
            origin: [0, 0],
            stride: 8,
            scissor: [3, 2, 8, 8],
            unorm8: false,
        };
        CpuShader::new(program).draw(
            &mut target,
            &CpuDraw {
                uniforms: &bytes,
                instances: &[],
                count: 1,
                textures: &[],
                samplers: &[],
            },
            BlendMode::PremultipliedOver,
        );
        let clipped: Vec<(u32, u32)> = (0..64)
            .filter(|i| pixels[*i as usize] != [0.0; 4])
            .map(|i| (i % 8, i / 8))
            .collect();
        let inside: Vec<(u32, u32)> = expected
            .into_iter()
            .filter(|&(x, y)| x >= 3 && y >= 2)
            .collect();
        assert_eq!(clipped, inside, "the scissor clips the quad");
        assert!(
            clipped
                .iter()
                .all(|(x, y)| pixels[(y * 8 + x) as usize] == [0.5; 4])
        );
    }
}
