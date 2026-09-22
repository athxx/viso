//! Backdrop capture & sharing contract (§17.1, §17.2).
//!
//! A backdrop is the one effect whose input is "whatever is already drawn behind
//! me". The two things that must never regress:
//!
//! 1. **The dependency is a graph edge, not a framebuffer read.** A capture is a
//!    render pass of its own that re-renders the under-content into a target
//!    sized to exactly the region the blur needs. A widget never samples the
//!    surface it is being composited into, whose contents at that moment are
//!    undefined, and never captures more than its ROI.
//! 2. **N frosted panels are not N full-screen captures and N blurs.** Panels
//!    over the same background, at the same sigma, with nothing painted between
//!    them that a shared capture would wrongly include, share one capture and one
//!    blur ladder and get one composite each.
//!
//! Everything here goes through the public surface only: `LayerClip`'s
//! `backdrop_sigma` in, `FrameStats` out. The sharing heuristics' numbers are
//! private consts, so the assertions pin the *behaviour* (shared vs split, tight
//! vs full-screen) and pick scene geometry that is unambiguous under any sane
//! threshold.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, TextureFormat};
use viso_render::{Border, FrameStats, LayerClip, Primitive, Quad, Rect, Renderer, Rgba};

/// The blur reach a backdrop pads its clip by, per side, in pixels: the same
/// `ceil(3 * sigma)` the ladder's kernel spans. Mirrored here (not imported —
/// it is private) so the expected ROI is written out rather than derived from
/// the code under test.
fn reach(sigma: f32) -> f32 {
    (3.0 * sigma).ceil()
}

fn renderer(w: u32, h: u32) -> (HeadlessRaster, Renderer) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
    let format: TextureFormat = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([w as f32, h as f32]);
    (gpu, r)
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn quad(rect: Rect) -> Primitive {
    Primitive::Quad(Quad {
        rect,
        color: Rgba::new(0.4, 0.5, 0.6, 1.0),
        radius: 0.0,
        border: Border::NONE,
    })
}

/// A frosted panel: opaque, unblurred content, blurring what is *behind* it.
fn frosted(clip: Rect, sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: sigma,
    })
}

/// A layer that goes offscreen because it is translucent.
fn layer(clip: Rect, opacity: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

/// A full-window background, so every scene here has content to capture.
fn background() -> Primitive {
    quad(rect(0.0, 0.0, 512.0, 512.0))
}

fn frame(r: &mut Renderer, gpu: &mut HeadlessRaster, primitives: &[Primitive]) -> FrameStats {
    r.upload(gpu, primitives);
    r.frame_stats()
}

// ---------------------------------------------------------------------------
// 1. Capture is a pass over a tight ROI
// ---------------------------------------------------------------------------

/// A frosted panel opens exactly one capture pass, and that pass renders the
/// panel's clip padded by the blur reach — nothing wider. The forbidden default
/// (capture the whole surface) would be ~23x the pixels here.
#[test]
fn a_backdrop_captures_only_its_padded_clip() {
    let (mut gpu, mut r) = renderer(512, 512);
    let panel = rect(100.0, 100.0, 80.0, 40.0);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[background(), frosted(panel, 8.0), Primitive::LayerEnd],
    );

    let pad = reach(8.0);
    let expect = ((panel.w + 2.0 * pad) as usize) * ((panel.h + 2.0 * pad) as usize);
    assert_eq!(stats.backdrop_captures, 1);
    assert_eq!(
        stats.backdrop_capture_pixels, expect,
        "the capture is the clip padded by the blur reach, clamped to the surface"
    );
    assert!(
        stats.backdrop_capture_pixels * 20 < 512 * 512,
        "capturing the full surface is the regression this guards"
    );
}

/// The padding is clamped by the surface: a panel at the window edge cannot make
/// the renderer allocate outside it, and the capture shrinks accordingly.
#[test]
fn the_capture_roi_is_clamped_to_the_surface() {
    let (mut gpu, mut r) = renderer(512, 512);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            frosted(rect(0.0, 0.0, 80.0, 40.0), 8.0),
            Primitive::LayerEnd,
        ],
    );
    let pad = reach(8.0);
    assert_eq!(
        stats.backdrop_capture_pixels,
        ((80.0 + pad) as usize) * ((40.0 + pad) as usize),
        "the off-surface half of the padding is dropped, not allocated"
    );
}

