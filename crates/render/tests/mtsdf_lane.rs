//! The scalable-text lane contract (§13.2): a second glyph raster lane that
//! samples a multi-channel distance field instead of exact coverage.
//!
//! Viso 1.0 ships exactly two glyph lanes. The A8 coverage lane is the default —
//! for fixed-size text it is both cheaper and pixel-exact, and nothing beats an
//! exact rasterization of the size you are drawing. The MTSDF lane exists for the
//! case coverage cannot serve: one cached raster asked to cover a *range* of
//! sizes, because the text animates, zooms, or scales under a transform.
//!
//! The contracts defended here:
//!
//! 1. **The lane is its own pipeline.** A field decodes by taking a median of
//!    three per-edge distances; coverage decodes by reading one channel. That is
//!    a different fragment program, so it is a different pipeline and a different
//!    batch family — not a uniform on the coverage lane.
//! 2. **The two lanes coexist in one paint stream** and neither merges into the
//!    other, while still sharing one instance buffer (the ABI is identical; only
//!    the decode differs).
//! 3. **A field reproduces the coverage it was generated from.** At the bucket
//!    scale the field was generated at, the lane paints the same amount of ink as
//!    the exact rasterizer — this is the fidelity floor: scalability must not be
//!    bought with a wrong glyph shape.
//! 4. **Magnified, the field stays crisp.** This is the whole reason the lane
//!    exists: at 4× its bucket the field's edge transition stays about a pixel
//!    wide, where the same glyph stretched from an A8 bitmap smears over several.
//!
//! Everything here runs on the headless raster backend, which implements the
//! MTSDF fragment decode with the same median-of-three and the same screen-space
//! distance range the MSL emits, so the numbers below are real sampled pixels.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureDesc};
use viso_render::{
    AtlasAlloc, BatchPipeline, GlyphAtlas, GlyphInstanceData, GlyphLane, GlyphRunDraw, MtsdfAlloc,
    MtsdfAtlas, Primitive, Rect, Renderer, Rgba,
};
use viso_text::{
    Direction, FontFaceId, FontRevision, MtsdfGenerator, MtsdfGlyph, MtsdfRequest, Shaper,
    rasterize_coverage,
};

/// The same embedded ASCII-subset face the A8 golden scene uses, so the two
/// lanes are compared on identical outlines.
const FONT: &[u8] = include_bytes!("../src/fixtures/DejaVuSans-subset.ttf");

const W: u32 = 224;
const H: u32 = 192;
/// The resolution bucket the field is generated at.
const BUCKET: f32 = 32.0;
/// Edge length of the single-page atlases these tests pack into.
const ATLAS: u32 = 256;
/// A glyph with two straight edges meeting at a sharp apex — the shape a single
/// distance channel rounds off and a multi-channel field keeps.
const GLYPH: &str = "V";

fn glyph_id() -> u16 {
    let mut shaper = Shaper::new();
    let run = shaper
        .shape_run(FontFaceId(0), FONT, 0, GLYPH, Direction::LeftToRight)
        .expect("the subset face shapes its own demo string");
    run.glyphs[0].glyph_id
}

/// Generate the MTSDF field for [`GLYPH`] at [`BUCKET`], pack it into a fresh
/// atlas, and upload that atlas as a `Rgba8Data` texture.
fn mtsdf_run(gpu: &mut HeadlessRaster, at_px_per_em: f32, origin: [f32; 2]) -> GlyphRunDraw {
    let mut generator = MtsdfGenerator::default();
    let mut field = Vec::new();
    let meta: MtsdfGlyph = generator
        .generate(
            MtsdfRequest {
                sfnt: FONT,
                index: 0,
                face: FontFaceId(0),
                glyph: glyph_id(),
                revision: FontRevision(0),
                px_per_em: BUCKET,
            },
            &mut field,
        )
        .expect("the subset face generates a field");

    let texture = gpu.create_texture(&TextureDesc {
        width: ATLAS,
        height: ATLAS,
        format: MtsdfAtlas::FORMAT,
        render_target: false,
        label: "mtsdf-lane-field-atlas",
    });
    let mut atlas = MtsdfAtlas::new(ATLAS, ATLAS, texture);
    let MtsdfAlloc::Placed(uv) = atlas.alloc_in_page(0, &meta, &field) else {
        panic!("one field fits an empty page");
    };
    let (x, y, w, h, bytes) = atlas.take_dirty().expect("the field needs uploading");
    gpu.write_texture(texture, x, y, w, h, &bytes);

    // Field texels are sized at the bucket; drawing at another size is a plain
    // scale of the quad, which is exactly the lane's purpose.
    let s = meta.scale_for(at_px_per_em);
    GlyphRunDraw {
        glyphs: vec![GlyphInstanceData {
            rect: Rect {
                x: origin[0] + meta.left * s,
                y: origin[1] - meta.top * s,
                w: meta.width as f32 * s,
                h: meta.height as f32 * s,
            },
            uv,
        }],
        atlas: texture,
        color: Rgba::new(1.0, 1.0, 1.0, 1.0),
        lane: GlyphLane::Mtsdf,
    }
}

