//! The reference interpreter: a program run on the CPU, and a small
//! rasterizer that draws instanced quads with it the way the GPU backends do.
//!
//! It is a test oracle, not a renderer. Semantics follow the backends': `round`
//! ties to even, float `%` truncates, shift amounts wrap at the lane width,
//! indices clamp to the last lane or column, integer arithmetic wraps, integer
//! division by zero gives the dividend and remainder by zero gives zero. The
//! fragment entry runs a 2×2 quad of pixels at a time, so `dpdx`, `dpdy` and
//! `fwidth` take fine differences across the quad; lanes outside the triangle
//! run as helpers and write nothing.
//!
//! The rasterizer draws six vertices per instance as two triangles, maps clip
//! space to pixels with the y axis pointing down, samples pixel centers with a
//! top-left fill rule, interpolates `F32` varyings perspective-correct and
//! integer ones from the first vertex, and blends into a float target that
//! rounds to 8 bits per channel after every write, as a `Unorm8` attachment
//! stores it.

use viso_gpu::{AddressMode, BlendMode, FilterMode, SamplerDesc};

use super::{
    BinaryOp, Block, Builtin, Expr, ExprKind, Function, Intrinsic, Lane, Loop, Place, Program,
    Root, Scalar, Stage, Stmt, Texel, Ty, UnaryOp, Value,
};

/// A texture as the program samples it: RGBA texels in `0..=1`, row-major
/// from the top.
#[derive(Debug, Clone, PartialEq)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    pub texels: Vec<[f32; 4]>,
}

/// What a draw binds besides its instances.
#[derive(Debug, Clone, Copy)]
pub struct Bindings<'a> {
    /// One value per uniform member.
    pub uniforms: &'a [Value],
    pub textures: &'a [TextureData],
    pub samplers: &'a [SamplerDesc],
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
    /// values) with `program`.
    pub fn draw(
        &mut self,
        program: &Program,
        bindings: &Bindings<'_>,
        instances: &[Vec<Value>],
        blend: BlendMode,
    ) {
        let (Some(vertex), Some(fragment)) = (&program.vertex, &program.fragment) else {
            return;
        };
        for (iid, instance) in instances.iter().enumerate() {
            let run = Run {
                p: program,
                bindings,
                instance,
            };
            let corners: Vec<(Point, Vec<Value>)> = (0..6u32)
                .map(|vid| {
                    let (clip, varyings) = run.vertex(vertex, vid, iid as u32);
                    let w = clip[3];
                    let point = Point {
                        x: (clip[0] / w * 0.5 + 0.5) * self.width as f32,
                        y: (0.5 - clip[1] / w * 0.5) * self.height as f32,
                        z: clip[2] / w,
                        inv_w: 1.0 / w,
                    };
                    (point, varyings)
                })
                .collect();
            for triangle in corners.as_chunks::<3>().0 {
                self.triangle(&run, fragment, triangle, blend);
            }
        }
    }

    fn triangle(
        &mut self,
        run: &Run<'_>,
        fragment: &Function,
        t: &[(Point, Vec<Value>)],
        blend: BlendMode,
    ) {
        let (a, b, c) = (t[0].0, t[1].0, t[2].0);
        let area = edge(a, b, c.x, c.y);
        if area == 0.0 || !area.is_finite() {
            return;
        }
        let min_x = a.x.min(b.x).min(c.x).floor().max(0.0) as i64 & !1;
        let min_y = a.y.min(b.y).min(c.y).floor().max(0.0) as i64 & !1;
        let max_x = (a.x.max(b.x).max(c.x).ceil() as i64).min(i64::from(self.width));
        let max_y = (a.y.max(b.y).max(c.y).ceil() as i64).min(i64::from(self.height));
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
                    let on_target = px < i64::from(self.width) && py < i64::from(self.height);
                    *lane = Pixel {
                        x: px,
                        y: py,
                        weights: w,
                        covered: inside && on_target,
                    };
                    any |= lane.covered;
                }
                if any {
                    self.quad(run, fragment, t, &lanes, blend);
                }
                x += 2;
            }
            y += 2;
        }
    }

    fn quad(
        &mut self,
        run: &Run<'_>,
        fragment: &Function,
        t: &[(Point, Vec<Value>)],
        lanes: &[Pixel; 4],
        blend: BlendMode,
    ) {
        let varyings: Vec<Vec<Value>> = run
            .p
            .varyings
            .iter()
            .enumerate()
            .map(|(v, binding)| {
                lanes
                    .iter()
                    .map(|px| {
                        if !binding.ty.is_float() {
                            return t[0].1[v].clone();
                        }
                        // Perspective-correct: interpolate `a / w` and `1 / w`.
                        let inv_w: f32 = (0..3).map(|i| px.weights[i] * t[i].0.inv_w).sum();
                        let values: Vec<Vec<f32>> =
                            t.iter().map(|(_, vs)| vs[v].to_floats()).collect();
                        let n = values[0].len();
                        let lanes: Vec<f32> = (0..n)
                            .map(|j| {
                                (0..3)
                                    .map(|i| px.weights[i] * t[i].0.inv_w * values[i][j])
                                    .sum::<f32>()
                                    / inv_w
                            })
                            .collect();
                        Value::floats(&lanes)
                    })
                    .collect()
            })
            .collect();
        let frag_coord: Vec<Value> = lanes
            .iter()
            .map(|px| {
                let z: f32 = (0..3).map(|i| px.weights[i] * t[i].0.z).sum();
                let inv_w: f32 = (0..3).map(|i| px.weights[i] * t[i].0.inv_w).sum();
                Value::floats(&[px.x as f32 + 0.5, px.y as f32 + 0.5, z, inv_w])
            })
            .collect();
        let colors = run.fragment(fragment, &varyings, &frag_coord);
        for (px, color) in lanes.iter().zip(colors) {
            let (true, Some(src)) = (px.covered, color) else {
                continue;
            };
            let at = (px.y as u32 * self.width + px.x as u32) as usize;
            let dst = self.pixels[at];
            let out = match blend {
                BlendMode::Replace => src,
                BlendMode::PremultipliedOver => {
                    std::array::from_fn(|i| src[i] + dst[i] * (1.0 - src[3]))
                }
            };
            self.pixels[at] = unorm8(out);
        }
    }
}

