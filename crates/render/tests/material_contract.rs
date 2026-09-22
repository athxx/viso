//! Frosted material surface contract (§18).
//!
//! A material surface is the composition every glass UI asks for:
//! `backdrop → blur → saturation/tint → optional grain → rounded mask →
//! border/highlight`. Each stage already exists and is frozen lower down — E2's
//! backdrop capture and sharing, E1's blur ladder and ROI, E2's fused `ColorOp`,
//! E0's analytic rounded rect. The contract this file defends is that composing
//! them costs *nothing new*:
//!
//! 1. **One draw, no new passes or targets.** A frosted panel composites through
//!    the material pipeline, which does the whole chain in one fragment. Against
//!    the same scene expressed as a plain backdrop layer, the pass count, the
//!    capture count, the target allocations and the blur ladder are identical.
//! 2. **Sharing is inherited, not reimplemented.** `N` panels at one sigma over
//!    one background still open one capture and one blur ladder, and get one
//!    composite each — the material surface joins the very same capture group a
//!    backdrop layer would.
//! 3. **The grain is a function of the pixel, never of time.** Two identical
//!    frames read back byte-identical, so a static screen stays static (the E2.5
//!    gate) even with noise turned all the way up.
//! 4. **The mask rounds and the color op tints**, verified by read-back rather
//!    than by counter.
//!
//! Everything goes through the public surface: `Primitive::Frosted` in,
//! `FrameStats` and read-back pixels out.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat};
use viso_render::{
    Border, ColorEffect, ColorOp, Corners, FrameStats, FrostedMaterial, LayerClip, Primitive, Quad,
    Rect, Renderer, Rgba, fuse,
};

const W: u32 = 128;
const H: u32 = 128;

/// The sigma every test blurs at unless it is testing the sigma itself: well
/// above the ladder's minimum, small enough that the padded ROI stays far inside
/// the surface.
const SIGMA: f32 = 4.0;

fn renderer(w: u32, h: u32) -> (HeadlessRaster, SurfaceId, Renderer) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
    let format: TextureFormat = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([w as f32, h as f32]);
    (gpu, surface, r)
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn quad(rect: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect,
        color,
        radius: 0.0,
        border: Border::NONE,
    })
}

/// A saturated full-window background, so every scene has something to capture
/// and a tint is visible in the read-back.
fn background() -> Primitive {
    quad(
        rect(0.0, 0.0, W as f32, H as f32),
        Rgba::new(0.2, 0.5, 0.9, 1.0),
    )
}

/// The single fused op for one matrix-expressible effect — the shape a widget
/// hands a material surface, written through the public `fuse` so the test does
/// not restate E2's matrices.
fn fused(effect: ColorEffect) -> ColorOp {
    let mut ops = Vec::new();
    fuse([effect], &mut ops);
    assert_eq!(ops.len(), 1, "one affine effect fuses to one op");
    ops[0]
}

fn frosted(rect: Rect, sigma: f32) -> FrostedMaterial {
    FrostedMaterial::new(rect, Corners::SHARP, sigma)
}

/// The same scene a material surface expresses, written as a plain backdrop
/// layer instead: the baseline every "no new passes" assertion compares against.
fn backdrop_layer(clip: Rect, sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: sigma,
    })
}

fn frame(r: &mut Renderer, gpu: &mut HeadlessRaster, primitives: &[Primitive]) -> FrameStats {
    r.upload(gpu, primitives);
    r.frame_stats()
}

/// Upload `primitives`, present them over an opaque black clear, and return the
/// raw BGRA8 framebuffer.
fn present(primitives: &[Primitive]) -> Vec<u8> {
    let (mut gpu, surface, mut r) = renderer(W, H);
    r.upload(&mut gpu, primitives);
    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    gpu.read_pixels_bgra8(surface)
}

