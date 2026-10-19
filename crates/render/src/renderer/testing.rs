//! What the patch and splice tests share: a headless renderer, a few
//! primitives, and the comparison of a renderer brought to a stream in place
//! with one that uploaded it whole.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId, TextureDesc, TextureFormat};

use super::patch::NOT_LOWERED;
use super::{PassTarget, Renderer, Segment, SegmentKind};
use crate::batch::joins;
use crate::primitive::{
    AnalyticCapsule, AnalyticEllipse, AnalyticLine, AnalyticRRect, AnalyticShadow, Border, Corners,
    GlyphInstanceData, GlyphLane, GlyphRunDraw, ImageDraw, LayerClip, LineCap, LineJoin, Point,
    Primitive, Quad, Rect, Rgba, ShadowShape,
};
use crate::scene::key::pod_bytes;
use crate::scene::store::StoreRef;
use crate::transform::Space;

pub(super) const SIZE: f32 = 128.0;

pub(super) struct Gpu {
    pub(super) gpu: CpuRaster,
    pub(super) surface: SurfaceId,
    pub(super) r: Renderer,
}

impl Gpu {
    pub(super) fn new() -> Gpu {
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, SIZE as u32, SIZE as u32);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([SIZE, SIZE]);
        Gpu { gpu, surface, r }
    }

    pub(super) fn texture(&mut self) -> viso_gpu::TextureId {
        self.gpu.create_texture(&TextureDesc {
            width: 4,
            height: 4,
            format: TextureFormat::Bgra8Unorm,
            render_target: false,
            label: "t",
        })
    }

    pub(super) fn upload(&mut self, scene: &[Primitive]) {
        self.r.upload(&mut self.gpu, scene);
    }

    pub(super) fn patch(&mut self, scene: &[Primitive], changed: &[u32]) -> bool {
        self.r.patch(&mut self.gpu, scene, changed)
    }

    pub(super) fn splice(&mut self, scene: &[Primitive], splices: &[super::Splice]) -> bool {
        self.r.splice(&mut self.gpu, scene, splices)
    }

    pub(super) fn present(&mut self) -> Vec<u8> {
        self.r.submit(
            &mut self.gpu,
            self.surface,
            [0.0, 0.0, 0.0, 1.0],
            [SIZE, SIZE],
        );
        self.gpu.read_pixels_bgra8(self.surface)
    }
}

pub(super) fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

pub(super) fn rgba(r: f32, a: f32) -> Rgba {
    Rgba::new(r, 0.5, 0.25, a)
}

pub(super) fn quad(r: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color,
        radius: 2.0,
        border: Border::NONE,
    })
}