/// `v` as a `Unorm8` attachment stores it.
fn unorm8(v: [f32; 4]) -> [f32; 4] {
    v.map(|c| (c.clamp(0.0, 1.0) * 255.0).round_ties_even() / 255.0)
}

#[derive(Debug, Clone, Copy)]
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

/// One instance's draw.
struct Run<'a> {
    p: &'a Program,
    bindings: &'a Bindings<'a>,
    instance: &'a [Value],
}

/// The per-lane state of one invocation group.
struct Frame {
    stage: Option<Stage>,
    /// Per local, per lane.
    locals: Vec<Vec<Value>>,
    /// A vertex invocation's varyings and output.
    varyings_out: Vec<Value>,
    /// A fragment invocation's varyings, per varying, per lane.
    varyings_in: Vec<Vec<Value>>,
    returned: Vec<Option<Value>>,
    discarded: Vec<bool>,
    broke: Vec<bool>,
    continued: Vec<bool>,
}

impl Frame {
    fn new(p: &Program, f: &Function, width: usize) -> Frame {
        Frame {
            stage: f.stage,
            locals: f
                .locals
                .iter()
                .map(|l| vec![Value::zero(p, l.ty); width])
                .collect(),
            varyings_out: p.varyings.iter().map(|v| Value::zero(p, v.ty)).collect(),
            varyings_in: Vec::new(),
            returned: vec![None; width],
            discarded: vec![false; width],
            broke: vec![false; width],
            continued: vec![false; width],
        }
    }

    fn finished(&self, lane: usize) -> bool {
        self.returned[lane].is_some() || self.discarded[lane]
    }
}