/// Each capture is a render pass of its own in the compiled plan, feeding the
/// blur ladder that the surface composite samples. The frame's passes account
/// for exactly: every offscreen layer, every capture, every blur rung, and the
/// surface — no pass is culled, because the composite reads the result, and no
/// pass is implicit.
#[test]
fn a_capture_is_its_own_graph_pass() {
    let (mut gpu, mut r) = renderer(512, 512);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            frosted(rect(100.0, 100.0, 80.0, 40.0), 8.0),
            quad(rect(110.0, 110.0, 20.0, 20.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.backdrop_captures, 1);
    assert!(stats.blur_passes >= 2, "a backdrop plans a blur ladder");
    assert_eq!(
        stats.render_passes,
        stats.offscreen_passes + stats.backdrop_captures as usize + stats.blur_passes as usize + 1,
        "capture, ladder and surface are each their own pass"
    );
    assert_eq!(
        stats.culled_render_passes, 0,
        "the surface composite keeps the capture alive"
    );
}

// ---------------------------------------------------------------------------
// 2. Sharing: N panels are not N captures
// ---------------------------------------------------------------------------

/// Two panels over the same background, same sigma, far enough apart that
/// neither's frosted composite lands inside the other's ROI: one capture, one
/// blur ladder, two composites. This is the case the forbidden default gets
/// wrong, and the reason the union is captured rather than each panel separately.
#[test]
fn neighbouring_panels_share_one_capture_and_one_ladder() {
    let (mut gpu, mut r) = renderer(512, 512);
    let a = rect(100.0, 100.0, 80.0, 40.0);
    let b = rect(204.0, 100.0, 80.0, 40.0);
    let shared = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            frosted(a, 8.0),
            Primitive::LayerEnd,
            frosted(b, 8.0),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(shared.backdrop_captures, 1, "one shared capture, not two");

    // The shared ROI is the union of the two padded clips, so it is wider than
    // either alone but far short of the two captured separately at full size.
    let pad = reach(8.0);
    let union_w = (b.x + b.w + pad) - (a.x - pad);
    assert_eq!(
        shared.backdrop_capture_pixels,
        (union_w as usize) * ((a.h + 2.0 * pad) as usize)
    );

    // One panel alone plans the same ladder: sharing adds panels, not rungs.
    let single = frame(
        &mut r,
        &mut gpu,
        &[background(), frosted(a, 8.0), Primitive::LayerEnd],
    );
    assert_eq!(
        shared.blur_passes, single.blur_passes,
        "a shared group blurs once, not once per panel"
    );
    assert_eq!(
        shared.render_passes, single.render_passes,
        "and adds no pass either"
    );
    // Each panel still composites itself — one instance each — but the two
    // composites sample the same blurred source, so bind-group batching puts them
    // in one draw (§20.2): the second panel costs an instance, not a draw call.
    assert_eq!(shared.instances, single.instances + 1);
    assert_eq!(
        shared.draw_calls, single.draw_calls,
        "panels sharing a blurred source share the composite draw"
    );
}

/// Different sigmas cannot share a ladder, so they cannot share a capture: the
/// group splits even for panels that are otherwise perfectly compatible.
#[test]
fn a_different_sigma_splits_the_group() {
    let (mut gpu, mut r) = renderer(512, 512);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            frosted(rect(100.0, 100.0, 80.0, 40.0), 8.0),
            Primitive::LayerEnd,
            frosted(rect(204.0, 100.0, 80.0, 40.0), 4.0),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.backdrop_captures, 2);
}

/// A panel painted *over* an earlier panel is not sharable: a shared capture is
/// taken below both, so the first panel's own frosted pixels would vanish from
/// under the second. Correctness wins; the group splits.
#[test]
fn a_panel_overlapping_an_earlier_one_splits_the_group() {
    let (mut gpu, mut r) = renderer(512, 512);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            frosted(rect(100.0, 100.0, 80.0, 40.0), 8.0),
            Primitive::LayerEnd,
            frosted(rect(140.0, 110.0, 80.0, 40.0), 8.0),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(
        stats.backdrop_captures, 2,
        "the second panel must see the first one's composite"
    );
}

/// Sharing is bounded by area, not just compatibility: two panels at opposite
/// corners must not silently promote themselves to one full-window capture. They
/// split, and the two tight captures together stay far below the surface.
#[test]
fn distant_panels_do_not_promote_to_a_full_screen_capture() {
    let (mut gpu, mut r) = renderer(512, 512);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            frosted(rect(0.0, 0.0, 80.0, 40.0), 8.0),
            Primitive::LayerEnd,
            frosted(rect(400.0, 400.0, 80.0, 40.0), 8.0),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.backdrop_captures, 2);
    assert!(
        stats.backdrop_capture_pixels * 10 < 512 * 512,
        "two tight captures must cost less than their union would"
    );
}

