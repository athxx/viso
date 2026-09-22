//! Color-domain contract: what format an intermediate target gets (§19).
//!
//! A frame's offscreen targets — the backdrop capture, the offscreen layer, the
//! blur scratch, the color scratch — are not free-floating buffers. They sit
//! mid-pipeline between the scene and the surface, so their format decides
//! whether what the surface can *display* is what the pipeline actually
//! *carries*. Three targets have to be told apart: an SDR sRGB surface, a
//! wide-gamut one, and an extended-range HDR one.
//!
//! Two failure modes are the point of the rule, and they pull in opposite
//! directions:
//!
//! 1. **Every offscreen forced to half-float.** An SDR window would pay double
//!    the bandwidth and residency on every layer, blur rung and capture for range
//!    it can never show.
//! 2. **An HDR scene narrowed mid-pipeline.** A value above `1.0` that reaches an
//!    8-bit intermediate is gone; re-widening afterwards recovers nothing, and the
//!    loss happens where no pixel test on the surface would see a cause.
//!
//! The planner's answer to both is the same one: an intermediate is allocated in
//! the *surface's own* format. That format is simultaneously inside the target's
//! domain, lossless for its precision, no wider than that precision needs, and
//! compatible with the one prewarmed pipeline set (a pipeline is bound to its
//! attachment format). The tests below pin that rule from both ends, plus the two
//! places a domain does change a format: the gradient ramp bake, which is a CPU
//! quantization step and the only path that could clamp an authored stop; and
//! coverage planes, which are excluded by construction because a mask is not
//! color.

use viso_gpu::{
    ColorDomain, ColorSpace, GpuBackend, HeadlessRaster, RawWindowHandle, SurfaceId, TextureFormat,
    f16_to_f32,
};
use viso_render::{Border, LayerClip, Primitive, Quad, Rect, Renderer, Rgba};

const W: u32 = 64;
const H: u32 = 64;

/// A renderer over a surface reporting `format` in `space` — the headless raster
/// stands in for a swapchain Viso could not negotiate on a test machine.
fn renderer(format: TextureFormat, space: ColorSpace) -> (HeadlessRaster, SurfaceId, Renderer) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    gpu.set_surface_color_target(surface, format, space);
    let mut r = Renderer::for_surface(&mut gpu, surface);
    r.set_surface_size([W as f32, H as f32]);
    (gpu, surface, r)
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

/// A full-window quad of `color` wrapped in a translucent blurred layer, so the
/// frame is forced through a capture/blur/composite chain of intermediates rather
/// than drawn straight to the surface.
fn scene_through_intermediates(color: Rgba) -> Vec<Primitive> {
    vec![
        Primitive::Layer(LayerClip {
            clip: rect(0.0, 0.0, W as f32, H as f32),
            opacity: 0.5,
            blur_sigma: 2.0,
            backdrop_sigma: 0.0,
        }),
        Primitive::Quad(Quad {
            rect: rect(0.0, 0.0, W as f32, H as f32),
            color,
            radius: 0.0,
            border: Border::NONE,
        }),
        Primitive::LayerEnd,
    ]
}

/// The center pixel's red channel as the surface stores it, read through an
/// extended-range readback so a value above `1.0` is reported rather than clipped
/// by the readback itself.
fn center_red(gpu: &HeadlessRaster, surface: SurfaceId) -> f32 {
    let px = gpu.read_pixels_rgba16f(surface);
    let i = ((H / 2) * W + W / 2) as usize * 8;
    f16_to_f32(u16::from_le_bytes([px[i], px[i + 1]]))
}

// ---------------------------------------------------------------------------
// 1. The three targets are told apart
// ---------------------------------------------------------------------------

/// An ordinary window is SDR sRGB, and its intermediates stay 8-bit. This is the
/// first ban stated directly: the common case must not pay half-float bandwidth
/// on every capture, layer and blur rung for range it cannot display.
#[test]
fn an_sdr_surface_keeps_its_intermediates_at_eight_bits() {
    let (_gpu, _s, r) = renderer(TextureFormat::Bgra8Unorm, ColorSpace::Srgb);

    assert_eq!(r.color_domain(), ColorDomain::Sdr);
    assert_eq!(r.intermediate_format(), TextureFormat::Bgra8Unorm);
    assert!(
        !r.intermediate_format().is_extended_range(),
        "an SDR target must not be given extended-range intermediates"
    );
}

/// A wide-gamut target is a different *space*, not a different precision: P3
/// primaries at 8 bits are the same format as sRGB at 8 bits. So it is classified
/// apart from SDR while keeping the same narrow intermediates — the distinction
/// §19 asks for is three domains, not three formats.
#[test]
fn a_wide_gamut_surface_is_wider_in_space_not_in_range() {
    let (_gpu, _s, r) = renderer(TextureFormat::Bgra8Unorm, ColorSpace::DisplayP3);

    assert_eq!(r.color_domain(), ColorDomain::WideGamut);
    assert_ne!(r.color_domain(), ColorDomain::Sdr, "P3 is its own domain");
    assert_eq!(r.intermediate_format(), TextureFormat::Bgra8Unorm);
    assert!(!r.color_domain().requires_extended_range());
}