impl Run<'_> {
    /// The vertex entry for vertex `vid`: its clip position and varyings.
    fn vertex(&self, f: &Function, vid: u32, iid: u32) -> ([f32; 4], Vec<Value>) {
        let mut frame = Frame::new(self.p, f, 1);
        for (local, builtin) in f.builtins.iter().enumerate() {
            frame.locals[local][0] = match builtin {
                Builtin::VertexId => Value::u32(vid),
                Builtin::InstanceId => Value::u32(iid),
                Builtin::FragCoord => Value::floats(&[0.0; 4]),
            };
        }
        let mut mask = vec![true];
        self.block(&mut frame, &f.body, &mut mask);
        let clip = match frame.returned[0].take() {
            Some(Value::Record(fields)) => fields[0].to_floats(),
            _ => vec![0.0; 4],
        };
        ([clip[0], clip[1], clip[2], clip[3]], frame.varyings_out)
    }

    /// The fragment entry for a quad: each lane's color, `None` when it
    /// discarded.
    fn fragment(
        &self,
        f: &Function,
        varyings: &[Vec<Value>],
        frag_coord: &[Value],
    ) -> Vec<Option<[f32; 4]>> {
        let mut frame = Frame::new(self.p, f, 4);
        frame.varyings_in = varyings.to_vec();
        for (local, builtin) in f.builtins.iter().enumerate() {
            if *builtin == Builtin::FragCoord {
                frame.locals[local] = frag_coord.to_vec();
            }
        }
        let mut mask = vec![true; 4];
        self.block(&mut frame, &f.body, &mut mask);
        (0..4)
            .map(|lane| {
                if frame.discarded[lane] {
                    return None;
                }
                let c = frame.returned[lane].as_ref()?.to_floats();
                Some([c[0], c[1], c[2], c[3]])
            })
            .collect()
    }

    /// Calls helper function `index` on one lane.
    fn call(&self, index: u32, args: Vec<Value>) -> Value {
        let f = &self.p.functions[index as usize];
        let mut frame = Frame::new(self.p, f, 1);
        for (slot, arg) in frame.locals.iter_mut().zip(args) {
            slot[0] = arg;
        }
        let mut mask = vec![true];
        self.block(&mut frame, &f.body, &mut mask);
        frame.returned[0]
            .take()
            .unwrap_or_else(|| Value::zero(self.p, f.ret))
    }

    /// Runs `block` on the lanes `mask` holds; a lane that returns, discards,
    /// breaks or continues leaves `mask`.
    fn block(&self, frame: &mut Frame, block: &Block, mask: &mut [bool]) {
        for stmt in &block.0 {
            if !mask.iter().any(|m| *m) {
                return;
            }
            self.stmt(frame, stmt, mask);
        }
    }

    fn stmt(&self, frame: &mut Frame, stmt: &Stmt, mask: &mut [bool]) {
        let width = mask.len();
        match stmt {
            Stmt::Let(local, value) => {
                for lane in active(mask) {
                    frame.locals[*local as usize][lane] = self.eval(frame, value, lane);
                }
            }
            Stmt::Declare(_) => {}
            Stmt::Assign(place, value) => {
                for lane in active(mask) {
                    let v = self.eval(frame, value, lane);
                    self.assign(frame, place, lane, v);
                }
            }
            Stmt::If(cond, then, otherwise) => {
                let truth: Vec<bool> = (0..width)
                    .map(|lane| {
                        mask[lane] && self.eval(frame, cond, lane).lanes()[0] == Lane::Bool(true)
                    })
                    .collect();
                let mut then_mask = truth.clone();
                let mut else_mask: Vec<bool> =
                    (0..width).map(|lane| mask[lane] && !truth[lane]).collect();
                self.block(frame, then, &mut then_mask);
                self.block(frame, otherwise, &mut else_mask);
                for ((m, t), e) in mask.iter_mut().zip(&then_mask).zip(&else_mask) {
                    *m = *t || *e;
                }
            }
            Stmt::For(l) => self.for_loop(frame, l, mask),
            Stmt::Break | Stmt::Continue => {
                let flags = if matches!(stmt, Stmt::Break) {
                    &mut frame.broke
                } else {
                    &mut frame.continued
                };
                for lane in active(mask) {
                    flags[lane] = true;
                    mask[lane] = false;
                }
            }
            Stmt::Return(value) => {
                for lane in active(mask) {
                    let v = value
                        .as_ref()
                        .map_or_else(|| Value::Record(Vec::new()), |e| self.eval(frame, e, lane));
                    frame.returned[lane] = Some(v);
                    mask[lane] = false;
                }
            }
            Stmt::Discard => {
                for lane in active(mask) {
                    frame.discarded[lane] = true;
                    mask[lane] = false;
                }
            }
        }
    }

    fn for_loop(&self, frame: &mut Frame, l: &Loop, mask: &mut [bool]) {
        let width = mask.len();
        let var = l.var as usize;
        let entered: Vec<bool> = mask.to_vec();
        let (saved_broke, saved_continued) = (frame.broke.clone(), frame.continued.clone());
        frame.broke.fill(false);
        let mut running = entered.clone();
        for lane in active(&running) {
            frame.locals[var][lane] = self.eval(frame, &l.start, lane);
        }
        for _ in 0..l.max {
            if l.guarded {
                for lane in active(&running) {
                    let end = self.eval(frame, &l.end, lane);
                    let order = match (frame.locals[var][lane].lanes()[0], end.lanes()[0]) {
                        (Lane::I32(a), Lane::I32(b)) => a.cmp(&b),
                        (Lane::U32(a), Lane::U32(b)) => a.cmp(&b),
                        _ => std::cmp::Ordering::Greater,
                    };
                    running[lane] = order.is_lt() || (l.inclusive && order.is_eq());
                }
            }
            if !running.iter().any(|r| *r) {
                break;
            }
            frame.continued.fill(false);
            let mut body = running.clone();
            self.block(frame, &l.body, &mut body);
            for lane in 0..width {
                running[lane] = (body[lane] || frame.continued[lane])
                    && !frame.broke[lane]
                    && !frame.finished(lane);
                if running[lane] {
                    frame.locals[var][lane] = step(&frame.locals[var][lane]);
                }
            }
        }
        for lane in 0..width {
            mask[lane] = entered[lane] && !frame.finished(lane);
        }
        frame.broke = saved_broke;
        frame.continued = saved_continued;
    }

    fn assign(&self, frame: &mut Frame, place: &Place, lane: usize, value: Value) {
        let slot = match place.root {
            Root::Local(l) => &mut frame.locals[l as usize][lane],
            Root::Varying(v) => &mut frame.varyings_out[v as usize],
        };
        set(slot, &place.path, value);
    }

    fn eval(&self, frame: &Frame, e: &Expr, lane: usize) -> Value {
        match &e.kind {
            ExprKind::Bool(b) => Value::bool(*b),
            ExprKind::I32(v) => Value::i32(*v),
            ExprKind::U32(v) => Value::u32(*v),
            ExprKind::F32(v) => Value::f32(*v),
            ExprKind::Local(l) => frame.locals[*l as usize][lane].clone(),
            ExprKind::Uniform(i) => self.bindings.uniforms[*i as usize].clone(),
            ExprKind::Instance(i) => self.instance[*i as usize].clone(),
            ExprKind::Varying(v) => match frame.stage {
                Some(Stage::Fragment) => frame.varyings_in[*v as usize][lane].clone(),
                _ => frame.varyings_out[*v as usize].clone(),
            },
            ExprKind::Texture(t) => Value::Texture(*t),
            ExprKind::Sampler(s) => Value::Sampler(*s),
            ExprKind::Unary(op, a) => unary(*op, &self.eval(frame, a, lane)),
            ExprKind::Binary(op, a, b) => {
                binary(*op, &self.eval(frame, a, lane), &self.eval(frame, b, lane))
            }
            ExprKind::Construct(parts) => {
                let parts: Vec<Value> = parts.iter().map(|p| self.eval(frame, p, lane)).collect();
                construct(e.ty, parts)
            }
            ExprKind::Swizzle(base, lanes) => {
                let base = self.eval(frame, base, lane);
                let n = e.ty.lanes().map_or(1, |(_, n)| n);
                let source = base.lanes();
                let mut out = [source[0]; 4];
                for (o, &l) in out.iter_mut().zip(&lanes[..usize::from(n)]) {
                    *o = source[usize::from(l)];
                }
                Value::Lanes(n, zero_tail(out, n))
            }
            ExprKind::Member(base, field) => match self.eval(frame, base, lane) {
                Value::Record(fields) => fields[*field as usize].clone(),
                other => other,
            },
            ExprKind::Index(base, index) => {
                let base = self.eval(frame, base, lane);
                let at = self.eval(frame, index, lane).lanes()[0].bits() as usize;
                match base {
                    Value::Matrix(n, columns) => {
                        let c = at.min(usize::from(n) - 1);
                        Value::floats(&columns[c][..usize::from(n)])
                    }
                    other => {
                        let lanes = other.lanes();
                        Value::scalar(lanes[at.min(lanes.len() - 1)])
                    }
                }
            }
            ExprKind::Call(callee, args) => {
                let args = args.iter().map(|a| self.eval(frame, a, lane)).collect();
                self.call(*callee, args)
            }
            ExprKind::Intrinsic(i, args) => self.intrinsic(frame, *i, args, e.ty, lane),
            ExprKind::Convert(a) => convert(e.ty, &self.eval(frame, a, lane)),
        }
    }

    fn intrinsic(&self, frame: &Frame, i: Intrinsic, args: &[Expr], ty: Ty, lane: usize) -> Value {
        use Intrinsic as I;
        // Fine derivatives across the quad: lanes are (0,0) (1,0) (0,1) (1,1).
        let across = |e: &Expr, pair: [usize; 2]| -> Value {
            let (a, b) = (self.eval(frame, e, pair[0]), self.eval(frame, e, pair[1]));
            binary(BinaryOp::Sub, &b, &a)
        };
        let dx = |e: &Expr| across(e, [lane & !1, lane | 1]);
        let dy = |e: &Expr| across(e, [lane & 1, (lane & 1) | 2]);
        match i {
            I::Dpdx => return dx(&args[0]),
            I::Dpdy => return dy(&args[0]),
            I::Fwidth => {
                let abs = |v: Value| floats_map(&v, f32::abs);
                return binary(BinaryOp::Add, &abs(dx(&args[0])), &abs(dy(&args[0])));
            }
            _ => {}
        }
        let v: Vec<Value> = args.iter().map(|a| self.eval(frame, a, lane)).collect();
        let f1 = |f: fn(f32) -> f32| floats_map(&v[0], f);
        let f2 = |f: fn(f32, f32) -> f32| zip_floats(&splat_to(&v[0], ty), &splat_to(&v[1], ty), f);
        match i {
            I::Abs => lanes_map(&v[0], |l| match l {
                Lane::I32(x) => Lane::I32(x.wrapping_abs()),
                Lane::F32(x) => Lane::F32(x.abs()),
                other => other,
            }),
            I::Sign => f1(|x| {
                if x > 0.0 {
                    1.0
                } else if x < 0.0 {
                    -1.0
                } else {
                    0.0
                }
            }),
            I::Floor => f1(f32::floor),
            I::Ceil => f1(f32::ceil),
            I::Round => f1(f32::round_ties_even),
            I::Trunc => f1(f32::trunc),
            I::Fract => f1(|x| x - x.floor()),
            I::Sqrt => f1(f32::sqrt),
            I::InverseSqrt => f1(|x| 1.0 / x.sqrt()),
            I::Exp => f1(f32::exp),
            I::Exp2 => f1(f32::exp2),
            I::Log => f1(f32::ln),
            I::Log2 => f1(f32::log2),
            I::Sin => f1(f32::sin),
            I::Cos => f1(f32::cos),
            I::Tan => f1(f32::tan),
            I::Asin => f1(f32::asin),
            I::Acos => f1(f32::acos),
            I::Atan => f1(f32::atan),
            I::Sinh => f1(f32::sinh),
            I::Cosh => f1(f32::cosh),
            I::Tanh => f1(f32::tanh),
            I::Radians => f1(|x| x * 0.017_453_292),
            I::Degrees => f1(|x| x * 57.295_78),
            I::Atan2 => f2(f32::atan2),
            I::Pow => f2(f32::powf),
            I::Step => zip_floats(&splat_to(&v[0], ty), &v[1], |edge, x| {
                if x < edge { 0.0 } else { 1.0 }
            }),
            I::Min | I::Max => {
                let min = i == I::Min;
                zip_lanes(&v[0], &v[1], |a, b| match (a, b) {
                    (Lane::I32(a), Lane::I32(b)) => {
                        Lane::I32(if min { a.min(b) } else { a.max(b) })
                    }
                    (Lane::U32(a), Lane::U32(b)) => {
                        Lane::U32(if min { a.min(b) } else { a.max(b) })
                    }
                    (a, b) => Lane::F32(if min {
                        a.f32().min(b.f32())
                    } else {
                        a.f32().max(b.f32())
                    }),
                })
            }
            I::Clamp => {
                let (lo, hi) = (splat_to(&v[1], ty), splat_to(&v[2], ty));
                let up = zip_lanes(&v[0], &lo, max_lane);
                zip_lanes(&up, &hi, min_lane)
            }
            I::Mix => {
                let t = splat_to(&v[2], ty);
                let span = zip_floats(&v[1], &v[0], |b, a| b - a);
                let scaled = zip_floats(&span, &t, |s, t| s * t);
                zip_floats(&v[0], &scaled, |a, s| a + s)
            }
            I::Smoothstep => {
                let (e0, e1) = (splat_to(&v[0], ty), splat_to(&v[1], ty));
                let x = &v[2];
                let lanes: Vec<f32> = (0..x.lanes().len())
                    .map(|k| {
                        let (e0, e1, x) =
                            (e0.lanes()[k].f32(), e1.lanes()[k].f32(), x.lanes()[k].f32());
                        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
                        t * t * (3.0 - 2.0 * t)
                    })
                    .collect();
                Value::floats(&lanes)
            }
            I::Length => Value::f32(dot(&v[0], &v[0]).sqrt()),
            I::Distance => {
                let d = zip_floats(&v[0], &v[1], |a, b| a - b);
                Value::f32(dot(&d, &d).sqrt())
            }
            I::Dot => Value::f32(dot(&v[0], &v[1])),
            I::Normalize => {
                let len = dot(&v[0], &v[0]).sqrt();
                floats_map(&v[0], move |x| x / len)
            }
            I::Cross => {
                let (a, b) = (v[0].to_floats(), v[1].to_floats());
                Value::floats(&[
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ])
            }
            I::Transpose => match &v[0] {
                Value::Matrix(n, m) => {
                    let mut t = [[0.0; 4]; 4];
                    for (c, column) in t.iter_mut().enumerate() {
                        for (r, x) in column.iter_mut().enumerate() {
                            *x = m[r][c];
                        }
                    }
                    Value::Matrix(*n, t)
                }
                other => other.clone(),
            },
            I::Sample | I::SampleLevel => {
                let (Value::Texture(t), Value::Sampler(s)) = (&v[0], &v[1]) else {
                    return Value::zero(self.p, ty);
                };
                let uv = v[2].to_floats();
                let texel = sample(
                    &self.bindings.textures[*t as usize],
                    &self.bindings.samplers[*s as usize],
                    uv[0],
                    uv[1],
                );
                match args[0].ty {
                    Ty::Texture(Texel::F32) => Value::f32(texel[0]),
                    _ => Value::floats(&texel),
                }
            }
            I::ToVec4 => v[0].clone(),
            I::QuadVertex => {
                const CORNERS: [[f32; 2]; 6] = [
                    [0.0, 0.0],
                    [1.0, 0.0],
                    [0.0, 1.0],
                    [1.0, 0.0],
                    [1.0, 1.0],
                    [0.0, 1.0],
                ];
                Value::floats(&CORNERS[(v[0].lanes()[0].bits() % 6) as usize])
            }
            I::ToClip => {
                let (p, viewport) = (v[0].to_floats(), v[1].to_floats());
                Value::floats(&[
                    p[0] / viewport[0] * 2.0 - 1.0,
                    1.0 - p[1] / viewport[1] * 2.0,
                    0.0,
                    1.0,
                ])
            }
            I::RoundedRectSdf => {
                let (p, size, radius) = (v[0].to_floats(), v[1].to_floats(), v[2].to_floats()[0]);
                let extent = [size[0] * 0.5, size[1] * 0.5];
                let r = radius.clamp(0.0, extent[0].min(extent[1]));
                let q = [
                    (p[0] - extent[0]).abs() - (extent[0] - r),
                    (p[1] - extent[1]).abs() - (extent[1] - r),
                ];
                let outside = [q[0].max(0.0), q[1].max(0.0)];
                let length = (outside[0] * outside[0] + outside[1] * outside[1]).sqrt();
                Value::f32(length + q[0].max(q[1]).min(0.0) - r)
            }
            I::Dpdx | I::Dpdy | I::Fwidth => unreachable!("taken above"),
        }
    }
}

