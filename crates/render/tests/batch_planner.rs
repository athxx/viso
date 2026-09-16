//! Order-safe batch planner contract (§9.6): the public paint stream lowers into
//! the maximum contiguous run of compatible instances *within* paint order, and
//! the render-chunk projection lines up one-to-one with the emitted draws.
//!
//! These assertions exercise the planner through the frozen immediate-mode
//! boundary — `Renderer::upload(&[Primitive])` — and read the cold-path
//! introspection surfaces (`inspect_batches`, `render_chunks`) the way Studio and
//! a golden dump do. They pin the two properties a batch planner must hold:
//! adjacent same-key primitives collapse into one draw, and an incompatible
//! primitive between two equal-key ones is a hard barrier that keeps them in
//! submission order (no cross-barrier merge).

use viso_gpu::{
    BindGroupId, GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc, TextureFormat, TextureId,
};
use viso_render::{
    BatchFamily, BatchKey, BatchPipeline, BatchTarget, Border, ImageDraw, Primitive, Quad, Rect,
    RenderChunkId, Renderer, Rgba,
};

const W: u32 = 128;
const H: u32 = 96;

/// The family a batch's pipeline draws through. `BatchPipeline::family` is
/// crate-private, so mirror the mapping here to cross-check a chunk against the
/// batch it projects.
fn pipeline_family(pipeline: BatchPipeline) -> BatchFamily {
    match pipeline {
        BatchPipeline::Quad => BatchFamily::Quad,
        BatchPipeline::AnalyticRRect => BatchFamily::AnalyticRRect,
        BatchPipeline::AnalyticEllipse => BatchFamily::AnalyticEllipse,
        BatchPipeline::AnalyticCapsule => BatchFamily::AnalyticCapsule,
        BatchPipeline::AnalyticLine => BatchFamily::AnalyticLine,
        BatchPipeline::Image => BatchFamily::Image,
        BatchPipeline::GlyphRun => BatchFamily::GlyphRun,
        BatchPipeline::Mesh => BatchFamily::Mesh,
        BatchPipeline::Gradient => BatchFamily::Gradient,
    }
}

/// Build a renderer over a headless surface, upload `prims`, and hand it to `f`
/// in the read window (after `upload`, before `submit`) so it can read the
/// batch/chunk introspection surfaces. `setup` runs first so a test can create
/// textures on the same backend before building its primitive list.
fn with_scene(
    setup: impl FnOnce(&mut HeadlessRaster) -> Vec<Primitive>,
    f: impl FnOnce(&Renderer),
) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let prims = setup(&mut gpu);
    let mut r = Renderer::new(&mut gpu, format);
    r.upload(&mut gpu, &prims);
    f(&r);
}

fn quad(x: f32, y: f32) -> Primitive {
    Primitive::Quad(Quad {
        rect: Rect {
            x,
            y,
            w: 10.0,
            h: 10.0,
        },
        color: Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
        radius: 0.0,
        border: Border::NONE,
    })
}

fn make_texture(gpu: &mut HeadlessRaster) -> TextureId {
    gpu.create_texture(&TextureDesc {
        width: 2,
        height: 2,
        format: TextureFormat::Bgra8Unorm,
        render_target: false,
        label: "batch-planner-test",
    })
}

fn image(texture: TextureId) -> Primitive {
    Primitive::Image(ImageDraw::new(
        Rect {
            x: 0.0,
            y: 0.0,
            w: 8.0,
            h: 8.0,
        },
        texture,
    ))
}

#[test]
fn adjacent_same_key_quads_collapse_to_one_batch() {
    // Three quads under the same clip/target/family: the planner grows one draw
    // spanning all three instances, and one render chunk spanning all three
    // paint-order positions.
    with_scene(
        |_| vec![quad(0.0, 0.0), quad(20.0, 0.0), quad(40.0, 0.0)],
        |r| {
            let batches = r.inspect_batches();
            assert_eq!(batches.len(), 1, "same-key adjacent quads → one batch");
            assert_eq!(batches.batches[0].range, (0, 3));

            let chunks = r.render_chunks();
            assert_eq!(chunks.len(), 1, "one chunk per batch");
            assert_eq!(
                chunks[0].geometry,
                (0, 3),
                "chunk draws all three instances"
            );
            assert_eq!(
                chunks[0].order,
                (0, 3),
                "chunk spans all three paint positions"
            );
            assert_eq!(chunks[0].family, BatchFamily::Quad);
        },
    );
}

