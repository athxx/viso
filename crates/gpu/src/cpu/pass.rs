//! One render pass on the CPU, tile by tile.
//!
//! The pass's draws are flattened into items — one per instance, per mesh
//! triangle, per user-program draw — each with the target pixels it can touch
//! (its shape's bounds cut by its scissor). Items are binned into 64×64
//! tiles in submission order. A row of tiles is a band: a disjoint run of
//! target rows, so bands rasterize in parallel without sharing a pixel, and
//! within a tile every item draws in order, which keeps the result identical
//! however the bands are spread over workers.
//!
//! A tile no item touches is not visited at all: a pass that loads its target
//! (a partial redraw) costs only the tiles its draws cover. A pass that
//! clears fills each band before drawing into it.
//!
//! A pass can also be a [`Layer`]: binned but not drawn, its texture never
//! allocated. A later draw that composites it evaluates just the texels it
//! reads for the tile it is drawing, into its worker's window, and samples
//! that — so a group with opacity costs a tile of scratch per worker, not a
//! target the size of the group.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::fill::{self, Cx, Place, PxRect, Rows, Triangle, to_pixels};
use super::pixels::{Fmt, Px};
use super::pool::Pool;
use super::sample::View;
use super::{Pipeline, Resources, TILE};
use crate::backend::{DrawCommand, Geometry, IndexFormat, LoadOp};
use crate::resource::{Binding, BlendMode, SamplerDesc, TextureFormat};
use crate::{
    CpuDraw, CpuTarget, CpuTexels, CpuTexture, PROGRAM_SAMPLERS, PROGRAM_TEXTURES, TextureId,
};

/// A command composites no layer.
pub(super) const NO_LAYER: u16 = u16::MAX;

/// A deferred pass: the draws that would fill `texture` (its commands in the
/// draw list), binned over its extent in the scratch of the same index,
/// evaluated region by region by the draws that composite it.
#[derive(Debug, Clone)]
pub(super) struct Layer {
    pub texture: TextureId,
    pub width: u32,
    pub height: u32,
    pub format: TextureFormat,
    pub clear: [f32; 4],
    /// The pass it stands for, and that pass's commands.
    pub pass: usize,
    pub commands: std::ops::Range<usize>,
}

/// One worker's scratch: the `f32` window a user program draws into, and the
/// window a deferred layer is evaluated into, in the layer's storage.
#[derive(Default)]
pub(super) struct Window {
    program: Vec<[f32; 4]>,
    unorm8: Vec<[u8; 4]>,
    float: Vec<[f32; 4]>,
}

impl Window {
    /// The bytes held.
    pub(super) fn bytes(&self) -> usize {
        (self.program.capacity() + self.float.capacity()) * 16 + self.unorm8.capacity() * 4
    }
}

/// What every band of a pass reads besides its target.
#[derive(Clone, Copy)]
pub(super) struct Shared<'a> {
    pub res: &'a Resources,
    /// The deferred layers this pass's draws may composite, each binned in
    /// the scratch of its index, its commands in `list`.
    pub layers: &'a [Layer],
    pub layer_scratch: &'a [Scratch],
    pub list: &'a [DrawCommand],
    /// Per command of the pass, the layer it composites, or [`NO_LAYER`];
    /// empty when none does.
    pub layer_of: &'a [u16],
    /// One window per worker.
    pub workers: &'a [Mutex<Window>],
}

/// What one item draws.
#[derive(Debug, Clone, Copy)]
enum Draw {
    /// Instance `n` of the command's generated geometry.
    Instance(u32),
    /// Triangle `n` of the pass's decoded mesh triangles.
    Triangle(u32),
    /// The command's whole user-program draw, clipped to the tile.
    Program,
}

#[derive(Debug, Clone, Copy)]
struct Item {
    cmd: u32,
    draw: Draw,
    rect: PxRect,
}

/// A pass's binning, kept between passes so a steady frame allocates nothing.
#[derive(Default)]
pub(super) struct Scratch {
    items: Vec<Item>,
    triangles: Vec<Triangle>,
    /// Per tile, its first entry; one past the last tile, the entry count.
    starts: Vec<u32>,
    /// Item indices, grouped by tile, each group in submission order.
    entries: Vec<u32>,
    tiles_x: usize,
}

impl Scratch {
    /// The items binned in tile `t`, in submission order.
    fn tile(&self, t: usize) -> &[u32] {
        &self.entries[self.starts[t] as usize..self.starts[t + 1] as usize]
    }
}