/// The lanes `mask` holds.
fn active(mask: &[bool]) -> Vec<usize> {
    mask.iter()
        .enumerate()
        .filter_map(|(lane, on)| on.then_some(lane))
        .collect()
}

/// The loop counter after one more trip.
fn step(at: &Value) -> Value {
    lanes_map(at, |l| match l {
        Lane::I32(x) => Lane::I32(x.wrapping_add(1)),
        Lane::U32(x) => Lane::U32(x.wrapping_add(1)),
        other => other,
    })
}

/// Writes `value` into `slot` at record field / lane / column `path`.
fn set(slot: &mut Value, path: &[u32], value: Value) {
    let Some((&first, rest)) = path.split_first() else {
        *slot = value;
        return;
    };
    match slot {
        Value::Record(fields) => set(&mut fields[first as usize], rest, value),
        Value::Matrix(n, columns) => {
            if rest.is_empty() {
                let lanes = value.to_floats();
                columns[first as usize][..usize::from(*n)]
                    .copy_from_slice(&lanes[..usize::from(*n)]);
            } else {
                let mut column = Value::floats(&columns[first as usize][..usize::from(*n)]);
                set(&mut column, rest, value);
                columns[first as usize][..usize::from(*n)].copy_from_slice(&column.to_floats());
            }
        }
        Value::Lanes(_, lanes) => lanes[first as usize] = value.lanes()[0],
        _ => {}
    }
}