pub(super) fn layer(clip: Rect, opacity: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

/// One primitive of every kind a patch rewrites, tinted `t`, over a
/// background quad.
pub(super) fn kinds(texture: viso_gpu::TextureId, t: f32) -> Vec<Primitive> {
    let color = rgba(t, 0.8);
    vec![
        quad(rect(0.0, 0.0, SIZE, SIZE), rgba(0.1, 1.0)),
        Primitive::AnalyticRRect(AnalyticRRect {
            rect: rect(4.0, 4.0, 20.0, 12.0),
            color,
            radius: Corners::SHARP,
            border: Border::NONE,
        }),
        Primitive::AnalyticEllipse(AnalyticEllipse {
            rect: rect(28.0, 4.0, 12.0, 12.0),
            color,
            border: Border::NONE,
        }),
        Primitive::AnalyticCapsule(AnalyticCapsule {
            rect: rect(44.0, 4.0, 24.0, 8.0),
            color,
            border: Border::NONE,
        }),
        Primitive::AnalyticLine(AnalyticLine {
            p0: Point { x: 4.0, y: 30.0 },
            p1: Point { x: 60.0, y: 40.0 },
            width: 3.0,
            color,
            cap: LineCap::Round,
            join: LineJoin::Miter,
            miter_limit: 4.0,
            border: Border::NONE,
        }),
        Primitive::AnalyticShadow(AnalyticShadow {
            rect: rect(70.0, 10.0, 20.0, 20.0),
            color,
            radius: Corners::SHARP,
            offset: [2.0, 2.0],
            sigma: 3.0,
            spread: 0.0,
            shape: ShadowShape::RoundedBox,
            inner: false,
        }),
        Primitive::Image(ImageDraw {
            tint: color,
            ..ImageDraw::new(rect(4.0, 50.0, 16.0, 16.0), texture)
        }),
        Primitive::GlyphRun(GlyphRunDraw {
            glyphs: (0..4)
                .map(|i| GlyphInstanceData {
                    rect: rect(30.0 + i as f32 * 6.0, 50.0, 5.0, 8.0),
                    uv: rect(0.0, 0.0, 0.5, 0.5),
                })
                .collect(),
            atlas: texture,
            color,
            lane: GlyphLane::CoverageA8,
        }),
    ]
}

/// Requires `patched` to hold exactly what `whole` holds after uploading
/// the same stream whole.
#[track_caller]
pub(super) fn same_state(patched: &Renderer, whole: &Renderer) {
    same_frame(patched, whole, true);
}

/// Requires `spliced` to hold what `whole` holds after uploading the same
/// stream whole — the same scene, store by store — but for the content stamps
/// of the entries it did not rewrite.
#[track_caller]
pub(super) fn same_scene(spliced: &Renderer, whole: &Renderer) {
    same_frame(spliced, whole, false);
    let (a, b) = (&spliced.scene, &whole.scene);
    assert_eq!(a.paint_order, b.paint_order);
    macro_rules! same_store {
        ($store:ident, $id:path) => {{
            assert_eq!(a.$store.len(), b.$store.len(), stringify!($store));
            for i in 0..a.$store.len() as u32 {
                assert_eq!(
                    a.$store.get($id(i)),
                    b.$store.get($id(i)),
                    stringify!($store)
                );
            }
        }};
    }
    use crate::scene::ids::{GeometryId, ImageId};
    same_store!(quads, GeometryId::new);
    same_store!(analytic_rrects, GeometryId::new);
    same_store!(analytic_ellipses, GeometryId::new);
    same_store!(analytic_capsules, GeometryId::new);
    same_store!(analytic_lines, GeometryId::new);
    same_store!(analytic_shadows, GeometryId::new);
    same_store!(images, ImageId::new);
    assert_eq!(a.glyph_runs.len(), b.glyph_runs.len());
    for run in 0..a.glyph_runs.len() as u32 {
        let (x, y) = (a.glyph_runs.run(run), b.glyph_runs.run(run));
        assert_eq!(x, y, "glyph run {run}");
        assert_eq!(
            x.map(|r| a.glyph_runs.glyphs(r)),
            y.map(|r| b.glyph_runs.glyphs(r)),
            "glyph run {run}'s glyphs"
        );
    }
    assert_eq!(spliced.marks, whole.marks);
    assert_eq!(spliced.root_context, whole.root_context);
    assert_eq!(spliced.stream_len, whole.stream_len);
}

/// One draw a renderer encodes, with the segments next to it it would join
/// joined: its kind, clip, pass and space, and the bytes of its instances.
type Drawn = (SegmentKind, Option<Rect>, PassTarget, Space, Vec<u8>);

/// What `r` draws, draw by draw.
fn drawn(r: &Renderer) -> Vec<Drawn> {
    let mut out: Vec<Drawn> = Vec::new();
    let mut prev: Option<Segment> = None;
    for seg in &r.segments {
        let bytes = instances(
            r,
            seg.kind,
            seg.start as usize..(seg.start + seg.count) as usize,
        );
        match out.last_mut() {
            Some(last) if prev.is_some_and(|p| joins(&p.batch_item(), &seg.batch_item())) => {
                last.4.extend(bytes);
            }
            _ => out.push((seg.kind, seg.clip, seg.target, seg.space, bytes)),
        }
        prev = Some(*seg);
    }
    out
}

/// The bytes of the instances `slots` of the scratch a segment of `kind`
/// draws from.
fn instances(r: &Renderer, kind: SegmentKind, slots: std::ops::Range<usize>) -> Vec<u8> {
    match kind {
        SegmentKind::Quad => pod_bytes(&r.quad_scratch[slots]).to_vec(),
        SegmentKind::AnalyticRRect => pod_bytes(&r.analytic_rrect_scratch[slots]).to_vec(),
        SegmentKind::AnalyticEllipse => pod_bytes(&r.analytic_ellipse_scratch[slots]).to_vec(),
        SegmentKind::AnalyticCapsule => pod_bytes(&r.analytic_capsule_scratch[slots]).to_vec(),
        SegmentKind::AnalyticLine => pod_bytes(&r.analytic_line_scratch[slots]).to_vec(),
        SegmentKind::AnalyticShadow => pod_bytes(&r.analytic_shadow_scratch[slots]).to_vec(),
        SegmentKind::Image { .. } => pod_bytes(&r.image_scratch[slots]).to_vec(),
        SegmentKind::GlyphRun { .. } | SegmentKind::MtsdfRun { .. } => {
            pod_bytes(&r.glyph_scratch[slots]).to_vec()
        }
        _ => [slots.start as u32, slots.end as u32]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect(),
    }
}

/// The bytes of the instances paint-order entry `i` of `r` was lowered to,
/// or `None` for one not lowered.
fn lowered(r: &Renderer, i: usize) -> Option<Vec<u8>> {
    let at = r.lowered[i];
    if at == NOT_LOWERED {
        return None;
    }
    let at = at as usize;
    let kind = match r.scene.paint_order[i].store {
        StoreRef::Quad(_) => SegmentKind::Quad,
        StoreRef::AnalyticRRect(_) => SegmentKind::AnalyticRRect,
        StoreRef::AnalyticEllipse(_) => SegmentKind::AnalyticEllipse,
        StoreRef::AnalyticCapsule(_) => SegmentKind::AnalyticCapsule,
        StoreRef::AnalyticLine(_) => SegmentKind::AnalyticLine,
        StoreRef::AnalyticShadow(_) => SegmentKind::AnalyticShadow,
        StoreRef::GlyphRun(run) => {
            let count = r.scene.glyph_runs.run(run).map_or(0, |e| e.count) as usize;
            return Some(pod_bytes(&r.glyph_scratch[at..at + count]).to_vec());
        }
        _ => return Some(pod_bytes(&r.image_scratch[at..at + 1]).to_vec()),
    };
    Some(instances(r, kind, at..at + 1))
}

/// Requires each leaf family's device buffer to hold what its scratch does.
#[track_caller]
fn synced(r: &Renderer) {
    assert_eq!(r.quad_pool.shadow(), &r.quad_scratch[..]);
    assert_eq!(
        r.analytic_rrect_pool.shadow(),
        &r.analytic_rrect_scratch[..]
    );
    assert_eq!(
        r.analytic_ellipse_pool.shadow(),
        &r.analytic_ellipse_scratch[..]
    );
    assert_eq!(
        r.analytic_capsule_pool.shadow(),
        &r.analytic_capsule_scratch[..]
    );
    assert_eq!(r.analytic_line_pool.shadow(), &r.analytic_line_scratch[..]);
    assert_eq!(
        r.analytic_shadow_pool.shadow(),
        &r.analytic_shadow_scratch[..]
    );
    assert_eq!(r.image_pool.shadow(), &r.image_scratch[..]);
    assert_eq!(r.glyph_pool.shadow(), &r.glyph_scratch[..]);
}

/// [`same_state`], but for the content stamps.
#[track_caller]
pub(super) fn same_state_but_stamps(patched: &Renderer, whole: &Renderer) {
    same_frame(patched, whole, false);
}

/// [`same_state`], comparing content stamps only when `stamps`.
#[track_caller]
fn same_frame(patched: &Renderer, whole: &Renderer, stamps: bool) {
    assert_eq!(drawn(patched), drawn(whole), "the same draws");
    synced(patched);
    synced(whole);
    let order = patched.scene.paint_order.len();
    assert_eq!(order, whole.scene.paint_order.len());
    assert_eq!(patched.lowered.len(), order);
    for i in 0..order {
        assert_eq!(lowered(patched, i), lowered(whole, i), "entry {i} lowered");
        assert_eq!(patched.scene.key(i), whole.scene.key(i), "entry {i} key");
        // A whole upload stamps every composite as changed; a patch
        // stamps only what it rewrote.
        if !stamps || matches!(whole.scene.paint_order[i].store, StoreRef::Composite { .. }) {
            continue;
        }
        assert_eq!(
            patched.scene.content_stamp(i),
            whole.scene.content_stamp(i),
            "entry {i} stamp"
        );
    }
    assert_eq!(patched.present.snapshot(), whole.present.snapshot());
    assert_eq!(patched.layers, whole.layers);
    assert_eq!(patched.layer_plans, whole.layer_plans);
    let passes = |r: &Renderer| -> Vec<(f32, u64)> {
        r.offscreen_passes
            .iter()
            .map(|p| (p.opacity, p.key))
            .collect()
    };
    assert_eq!(passes(patched), passes(whole));
    for (p, w) in patched
        .scene
        .paint_order
        .iter()
        .zip(&whole.scene.paint_order)
    {
        assert_eq!(p.context, w.context);
        assert_eq!(p.store, w.store);
    }
}