/// The target a pass draws into: its pixels, row-major `stride` per row.
pub(super) struct Target<'a, P> {
    pub px: &'a mut [P],
    pub width: u32,
    pub height: u32,
    pub stride: usize,
    pub fmt: Fmt,
    /// Whether the format stores 8 bits per channel.
    pub unorm8: bool,
}

/// Bins `commands` over a `width × height` target.
pub(super) fn prepare(
    res: &Resources,
    commands: &[DrawCommand],
    w: u32,
    h: u32,
    scratch: &mut Scratch,
) {
    collect(res, commands, w, h, scratch);
    scratch.tiles_x = w.div_ceil(TILE) as usize;
    bin(scratch, scratch.tiles_x, h.div_ceil(TILE) as usize);
}

/// Rasterizes `commands` into `target`; `shared.workers` holds a window for
/// every worker of `pool`.
pub(super) fn run<P: Px>(
    shared: Shared<'_>,
    target: Target<'_, P>,
    load: LoadOp,
    commands: &[DrawCommand],
    scratch: &mut Scratch,
    pool: Option<&Pool>,
) {
    let (w, h) = (target.width, target.height);
    if w == 0 || h == 0 {
        return;
    }
    prepare(shared.res, commands, w, h, scratch);
    let tiles_x = scratch.tiles_x;
    let tiles_y = h.div_ceil(TILE) as usize;

    let clear = match load {
        LoadOp::Clear(c) => Some(P::store(c, target.fmt)),
        LoadOp::Load => None,
    };
    let Target {
        px,
        stride,
        fmt,
        unorm8,
        ..
    } = target;
    let band_len = stride * TILE as usize;
    let base = Base(px.as_mut_ptr());
    let len = px.len();
    let next = AtomicUsize::new(0);
    let scratch = &*scratch;
    let pass = Pass {
        commands,
        scratch,
        size: [w, h],
        unorm8,
    };
    let band = |worker: usize, b: usize| {
        let start = b * band_len;
        let end = (start + band_len).min(len);
        // SAFETY: band `b` is the rows `[b * TILE, (b + 1) * TILE)`, a range of
        // `px` no other band overlaps, and each band index is handed out once
        // (by `next`), so this is the only live reference to these pixels.
        // `px` outlives the pass: every worker finishes before `run` returns.
        let rows = unsafe { std::slice::from_raw_parts_mut(base.at(start), end - start) };
        let y0 = b as u32 * TILE;
        let y1 = (y0 + TILE).min(h);
        if let Some(clear) = clear {
            for y in 0..(y1 - y0) as usize {
                rows[y * stride..y * stride + w as usize].fill(clear);
            }
        }
        let mut rows = Band {
            px: rows,
            stride,
            origin: [0, y0],
            fmt,
        };
        let mut window = shared.workers[worker].lock().expect("raster window lock");
        for tx in 0..tiles_x {
            let tile = [tx as u32 * TILE, y0, ((tx as u32 + 1) * TILE).min(w), y1];
            for &e in scratch.tile(b * tiles_x + tx) {
                let item = scratch.items[e as usize];
                let Some(clip) = intersect(tile, item.rect) else {
                    continue;
                };
                draw(&shared, &pass, item, &mut rows, clip, &mut window);
            }
        }
    };
    let job = |worker: usize| {
        loop {
            let b = next.fetch_add(1, Ordering::Relaxed);
            if b >= tiles_y {
                break;
            }
            band(worker, b);
        }
    };
    match pool {
        // Not worth waking the workers for one band.
        Some(pool) if tiles_y > 1 && !scratch.items.is_empty() => pool.broadcast(&job),
        _ => job(0),
    }
}

/// The target's pixels, shared by the bands that each take a disjoint run.
struct Base<P>(*mut P);

// SAFETY: the pointer is only turned into the disjoint band slices `run`
// hands out, one per band index, each to one worker at a time.
unsafe impl<P: Send> Sync for Base<P> {}

impl<P> Base<P> {
    /// The pointer to pixel `i`, which must be in bounds.
    fn at(&self, i: usize) -> *mut P {
        // SAFETY: the caller keeps `i` within the target's pixels.
        unsafe { self.0.add(i) }
    }
}

fn intersect(a: PxRect, b: PxRect) -> Option<PxRect> {
    let r = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    (r[2] > r[0] && r[3] > r[1]).then_some(r)
}