fn zero_tail(mut lanes: [Lane; 4], n: u8) -> [Lane; 4] {
    let zero = Lane::from_bits(lane_scalar(lanes[0]), 0);
    for l in &mut lanes[usize::from(n)..] {
        *l = zero;
    }
    lanes
}

fn lane_scalar(l: Lane) -> Scalar {
    match l {
        Lane::Bool(_) => Scalar::Bool,
        Lane::I32(_) => Scalar::I32,
        Lane::U32(_) => Scalar::U32,
        Lane::F32(_) => Scalar::F32,
    }
}

fn lanes_map(v: &Value, f: impl Fn(Lane) -> Lane) -> Value {
    match v {
        Value::Lanes(n, lanes) => Value::Lanes(*n, zero_tail(lanes.map(&f), *n)),
        Value::Matrix(n, m) => Value::Matrix(
            *n,
            m.map(|c| {
                c.map(|x| match f(Lane::F32(x)) {
                    Lane::F32(y) => y,
                    other => other.f32(),
                })
            }),
        ),
        other => other.clone(),
    }
}

fn floats_map(v: &Value, f: impl Fn(f32) -> f32) -> Value {
    lanes_map(v, |l| Lane::F32(f(l.f32())))
}

/// `v` spread to `ty`'s lanes when it is a scalar beside a vector.
fn splat_to(v: &Value, ty: Ty) -> Value {
    match (v, ty.lanes()) {
        (Value::Lanes(1, lanes), Some((_, n))) if n > 1 => Value::Lanes(n, [lanes[0]; 4]),
        _ => v.clone(),
    }
}

