//! Advanced blend isolation contract (§14.6).
//!
//! The property this file defends is the one §14.6 states outright: a blend the
//! fixed-function stage cannot express must not pollute the common `SrcOver`
//! pipeline. Two halves, both asserted here:
//!
//! 1. **`SrcOver` pays nothing.** The default blend is the fixed-function state
//!    the surface pipeline already runs, so a `SrcOver` layer at full opacity
//!    with no other effect stays inline: no offscreen pass, no destination
//!    capture, no second pipeline.
//! 2. **Everything else isolates.** A non-`SrcOver` layer is rendered as a unit
//!    into a pooled target, a *bounded* snapshot of what is behind it is captured
//!    (never the whole surface), and one draw through the dedicated advanced-blend
//!    pipeline samples both and writes the result. That draw costs no extra
//!    render-target pass beyond the layer's own offscreen and the capture.
//!
//! The blend math is checked against an independent CPU oracle written straight
//! from the W3C compositing definitions rather than from the shader — a mirror of
//! the implementation would prove only that it matches itself.
//!
//! Everything is asserted through the public surface: `Primitive::Blend` markers
//! in, `FrameStats` and read-back pixels out.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Blend, BlendRealization, Border, ColorEffect, FrameStats, LayerClip, Primitive, Quad, Rect,
    Renderer, Rgba, plan_blend,
};

const W: u32 = 64;
const H: u32 = 64;

/// Per-channel tolerance (in 0..=255) for a pixel comparison: absorbs the unorm
/// round-trip through two pooled targets, far too small to hide a wrong blend
/// function or a destination read that landed at the wrong UV.
const TOL: i32 = 3;

/// Every mode that isolates, in ABI order — the separable-artistic block followed
/// by the four non-separable HSL modes.
const ADVANCED: [Blend; 15] = [
    Blend::Multiply,
    Blend::Screen,
    Blend::Overlay,
    Blend::Darken,
    Blend::Lighten,
    Blend::ColorDodge,
    Blend::ColorBurn,
    Blend::HardLight,
    Blend::SoftLight,
    Blend::Difference,
    Blend::Exclusion,
    Blend::Hue,
    Blend::Saturation,
    Blend::Color,
    Blend::Luminosity,
];

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
/// or capture it gets is the blend's doing and nothing else's.
fn layer(clip: Rect) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

/// Opaque white: the destination fixed point for `Multiply`/`Screen`, and a
/// neutral filler where the color does not matter.
fn white() -> Rgba {
    Rgba::new(1.0, 1.0, 1.0, 1.0)
}

fn quad(r: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color,
        radius: 0.0,
        border: Border::NONE,
    })
}

fn frame(r: &mut Renderer, gpu: &mut HeadlessRaster, primitives: &[Primitive]) -> FrameStats {
    r.upload(gpu, primitives);
    r.frame_stats()
}

// ---------------------------------------------------------------------------
// The oracle: W3C compositing, written from the definitions
// ---------------------------------------------------------------------------

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        for v in &mut c {
            *v = l + (*v - l) * l / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + (*v - l) * (1.0 - l) / (x - l);
        }
    }
    c
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mn = c[0].min(c[1]).min(c[2]);
    let mx = c[0].max(c[1]).max(c[2]);
    if mx > mn {
        [
            (c[0] - mn) * s / (mx - mn),
            (c[1] - mn) * s / (mx - mn),
            (c[2] - mn) * s / (mx - mn),
        ]
    } else {
        [0.0; 3]
    }
}

