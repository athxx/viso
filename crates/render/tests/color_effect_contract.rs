//! Color effect fusion contract (§17.3).
//!
//! The property this file defends is a cost one, and it is easy to lose: a UI
//! that asks for `brightness → contrast → saturation` is asking for *one*
//! per-pixel computation, not three. Each of the nine color effects is an affine
//! map on straight linear RGBA, so a run of them is a single 4x5 matrix, and a
//! layer that already composites through a texture can carry that matrix on the
//! composite draw it was going to make anyway. The bill for an N-effect
//! mergeable chain is therefore **zero** extra render-target passes.
//!
//! Only a stage the matrix form cannot express — here `ColorEffect::Gamma`, the
//! stand-in for a custom filter — splits a chain, and it costs exactly one pass
//! per split, not one per effect.
//!
//! Everything is asserted through the public surface: `Primitive::ColorEffect`
//! markers in, `FrameStats` and read-back pixels out. `fuse`/`ColorMatrix` are
//! public too, so the fused matrix can be written out independently and compared
//! against what the renderer actually rasterizes.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Border, ColorEffect, ColorMatrix, ColorOp, FrameStats, LayerClip, Primitive, Quad, Rect,
    Renderer, Rgba, fuse,
};

const W: u32 = 64;
const H: u32 = 64;

/// Per-channel tolerance (in 0..=255) for a pixel comparison: enough to absorb
/// the unorm round-trip through an offscreen target, far too small to hide a
/// missing or doubly-applied effect.
const TOL: i32 = 2;

fn renderer() -> (HeadlessRaster, SurfaceId, Renderer) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format: TextureFormat = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([W as f32, H as f32]);
    (gpu, surface, r)
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

/// The layer under test: clip only, no blur, no backdrop — so any offscreen pass
/// it gets is the color chain's doing and nothing else's.
fn layer(clip: Rect) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

fn quad(clip: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect: clip,
        color,
        radius: 0.0,
        border: Border::NONE,
    })
}

fn frame(r: &mut Renderer, gpu: &mut HeadlessRaster, primitives: &[Primitive]) -> FrameStats {
    r.upload(gpu, primitives);
    r.frame_stats()
}

/// Paint `color` inside a layer carrying `effects`, present it over black, and
/// return the center pixel as straight linear RGBA.
fn recolored(color: Rgba, effects: &[ColorEffect]) -> [f32; 4] {
    let (mut gpu, surface, mut r) = renderer();
    let content = rect(16.0, 16.0, 32.0, 32.0);
    let mut prims = vec![layer(content)];
    prims.extend(effects.iter().copied().map(Primitive::ColorEffect));
    prims.push(quad(content, color));
    prims.push(Primitive::LayerEnd);

    r.upload(&mut gpu, &prims);
    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );

    let px = gpu.read_pixels_bgra8(surface);
    let i = ((H / 2) * W + W / 2) as usize * 4;
    // BGRA8 top-left, opaque over an opaque clear: straight == premultiplied.
    [
        px[i + 2] as f32 / 255.0,
        px[i + 1] as f32 / 255.0,
        px[i] as f32 / 255.0,
        px[i + 3] as f32 / 255.0,
    ]
}