/// `f` lane by lane, a one-lane operand broadcast.
fn zip_lanes(a: &Value, b: &Value, f: impl Fn(Lane, Lane) -> Lane) -> Value {
    let (la, lb) = (a.lanes(), b.lanes());
    let n = la.len().max(lb.len());
    let pick = |s: &[Lane], k: usize| if s.len() == 1 { s[0] } else { s[k] };
    let mut out = [Lane::F32(0.0); 4];
    for (k, o) in out.iter_mut().enumerate().take(n) {
        *o = f(pick(la, k), pick(lb, k));
    }
    Value::Lanes(n as u8, zero_tail(out, n as u8))
}

fn zip_floats(a: &Value, b: &Value, f: impl Fn(f32, f32) -> f32) -> Value {
    zip_lanes(a, b, |x, y| Lane::F32(f(x.f32(), y.f32())))
}

fn min_lane(a: Lane, b: Lane) -> Lane {
    match (a, b) {
        (Lane::I32(a), Lane::I32(b)) => Lane::I32(a.min(b)),
        (Lane::U32(a), Lane::U32(b)) => Lane::U32(a.min(b)),
        (a, b) => Lane::F32(a.f32().min(b.f32())),
    }
}

fn max_lane(a: Lane, b: Lane) -> Lane {
    match (a, b) {
        (Lane::I32(a), Lane::I32(b)) => Lane::I32(a.max(b)),
        (Lane::U32(a), Lane::U32(b)) => Lane::U32(a.max(b)),
        (a, b) => Lane::F32(a.f32().max(b.f32())),
    }
}

fn dot(a: &Value, b: &Value) -> f32 {
    a.to_floats()
        .iter()
        .zip(b.to_floats())
        .map(|(x, y)| x * y)
        .sum()
}

fn unary(op: UnaryOp, a: &Value) -> Value {
    lanes_map(a, |l| match (op, l) {
        (UnaryOp::Neg, Lane::I32(x)) => Lane::I32(x.wrapping_neg()),
        (UnaryOp::Neg, Lane::F32(x)) => Lane::F32(-x),
        (UnaryOp::Not, Lane::Bool(b)) => Lane::Bool(!b),
        (UnaryOp::BitNot, Lane::I32(x)) => Lane::I32(!x),
        (UnaryOp::BitNot, Lane::U32(x)) => Lane::U32(!x),
        (_, other) => other,
    })
}