/// The same glyph on the coverage lane: rasterized exactly at `raster_px_per_em`
/// and then drawn at `at_px_per_em`, so passing the two equal is the exact path
/// and passing a larger `at_px_per_em` is a stretched bitmap.
fn coverage_run(
    gpu: &mut HeadlessRaster,
    raster_px_per_em: f32,
    at_px_per_em: f32,
    origin: [f32; 2],
) -> GlyphRunDraw {
    let bitmap = rasterize_coverage(FONT, 0, glyph_id(), raster_px_per_em)
        .expect("the subset face rasterizes");
    let texture = gpu.create_texture(&TextureDesc {
        width: ATLAS,
        height: ATLAS,
        format: GlyphAtlas::FORMAT,
        render_target: false,
        label: "mtsdf-lane-coverage-atlas",
    });
    let mut atlas = GlyphAtlas::new(ATLAS, ATLAS, texture);
    let AtlasAlloc::Placed(uv) = atlas.alloc_in_page(0, &bitmap) else {
        panic!("one bitmap fits an empty page");
    };
    gpu.write_texture(texture, 0, 0, ATLAS, ATLAS, atlas.pixels());

    let s = at_px_per_em / raster_px_per_em;
    GlyphRunDraw {
        glyphs: vec![GlyphInstanceData {
            rect: Rect {
                x: origin[0] + bitmap.left * s,
                y: origin[1] - bitmap.top * s,
                w: bitmap.width as f32 * s,
                h: bitmap.height as f32 * s,
            },
            uv,
        }],
        atlas: texture,
        color: Rgba::new(1.0, 1.0, 1.0, 1.0),
        lane: GlyphLane::CoverageA8,
    }
}

/// Render `prims` on a black surface and return the coverage each pixel ended up
/// with — the run is opaque white, so the blue channel *is* the alpha the lane
/// resolved.
fn coverage_of(
    gpu: &mut HeadlessRaster,
    build: impl FnOnce(&mut HeadlessRaster) -> Vec<Primitive>,
) -> Vec<f32> {
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let prims = build(gpu);
    let mut r = Renderer::new(gpu, format);
    r.upload(gpu, &prims);
    r.submit(gpu, surface, [0.0, 0.0, 0.0, 1.0], [W as f32, H as f32]);
    gpu.read_pixels_bgra8(surface)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[0] as f32 / 255.0)
        .collect()
}

/// Total ink: the summed coverage over the whole surface, in pixels.
fn ink(cov: &[f32]) -> f32 {
    cov.iter().sum()
}

/// How many pixels landed on an edge rather than fully in or fully out — the
/// width of the antialiasing band, counted in pixels.
fn soft_pixels(cov: &[f32]) -> usize {
    cov.iter().filter(|c| **c > 0.05 && **c < 0.95).count()
}

// ---------------------------------------------------------------------------
// 1. The lane is its own pipeline
// ---------------------------------------------------------------------------

/// An MTSDF run draws through the scalable-text pipeline, not the coverage one.
/// The lane is a property of the run's *content* — a field atlas cannot be
/// decoded by the coverage fragment — so it selects the pipeline at ingest, and
/// the introspection surface names it.
#[test]
fn an_mtsdf_run_draws_through_the_scalable_text_pipeline() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let run = mtsdf_run(&mut gpu, BUCKET, [24.0, 120.0]);
    let mut r = Renderer::new(&mut gpu, format);
    r.upload(&mut gpu, &[Primitive::GlyphRun(run)]);

    let batches = r.inspect_batches();
    assert_eq!(batches.len(), 1, "one run is one batch");
    let b = batches.get(viso_render::BatchId(0)).expect("batch 0");
    assert_eq!(b.pipeline, BatchPipeline::MtsdfRun);
    assert_eq!(b.range, (0, 1), "one glyph instance, from the shared pool");
    assert!(
        b.bind_group.is_some(),
        "the field atlas is bound like any other sampled resource"
    );
}