/// An HDR target composites in an extended-linear space, and every intermediate
/// keeps its range. This is the second ban: nothing in the chain narrows to 8 bits
/// and then re-widens.
#[test]
fn an_hdr_surface_keeps_its_intermediates_extended() {
    let (_gpu, _s, r) = renderer(
        TextureFormat::Rgba16Float,
        ColorSpace::ExtendedLinearDisplayP3,
    );

    assert_eq!(r.color_domain(), ColorDomain::Hdr);
    assert!(r.color_domain().requires_extended_range());
    assert!(
        r.intermediate_format().is_extended_range(),
        "an HDR frame's intermediates must hold values above 1.0"
    );
    assert_eq!(r.intermediate_format(), TextureFormat::Rgba16Float);
}

/// The three domains are exactly the three §19 names, and each extended-linear
/// space collapses onto the HDR one — the domain is what a format is planned
/// against, so several spaces sharing one is the point, not a loss.
#[test]
fn every_space_maps_onto_one_of_the_three_domains() {
    assert_eq!(ColorSpace::Srgb.domain(), ColorDomain::Sdr);
    assert_eq!(ColorSpace::DisplayP3.domain(), ColorDomain::WideGamut);
    assert_eq!(ColorSpace::ExtendedLinearSrgb.domain(), ColorDomain::Hdr);
    assert_eq!(
        ColorSpace::ExtendedLinearDisplayP3.domain(),
        ColorDomain::Hdr
    );
    assert_eq!(ColorSpace::default(), ColorSpace::Srgb);
}

// ---------------------------------------------------------------------------
// 2. The intermediate format is the surface's, not the domain's
// ---------------------------------------------------------------------------

/// The rule is "the surface's own format", which is stronger than "narrow unless
/// HDR": a surface that reports half-float storage keeps half-float
/// intermediates even in an SDR space, because narrowing them would be a
/// mid-pipeline precision loss the target did not ask for — and because a
/// pipeline built for that attachment cannot draw into a different format anyway.
#[test]
fn the_intermediate_format_follows_the_surface_not_the_domain() {
    let (_gpu, _s, r) = renderer(TextureFormat::Rgba16Float, ColorSpace::Srgb);

    assert_eq!(r.color_domain(), ColorDomain::Sdr);
    assert_eq!(r.intermediate_format(), r.surface_format());
    assert_eq!(r.intermediate_format(), TextureFormat::Rgba16Float);
}

/// Whatever the surface reports, the intermediates match it. Stated as one
/// invariant over every target this build can describe, so a new format cannot be
/// added with an intermediate plan that silently disagrees with its attachment.
#[test]
fn the_intermediates_always_match_the_attachment() {
    for (format, space) in [
        (TextureFormat::Bgra8Unorm, ColorSpace::Srgb),
        (TextureFormat::Rgba8Unorm, ColorSpace::Srgb),
        (TextureFormat::Bgra8Unorm, ColorSpace::DisplayP3),
        (TextureFormat::Rgba16Float, ColorSpace::ExtendedLinearSrgb),
        (
            TextureFormat::Rgba16Float,
            ColorSpace::ExtendedLinearDisplayP3,
        ),
    ] {
        let (_gpu, _s, r) = renderer(format, space);
        assert_eq!(
            r.intermediate_format(),
            format,
            "{format:?} in {space:?} must keep its own format for intermediates"
        );
        assert!(r.intermediate_format().is_color());
    }
}

/// Constructed from a bare format, a renderer is SDR sRGB. The default is the
/// truthful answer for a caller that never negotiated a space, not an optimistic
/// one — and it keeps every existing SDR call site meaning exactly what it did.
#[test]
fn a_renderer_built_from_a_format_alone_is_sdr() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let r = Renderer::new(&mut gpu, format);

    assert_eq!(r.color_space(), ColorSpace::Srgb);
    assert_eq!(r.color_domain(), ColorDomain::Sdr);
}

// ---------------------------------------------------------------------------
// 3. What a whole frame does to an out-of-range value
// ---------------------------------------------------------------------------