/// One pixel as straight linear RGBA (opaque over an opaque clear, so
/// premultiplied == straight).
fn pixel(px: &[u8], x: u32, y: u32) -> [f32; 4] {
    let i = (y * W + x) as usize * 4;
    [
        px[i + 2] as f32 / 255.0,
        px[i + 1] as f32 / 255.0,
        px[i] as f32 / 255.0,
        px[i + 3] as f32 / 255.0,
    ]
}

// ---------------------------------------------------------------------------
// 1. The composition is free: no new pass, target, or capture
// ---------------------------------------------------------------------------

/// A frosted surface costs exactly one extra draw over the equivalent backdrop
/// layer, and not one extra render pass, transient target, capture, or blur rung.
/// This is the whole point of fusing the chain into one fragment: the §18 stack
/// is a *pipeline*, not a pass chain.
#[test]
fn a_frosted_surface_adds_no_pass_target_or_capture() {
    let panel = rect(32.0, 32.0, 48.0, 48.0);

    let (mut gpu, _s, mut r) = renderer(W, H);
    let baseline = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            backdrop_layer(panel, SIGMA),
            Primitive::LayerEnd,
        ],
    );

    let (mut gpu, _s, mut r) = renderer(W, H);
    let material = frame(
        &mut r,
        &mut gpu,
        &[background(), Primitive::Frosted(frosted(panel, SIGMA))],
    );

    assert_eq!(material.material_composites, 1);
    assert_eq!(
        material.backdrop_captures, baseline.backdrop_captures,
        "a material surface captures its backdrop the same way a layer does"
    );
    assert_eq!(
        material.backdrop_capture_pixels, baseline.backdrop_capture_pixels,
        "and over the same ROI"
    );
    assert_eq!(
        material.blur_passes, baseline.blur_passes,
        "the blur ladder is E1's, unchanged"
    );
    assert_eq!(
        material.render_passes, baseline.render_passes,
        "the fused chain buys no pass of its own"
    );
    assert_eq!(
        material.transient_targets, baseline.transient_targets,
        "and no target of its own"
    );
    assert_eq!(
        material.color_transform_passes, 0,
        "the color op rides the composite draw, never a pass"
    );
    assert_eq!(
        material.draw_calls, baseline.draw_calls,
        "one composite, exactly as the backdrop layer's own composite was"
    );
}

/// Below the ladder's minimum sigma there is nothing to blur, so no capture is
/// opened and the surface contributes only its border ring — it does not silently
/// fall back to a full-screen capture or drop the border.
#[test]
fn a_subpixel_sigma_captures_nothing_and_keeps_the_border() {
    let panel = rect(32.0, 32.0, 48.0, 48.0);
    let mut m = frosted(panel, 0.25);
    m.border = Border {
        width: 2.0,
        color: Rgba::new(1.0, 1.0, 1.0, 1.0),
    };

    let (mut gpu, _s, mut r) = renderer(W, H);
    let stats = frame(&mut r, &mut gpu, &[background(), Primitive::Frosted(m)]);

    assert_eq!(stats.backdrop_captures, 0);
    assert_eq!(stats.material_composites, 0);
    assert_eq!(stats.blur_passes, 0);
    assert_eq!(stats.render_passes, 1, "surface pass only");
    // Background quad + the border ring: two draws, because the ring is an
    // ordinary analytic rrect and that family does not merge with quads.
    assert_eq!(stats.draw_calls, 2);
}

// ---------------------------------------------------------------------------
// 2. Sharing is inherited from E2, not reimplemented
// ---------------------------------------------------------------------------