fn assert_pixel_close(actual: [f32; 4], expect: [f32; 4], what: &str) {
    for c in 0..4 {
        let a = (actual[c] * 255.0).round() as i32;
        let e = (expect[c] * 255.0).round() as i32;
        assert!(
            (a - e).abs() <= TOL,
            "{what}: channel {c} is {a}, expected {e} (±{TOL}); got {actual:?} want {expect:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 1. A mergeable run is one op and no extra passes
// ---------------------------------------------------------------------------

/// The headline case from §17.3: `Brightness → Contrast → Saturation` must not
/// become three render-target passes. It fuses into one matrix, that matrix
/// rides the composite the layer already draws, and the frame costs the layer's
/// own offscreen pass plus the surface — two passes total.
#[test]
fn brightness_contrast_saturation_is_one_op_and_no_extra_pass() {
    let (mut gpu, _surface, mut r) = renderer();
    let clip = rect(8.0, 8.0, 32.0, 32.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            layer(clip),
            Primitive::ColorEffect(ColorEffect::Brightness(1.2)),
            Primitive::ColorEffect(ColorEffect::Contrast(0.8)),
            Primitive::ColorEffect(ColorEffect::Saturation(0.5)),
            quad(clip, Rgba::new(0.4, 0.5, 0.6, 1.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.color_effect_ops, 1, "three effects, one fused op");
    assert_eq!(
        stats.color_transform_passes, 0,
        "the fused op rides the composite: no color pass of its own"
    );
    assert_eq!(stats.offscreen_passes, 1);
    assert_eq!(stats.render_passes, 2, "the layer and the surface");
}

/// What splits a chain is expressibility, not length: all nine color effects at
/// once still fuse to one op and still cost nothing extra.
#[test]
fn every_color_effect_is_mergeable_with_every_other() {
    let (mut gpu, _surface, mut r) = renderer();
    let clip = rect(8.0, 8.0, 32.0, 32.0);
    let chain = [
        ColorEffect::Brightness(1.1),
        ColorEffect::Contrast(1.2),
        ColorEffect::Saturation(0.9),
        ColorEffect::HueRotate(0.6),
        ColorEffect::Grayscale(0.2),
        ColorEffect::Sepia(0.3),
        ColorEffect::Invert(0.15),
        ColorEffect::ColorMatrix(ColorMatrix::brightness(0.9)),
        ColorEffect::Tint {
            color: [0.9, 0.3, 0.1],
            amount: 0.25,
        },
    ];
    let mut prims = vec![layer(clip)];
    prims.extend(chain.iter().copied().map(Primitive::ColorEffect));
    prims.push(quad(clip, Rgba::new(0.4, 0.5, 0.6, 1.0)));
    prims.push(Primitive::LayerEnd);

    let stats = frame(&mut r, &mut gpu, &prims);
    assert_eq!(stats.color_effect_ops, 1, "nine effects, one fused op");
    assert_eq!(stats.color_transform_passes, 0);
    assert_eq!(stats.render_passes, 2);

    // And the public fuser agrees with the renderer's own count.
    let mut ops = Vec::new();
    fuse(chain, &mut ops);
    assert_eq!(ops.len(), 1);
}

// ---------------------------------------------------------------------------
// 2. Only a non-expressible stage earns a pass
// ---------------------------------------------------------------------------

/// A gamma sits between two affine runs and cannot be folded into either, so it
/// splits the chain — once. Five effects around one gamma are two ops and one
/// extra render-target pass, not five.
#[test]
fn a_non_expressible_stage_costs_exactly_one_extra_pass() {
    let (mut gpu, _surface, mut r) = renderer();
    let clip = rect(8.0, 8.0, 32.0, 32.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            layer(clip),
            Primitive::ColorEffect(ColorEffect::Brightness(1.2)),
            Primitive::ColorEffect(ColorEffect::Contrast(0.8)),
            Primitive::ColorEffect(ColorEffect::Gamma(2.2)),
            Primitive::ColorEffect(ColorEffect::Saturation(0.5)),
            Primitive::ColorEffect(ColorEffect::Invert(0.2)),
            quad(clip, Rgba::new(0.4, 0.5, 0.6, 1.0)),
            Primitive::LayerEnd,
        ],
    );

    assert_eq!(stats.color_effect_ops, 2, "the gamma splits the run once");
    assert_eq!(
        stats.color_transform_passes, 1,
        "the leading op gets a pass; the trailing one still rides the composite"
    );
    assert_eq!(stats.offscreen_passes, 1);
    assert_eq!(
        stats.render_passes, 3,
        "the layer, the one unfused op, and the surface"
    );
}

/// Two gammas in a row multiply instead of splitting: a non-expressible stage is
/// only a barrier to the *affine* math, never to another of its own kind.
#[test]
fn consecutive_non_expressible_stages_still_share_one_op() {
    let (mut gpu, _surface, mut r) = renderer();
    let clip = rect(8.0, 8.0, 32.0, 32.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            layer(clip),
            Primitive::ColorEffect(ColorEffect::Gamma(1.5)),
            Primitive::ColorEffect(ColorEffect::Gamma(2.0)),
            quad(clip, Rgba::new(0.4, 0.5, 0.6, 1.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.color_effect_ops, 1);
    assert_eq!(stats.color_transform_passes, 0);
}

/// A chain of neutral parameters computes nothing, so it must not drag the layer
/// offscreen: the clip stays an in-pass scissor and the frame is one pass.
#[test]
fn a_neutral_chain_costs_nothing_at_all() {
    let (mut gpu, _surface, mut r) = renderer();
    let clip = rect(8.0, 8.0, 32.0, 32.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            layer(clip),
            Primitive::ColorEffect(ColorEffect::Brightness(1.0)),
            Primitive::ColorEffect(ColorEffect::Gamma(1.0)),
            Primitive::ColorEffect(ColorEffect::Saturation(1.0)),
            Primitive::ColorEffect(ColorEffect::Grayscale(0.0)),
            quad(clip, Rgba::new(0.4, 0.5, 0.6, 1.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.color_effect_ops, 0);
    assert_eq!(stats.color_transform_passes, 0);
    assert_eq!(
        stats.offscreen_passes, 0,
        "nothing to compute, nothing to build"
    );
    assert_eq!(stats.render_passes, 1, "the surface pass alone");
}

// ---------------------------------------------------------------------------
// 3. Fusing is value-preserving
// ---------------------------------------------------------------------------

/// Cheaper is worthless if it is not the same picture. A fused three-effect
/// chain rasterizes to the same pixel as the single authored `ColorMatrix` built
/// by composing those three matrices by hand.
#[test]
fn a_fused_chain_matches_the_composed_matrix() {
    let color = Rgba::new(0.4, 0.55, 0.7, 1.0);
    let chained = recolored(
        color,
        &[
            ColorEffect::Brightness(1.2),
            ColorEffect::Contrast(0.8),
            ColorEffect::Saturation(0.5),
        ],
    );
    let composed = ColorMatrix::brightness(1.2)
        .then(&ColorMatrix::contrast(0.8))
        .then(&ColorMatrix::saturation(0.5));
    let authored = recolored(color, &[ColorEffect::ColorMatrix(composed)]);

    assert_pixel_close(chained, authored, "fused chain vs composed matrix");

    // And both match the CPU model of one pass over that matrix.
    let expect = ColorOp {
        matrix: composed,
        gamma: 1.0,
    }
    .apply([color.r, color.g, color.b, color.a]);
    assert_pixel_close(chained, expect, "fused chain vs the CPU op");
}

/// The split case is value-preserving too: the pass boundary the gamma forces is
/// an implementation detail, not a different picture. The rasterized result
/// matches applying the two ops in order on the CPU.
#[test]
fn a_split_chain_matches_its_ops_applied_in_order() {
    let color = Rgba::new(0.45, 0.6, 0.35, 1.0);
    let chain = [
        ColorEffect::Brightness(1.1),
        ColorEffect::Gamma(1.8),
        ColorEffect::Saturation(0.4),
    ];
    let actual = recolored(color, &chain);

    let mut ops = Vec::new();
    fuse(chain, &mut ops);
    assert_eq!(ops.len(), 2, "the gamma splits this chain");
    let expect = ops
        .iter()
        .fold([color.r, color.g, color.b, color.a], |p, op| op.apply(p));

    assert_pixel_close(actual, expect, "split chain vs its ops in order");
}

/// The classic single effects land where a reader expects: a full grayscale
/// collapses to the Rec.709 luminance of the source, and a full invert is its
/// negative.
#[test]
fn grayscale_and_invert_land_where_expected() {
    let color = Rgba::new(0.8, 0.4, 0.2, 1.0);
    let luma = 0.213 * color.r + 0.715 * color.g + 0.072 * color.b;
    assert_pixel_close(
        recolored(color, &[ColorEffect::Grayscale(1.0)]),
        [luma, luma, luma, 1.0],
        "full grayscale is luminance",
    );
    assert_pixel_close(
        recolored(color, &[ColorEffect::Invert(1.0)]),
        [1.0 - color.r, 1.0 - color.g, 1.0 - color.b, 1.0],
        "full invert is the negative",
    );
}

/// Layer opacity rides the fused op's alpha row rather than a second tint on the
/// composite, so a translucent recolored layer still costs one offscreen and no
/// extra pass — and still blends at the authored strength.
#[test]
fn layer_opacity_rides_the_fused_op() {
    let (mut gpu, surface, mut r) = renderer();
    let clip = rect(16.0, 16.0, 32.0, 32.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            Primitive::Layer(LayerClip {
                clip,
                opacity: 0.5,
                blur_sigma: 0.0,
                backdrop_sigma: 0.0,
            }),
            Primitive::ColorEffect(ColorEffect::Grayscale(1.0)),
            quad(clip, Rgba::new(1.0, 1.0, 1.0, 1.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.color_effect_ops, 1);
    assert_eq!(stats.color_transform_passes, 0);
    assert_eq!(stats.offscreen_passes, 1);

    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    let px = gpu.read_pixels_bgra8(surface);
    let i = ((H / 2) * W + W / 2) as usize * 4;
    // Grayscale white is white; at half opacity over black it is half white.
    assert_pixel_close(
        [
            px[i + 2] as f32 / 255.0,
            px[i + 1] as f32 / 255.0,
            px[i] as f32 / 255.0,
            px[i + 3] as f32 / 255.0,
        ],
        [0.5, 0.5, 0.5, 1.0],
        "opacity folded into the alpha row",
    );
}

// ---------------------------------------------------------------------------
// 4. Steady state and scope
// ---------------------------------------------------------------------------

/// The scratch targets a split chain ping-pongs on come from the transient pool,
/// so a repeated frame allocates nothing new and replans nothing (§16.3, §17.4).
#[test]
fn a_repeated_frame_allocates_and_recompiles_nothing() {
    let (mut gpu, _surface, mut r) = renderer();
    let clip = rect(8.0, 8.0, 32.0, 32.0);
    let scene = [
        Primitive::Layer(LayerClip {
            clip,
            opacity: 0.75,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }),
        Primitive::ColorEffect(ColorEffect::Saturation(0.4)),
        Primitive::ColorEffect(ColorEffect::Gamma(1.8)),
        Primitive::ColorEffect(ColorEffect::Brightness(1.3)),
        quad(clip, Rgba::new(0.4, 0.5, 0.6, 1.0)),
        Primitive::LayerEnd,
    ];

    let first = frame(&mut r, &mut gpu, &scene);
    let second = frame(&mut r, &mut gpu, &scene);

    assert_eq!(first.color_transform_passes, 1);
    assert_eq!(second.color_transform_passes, 1);
    assert_eq!(second.render_passes, first.render_passes);
    assert_eq!(second.draw_calls, first.draw_calls);
    assert_eq!(
        second.transient_target_allocations, 0,
        "the scratch target is reused, not reallocated"
    );
    assert_eq!(
        second.render_graph_compiles, 0,
        "an identical topology hits the graph cache"
    );
}

/// The marker annotates the layer it follows and nothing else: outside a layer
/// open it is inert, and once the layer's content has started the run is closed.
#[test]
fn markers_bind_only_to_the_layer_they_follow() {
    let (mut gpu, _surface, mut r) = renderer();
    let clip = rect(8.0, 8.0, 32.0, 32.0);

    let loose = frame(
        &mut r,
        &mut gpu,
        &[
            Primitive::ColorEffect(ColorEffect::Invert(1.0)),
            quad(clip, Rgba::new(0.4, 0.5, 0.6, 1.0)),
            Primitive::ColorEffect(ColorEffect::Invert(1.0)),
        ],
    );
    assert_eq!(loose.color_effect_ops, 0, "a marker with no layer is inert");
    assert_eq!(loose.offscreen_passes, 0);
    assert_eq!(loose.render_passes, 1);

    let trailing = frame(
        &mut r,
        &mut gpu,
        &[
            layer(clip),
            Primitive::ColorEffect(ColorEffect::Invert(1.0)),
            quad(clip, Rgba::new(0.4, 0.5, 0.6, 1.0)),
            Primitive::ColorEffect(ColorEffect::Gamma(2.2)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(
        trailing.color_effect_ops, 1,
        "only the leading run belongs to the layer"
    );
    assert_eq!(trailing.color_transform_passes, 0);
}

/// Two sibling layers keep their own chains: effects never leak across a
/// `LayerEnd`, and two mergeable chains are two ops on two composites — still no
/// extra color pass between them.
#[test]
fn sibling_layers_do_not_share_a_chain() {
    let (mut gpu, _surface, mut r) = renderer();
    let a = rect(4.0, 4.0, 20.0, 20.0);
    let b = rect(36.0, 36.0, 20.0, 20.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            layer(a),
            Primitive::ColorEffect(ColorEffect::Grayscale(1.0)),
            quad(a, Rgba::new(0.8, 0.2, 0.2, 1.0)),
            Primitive::LayerEnd,
            layer(b),
            Primitive::ColorEffect(ColorEffect::Invert(1.0)),
            quad(b, Rgba::new(0.2, 0.8, 0.2, 1.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.color_effect_ops, 2, "one op each, not one shared");
    assert_eq!(stats.color_transform_passes, 0);
    assert_eq!(stats.offscreen_passes, 2);
    assert_eq!(stats.render_passes, 3, "two layers and the surface");
}

/// The marker is a small tag on the stream, not a new payload: `ColorEffect` must
/// not be what decides `size_of::<Primitive>()`, or every quad in the frame pays
/// for the color feature.
#[test]
fn the_color_marker_does_not_widen_the_primitive_stream() {
    assert!(
        size_of::<ColorEffect>() <= size_of::<Primitive>() - 8,
        "ColorEffect ({}) must fit in Primitive ({}) beside its tag",
        size_of::<ColorEffect>(),
        size_of::<Primitive>()
    );
}