/// The end-to-end statement of the rule. A highlight brighter than white is drawn
/// through a blurred, half-opacity layer — so it passes through an offscreen
/// target, the blur scratch and a composite before reaching the surface — and it
/// arrives still brighter than white. Had any of those intermediates been 8-bit
/// the value would have clipped to `1.0` on the way in, and no later stage could
/// tell that it had.
#[test]
fn an_out_of_range_highlight_survives_the_whole_chain() {
    let (mut gpu, surface, mut r) = renderer(
        TextureFormat::Rgba16Float,
        ColorSpace::ExtendedLinearDisplayP3,
    );

    r.upload(
        &mut gpu,
        &scene_through_intermediates(Rgba::new(4.0, 0.0, 0.0, 1.0)),
    );
    r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);

    // The layer's own 0.5 opacity is applied at composite time, so the expected
    // value is half the authored 4.0 — the point is that it is far above 1.0, not
    // that it is unattenuated.
    let red = center_red(&gpu, surface);
    assert!(
        red > 1.5,
        "an HDR highlight must survive the intermediate chain; got {red}"
    );
}

/// The same scene on an SDR surface clips, and that is correct: the loss is the
/// target's own limit at the place the target imposes it, not a narrowing hidden
/// in the middle of the pipeline. Pairing this with the test above is what makes
/// the previous one evidence rather than coincidence.
#[test]
fn the_same_highlight_clips_on_an_sdr_surface() {
    let (mut gpu, surface, mut r) = renderer(TextureFormat::Bgra8Unorm, ColorSpace::Srgb);

    r.upload(
        &mut gpu,
        &scene_through_intermediates(Rgba::new(4.0, 0.0, 0.0, 1.0)),
    );
    r.submit(&mut gpu, surface, [0.0; 4], [W as f32, H as f32]);

    let red = center_red(&gpu, surface);
    assert!(
        red <= 1.0 + 1.0 / 255.0,
        "an 8-bit surface cannot store more than 1.0; got {red}"
    );
}

// ---------------------------------------------------------------------------
// 4. The two places a domain does change a format
// ---------------------------------------------------------------------------

/// The gradient ramp is baked on the CPU into a texel, so its format is a real
/// decision rather than an attachment constraint: 8-bit unorm rounds and clamps,
/// which would destroy an authored stop above 1.0 before the GPU ever sampled it.
/// So the ramp follows the domain — and only the domain, since it is not a render
/// target and no pipeline is bound to it.
#[test]
fn the_gradient_ramp_follows_the_domain() {
    let (_gpu, _s, sdr) = renderer(TextureFormat::Bgra8Unorm, ColorSpace::Srgb);
    assert_eq!(sdr.gradient_lut_format(), TextureFormat::Rgba8Unorm);

    let (_gpu, _s, wide) = renderer(TextureFormat::Bgra8Unorm, ColorSpace::DisplayP3);
    assert_eq!(
        wide.gradient_lut_format(),
        TextureFormat::Rgba8Unorm,
        "wide gamut is a space, not extra range: 8-bit ramps still suffice"
    );

    let (_gpu, _s, hdr) = renderer(
        TextureFormat::Rgba16Float,
        ColorSpace::ExtendedLinearDisplayP3,
    );
    assert!(
        hdr.gradient_lut_format().is_extended_range(),
        "an HDR ramp must not clamp its stops at the bake"
    );
}

/// Coverage is not color, so no domain ever promotes a coverage plane. A glyph
/// mask and a clip mask are occupancy fractions in `[0, 1]` by definition;
/// widening them alongside the color targets would multiply the largest textures
/// in the frame for range that cannot exist in them.
#[test]
fn coverage_planes_are_never_promoted() {
    assert!(!TextureFormat::R8Unorm.is_color());
    assert!(!TextureFormat::R8Unorm.is_extended_range());
    assert_eq!(TextureFormat::R8Unorm.bytes_per_texel(), 1);

    // Nor is depth a color target, so the color plan never reaches it either.
    assert!(!TextureFormat::Depth32Float.is_color());
}

/// Extended range is a property of the format's *representation*, not its bit
/// depth: no unorm format holds a value above 1.0 at any width, which is why the
/// plan branches on this and never on bytes per texel.
#[test]
fn extended_range_is_not_a_question_of_bit_depth() {
    assert!(!TextureFormat::Bgra8Unorm.is_extended_range());
    assert!(!TextureFormat::Rgba8Unorm.is_extended_range());
    assert!(TextureFormat::Rgba16Float.is_extended_range());
    assert_eq!(TextureFormat::Rgba16Float.bytes_per_texel(), 8);
}

/// A backend that has negotiated nothing with its compositor reports SDR sRGB,
/// which is what it is actually presenting — an optimistic default would have the
/// renderer plan an HDR chain for a target that cannot show it.
#[test]
fn an_unnegotiated_surface_reports_the_sdr_default() {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);

    assert_eq!(gpu.surface_color_space(surface), ColorSpace::Srgb);
    assert_eq!(
        gpu.surface_color_space(surface).domain(),
        ColorDomain::Sdr,
        "no display is assumed to be better than it said it was"
    );
}