/// A row of frosted panels — the shape a real toolbar or sidebar takes — costs
/// one capture and one ladder, and its captured pixels grow with the row, not
/// with the panel count times the window.
#[test]
fn a_row_of_panels_costs_one_capture() {
    let (mut gpu, mut r) = renderer(512, 512);
    let mut scene = vec![background()];
    for i in 0..6 {
        scene.push(frosted(rect(8.0 + i as f32 * 80.0, 40.0, 56.0, 32.0), 6.0));
        scene.push(Primitive::LayerEnd);
    }
    let stats = frame(&mut r, &mut gpu, &scene);
    assert_eq!(stats.backdrop_captures, 1);
    assert!(stats.blur_passes <= 2, "one ladder for the whole row");
    assert!(
        stats.backdrop_capture_pixels < 6 * 512 * 512,
        "the forbidden default is six full-screen captures"
    );
}

// ---------------------------------------------------------------------------
// 3. When a backdrop costs nothing
// ---------------------------------------------------------------------------

/// A sub-pixel sigma would composite the captured content back unchanged, so no
/// capture is taken at all: the layer stays a plain scissor.
#[test]
fn a_subpixel_backdrop_captures_nothing() {
    let (mut gpu, mut r) = renderer(512, 512);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            frosted(rect(100.0, 100.0, 80.0, 40.0), 0.5),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.backdrop_captures, 0);
    assert_eq!(stats.backdrop_capture_pixels, 0);
    assert_eq!(stats.blur_passes, 0);
    assert_eq!(stats.render_passes, 1, "the surface pass alone");
}

/// A clip that lands entirely off-surface has no ROI to capture, so the backdrop
/// is dropped rather than clamped to a degenerate target.
#[test]
fn an_offscreen_clip_captures_nothing() {
    let (mut gpu, mut r) = renderer(512, 512);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            frosted(rect(900.0, 900.0, 80.0, 40.0), 8.0),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.backdrop_captures, 0);
}

/// A backdrop nested inside a translucent layer is dropped: it draws into its
/// parent's offscreen texture, whose pass closes mid-walk, while a capture group
/// is only final once the walk ends. The clip still applies; only the frost is
/// omitted, and the parent's own offscreen pass is unaffected.
#[test]
fn a_backdrop_inside_an_offscreen_layer_is_dropped() {
    let (mut gpu, mut r) = renderer(512, 512);
    let stats = frame(
        &mut r,
        &mut gpu,
        &[
            background(),
            layer(rect(0.0, 0.0, 300.0, 300.0), 0.5),
            quad(rect(0.0, 0.0, 200.0, 200.0)),
            frosted(rect(100.0, 100.0, 80.0, 40.0), 8.0),
            Primitive::LayerEnd,
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(stats.backdrop_captures, 0);
    assert_eq!(stats.offscreen_passes, 1, "the parent layer is unaffected");
}

// ---------------------------------------------------------------------------
// 4. Steady state
// ---------------------------------------------------------------------------

/// Re-uploading an identical frosted scene re-plans nothing: the capture, its
/// target and its ladder come back from the pools, and the graph's cached plan is
/// reused. A backdrop is not a per-frame allocation.
#[test]
fn a_steady_frosted_scene_replans_nothing() {
    let (mut gpu, mut r) = renderer(512, 512);
    let scene = [
        background(),
        frosted(rect(100.0, 100.0, 80.0, 40.0), 8.0),
        Primitive::LayerEnd,
        frosted(rect(204.0, 100.0, 80.0, 40.0), 8.0),
        Primitive::LayerEnd,
    ];
    let first = frame(&mut r, &mut gpu, &scene);
    for _ in 0..4 {
        let next = frame(&mut r, &mut gpu, &scene);
        assert_eq!(next.backdrop_captures, first.backdrop_captures);
        assert_eq!(next.backdrop_capture_pixels, first.backdrop_capture_pixels);
        assert_eq!(next.blur_passes, first.blur_passes);
        assert_eq!(next.render_passes, first.render_passes);
        assert_eq!(next.transient_target_allocations, 0);
        assert_eq!(next.render_graph_compiles, 0);
    }
}