/// Target pixels per unit of a built-in's coordinates on a `width`-wide
/// target: the target over the viewport its inline uniforms map onto it, one
/// when the draw carries none.
fn uniform_scale(cmd: &DrawCommand, width: u32) -> f32 {
    let bytes = cmd.uniforms.as_bytes();
    if bytes.len() < 8 {
        return 1.0;
    }
    let viewport = f32::from_ne_bytes(bytes[0..4].try_into().expect("four bytes"));
    if viewport > 0.0 && viewport.is_finite() {
        width as f32 / viewport
    } else {
        1.0
    }
}

/// The place a built-in's inline uniforms put its draw at: the `shift` after
/// the viewport, then the images of the axes; the identity when the draw
/// carries none.
fn uniform_place(cmd: &DrawCommand) -> Place {
    let bytes = cmd.uniforms.as_bytes();
    let at = |i: usize| f32::from_ne_bytes(bytes[i..i + 4].try_into().expect("four bytes"));
    match bytes.len() {
        32.. => Place {
            a: at(16),
            b: at(20),
            c: at(24),
            d: at(28),
            e: at(8),
            f: at(12),
        },
        16.. => Place::shift([at(8), at(12)]),
        _ => Place::IDENTITY,
    }
}

/// The pipeline `cmd` draws with.
fn pipeline<'a>(res: &'a Resources, cmd: &DrawCommand) -> &'a Pipeline {
    res.pipelines
        .get(cmd.pipeline.into())
        .expect("pipeline handle does not resolve")
}

/// Flattens the pass's draws into items with the pixels each can touch.
fn collect(res: &Resources, commands: &[DrawCommand], w: u32, h: u32, scratch: &mut Scratch) {
    scratch.items.clear();
    scratch.triangles.clear();
    for (ci, cmd) in commands.iter().enumerate() {
        let target = [0, 0, w, h];
        let clip = match cmd.scissor {
            Some((x, y, sw, sh)) => {
                intersect(target, [x, y, x.saturating_add(sw), y.saturating_add(sh)])
            }
            None => Some(target),
        };
        let Some(clip) = clip else {
            continue;
        };
        let pipe = pipeline(res, cmd);
        let cmd_index = ci as u32;
        if pipe.program.is_some() {
            if matches!(cmd.geometry, Geometry::Generated { .. }) {
                scratch.items.push(Item {
                    cmd: cmd_index,
                    draw: Draw::Program,
                    rect: clip,
                });
            }
            continue;
        }
        let place = uniform_place(cmd);
        let scale = uniform_scale(cmd, w);
        match cmd.geometry {
            Geometry::Generated { count } => {
                let bytes = &res.buffer(cmd.instance_buffer)[..];
                for n in 0..count {
                    let Some(inst) = pipe.instance(bytes, cmd.instance_offset, n) else {
                        break;
                    };
                    let rect = fill::bounds(pipe.builtin, &inst, place, scale)
                        .and_then(|r| to_pixels(r, w, h))
                        .and_then(|r| intersect(r, clip));
                    if let Some(rect) = rect {
                        scratch.items.push(Item {
                            cmd: cmd_index,
                            draw: Draw::Instance(n),
                            rect,
                        });
                    }
                }
            }
            Geometry::IndexedMesh {
                vertex_buffer,
                index_buffer,
                index_format,
                index_offset,
                index_count,
            } => {
                let vertices = &res.buffer(vertex_buffer)[..];
                let indices = &res.buffer(index_buffer)[..];
                let vertex = |i: u32| {
                    let v = pipe
                        .instance(vertices, 0, i)
                        .expect("a mesh index past its vertices");
                    let p = place.apply(v.f2(0));
                    ([p[0] * scale, p[1] * scale], v.f4(1), v.f1(2))
                };
                for t in 0..index_count / 3 {
                    let at = index_offset + t * 3;
                    let [(p0, c0, e0), (p1, c1, e1), (p2, c2, e2)] =
                        [0, 1, 2].map(|k| vertex(read_index(indices, index_format, at + k)));
                    let triangle = Triangle {
                        pos: [p0, p1, p2],
                        color: [c0, c1, c2],
                        edge: [e0, e1, e2],
                    };
                    let rect = to_pixels(triangle.bounds(), w, h).and_then(|r| intersect(r, clip));
                    if let Some(rect) = rect {
                        scratch.items.push(Item {
                            cmd: cmd_index,
                            draw: Draw::Triangle(scratch.triangles.len() as u32),
                            rect,
                        });
                        scratch.triangles.push(triangle);
                    }
                }
            }
            // Drawn by the scene pass alone, against its depth.
            Geometry::InstancedMesh { .. } => {}
        }
    }
}