#[test]
fn incompatible_primitive_splits_equal_key_quads_in_order() {
    // Two quads with an image between them. The image binds its own texture and
    // is unmergeable, so it is a hard barrier: the trailing quad may NOT reach
    // back past it to join the leading quad. The emitted draws replay the scene
    // in submission order — quad, image, quad — three batches, not two.
    with_scene(
        |gpu| {
            let tex = make_texture(gpu);
            vec![quad(0.0, 0.0), image(tex), quad(40.0, 0.0)]
        },
        |r| {
            let batches = r.inspect_batches();
            assert_eq!(
                batches.len(),
                3,
                "an unmergeable primitive is a barrier: no cross-barrier merge"
            );

            let chunks = r.render_chunks();
            assert_eq!(chunks.len(), 3, "one chunk per batch");
            // Chunks replay in submission order: quad @0, image @1, quad @2.
            assert_eq!(chunks[0].family, BatchFamily::Quad);
            assert_eq!(chunks[0].order, (0, 1));
            assert_eq!(chunks[1].family, BatchFamily::Image);
            assert_eq!(chunks[1].order, (1, 2));
            assert_eq!(chunks[2].family, BatchFamily::Quad);
            assert_eq!(chunks[2].order, (2, 3));
            // The two quads carry the same packed key but stayed distinct draws:
            // the barrier split them, key equality did not re-merge them.
            assert_eq!(chunks[0].key, chunks[2].key);
        },
    );
}

#[test]
fn batch_key_packs_and_unpacks_losslessly() {
    // The packed key round-trips through its field accessors for every family,
    // every target class, and a bound resource — the frozen §9.6 field layout.
    for (family, tag) in [
        (BatchFamily::Quad, 0u64),
        (BatchFamily::Image, 1),
        (BatchFamily::GlyphRun, 2),
        (BatchFamily::Mesh, 3),
        (BatchFamily::AnalyticRRect, 4),
        (BatchFamily::AnalyticEllipse, 5),
        (BatchFamily::AnalyticCapsule, 6),
        (BatchFamily::AnalyticLine, 7),
        (BatchFamily::Gradient, 8),
    ] {
        let main = BatchKey::pack(family, BatchTarget::Main, None);
        assert_eq!(main.family(), family);
        assert_eq!(main.target_field(), 0, "Main packs render target 0");
        assert_eq!(main.resource_field(), 0, "no resource packs 0");

        let off = BatchKey::pack(family, BatchTarget::Offscreen(3), None);
        assert_eq!(off.family(), family);
        assert_eq!(off.target_field(), 4, "Offscreen(i) packs i + 1");

        let bound = BatchKey::pack(family, BatchTarget::Main, Some(BindGroupId::new(7)));
        assert_eq!(bound.family(), family);
        assert_eq!(
            bound.resource_field(),
            7,
            "resource packs the bind-group index"
        );

        // The tag lives in the low four bits; distinct families never collide.
        assert_eq!(main.bits() & 0b1111, tag);
    }

    // Distinct dimensions produce distinct keys — no two of these alias.
    let a = BatchKey::pack(BatchFamily::Quad, BatchTarget::Main, None);
    let b = BatchKey::pack(BatchFamily::Mesh, BatchTarget::Main, None);
    let c = BatchKey::pack(BatchFamily::Quad, BatchTarget::Offscreen(0), None);
    let d = BatchKey::pack(
        BatchFamily::Quad,
        BatchTarget::Main,
        Some(BindGroupId::new(1)),
    );
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_ne!(a, d);
}

#[test]
fn render_chunks_correspond_one_to_one_with_batches() {
    // The chunk projection is a cold-path view *over* the segment stream: every
    // chunk matches its batch's key, geometry, and clip, and `chunks[i]` is
    // addressed by `RenderChunkId(i)` == `inspect_batches().batches[i]`.
    with_scene(
        |gpu| {
            let tex = make_texture(gpu);
            vec![quad(0.0, 0.0), quad(20.0, 0.0), image(tex)]
        },
        |r| {
            let batches = r.inspect_batches();
            let chunks = r.render_chunks();
            assert_eq!(chunks.len(), batches.len(), "one chunk per batch");

            for (i, (chunk, batch)) in chunks.iter().zip(&batches.batches).enumerate() {
                assert_eq!(
                    chunk.geometry, batch.range,
                    "chunk {i} geometry == batch range"
                );
                assert_eq!(chunk.clip, batch.clip, "chunk {i} clip == batch clip");
                assert_eq!(
                    chunk.family,
                    pipeline_family(batch.pipeline),
                    "chunk {i} family"
                );
                // The keyed handle addresses the same chunk as the positional one.
                assert_eq!(
                    r.render_chunk(RenderChunkId(i as u32)),
                    Some(*chunk),
                    "RenderChunkId({i}) resolves to chunks[{i}]"
                );
            }

            // An out-of-range handle resolves to nothing.
            assert_eq!(r.render_chunk(RenderChunkId(chunks.len() as u32)), None);
        },
    );
}

#[test]
fn distinct_textures_yield_distinct_chunk_keys() {
    // Two images sampling different textures each bind their own resource, so
    // their chunk keys differ — the chunk key carries the real bound resource,
    // proving distinct textures are never confused for one draw.
    with_scene(
        |gpu| {
            let a = make_texture(gpu);
            let b = make_texture(gpu);
            vec![image(a), image(b)]
        },
        |r| {
            let chunks = r.render_chunks();
            assert_eq!(chunks.len(), 2, "images are unmergeable → two chunks");
            assert_eq!(chunks[0].family, BatchFamily::Image);
            assert_eq!(chunks[1].family, BatchFamily::Image);
            assert_ne!(
                chunks[0].key, chunks[1].key,
                "distinct textures pack distinct resource fields → distinct keys"
            );
        },
    );
}