/// The per-channel separable blend function `B(cb, cs)`, straight from the spec.
fn separable(mode: Blend, cb: f32, cs: f32) -> f32 {
    let screen = |b: f32, s: f32| b + s - b * s;
    let hard_light = |b: f32, s: f32| {
        if s <= 0.5 {
            b * 2.0 * s
        } else {
            screen(b, 2.0 * s - 1.0)
        }
    };
    match mode {
        Blend::Multiply => cb * cs,
        Blend::Screen => screen(cb, cs),
        // Overlay is hard-light with the operands swapped.
        Blend::Overlay => hard_light(cs, cb),
        Blend::Darken => cb.min(cs),
        Blend::Lighten => cb.max(cs),
        Blend::ColorDodge => {
            if cb <= 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        Blend::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        Blend::HardLight => hard_light(cb, cs),
        Blend::SoftLight => {
            let d = if cb <= 0.25 {
                ((16.0 * cb - 12.0) * cb + 4.0) * cb
            } else {
                cb.sqrt()
            };
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        Blend::Difference => (cb - cs).abs(),
        Blend::Exclusion => cb + cs - 2.0 * cb * cs,
        other => panic!("{other:?} is not a separable blend"),
    }
}

/// `B(cb, cs)` for the four non-separable modes.
fn nonseparable(mode: Blend, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    match mode {
        Blend::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        Blend::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        Blend::Color => set_lum(cs, lum(cb)),
        Blend::Luminosity => set_lum(cb, lum(cs)),
        other => panic!("{other:?} is not a non-separable blend"),
    }
}

/// Composite an opaque source over an opaque destination with `mode`. With both
/// alphas at 1 the general formula collapses to `B(cb, cs)`, which is exactly the
/// case the pixel tests set up — deliberately, so a wrong blend function cannot
/// hide behind alpha weighting.
fn oracle(mode: Blend, dst: [f32; 3], src: [f32; 3]) -> [f32; 3] {
    if matches!(
        mode,
        Blend::Hue | Blend::Saturation | Blend::Color | Blend::Luminosity
    ) {
        nonseparable(mode, dst, src)
    } else {
        [
            separable(mode, dst[0], src[0]),
            separable(mode, dst[1], src[1]),
            separable(mode, dst[2], src[2]),
        ]
    }
}

/// Paint an opaque `dst` quad over the whole surface, then an opaque `src` quad
/// inside a layer blending with `mode`, and read back the center pixel.
fn blended(mode: Blend, dst: [f32; 3], src: [f32; 3]) -> [f32; 3] {
    let (mut gpu, surface, mut r) = renderer();
    let content = rect(16.0, 16.0, 32.0, 32.0);
    let prims = vec![
        quad(
            rect(0.0, 0.0, W as f32, H as f32),
            Rgba {
                r: dst[0],
                g: dst[1],
                b: dst[2],
                a: 1.0,
            },
        ),
        layer(content),
        Primitive::Blend(mode),
        quad(
            content,
            Rgba {
                r: src[0],
                g: src[1],
                b: src[2],
                a: 1.0,
            },
        ),
        Primitive::LayerEnd,
    ];
    r.upload(&mut gpu, &prims);
    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    let px = gpu.read_pixels_bgra8(surface);
    let i = ((H / 2) * W + W / 2) as usize * 4;
    [
        px[i + 2] as f32 / 255.0,
        px[i + 1] as f32 / 255.0,
        px[i] as f32 / 255.0,
    ]
}

fn assert_rgb_close(actual: [f32; 3], expect: [f32; 3], what: &str) {
    for c in 0..3 {
        let a = (actual[c] * 255.0).round() as i32;
        let e = (expect[c] * 255.0).round() as i32;
        assert!(
            (a - e).abs() <= TOL,
            "{what}: channel {c} is {a}, expected {e} (±{TOL}); got {actual:?} want {expect:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 1. `SrcOver` stays on the common pipeline
// ---------------------------------------------------------------------------

/// The load-bearing negative: the default blend must cost nothing. A `SrcOver`
/// layer at full opacity with no other effect draws inline — one surface pass,
/// no offscreen, no capture, no isolation.
#[test]
fn src_over_never_isolates() {
    let (mut gpu, _s, mut r) = renderer();
    let content = rect(8.0, 8.0, 16.0, 16.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            layer(content),
            Primitive::Blend(Blend::SrcOver),
            quad(content, white()),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.blend_isolations, 0);
    assert_eq!(
        stats.offscreen_passes, 0,
        "an explicit SrcOver stays inline"
    );
    assert_eq!(stats.backdrop_captures, 0, "nothing reads the destination");
    assert_eq!(stats.render_passes, 1);
}

/// An explicit `Blend::SrcOver` marker and no marker at all must produce byte-for
/// byte the same frame — otherwise "the default is free" is only true when the
/// author stays silent.
#[test]
fn an_explicit_src_over_marker_costs_exactly_nothing() {
    let content = rect(8.0, 8.0, 16.0, 16.0);
    let bare = {
        let (mut gpu, _s, mut r) = renderer();
        frame(
            &mut r,
            &mut gpu,
            &[layer(content), quad(content, white()), Primitive::LayerEnd],
        )
    };
    let marked = {
        let (mut gpu, _s, mut r) = renderer();
        frame(
            &mut r,
            &mut gpu,
            &[
                layer(content),
                Primitive::Blend(Blend::SrcOver),
                quad(content, white()),
                Primitive::LayerEnd,
            ],
        )
    };
    assert_eq!(marked.draw_calls, bare.draw_calls);
    assert_eq!(marked.render_passes, bare.render_passes);
    assert_eq!(marked.offscreen_passes, bare.offscreen_passes);
    assert_eq!(marked.instances, bare.instances);
}

/// The Porter-Duff modes classify as fixed-function even though the RHI exposes
/// only the `SrcOver` state today: the classification is the *contract*, and
/// `plan_blend` is where a backend that gains more blend states plugs in.
#[test]
fn the_realization_ladder_is_classified_not_guessed() {
    assert!(
        !plan_blend(Blend::SrcOver).cost.reads_destination(),
        "the common path never reads the destination"
    );
    assert_eq!(
        Blend::SrcOver.realization(),
        BlendRealization::FixedFunction
    );
    assert_eq!(
        Blend::Multiply.realization(),
        BlendRealization::DestinationRead
    );
    assert_eq!(Blend::Hue.realization(), BlendRealization::Isolation);
    for mode in ADVANCED {
        let plan = plan_blend(mode);
        assert!(
            plan.needs_offscreen() || plan.reads_destination(),
            "{mode:?} must be recorded as needing isolation or a destination read"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Every advanced mode isolates, through the dedicated pipeline
// ---------------------------------------------------------------------------

/// Each advanced mode forces its layer offscreen, takes exactly one bounded
/// destination snapshot, and adds no third render-target pass: the composite that
/// evaluates the blend is the draw the layer was going to make anyway.
#[test]
fn every_advanced_mode_isolates_with_one_bounded_capture() {
    for mode in ADVANCED {
        let (mut gpu, _s, mut r) = renderer();
        let content = rect(16.0, 16.0, 24.0, 24.0);
        let stats = frame(
            &mut r,
            &mut gpu,
            &[
                quad(rect(0.0, 0.0, W as f32, H as f32), white()),
                layer(content),
                Primitive::Blend(mode),
                quad(content, white()),
                Primitive::LayerEnd,
            ],
        );
        assert_eq!(stats.blend_isolations, 1, "{mode:?} must isolate");
        assert_eq!(stats.offscreen_passes, 1, "{mode:?}");
        assert_eq!(stats.backdrop_captures, 1, "{mode:?}");
        assert_eq!(
            stats.blur_passes, 0,
            "{mode:?} captures at sigma 0 — a blend snapshot is sharp"
        );
        assert_eq!(
            stats.color_transform_passes, 0,
            "{mode:?} has no color chain to realize"
        );
        // Layer offscreen + capture + surface, and nothing more: the blend draw
        // rides the composite.
        assert_eq!(stats.render_passes, 3, "{mode:?}");
    }
}

/// The capture is bounded by the layer's rect, not by the surface. A small
/// blended layer must not re-render the whole frame into a full-surface target —
/// that is the cost blowup §14.6 exists to prevent.
#[test]
fn the_destination_snapshot_is_bounded_by_the_layer() {
    let (mut gpu, _s, mut r) = renderer();
    let content = rect(20.0, 20.0, 8.0, 8.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            quad(rect(0.0, 0.0, W as f32, H as f32), white()),
            layer(content),
            Primitive::Blend(Blend::Multiply),
            quad(content, white()),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.backdrop_captures, 1);
    let surface_pixels = (W * H) as usize;
    assert!(
        stats.backdrop_capture_pixels * 4 < surface_pixels,
        "an 8x8 layer captured {} pixels of a {surface_pixels}-pixel surface",
        stats.backdrop_capture_pixels
    );
}

/// N blended layers over disjoint regions each capture their own tight snapshot:
/// the total captured area stays proportional to the layers, and no single
/// capture grows to the surface.
#[test]
fn disjoint_blended_layers_do_not_each_capture_the_surface() {
    let (mut gpu, _s, mut r) = renderer();
    let spots = [
        rect(2.0, 2.0, 8.0, 8.0),
        rect(50.0, 2.0, 8.0, 8.0),
        rect(2.0, 50.0, 8.0, 8.0),
        rect(50.0, 50.0, 8.0, 8.0),
    ];
    let mut prims = vec![quad(rect(0.0, 0.0, W as f32, H as f32), white())];
    for spot in spots {
        prims.push(layer(spot));
        prims.push(Primitive::Blend(Blend::Screen));
        prims.push(quad(spot, white()));
        prims.push(Primitive::LayerEnd);
    }
    let stats = frame(&mut r, &mut gpu, &prims);
    assert_eq!(stats.blend_isolations, 4);
    assert!(
        stats.backdrop_capture_pixels < (W * H) as usize,
        "four 8x8 layers captured {} pixels, more than the whole {}-pixel surface",
        stats.backdrop_capture_pixels,
        W * H
    );
}

// ---------------------------------------------------------------------------
// 3. The pixels match an independent oracle
// ---------------------------------------------------------------------------

/// Every advanced mode, on an opaque source over an opaque destination, must
/// match `B(cb, cs)` computed from the W3C definitions. This is the test that
/// catches a swapped `Overlay`, a `ColorDodge` divide-by-zero, or a destination
/// sampled at the wrong UV (which would read black and fail every mode at once).
#[test]
fn advanced_blend_pixels_match_the_oracle() {
    let dst = [0.8, 0.4, 0.2];
    let src = [0.3, 0.6, 0.9];
    for mode in ADVANCED {
        let actual = blended(mode, dst, src);
        let expect = oracle(mode, dst, src);
        assert_rgb_close(actual, expect, &format!("{mode:?}"));
    }
}

/// `Multiply` against white is the identity, and against black is black: two
/// fixed points that pin the destination read to the *right* destination rather
/// than to an arbitrary texture that happens to be the same size.
#[test]
fn multiply_against_white_and_black_hits_its_fixed_points() {
    let src = [0.25, 0.5, 0.75];
    assert_rgb_close(
        blended(Blend::Multiply, [1.0; 3], src),
        src,
        "multiply over white",
    );
    assert_rgb_close(
        blended(Blend::Multiply, [0.0; 3], src),
        [0.0; 3],
        "multiply over black",
    );
}

/// A layer opacity on a blended layer folds into the source *before* the blend,
/// not into the result after it. `Difference` makes the distinction visible: with
/// alpha 0.5 the general formula gives `0.5*B(cb,cs) + 0.5*cb`, which is not
/// `B(cb, 0.5*cs)`.
#[test]
fn layer_opacity_enters_the_blend_as_source_alpha() {
    let (mut gpu, surface, mut r) = renderer();
    let content = rect(16.0, 16.0, 32.0, 32.0);
    let dst = [0.8, 0.8, 0.8];
    let src = [0.25, 0.5, 0.75];
    let prims = vec![
        quad(
            rect(0.0, 0.0, W as f32, H as f32),
            Rgba {
                r: dst[0],
                g: dst[1],
                b: dst[2],
                a: 1.0,
            },
        ),
        Primitive::Layer(LayerClip {
            clip: content,
            opacity: 0.5,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }),
        Primitive::Blend(Blend::Difference),
        quad(
            content,
            Rgba {
                r: src[0],
                g: src[1],
                b: src[2],
                a: 1.0,
            },
        ),
        Primitive::LayerEnd,
    ];
    r.upload(&mut gpu, &prims);
    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    let px = gpu.read_pixels_bgra8(surface);
    let i = ((H / 2) * W + W / 2) as usize * 4;
    let actual = [
        px[i + 2] as f32 / 255.0,
        px[i + 1] as f32 / 255.0,
        px[i] as f32 / 255.0,
    ];
    // co = as*(1-ab)*cs + as*ab*B(cb,cs) + (1-as)*ab*cb, with ab = 1, as = 0.5.
    let expect = [
        0.5 * (dst[0] - src[0]).abs() + 0.5 * dst[0],
        0.5 * (dst[1] - src[1]).abs() + 0.5 * dst[1],
        0.5 * (dst[2] - src[2]).abs() + 0.5 * dst[2],
    ];
    assert_rgb_close(actual, expect, "half-opaque difference");
}

// ---------------------------------------------------------------------------
// 4. Markers, chains, and steady state
// ---------------------------------------------------------------------------

/// The last blend marker in a layer's run wins, and a blend marker sitting
/// between two color effects truncates neither chain — both kinds are
/// annotations on the same layer and are read independently.
#[test]
fn markers_interleave_and_the_last_blend_wins() {
    let (mut gpu, _s, mut r) = renderer();
    let content = rect(16.0, 16.0, 24.0, 24.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            quad(rect(0.0, 0.0, W as f32, H as f32), white()),
            layer(content),
            Primitive::ColorEffect(ColorEffect::Brightness(1.2)),
            Primitive::Blend(Blend::Screen),
            Primitive::ColorEffect(ColorEffect::Contrast(1.1)),
            Primitive::Blend(Blend::Multiply),
            quad(content, white()),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.blend_isolations, 1, "one layer, one isolation");
    assert_eq!(
        stats.color_effect_ops, 1,
        "the blend marker between them must not split the color chain"
    );
    // A blended layer's composite carries no color matrix, so the single fused op
    // becomes its own render-target pass — the documented extra cost.
    assert_eq!(stats.color_transform_passes, 1);
}

/// A blend marker outside a layer's marker run is ignored rather than an error:
/// the same forgiving rule `ColorEffect` follows, so a stray marker cannot
/// silently reblend unrelated content.
#[test]
fn a_stray_blend_marker_is_ignored() {
    let (mut gpu, _s, mut r) = renderer();
    let content = rect(8.0, 8.0, 16.0, 16.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            quad(content, white()),
            Primitive::Blend(Blend::Multiply),
            quad(content, white()),
        ],
    );
    assert_eq!(stats.blend_isolations, 0);
    assert_eq!(stats.offscreen_passes, 0);
    assert_eq!(stats.backdrop_captures, 0);
}

/// A blended layer nested inside another offscreen layer still isolates, but
/// cannot capture a destination — only layers drawing straight into the surface
/// pass can. It composites `SrcOver` instead of silently reading the wrong
/// pixels, which is the honest degradation.
#[test]
fn a_nested_blended_layer_isolates_without_capturing() {
    let (mut gpu, _s, mut r) = renderer();
    let outer = rect(8.0, 8.0, 40.0, 40.0);
    let inner = rect(16.0, 16.0, 16.0, 16.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            Primitive::Layer(LayerClip {
                clip: outer,
                opacity: 0.5,
                blur_sigma: 0.0,
                backdrop_sigma: 0.0,
            }),
            layer(inner),
            Primitive::Blend(Blend::Multiply),
            quad(inner, white()),
            Primitive::LayerEnd,
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.offscreen_passes, 2, "both layers isolate");
    assert_eq!(
        stats.backdrop_captures, 0,
        "a nested layer has no surface destination to snapshot"
    );
    assert_eq!(stats.blend_isolations, 0, "no blend was realized");
}

/// Two identical frames must produce identical work: the layer target, the
/// capture target, and the two-texture bind group all come from caches keyed by
/// content, so a steady blended UI recompiles and reallocates nothing.
#[test]
fn a_repeated_blended_frame_is_steady() {
    let (mut gpu, _s, mut r) = renderer();
    let content = rect(16.0, 16.0, 24.0, 24.0);
    let prims = vec![
        quad(rect(0.0, 0.0, W as f32, H as f32), white()),
        layer(content),
        Primitive::Blend(Blend::Overlay),
        quad(content, white()),
        Primitive::LayerEnd,
    ];
    let first = frame(&mut r, &mut gpu, &prims);
    let second = frame(&mut r, &mut gpu, &prims);
    assert_eq!(second.blend_isolations, first.blend_isolations);
    assert_eq!(second.draw_calls, first.draw_calls);
    assert_eq!(second.render_passes, first.render_passes);
    assert_eq!(second.backdrop_captures, first.backdrop_captures);
    assert_eq!(second.transient_targets, first.transient_targets);
    assert_eq!(
        second.transient_target_allocations, 0,
        "the second frame reuses every pooled target"
    );
    assert_eq!(
        second.shader_pipeline_creations, first.shader_pipeline_creations,
        "the advanced-blend pipeline is prewarmed at construction, never compiled on demand"
    );
    assert_eq!(
        second.uploaded_ranges, 0,
        "identical instance data re-uploads nothing"
    );
}