/// Groups the items by the tiles they touch, in submission order.
fn bin(scratch: &mut Scratch, tiles_x: usize, tiles_y: usize) {
    let tiles = tiles_x * tiles_y;
    let tile_span = |r: PxRect| {
        (
            (r[0] / TILE) as usize..r[2].div_ceil(TILE) as usize,
            (r[1] / TILE) as usize..r[3].div_ceil(TILE) as usize,
        )
    };
    scratch.starts.clear();
    scratch.starts.resize(tiles + 1, 0);
    for item in &scratch.items {
        let (xs, ys) = tile_span(item.rect);
        for ty in ys {
            for tx in xs.clone() {
                scratch.starts[ty * tiles_x + tx + 1] += 1;
            }
        }
    }
    for t in 0..tiles {
        scratch.starts[t + 1] += scratch.starts[t];
    }
    let total = scratch.starts[tiles] as usize;
    scratch.entries.clear();
    scratch.entries.resize(total, 0);
    // Fill each tile's group from its start; `cursor` walks every group.
    for (e, item) in scratch.items.iter().enumerate() {
        let (xs, ys) = tile_span(item.rect);
        for ty in ys {
            for tx in xs.clone() {
                let t = ty * tiles_x + tx;
                // `starts[t]` doubles as the group's write cursor, restored below.
                let at = scratch.starts[t] as usize;
                scratch.entries[at] = e as u32;
                scratch.starts[t] += 1;
            }
        }
    }
    // Each cursor ended at the next group's start: shift them back by one group.
    for t in (1..=tiles).rev() {
        scratch.starts[t] = scratch.starts[t - 1];
    }
    scratch.starts[0] = 0;
}

fn read_index(bytes: &[u8], format: IndexFormat, n: u32) -> u32 {
    match format {
        IndexFormat::U16 => {
            let at = n as usize * 2;
            u32::from(u16::from_le_bytes([bytes[at], bytes[at + 1]]))
        }
        IndexFormat::U32 => {
            let at = n as usize * 4;
            u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
        }
    }
}

/// A run of a target's rows — a band of the target, or a layer's window —
/// holding the pixels from `origin`, `stride` per row.
struct Band<'a, P> {
    px: &'a mut [P],
    stride: usize,
    origin: [u32; 2],
    fmt: Fmt,
}

impl<P: Px> Band<'_, P> {
    #[inline]
    fn at(&self, y: u32, x0: u32) -> usize {
        (y - self.origin[1]) as usize * self.stride + (x0 - self.origin[0]) as usize
    }
}

/// Rows whose every write replaces, for a pipeline that does not blend.
struct Replacing<'r, R>(&'r mut R);

impl<R: Rows> Rows for Replacing<'_, R> {
    fn blend(&mut self, y: u32, x0: u32, src: &[[f32; 4]]) {
        self.0.replace(y, x0, src);
    }

    fn blend_dithered(&mut self, y: u32, x0: u32, src: &[[f32; 4]]) {
        self.0.replace(y, x0, src);
    }

    fn replace(&mut self, y: u32, x0: u32, src: &[[f32; 4]]) {
        self.0.replace(y, x0, src);
    }

    fn load(&self, y: u32, x0: u32, out: &mut [[f32; 4]]) {
        self.0.load(y, x0, out);
    }
}

/// The pass an item belongs to.
struct Pass<'a> {
    commands: &'a [DrawCommand],
    scratch: &'a Scratch,
    /// The target's `[width, height]`.
    size: [u32; 2],
    unorm8: bool,
}

impl<P: Px> Rows for Band<'_, P> {
    fn blend(&mut self, y: u32, x0: u32, src: &[[f32; 4]]) {
        let at = self.at(y, x0);
        P::blend(&mut self.px[at..at + src.len()], src, self.fmt, None);
    }

    fn blend_dithered(&mut self, y: u32, x0: u32, src: &[[f32; 4]]) {
        let at = self.at(y, x0);
        P::blend(
            &mut self.px[at..at + src.len()],
            src,
            self.fmt,
            Some((x0, y)),
        );
    }

    fn replace(&mut self, y: u32, x0: u32, src: &[[f32; 4]]) {
        let at = self.at(y, x0);
        P::replace(&mut self.px[at..at + src.len()], src, self.fmt);
    }

    fn load(&self, y: u32, x0: u32, out: &mut [[f32; 4]]) {
        let at = self.at(y, x0);
        P::load(&self.px[at..at + out.len()], out, self.fmt);
    }
}