fn binary(op: BinaryOp, a: &Value, b: &Value) -> Value {
    use BinaryOp as B;
    match (op, a, b) {
        (B::Mul, Value::Matrix(n, m), Value::Matrix(_, k)) => {
            let n = usize::from(*n);
            let mut out = [[0.0; 4]; 4];
            for (c, column) in out.iter_mut().enumerate().take(n) {
                for (r, x) in column.iter_mut().enumerate().take(n) {
                    *x = (0..n).map(|j| m[j][r] * k[c][j]).sum();
                }
            }
            return Value::Matrix(n as u8, out);
        }
        (B::Mul, Value::Matrix(n, m), Value::Lanes(len, _)) if *len > 1 => {
            let v = b.to_floats();
            let n = usize::from(*n);
            let out: Vec<f32> = (0..n)
                .map(|r| (0..n).map(|c| m[c][r] * v[c]).sum())
                .collect();
            return Value::floats(&out);
        }
        (B::Mul, Value::Lanes(len, _), Value::Matrix(n, m)) if *len > 1 => {
            let v = a.to_floats();
            let n = usize::from(*n);
            let out: Vec<f32> = (0..n)
                .map(|c| (0..n).map(|r| v[r] * m[c][r]).sum())
                .collect();
            return Value::floats(&out);
        }
        (_, Value::Matrix(n, m), Value::Matrix(_, k)) => {
            let sign = if op == B::Sub { -1.0 } else { 1.0 };
            let mut out = *m;
            for (c, column) in out.iter_mut().enumerate() {
                for (r, x) in column.iter_mut().enumerate() {
                    *x += sign * k[c][r];
                }
            }
            return Value::Matrix(*n, out);
        }
        (B::Mul, Value::Matrix(_, _), Value::Lanes(..))
        | (B::Mul, Value::Lanes(..), Value::Matrix(_, _)) => {
            let (m, s) = if let Value::Matrix(..) = a {
                (a, b)
            } else {
                (b, a)
            };
            let s = s.lanes()[0].f32();
            return floats_map(m, |x| x * s);
        }
        _ => {}
    }
    zip_lanes(a, b, |x, y| lane_op(op, x, y))
}

fn lane_op(op: BinaryOp, x: Lane, y: Lane) -> Lane {
    use BinaryOp as B;
    use std::cmp::Ordering;
    let order = |o: Ordering| -> Lane {
        Lane::Bool(match op {
            B::Eq => o == Ordering::Equal,
            B::Ne => o != Ordering::Equal,
            B::Lt => o == Ordering::Less,
            B::Le => o != Ordering::Greater,
            B::Gt => o == Ordering::Greater,
            _ => o != Ordering::Less,
        })
    };
    match (x, y) {
        (Lane::F32(a), Lane::F32(b)) => match op {
            B::Add => Lane::F32(a + b),
            B::Sub => Lane::F32(a - b),
            B::Mul => Lane::F32(a * b),
            B::Div => Lane::F32(a / b),
            B::Rem => Lane::F32(a % b),
            // A comparison with NaN is false; `!=` with NaN is true.
            _ => match a.partial_cmp(&b) {
                Some(o) => order(o),
                None => Lane::Bool(op == B::Ne),
            },
        },
        // A shift amount may be `U32` lanes beside `I32` ones.
        (Lane::I32(a), y) => {
            let bits = y.bits();
            let b = bits.cast_signed();
            match op {
                B::Add => Lane::I32(a.wrapping_add(b)),
                B::Sub => Lane::I32(a.wrapping_sub(b)),
                B::Mul => Lane::I32(a.wrapping_mul(b)),
                B::Div if b == 0 => Lane::I32(a),
                B::Div => Lane::I32(a.wrapping_div(b)),
                B::Rem if b == 0 => Lane::I32(0),
                B::Rem => Lane::I32(a.wrapping_rem(b)),
                B::BitAnd => Lane::I32(a & b),
                B::BitOr => Lane::I32(a | b),
                B::BitXor => Lane::I32(a ^ b),
                B::Shl => Lane::I32(a.wrapping_shl(bits & 31)),
                B::Shr => Lane::I32(a.wrapping_shr(bits & 31)),
                _ => order(a.cmp(&b)),
            }
        }
        (Lane::U32(a), y) => {
            let b = y.bits();
            match op {
                B::Add => Lane::U32(a.wrapping_add(b)),
                B::Sub => Lane::U32(a.wrapping_sub(b)),
                B::Mul => Lane::U32(a.wrapping_mul(b)),
                B::Div if b == 0 => Lane::U32(a),
                B::Div => Lane::U32(a / b),
                B::Rem if b == 0 => Lane::U32(0),
                B::Rem => Lane::U32(a % b),
                B::BitAnd => Lane::U32(a & b),
                B::BitOr => Lane::U32(a | b),
                B::BitXor => Lane::U32(a ^ b),
                B::Shl => Lane::U32(a.wrapping_shl(b & 31)),
                B::Shr => Lane::U32(a.wrapping_shr(b & 31)),
                _ => order(a.cmp(&b)),
            }
        }
        (Lane::Bool(a), Lane::Bool(b)) => Lane::Bool(match op {
            B::And => a && b,
            B::Or => a || b,
            B::Eq => a == b,
            _ => a != b,
        }),
        (x, _) => x,
    }
}