/// The two lanes do not merge. They share the instance ABI — a paragraph mixing
/// them fills one buffer at one stride — but a lane switch is a pipeline switch,
/// so the planner keeps them as two batches in paint order.
#[test]
fn the_two_text_lanes_are_separate_batches_over_one_buffer() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let a8 = coverage_run(&mut gpu, BUCKET, BUCKET, [24.0, 60.0]);
    let field = mtsdf_run(&mut gpu, BUCKET, [120.0, 60.0]);
    let mut r = Renderer::new(&mut gpu, format);
    r.upload(
        &mut gpu,
        &[Primitive::GlyphRun(a8), Primitive::GlyphRun(field)],
    );

    let batches = r.inspect_batches();
    assert_eq!(batches.len(), 2, "a lane switch breaks the batch");
    let kinds: Vec<BatchPipeline> = batches.batches.iter().map(|b| b.pipeline).collect();
    assert_eq!(
        kinds,
        vec![BatchPipeline::GlyphRun, BatchPipeline::MtsdfRun]
    );
    assert_ne!(
        batches.batches[0].pipeline_id, batches.batches[1].pipeline_id,
        "two decodes are two pipelines"
    );
    // One buffer, consecutive ranges: the second batch continues where the first
    // stopped, which is what sharing the glyph instance pool means.
    assert_eq!(batches.batches[0].range, (0, 1));
    assert_eq!(batches.batches[1].range, (1, 1));
    assert_eq!(r.frame_stats().glyph_instances, 2);
}

// ---------------------------------------------------------------------------
// 2. Fidelity at the bucket, sharpness above it
// ---------------------------------------------------------------------------

/// At the scale it was generated for, the field paints the glyph the exact
/// rasterizer paints. Scalability is only worth having if the shape is right
/// first, so this is the floor: same outline, same ink, within the difference
/// two antialiasing models can legitimately disagree by along one edge.
#[test]
fn a_field_reproduces_the_coverage_it_was_generated_from() {
    let origin = [40.0, 120.0];
    let mut gpu = HeadlessRaster::new();
    let exact = coverage_of(&mut gpu, |g| {
        vec![Primitive::GlyphRun(coverage_run(g, BUCKET, BUCKET, origin))]
    });
    let mut gpu = HeadlessRaster::new();
    let field = coverage_of(&mut gpu, |g| {
        vec![Primitive::GlyphRun(mtsdf_run(g, BUCKET, origin))]
    });

    let (a, b) = (ink(&exact), ink(&field));
    assert!(a > 50.0, "the exact raster inked the glyph ({a} px)");
    assert!(b > 50.0, "the field inked the glyph ({b} px)");
    assert!(
        (a - b).abs() / a < 0.12,
        "field ink {b} px must match exact coverage {a} px within tolerance"
    );
}

/// Four times its bucket, the field's edge is still about a pixel wide. The same
/// glyph magnified from an A8 bitmap smears its edge over the whole interpolated
/// span — the exact failure this lane exists to avoid — so the field's
/// antialiasing band must be decisively narrower, not merely different.
#[test]
fn magnifying_a_field_keeps_the_edge_crisp() {
    let origin = [24.0, 160.0];
    let big = BUCKET * 4.0;

    let mut gpu = HeadlessRaster::new();
    let stretched = coverage_of(&mut gpu, |g| {
        vec![Primitive::GlyphRun(coverage_run(g, BUCKET, big, origin))]
    });
    let mut gpu = HeadlessRaster::new();
    let field = coverage_of(&mut gpu, |g| {
        vec![Primitive::GlyphRun(mtsdf_run(g, big, origin))]
    });

    // Both drew the same glyph at the same size, so both cover about the same
    // area; only the edge differs.
    let (a, b) = (ink(&stretched), ink(&field));
    assert!(
        (a - b).abs() / a < 0.15,
        "same glyph, same size: {a} vs {b}"
    );

    let (soft_a8, soft_field) = (soft_pixels(&stretched), soft_pixels(&field));
    assert!(
        soft_field * 2 < soft_a8,
        "the field's edge band ({soft_field} px) must be far narrower than a \
         stretched bitmap's ({soft_a8} px)"
    );
    assert!(
        field.iter().any(|c| *c > 0.99),
        "the interior is fully inked, not a soft blob"
    );
}

/// A field carries one distance range for the whole atlas, and the lane converts
/// it to pixels from the quad's own scale. Drawing the same field at two sizes
/// must therefore keep the edge band about one pixel wide at both — a range that
/// failed to track the scale would sharpen or soften with it.
#[test]
fn the_edge_band_tracks_the_quad_scale() {
    let origin = [24.0, 160.0];
    let mut gpu = HeadlessRaster::new();
    let one_x = coverage_of(&mut gpu, |g| {
        vec![Primitive::GlyphRun(mtsdf_run(g, BUCKET, origin))]
    });
    let mut gpu = HeadlessRaster::new();
    let four_x = coverage_of(&mut gpu, |g| {
        vec![Primitive::GlyphRun(mtsdf_run(g, BUCKET * 4.0, origin))]
    });

    // The band is a 1-pixel-wide outline of the glyph, so its length grows with
    // the glyph's perimeter — linearly in scale, not with its area.
    let (n1, n4) = (soft_pixels(&one_x) as f32, soft_pixels(&four_x) as f32);
    assert!(n1 > 8.0 && n4 > 8.0, "both drew an antialiased edge");
    assert!(
        (n4 / n1) < 8.0,
        "a 4x quad grew its edge band {}x — the range is not tracking the scale",
        n4 / n1
    );
}