fn draw<P: Px>(
    shared: &Shared<'_>,
    pass: &Pass<'_>,
    item: Item,
    rows: &mut Band<'_, P>,
    clip: PxRect,
    window: &mut Window,
) {
    let res = shared.res;
    let cmd = &pass.commands[item.cmd as usize];
    match item.draw {
        Draw::Instance(n) => {
            let pipe = pipeline(res, cmd);
            let bytes = &res.buffer(cmd.instance_buffer)[..];
            let inst = pipe
                .instance(bytes, cmd.instance_offset, n)
                .expect("an instance collected in range");
            let place = uniform_place(cmd);
            let unplace = place.inverse();
            let scale = uniform_scale(cmd, pass.size[0]);
            // A replacing pipeline writes its fragments as they are.
            let mut replacing = Replacing(&mut *rows);
            let rows: &mut dyn Rows = if pipe.blend == BlendMode::Replace {
                &mut replacing
            } else {
                replacing.0
            };
            let l = match shared.layer_of.get(item.cmd as usize) {
                Some(&l) if l != NO_LAYER => l as usize,
                _ => {
                    let cx = Cx {
                        res,
                        cmd,
                        place,
                        unplace,
                        scale,
                        layer: None,
                    };
                    return fill::draw(pipe.builtin, &cx, &inst, rows, clip);
                }
            };
            let layer = &shared.layers[l];
            let size = [layer.width, layer.height];
            let Some(region) = fill::source_region(pipe.builtin, &inst, place, scale, clip, size)
            else {
                return;
            };
            let view = evaluate(shared, layer, &shared.layer_scratch[l], region, window);
            let cx = Cx {
                res,
                cmd,
                place,
                unplace,
                scale,
                layer: Some((layer.texture, view)),
            };
            fill::draw(pipe.builtin, &cx, &inst, rows, clip);
        }
        Draw::Triangle(t) => pass.scratch.triangles[t as usize].draw(rows, clip),
        Draw::Program => program(
            res,
            cmd,
            rows,
            clip,
            pass.size,
            pass.unorm8,
            &mut window.program,
        ),
    }
}

/// `layer`'s texels in `region`, drawn into `window` as its own pass would
/// leave them.
fn evaluate<'w>(
    shared: &Shared<'_>,
    layer: &Layer,
    scratch: &Scratch,
    region: PxRect,
    window: &'w mut Window,
) -> View<'w> {
    let fmt = Fmt::of(layer.format);
    let Window {
        program,
        unorm8,
        float,
    } = window;
    let texels = if layer.format.is_extended_range() {
        region_into(shared, layer, scratch, region, fmt, false, float, program);
        CpuTexels::Float(float)
    } else {
        region_into(shared, layer, scratch, region, fmt, true, unorm8, program);
        CpuTexels::Rgba8 {
            texels: unorm8,
            bgra: fmt.bgra,
            srgb: fmt.srgb,
        }
    };
    View {
        width: layer.width,
        height: layer.height,
        texels,
        origin: [region[0], region[1]],
        extent: [region[2] - region[0], region[3] - region[1]],
    }
}

/// Draws `layer`'s items over `region` into `px`, cleared first.
#[allow(clippy::too_many_arguments)]
fn region_into<P: Px>(
    outer: &Shared<'_>,
    layer: &Layer,
    scratch: &Scratch,
    region: PxRect,
    fmt: Fmt,
    unorm8: bool,
    px: &mut Vec<P>,
    program: &mut Vec<[f32; 4]>,
) {
    let stride = (region[2] - region[0]) as usize;
    px.clear();
    px.resize(
        stride * (region[3] - region[1]) as usize,
        P::store(layer.clear, fmt),
    );
    let mut rows = Band {
        px,
        stride,
        origin: [region[0], region[1]],
        fmt,
    };
    let pass = Pass {
        commands: &outer.list[layer.commands.clone()],
        scratch,
        size: [layer.width, layer.height],
        unorm8,
    };
    // A layer composites no layer of its own, so its draws need no lookup.
    let shared = Shared {
        layers: &[],
        layer_of: &[],
        workers: &[],
        ..*outer
    };
    let mut window = Window {
        program: std::mem::take(program),
        ..Window::default()
    };
    for ty in region[1] / TILE..region[3].div_ceil(TILE) {
        for tx in region[0] / TILE..region[2].div_ceil(TILE) {
            let tile = [
                tx * TILE,
                ty * TILE,
                ((tx + 1) * TILE).min(layer.width),
                ((ty + 1) * TILE).min(layer.height),
            ];
            let Some(tile) = intersect(tile, region) else {
                continue;
            };
            for &e in scratch.tile(ty as usize * scratch.tiles_x + tx as usize) {
                let item = scratch.items[e as usize];
                if let Some(clip) = intersect(tile, item.rect) {
                    draw(&shared, &pass, item, &mut rows, clip, &mut window);
                }
            }
        }
    }
    *program = window.program;
}