fn construct(ty: Ty, parts: Vec<Value>) -> Value {
    match ty {
        Ty::Record(_) | Ty::VertexOutput => Value::Record(parts),
        Ty::Matrix(n) => {
            let n = usize::from(n);
            let flat: Vec<f32> = parts.iter().flat_map(Value::to_floats).collect();
            let mut m = [[0.0; 4]; 4];
            for (c, column) in m.iter_mut().enumerate().take(n) {
                column[..n].copy_from_slice(&flat[c * n..c * n + n]);
            }
            Value::Matrix(n as u8, m)
        }
        _ => {
            let n = match ty {
                Ty::Color => 4,
                other => other.lanes().map_or(1, |(_, n)| n),
            };
            let flat: Vec<Lane> = parts.iter().flat_map(|p| p.lanes().to_vec()).collect();
            let mut out = [flat[0]; 4];
            if flat.len() > 1 {
                out[..usize::from(n)].copy_from_slice(&flat[..usize::from(n)]);
            }
            Value::Lanes(n, zero_tail(out, n))
        }
    }
}

fn convert(ty: Ty, v: &Value) -> Value {
    let Some((to, _)) = ty.lanes() else {
        return v.clone();
    };
    lanes_map(v, |l| match (l, to) {
        (Lane::F32(x), Scalar::I32) => Lane::I32(x as i32),
        (Lane::F32(x), Scalar::U32) => Lane::U32(x as u32),
        (Lane::F32(x), Scalar::Bool) => Lane::Bool(x != 0.0),
        (l, Scalar::F32) => Lane::F32(l.f32()),
        (Lane::I32(x), Scalar::U32) => Lane::U32(x.cast_unsigned()),
        (Lane::U32(x), Scalar::I32) => Lane::I32(x.cast_signed()),
        (l, Scalar::Bool) => Lane::Bool(l.bits() != 0),
        (Lane::Bool(b), Scalar::I32) => Lane::I32(i32::from(b)),
        (Lane::Bool(b), Scalar::U32) => Lane::U32(u32::from(b)),
        (l, _) => l,
    })
}

/// `texture` at `(u, v)` through `sampler`, at the base level.
fn sample(texture: &TextureData, sampler: &SamplerDesc, u: f32, v: f32) -> [f32; 4] {
    let (w, h) = (texture.width as i64, texture.height as i64);
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
    let texel = |x: i64, y: i64| texture.texels[(wrap(y, h) * w + wrap(x, w)) as usize];
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
    use crate::program::{Binding, Span};

    #[test]
    fn matrices_are_by_column() {
        // Columns (1, 2) and (3, 4): the matrix [[1, 3], [2, 4]].
        let m = Value::Matrix(
            2,
            [
                [1.0, 2.0, 0.0, 0.0],
                [3.0, 4.0, 0.0, 0.0],
                [0.0; 4],
                [0.0; 4],
            ],
        );
        let v = Value::floats(&[1.0, 10.0]);
        assert_eq!(binary(BinaryOp::Mul, &m, &v), Value::floats(&[31.0, 42.0]));
        assert_eq!(binary(BinaryOp::Mul, &v, &m), Value::floats(&[21.0, 43.0]));
        let squared = binary(BinaryOp::Mul, &m, &m);
        assert_eq!(
            squared,
            Value::Matrix(
                2,
                [
                    [7.0, 10.0, 0.0, 0.0],
                    [15.0, 22.0, 0.0, 0.0],
                    [0.0; 4],
                    [0.0; 4]
                ]
            )
        );
        assert_eq!(
            binary(BinaryOp::Mul, &m, &Value::f32(2.0)),
            Value::Matrix(
                2,
                [
                    [2.0, 4.0, 0.0, 0.0],
                    [6.0, 8.0, 0.0, 0.0],
                    [0.0; 4],
                    [0.0; 4]
                ]
            )
        );
    }

    #[test]
    fn integer_edges_are_defined() {
        let op = |op, a: Value, b: Value| binary(op, &a, &b);
        assert_eq!(
            op(BinaryOp::Div, Value::i32(7), Value::i32(0)),
            Value::i32(7)
        );
        assert_eq!(
            op(BinaryOp::Rem, Value::u32(7), Value::u32(0)),
            Value::u32(0)
        );
        assert_eq!(
            op(BinaryOp::Div, Value::i32(i32::MIN), Value::i32(-1)),
            Value::i32(i32::MIN)
        );
        assert_eq!(
            op(BinaryOp::Shl, Value::u32(1), Value::u32(33)),
            Value::u32(2)
        );
        assert_eq!(
            op(BinaryOp::Shr, Value::i32(-8), Value::u32(1)),
            Value::i32(-4)
        );
        assert_eq!(
            op(BinaryOp::Add, Value::i32(i32::MAX), Value::i32(1)),
            Value::i32(i32::MIN)
        );
        assert_eq!(
            op(BinaryOp::Rem, Value::f32(-7.5), Value::f32(2.0)),
            Value::f32(-1.5)
        );
        assert_eq!(
            op(BinaryOp::Add, Value::floats(&[1.0, 2.0]), Value::f32(0.5)),
            Value::floats(&[1.5, 2.5])
        );
        assert_eq!(convert(Ty::I32, &Value::f32(-2.7)), Value::i32(-2));
        assert_eq!(convert(Ty::U32, &Value::i32(-1)), Value::u32(u32::MAX));
    }

    #[test]
    fn a_quad_covers_each_pixel_once() {
        use crate::program::{Function, Stmt};
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
                locals: vec![crate::program::Local {
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
    }
}