/// Four frosted panels over one background at one sigma: one capture, one blur
/// ladder, four composites. The forbidden shape — `N` captures and `N` blurs — is
/// what makes glass UIs unaffordable, and it is exactly what this asserts away.
#[test]
fn n_panels_share_one_capture_and_one_blur_ladder() {
    let (mut gpu, _s, mut r) = renderer(256, 256);
    r.set_surface_size([256.0, 256.0]);
    let panels = [
        rect(16.0, 16.0, 40.0, 24.0),
        rect(16.0, 64.0, 40.0, 24.0),
        rect(16.0, 112.0, 40.0, 24.0),
        rect(16.0, 160.0, 40.0, 24.0),
    ];
    let mut prims = vec![quad(
        rect(0.0, 0.0, 256.0, 256.0),
        Rgba::new(0.3, 0.6, 0.2, 1.0),
    )];
    prims.extend(panels.map(|p| Primitive::Frosted(frosted(p, SIGMA))));
    let stats = frame(&mut r, &mut gpu, &prims);

    assert_eq!(stats.material_composites, 4);
    assert_eq!(
        stats.backdrop_captures, 1,
        "four panels at one sigma share one capture"
    );
    let one = {
        let (mut gpu, _s, mut r) = renderer(256, 256);
        r.set_surface_size([256.0, 256.0]);
        frame(
            &mut r,
            &mut gpu,
            &[
                quad(rect(0.0, 0.0, 256.0, 256.0), Rgba::new(0.3, 0.6, 0.2, 1.0)),
                Primitive::Frosted(frosted(panels[0], SIGMA)),
            ],
        )
    };
    assert_eq!(
        stats.blur_passes, one.blur_passes,
        "the ladder does not scale with the number of panels"
    );
}

/// Two panels at *different* sigmas cannot share: one blurred backdrop cannot
/// serve two blur radii, so the group splits. The sharing rule is E2's, so this
/// pins that a material surface routes through it rather than around it.
#[test]
fn panels_at_different_sigmas_do_not_share() {
    let (mut gpu, _s, mut r) = renderer(W, H);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            Primitive::Frosted(frosted(rect(8.0, 8.0, 32.0, 24.0), 3.0)),
            Primitive::Frosted(frosted(rect(8.0, 72.0, 32.0, 24.0), 9.0)),
        ],
    );
    assert_eq!(stats.material_composites, 2);
    assert_eq!(stats.backdrop_captures, 2);
}

// ---------------------------------------------------------------------------
// 3. Determinism: the grain is a function of the pixel, not of time
// ---------------------------------------------------------------------------

/// Two identical uploads of a noisy frosted panel read back byte-identical. The
/// grain hashes the integer device pixel, so there is no clock in the fragment and
/// a static screen stays static — the E2.5 static-UI gate, extended to material
/// surfaces at full noise amplitude.
#[test]
fn noise_is_deterministic_across_identical_frames() {
    let panel = rect(24.0, 24.0, 64.0, 64.0);
    let mut m = frosted(panel, SIGMA);
    m.noise = 0.5;
    let prims = [background(), Primitive::Frosted(m)];

    let (mut gpu, surface, mut r) = renderer(W, H);
    let mut shots = Vec::new();
    for _ in 0..2 {
        r.upload(&mut gpu, &prims);
        r.submit(
            &mut gpu,
            surface,
            [0.0, 0.0, 0.0, 1.0],
            [W as f32, H as f32],
        );
        shots.push(gpu.read_pixels_bgra8(surface));
    }
    assert_eq!(
        shots[0], shots[1],
        "a static frosted panel must render byte-identically frame to frame"
    );
}

/// The grain actually varies per pixel (a deterministic hash is not a constant):
/// a noisy panel's interior is not uniform, while the same panel without noise is.
#[test]
fn noise_varies_per_pixel_but_only_when_asked_for() {
    let panel = rect(24.0, 24.0, 64.0, 64.0);

    let plain = present(&[background(), Primitive::Frosted(frosted(panel, SIGMA))]);
    let mut noisy_material = frosted(panel, SIGMA);
    noisy_material.noise = 0.5;
    let noisy = present(&[background(), Primitive::Frosted(noisy_material)]);

    // Sample a row well inside the panel, away from its antialiased edge.
    let y = 56;
    let uniform = |px: &[u8]| {
        let first = pixel(px, 40, y);
        (40..72).all(|x| pixel(px, x, y) == first)
    };
    assert!(
        uniform(&plain),
        "a blurred flat backdrop with no grain is flat"
    );
    assert!(
        !uniform(&noisy),
        "grain must vary across the surface, not apply a constant offset"
    );
}