/// A user program's draw over `clip`: the pixels read into an `f32` window,
/// drawn by the program, written back.
fn program<P: Px>(
    res: &Resources,
    cmd: &DrawCommand,
    rows: &mut Band<'_, P>,
    clip: PxRect,
    [width, height]: [u32; 2],
    unorm8: bool,
    window: &mut Vec<[f32; 4]>,
) {
    let Geometry::Generated { count } = cmd.geometry else {
        return;
    };
    let pipe = pipeline(res, cmd);
    let Some(program) = &pipe.program else {
        return;
    };
    let span = count as usize * program.abi.instance_stride as usize;
    let Some(instances) = res
        .buffer(cmd.instance_buffer)
        .get(cmd.instance_offset..cmd.instance_offset + span)
    else {
        return;
    };
    let bindings = cmd
        .bind_group
        .and_then(|bg| res.bind_groups.get(bg.into()))
        .map_or(&[][..], |b| &b[..]);
    let uniforms = if cmd.uniforms.is_empty() {
        bindings
            .iter()
            .find_map(|b| match b {
                Binding::Uniform(id) => Some(&res.buffer(*id)[..]),
                _ => None,
            })
            .map_or(&[][..], |bytes| {
                &bytes[..(program.abi.uniform_size as usize).min(bytes.len())]
            })
    } else {
        cmd.uniforms.as_bytes()
    };
    let none = CpuTexture {
        width: 0,
        height: 0,
        texels: CpuTexels::None,
    };
    let mut textures = [none; PROGRAM_TEXTURES as usize];
    let mut samplers = [SamplerDesc::default(); PROGRAM_SAMPLERS as usize];
    let (mut t, mut s) = (0, 0);
    for binding in bindings {
        match *binding {
            Binding::Texture(id) if t < textures.len() => {
                if let Some(texture) = res.textures.get(id.into()) {
                    textures[t] = texture.view();
                }
                t += 1;
            }
            Binding::Sampler(id) if s < samplers.len() => {
                samplers[s] = res.samplers.get(id.into()).copied().unwrap_or_default();
                s += 1;
            }
            _ => {}
        }
    }
    let w = (clip[2] - clip[0]) as usize;
    window.resize(w * (clip[3] - clip[1]) as usize, [0.0; 4]);
    for (k, y) in (clip[1]..clip[3]).enumerate() {
        rows.load(y, clip[0], &mut window[k * w..(k + 1) * w]);
    }
    let mut target = CpuTarget {
        pixels: window,
        width,
        height,
        origin: [clip[0], clip[1]],
        stride: w as u32,
        scissor: clip,
        unorm8,
    };
    program.cpu.draw(
        &mut target,
        &CpuDraw {
            uniforms,
            instances,
            count,
            textures: &textures[..t],
            samplers: &samplers[..s],
        },
        program.blend,
    );
    for (k, y) in (clip[1]..clip[3]).enumerate() {
        rows.replace(y, clip[0], &window[k * w..(k + 1) * w]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(rect: PxRect) -> Item {
        Item {
            cmd: 0,
            draw: Draw::Program,
            rect,
        }
    }

    #[test]
    fn binning_groups_items_by_tile_in_submission_order() {
        let mut s = Scratch {
            items: vec![
                item([0, 0, 10, 10]),
                item([60, 0, 70, 10]),
                item([0, 60, 130, 70]),
                item([5, 5, 6, 6]),
            ],
            ..Scratch::default()
        };
        bin(&mut s, 3, 2);
        let tile = |t: usize| &s.entries[s.starts[t] as usize..s.starts[t + 1] as usize];
        assert_eq!(tile(0), [0, 1, 2, 3]);
        assert_eq!(tile(1), [1, 2]);
        assert_eq!(tile(2), [2]);
        assert_eq!(tile(3), [2]);
        assert_eq!(tile(4), [2]);
        assert_eq!(tile(5), [2]);
    }
}