// ---------------------------------------------------------------------------
// 4. The mask rounds, and the color op tints
// ---------------------------------------------------------------------------

/// A rounded frosted panel leaves its corners untouched: inside the panel's rect
/// but outside the corner arc the background shows through unchanged, while the
/// panel's center carries the material's darkened glass. The mask is E0's
/// per-corner rrect SDF evaluated in the material fragment, so the surface rounds
/// without a clip mask, a stencil, or a second draw.
#[test]
fn the_material_mask_rounds_its_corners() {
    let panel = rect(32.0, 32.0, 64.0, 64.0);
    let mut m = frosted(panel, SIGMA);
    m.radius = Corners::uniform(20.0);
    // A visible change the mask can gate: the glass darkens what it covers.
    m.color = fused(ColorEffect::Brightness(0.25));
    let px = present(&[background(), Primitive::Frosted(m)]);

    let outside = pixel(&px, 4, 4);
    // Inside the panel's rect but well outside the corner arc: the mask must have
    // cut the surface away here, leaving the bare background.
    let corner = pixel(&px, panel.x as u32 + 1, panel.y as u32 + 1);
    let center = pixel(
        &px,
        (panel.x + panel.w * 0.5) as u32,
        (panel.y + panel.h * 0.5) as u32,
    );
    assert_eq!(
        corner, outside,
        "the corner outside the arc must be untouched background"
    );
    assert!(
        center[2] < outside[2] * 0.6,
        "the panel's interior is darkened glass: {center:?} vs {outside:?}"
    );
}

/// The fused color op tints the blurred backdrop. A fully-desaturating op turns a
/// saturated blue background grey under the panel, while the background just
/// outside the panel stays blue — the transform applies to the material's sample,
/// not to the surface.
#[test]
fn the_fused_color_op_tints_the_backdrop() {
    let panel = rect(32.0, 32.0, 64.0, 64.0);
    let mut m = frosted(panel, SIGMA);
    m.color = fused(ColorEffect::Grayscale(1.0));
    let px = present(&[background(), Primitive::Frosted(m)]);

    let inside = pixel(
        &px,
        (panel.x + panel.w * 0.5) as u32,
        (panel.y + panel.h * 0.5) as u32,
    );
    let outside = pixel(&px, 4, 4);
    let spread = |c: [f32; 4]| {
        let max = c[0].max(c[1]).max(c[2]);
        let min = c[0].min(c[1]).min(c[2]);
        max - min
    };
    assert!(
        spread(outside) > 0.3,
        "the untouched background is saturated, got {outside:?}"
    );
    assert!(
        spread(inside) < 0.02,
        "under the panel the fused op must have desaturated it, got {inside:?}"
    );
}

/// Opacity scales the finished composite: a half-opaque panel blends its glass
/// with the background it covers, so the result sits strictly between the panel's
/// opaque appearance and the bare background.
#[test]
fn opacity_scales_the_finished_composite() {
    let panel = rect(32.0, 32.0, 64.0, 64.0);
    let cx = (panel.x + panel.w * 0.5) as u32;
    let cy = (panel.y + panel.h * 0.5) as u32;

    let mut dark = frosted(panel, SIGMA);
    dark.color = fused(ColorEffect::Brightness(0.2));
    let opaque = pixel(&present(&[background(), Primitive::Frosted(dark)]), cx, cy);

    let mut half = dark;
    half.opacity = 0.5;
    let blended = pixel(&present(&[background(), Primitive::Frosted(half)]), cx, cy);

    let plain = pixel(&present(&[background()]), cx, cy);
    for c in 0..3 {
        let lo = opaque[c].min(plain[c]);
        let hi = opaque[c].max(plain[c]);
        assert!(
            blended[c] >= lo - 0.02 && blended[c] <= hi + 0.02,
            "channel {c}: {blended:?} must lie between {opaque:?} and {plain:?}"
        );
    }
}
