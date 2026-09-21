//! Frozen contract for the offscreen effect pipeline (§16.1, §16.3, §16.4).
//!
//! Four contracts are pinned here, because the backdrop / shared-pyramid /
//! Effect-Planner work builds directly on them and must not silently redefine
//! them:
//!
//! 1. **Tight ROI.** An offscreen layer's target is `content_union ∩ clip ∩
//!    surface`, resolved at `LayerEnd` — never the layer's full clip and never
//!    the full surface. A small panel inside a large window must cost a small
//!    texture.
//! 2. **Blur ladder selection, thresholds internal.** The only public authoring
//!    knob is `LayerClip::blur_sigma` and the only public observation is
//!    `FrameStats`. The tier boundaries are private consts, so this pins the
//!    *behaviour* (how many rungs, how many bytes, what is stable) rather than
//!    the numbers that produce it — which is itself half the contract.
//! 3. **Transient pool keys + alias reuse.** What makes two targets compatible
//!    ([`TargetKey`]), and the discipline that lets them share one texture
//!    (`free_at <= first_write`, i.e. strictly non-overlapping lifetimes).
//! 4. **RenderGraph responsibilities + compile/reuse.** The five lowering jobs
//!    and the topology-only recompile rule: extents, colors, transforms and
//!    within-tier sigma changes all reuse the cached plan.
//!
//! The in-crate unit tests cover the pure functions directly; this integration
//! test exists because a `cargo test --workspace` run is the gate that a
//! downstream slice actually trips, and because it can only see the *public*
//! surface — which is precisely what "frozen" has to mean.

use viso_gpu::{GpuBackend, HeadlessRaster, RawWindowHandle, SamplerDesc, TextureFormat};
use viso_render::graph::{GraphStats, PassLoad, PassWork, RenderGraph};
use viso_render::transient::{
    SURFACE_SLOT, TRANSIENT_TARGET_IDLE_FRAMES, TargetDesc, TargetId, TargetKey, TargetUsage,
    TransientStats, TransientTargets, size_class,
};
use viso_render::{Border, FrameStats, LayerClip, Primitive, Quad, Rect, Renderer, Rgba};

/// Bytes per texel of every target the offscreen pipeline allocates today.
const BPT: usize = 4;

fn renderer(w: u32, h: u32) -> (HeadlessRaster, Renderer) {
    let mut gpu = HeadlessRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
    let format = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([w as f32, h as f32]);
    (gpu, r)
}

fn quad(rect: Rect) -> Primitive {
    Primitive::Quad(Quad {
        rect,
        color: Rgba::new(0.4, 0.5, 0.6, 1.0),
        radius: 0.0,
        border: Border::NONE,
    })
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

/// A layer that goes offscreen for `opacity`, with no blur.
fn layer(clip: Rect, opacity: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity,
        blur_sigma: 0.0,
    })
}

/// A fully opaque layer that goes offscreen only because it blurs.
fn blurred(clip: Rect, sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity: 1.0,
        blur_sigma: sigma,
    })
}

/// The pooled bytes one target of `w x h` addresses, after size-class bucketing.
fn pooled_bytes(w: u32, h: u32) -> usize {
    size_class(w) as usize * size_class(h) as usize * BPT
}

// ---------------------------------------------------------------------------
// 1. Tight ROI
// ---------------------------------------------------------------------------

/// The ROI follows the layer's *content*, not its clip: a small panel inside a
/// large window allocates a small-panel texture. The forbidden shape — sizing
/// the target to the clip, or to the surface — would be ~170x this.
#[test]
fn roi_is_bounded_by_layer_content() {
    let (mut gpu, mut r) = renderer(512, 512);
    // The clip spans the whole window; only a 40x24 quad is painted into it.
    r.upload(
        &mut gpu,
        &[
            layer(rect(0.0, 0.0, 512.0, 512.0), 0.5),
            quad(rect(100.0, 100.0, 40.0, 24.0)),
            Primitive::LayerEnd,
        ],
    );
    let stats = r.frame_stats();
    assert_eq!(stats.offscreen_passes, 1);
    assert_eq!(
        stats.transient_target_bytes,
        pooled_bytes(40, 24),
        "the target is the content union, size-class rounded"
    );
    assert!(
        stats.transient_target_bytes * 100 < pooled_bytes(512, 512),
        "sizing to the clip/surface is the regression this guards"
    );
}

/// The clip is the second term of the intersection: content wider than the clip
/// is cropped to it, and the target shrinks accordingly.
#[test]
fn roi_is_bounded_by_the_layer_clip() {
    let (mut gpu, mut r) = renderer(512, 512);
    r.upload(
        &mut gpu,
        &[
            layer(rect(10.0, 10.0, 40.0, 24.0), 0.5),
            quad(rect(0.0, 0.0, 400.0, 400.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(r.frame_stats().transient_target_bytes, pooled_bytes(40, 24));
}

/// The surface is the third term: content and clip that both run off-screen
/// cannot make the renderer allocate past the window.
#[test]
fn roi_is_bounded_by_the_surface() {
    let (mut gpu, mut r) = renderer(64, 64);
    r.upload(
        &mut gpu,
        &[
            layer(rect(-500.0, -500.0, 2000.0, 2000.0), 0.5),
            quad(rect(-100.0, -100.0, 1000.0, 1000.0)),
            Primitive::LayerEnd,
        ],
    );
    assert_eq!(r.frame_stats().transient_target_bytes, pooled_bytes(64, 64));
}

/// The ROI is resolved at `LayerEnd`, not at `Layer`: a primitive recorded after
/// the layer opens still widens the target. This is why the texture claim is
/// deferred — the union is not known when the pass is opened.
#[test]
fn roi_is_resolved_at_layer_end() {
    let (mut gpu, mut r) = renderer(256, 256);
    let one = &[
        layer(rect(0.0, 0.0, 256.0, 256.0), 0.5),
        quad(rect(0.0, 0.0, 16.0, 16.0)),
        Primitive::LayerEnd,
    ];
    r.upload(&mut gpu, one);
    let narrow = r.frame_stats().transient_target_bytes;

    let two = &[
        layer(rect(0.0, 0.0, 256.0, 256.0), 0.5),
        quad(rect(0.0, 0.0, 16.0, 16.0)),
        quad(rect(200.0, 0.0, 16.0, 16.0)),
        Primitive::LayerEnd,
    ];
    r.upload(&mut gpu, two);
    let wide = r.frame_stats().transient_target_bytes;

    assert_eq!(narrow, pooled_bytes(16, 16));
    assert_eq!(wide, pooled_bytes(216, 16));
    assert!(wide > narrow, "a later primitive still grows the ROI");
}

/// An opaque, unblurred layer needs no texture at all: it stays inline as a
/// scissor. Offscreen is a cost the renderer pays only when asked.
#[test]
fn an_opaque_unblurred_layer_allocates_nothing() {
    let (mut gpu, mut r) = renderer(128, 128);
    r.upload(
        &mut gpu,
        &[
            layer(rect(0.0, 0.0, 64.0, 64.0), 1.0),
            quad(rect(0.0, 0.0, 32.0, 32.0)),
            Primitive::LayerEnd,
        ],
    );
    let stats = r.frame_stats();
    assert_eq!(stats.offscreen_passes, 0);
    assert_eq!(stats.transient_target_bytes, 0);
    assert_eq!(stats.render_passes, 1, "the surface pass alone");
}

// ---------------------------------------------------------------------------
// 2. Blur ladder selection (thresholds internal)
// ---------------------------------------------------------------------------

/// The ladder for one 64x64 blurred layer, keyed by sigma only.
fn blur_frame(r: &mut Renderer, gpu: &mut HeadlessRaster, sigma: f32) -> FrameStats {
    r.upload(
        gpu,
        &[
            blurred(rect(0.0, 0.0, 64.0, 64.0), sigma),
            quad(rect(0.0, 0.0, 64.0, 64.0)),
            Primitive::LayerEnd,
        ],
    );
    r.frame_stats()
}

/// Sigma selects a rung count from a fixed, coarse ladder — never a per-sigma
/// pass count. A sub-pixel sigma is skipped outright (the opaque layer stays
/// inline, paying nothing), a small sigma is one separable pair, and a large
/// sigma is a downsample pyramid plus a pair.
#[test]
fn the_blur_ladder_has_three_tiers() {
    let (mut gpu, mut r) = renderer(64, 64);
    for (sigma, offscreen, rungs) in [
        (0.0, 0usize, 0u32),
        (0.5, 0, 0),
        (1.0, 0, 0),
        (2.0, 1, 2),
        (4.0, 1, 2),
        (10.0, 1, 2),
        (24.0, 1, 4),
        (64.0, 1, 4),
    ] {
        let stats = blur_frame(&mut r, &mut gpu, sigma);
        assert_eq!(
            stats.blur_passes, rungs,
            "sigma {sigma} must plan {rungs} blur passes"
        );
        assert_eq!(
            stats.offscreen_passes, offscreen,
            "sigma {sigma} must plan {offscreen} offscreen passes at full opacity"
        );
        assert_eq!(
            stats.render_passes,
            1 + offscreen + rungs as usize,
            "the frame is the layer pass, its rungs, and the surface"
        );
    }
}

/// A sigma the ladder would skip costs nothing at all: no offscreen pass, no
/// texture, no composite. The alternative — rendering the subtree to a target
/// and compositing it back unchanged — is the regression this guards.
#[test]
fn a_subpixel_blur_stays_inline() {
    let (mut gpu, mut r) = renderer(64, 64);
    let stats = blur_frame(&mut r, &mut gpu, 0.5);
    assert_eq!(stats.blur_passes, 0);
    assert_eq!(stats.offscreen_passes, 0);
    assert_eq!(stats.transient_target_bytes, 0);
    assert_eq!(stats.transient_target_allocations, 0);
    assert_eq!(stats.render_passes, 1);

    // A translucent layer still goes offscreen — opacity, not blur, is why.
    r.upload(
        &mut gpu,
        &[
            Primitive::Layer(LayerClip {
                clip: rect(0.0, 0.0, 64.0, 64.0),
                opacity: 0.5,
                blur_sigma: 0.5,
            }),
            quad(rect(0.0, 0.0, 64.0, 64.0)),
            Primitive::LayerEnd,
        ],
    );
    let stats = r.frame_stats();
    assert_eq!(stats.offscreen_passes, 1);
    assert_eq!(stats.blur_passes, 0);
}

/// Going up a tier adds passes but *reduces* addressed bytes: the pyramid tier
/// exists so a wide blur costs less memory traffic, not more. Within the tap
/// budget the ladder runs at full ROI resolution; past it, resolution drops.
#[test]
fn the_pyramid_tier_addresses_fewer_bytes_than_full_resolution() {
    let (mut gpu, mut r) = renderer(64, 64);

    let small = blur_frame(&mut r, &mut gpu, 4.0);
    assert_eq!(
        small.blur_target_bytes,
        2 * pooled_bytes(64, 64),
        "the small tier blurs at full ROI resolution, twice"
    );

    let large = blur_frame(&mut r, &mut gpu, 24.0);
    assert!(
        large.blur_target_bytes < small.blur_target_bytes,
        "the pyramid tier must reduce resolution, not widen the kernel"
    );

    let huge = blur_frame(&mut r, &mut gpu, 64.0);
    assert!(
        huge.blur_target_bytes <= large.blur_target_bytes,
        "scratch bytes are non-increasing once the tap budget is exceeded"
    );
}

/// Sigma is a dynamic parameter, not a plan key: nudging it inside a tier
/// changes no pass, no byte, and forces no recompile. Crossing a tier does.
#[test]
fn a_within_tier_sigma_nudge_changes_nothing() {
    let (mut gpu, mut r) = renderer(64, 64);
    let a = blur_frame(&mut r, &mut gpu, 4.0);
    let b = blur_frame(&mut r, &mut gpu, 4.5);
    assert_eq!(a.blur_passes, b.blur_passes);
    assert_eq!(a.blur_target_bytes, b.blur_target_bytes);
    assert_eq!(a.render_passes, b.render_passes);
    assert_eq!(
        b.render_graph_compiles, 0,
        "sigma is not part of the graph topology"
    );

    let c = blur_frame(&mut r, &mut gpu, 24.0);
    assert_eq!(c.render_graph_compiles, 1, "crossing a tier is a new plan");
}

/// The blur ladder reuses the transient pool: a steady blurred scene mints no
/// textures after its first frame, however many rungs it has.
#[test]
fn a_steady_blurred_scene_stops_allocating() {
    let (mut gpu, mut r) = renderer(64, 64);
    let first = blur_frame(&mut r, &mut gpu, 24.0);
    assert!(first.transient_target_allocations > 0);
    for _ in 0..3 {
        let next = blur_frame(&mut r, &mut gpu, 24.0);
        assert_eq!(next.transient_target_allocations, 0);
        assert_eq!(next.transient_targets, first.transient_targets);
        assert_eq!(next.blur_target_bytes, first.blur_target_bytes);
    }
}

/// Blur rungs alias the layer's own base target: the base dies once rung 0 has
/// read it, so a fan-out of blurred siblings does not scale physical textures
/// with the sibling count. Twice the layers must not mean twice the pool.
#[test]
fn blurred_siblings_share_the_pool() {
    let (mut gpu, mut r) = renderer(256, 256);
    let scene = |n: u32| {
        let mut out = Vec::new();
        for i in 0..n {
            let x = (i % 8) as f32 * 32.0;
            let y = (i / 8) as f32 * 32.0;
            out.push(blurred(rect(x, y, 24.0, 24.0), 4.0));
            out.push(quad(rect(x, y, 24.0, 24.0)));
            out.push(Primitive::LayerEnd);
        }
        out
    };

    r.upload(&mut gpu, &scene(8));
    let eight = r.frame_stats();
    r.upload(&mut gpu, &scene(16));
    let sixteen = r.frame_stats();

    assert_eq!(eight.offscreen_passes, 8);
    assert_eq!(sixteen.offscreen_passes, 16);
    assert!(
        sixteen.transient_targets < 2 * eight.transient_targets,
        "physical targets must grow sublinearly in blurred layer count"
    );
    assert!(
        sixteen.transient_peak_bytes <= sixteen.transient_pool_bytes,
        "the live high-water mark never exceeds what the pool holds"
    );
}

// ---------------------------------------------------------------------------
// 3. Transient pool keys + alias-reuse discipline
// ---------------------------------------------------------------------------

fn new_pool() -> (HeadlessRaster, TransientTargets) {
    (HeadlessRaster::new(), TransientTargets::new())
}

fn desc(width: u32, height: u32) -> TargetDesc {
    TargetDesc {
        width,
        height,
        format: TextureFormat::Bgra8Unorm,
        usage: TargetUsage::COLOR_ATTACHMENT,
        samples: 1,
        label: "frozen",
    }
}

/// Assign `pool` against a frame of `timeline_len` texture passes.
fn assign(gpu: &mut HeadlessRaster, pool: &mut TransientTargets, timeline_len: u32) {
    let sampler = gpu.create_sampler(&SamplerDesc::LINEAR_CLAMP);
    pool.assign(gpu, sampler, timeline_len);
}

/// Size classes round *up*, are monotonic, and waste a bounded fraction. Exact
/// extents would make reuse an accident; unbounded rounding would make it
/// expensive. Both halves are the contract.
#[test]
fn size_classes_are_monotonic_with_bounded_waste() {
    for (n, class) in [
        (0u32, 16u32),
        (1, 16),
        (10, 16),
        (16, 16),
        (17, 32),
        (40, 48),
        (80, 80),
        (1000, 1024),
        (1080, 1152),
        (1920, 1920),
    ] {
        assert_eq!(size_class(n), class, "size_class({n})");
    }

    let mut previous = 0;
    for n in 0..4096u32 {
        let class = size_class(n);
        assert!(class >= n.max(1), "a class never shrinks the request");
        assert!(class >= previous, "classes are monotonic in the request");
        assert!(
            class - n.max(1) <= (n / 8).max(16),
            "waste at {n} ({class}) is unbounded"
        );
        previous = class;
    }
}

/// The pool key is exactly these five dimensions — no more (or reuse would be
/// needlessly rare) and no fewer (or two incompatible targets would alias). The
/// literal names every field, so adding or dropping one fails to compile here.
#[test]
fn the_pool_key_is_five_dimensions() {
    let key = TargetKey {
        format: TextureFormat::Bgra8Unorm,
        usage: TargetUsage::COLOR_ATTACHMENT,
        samples: 1,
        width: 64,
        height: 32,
    };
    assert_eq!(key.bytes(), 64 * 32 * BPT);
    assert_eq!(
        TargetKey { samples: 4, ..key }.bytes(),
        4 * key.bytes(),
        "sample count multiplies the footprint"
    );
    assert_ne!(
        key,
        TargetKey {
            format: TextureFormat::Rgba8Unorm,
            ..key
        }
    );
    assert_ne!(
        key,
        TargetKey {
            usage: TargetUsage::SAMPLED,
            ..key
        }
    );

    // Usage is a bit set, and an offscreen layer target is both: the pass writes
    // it, the composite samples it.
    assert!(TargetUsage::COLOR_ATTACHMENT.is_render_target());
    assert!(TargetUsage::COLOR_ATTACHMENT.is_sampled());
    assert!(TargetUsage::RENDER_TARGET.is_render_target());
    assert!(!TargetUsage::RENDER_TARGET.is_sampled());
    assert!(TargetUsage::SAMPLED.is_sampled());
    assert!(!TargetUsage::SAMPLED.is_render_target());
}

/// Two same-key targets alias iff the first is dead before the second is
/// written — `free_at <= first_write`, with `free_at` one past the last read.
/// Aliasing a still-live target would make a blur pass read its own output.
#[test]
fn aliasing_requires_strictly_non_overlapping_lifetimes() {
    // Disjoint: `a` is read at slot 1, `b` is written at slot 2.
    let (mut gpu, mut pool) = new_pool();
    let a = pool.declare(desc(40, 40), 0);
    let b = pool.declare(desc(40, 40), 2);
    pool.read_at(a, 1);
    pool.read_at(b, SURFACE_SLOT);
    assign(&mut gpu, &mut pool, 3);
    assert_eq!(pool.texture(a), pool.texture(b), "dead targets alias");
    assert_eq!(pool.stats().targets, 1);

    // Overlapping by exactly one slot: `a` is still read at slot 2.
    let (mut gpu, mut pool) = new_pool();
    let a = pool.declare(desc(40, 40), 0);
    let b = pool.declare(desc(40, 40), 2);
    pool.read_at(a, 2);
    pool.read_at(b, SURFACE_SLOT);
    assign(&mut gpu, &mut pool, 3);
    assert_ne!(pool.texture(a), pool.texture(b), "live targets never alias");
    assert_eq!(pool.stats().targets, 2);
}

/// Incompatible keys never share, whatever their lifetimes. `phys_extent` is the
/// size class the texture really has; `used_extent` is the sub-rect the frame
/// draws, which is what shader tap clamping must respect.
#[test]
fn incompatible_keys_never_alias() {
    let (mut gpu, mut pool) = new_pool();
    let small = pool.declare(desc(40, 40), 0);
    let wide = pool.declare(desc(200, 40), 1);
    let other = pool.declare(
        TargetDesc {
            format: TextureFormat::Rgba8Unorm,
            ..desc(40, 40)
        },
        2,
    );
    pool.read_at(small, 1);
    pool.read_at(wide, 2);
    pool.read_at(other, SURFACE_SLOT);
    assign(&mut gpu, &mut pool, 3);

    assert_eq!(pool.stats().targets, 3);
    assert_ne!(pool.texture(small), pool.texture(wide));
    assert_ne!(pool.texture(small), pool.texture(other));
    assert_eq!(pool.phys_extent(small), [48, 48]);
    assert_eq!(pool.used_extent(small), [40, 40]);
    assert_eq!(pool.phys_extent(wide), [208, 48]);
    assert_eq!(pool.used_extent(wide), [200, 40]);
}

/// A target the surface pass samples lives to the end of the frame, so nothing
/// written later can take its texture. [`SURFACE_SLOT`] is that "last" marker,
/// resolved against the real timeline length at assignment.
#[test]
fn a_surface_read_extends_a_lifetime_to_the_frame_end() {
    let (mut gpu, mut pool) = new_pool();
    let held = pool.declare(desc(40, 40), 0);
    let later = pool.declare(desc(40, 40), 1);
    pool.read_at(held, SURFACE_SLOT);
    pool.read_at(later, SURFACE_SLOT);
    assign(&mut gpu, &mut pool, 2);
    assert_eq!(pool.stats().targets, 2);
    assert_eq!(pool.stats().peak_bytes, 2 * 48 * 48 * BPT);
}

/// A steady frame allocates nothing, and a physical that stops being claimed is
/// held for a bounded window before release — so a one-frame topology blip does
/// not thrash the allocator, and an abandoned target does not leak.
#[test]
fn the_pool_is_steady_then_retires_idle_physicals() {
    assert_eq!(
        TRANSIENT_TARGET_IDLE_FRAMES, 60,
        "the idle window is part of the frozen contract"
    );

    let (mut gpu, mut pool) = new_pool();
    for frame in 0..3 {
        pool.begin_frame();
        let a = pool.declare(desc(40, 40), 0);
        let b = pool.declare(desc(40, 40), 1);
        pool.read_at(a, SURFACE_SLOT);
        pool.read_at(b, SURFACE_SLOT);
        assign(&mut gpu, &mut pool, 2);
        let stats = pool.stats();
        assert_eq!(stats.targets, 2);
        assert_eq!(stats.allocations, if frame == 0 { 2 } else { 0 });
    }

    // Only one target is needed from here on.
    for _ in 0..TRANSIENT_TARGET_IDLE_FRAMES {
        pool.begin_frame();
        let only = pool.declare(desc(40, 40), 0);
        pool.read_at(only, SURFACE_SLOT);
        assign(&mut gpu, &mut pool, 1);
        assert_eq!(pool.stats().targets, 2, "the spare survives the window");
    }
    pool.begin_frame();
    let only = pool.declare(desc(40, 40), 0);
    pool.read_at(only, SURFACE_SLOT);
    assign(&mut gpu, &mut pool, 1);
    let stats = pool.stats();
    assert_eq!(stats.targets, 1, "then the spare is released");
    assert_eq!(stats.pool_bytes, 48 * 48 * BPT);

    // All four counters exist and are named; a rename fails to compile here.
    let TransientStats {
        peak_bytes,
        pool_bytes,
        targets,
        allocations,
    } = stats;
    assert_eq!(peak_bytes, 48 * 48 * BPT);
    assert_eq!(pool_bytes, 48 * 48 * BPT);
    assert_eq!(targets, 1);
    assert_eq!(allocations, 0);
}

// ---------------------------------------------------------------------------
// 4. RenderGraph responsibilities + compile/reuse
// ---------------------------------------------------------------------------

/// The graph's vocabulary: the work a pass can carry, and the load op a pass can
/// be lowered to. `DontCare` is deliberately absent — every Viso pass clears,
/// which is the cheapest correct load on a tile GPU and needs no reasoning about
/// undefined contents.
#[test]
fn the_graph_vocabulary_is_closed() {
    for work in [PassWork::Offscreen(0), PassWork::Blur(0), PassWork::Surface] {
        match work {
            PassWork::Offscreen(idx) | PassWork::Blur(idx) => assert_eq!(idx, 0),
            PassWork::Surface => {}
        }
    }
    for load in [PassLoad::ClearTransparent, PassLoad::ClearBackground] {
        match load {
            PassLoad::ClearTransparent | PassLoad::ClearBackground => {}
        }
    }
}

/// Load lowering is derived from the attachment, not authored: a transient
/// target starts transparent, the surface starts at the frame's background.
#[test]
fn load_ops_are_derived_from_the_attachment() {
    let mut pool = TransientTargets::new();
    let target = pool.declare(desc(64, 64), 0);
    let mut g = RenderGraph::new();
    g.begin_frame();
    g.open(PassWork::Offscreen(0), Some(target));
    let surface = g.open(PassWork::Surface, None);
    g.read(surface, target);
    g.compile();

    let passes = g.passes();
    assert_eq!(passes.len(), 2);
    assert_eq!(passes[0].writes(), Some(target));
    assert_eq!(passes[0].load(), PassLoad::ClearTransparent);
    assert_eq!(passes[1].writes(), None);
    assert_eq!(passes[1].load(), PassLoad::ClearBackground);
    assert_eq!(g.surface_slot(), 1);
}

/// A pass nothing samples is dropped: the graph owns liveness, so an upstream
/// slice may record speculatively without the backend paying for it.
#[test]
fn the_graph_culls_passes_nothing_reads() {
    let mut pool = TransientTargets::new();
    let dead = pool.declare(desc(64, 64), 0);
    let mut g = RenderGraph::new();
    g.begin_frame();
    g.open(PassWork::Offscreen(0), Some(dead));
    g.open(PassWork::Surface, None);
    g.compile();

    let GraphStats {
        passes,
        merges,
        culled,
        compiles,
    } = g.stats();
    assert_eq!(passes, 1, "only the surface survives");
    assert_eq!(merges, 0);
    assert_eq!(culled, 1);
    assert_eq!(compiles, 1);
}

/// The graph drives transient lifetimes: it, not the renderer, tells the pool
/// which slot writes and which slot last reads each target. Two blurred layers
/// therefore need three textures, not four — a layer's base target dies as soon
/// as its first rung has sampled it, so the next layer's base takes it over,
/// while the rung the surface samples is held to the end of the frame.
#[test]
fn the_graph_drives_transient_lifetimes() {
    let (mut gpu, mut pool) = new_pool();
    let mut g = RenderGraph::new();
    g.begin_frame();

    let mut bases = Vec::new();
    let mut rungs = Vec::new();
    for i in 0..2u32 {
        let base = pool.declare(desc(40, 40), g.next_slot());
        g.open(PassWork::Offscreen(i), Some(base));
        let rung_target = pool.declare(desc(40, 40), g.next_slot());
        let rung = g.open(PassWork::Blur(i), Some(rung_target));
        g.read(rung, base);
        bases.push(base);
        rungs.push(rung_target);
    }
    let surface = g.open(PassWork::Surface, None);
    for rung in &rungs {
        g.read(surface, *rung);
    }
    g.compile();
    g.apply_lifetimes(&mut pool);
    assign(&mut gpu, &mut pool, 4);

    assert_eq!(g.stats().passes, 5);
    assert_eq!(g.stats().culled, 0);
    assert_ne!(
        pool.texture(bases[0]),
        pool.texture(rungs[0]),
        "a rung must never alias the base it is reading"
    );
    assert_eq!(
        pool.texture(bases[0]),
        pool.texture(bases[1]),
        "the graph's read slots are what let the second base reuse the first"
    );
    assert_eq!(
        pool.stats().targets,
        3,
        "four virtuals over three physicals"
    );
}

/// The plan is cached on topology alone. Everything that is *not* topology —
/// position, color, extent, surface size, within-tier sigma — must reuse it, or
/// a scrolling or animating scene pays a graph rebuild every frame.
#[test]
fn only_topology_forces_a_recompile() {
    let (mut gpu, mut r) = renderer(256, 256);
    let scene = |x: f32, color: Rgba, sigma: f32, n: u32| {
        let mut out = vec![blurred(rect(x, 0.0, 64.0, 64.0), sigma)];
        for i in 0..n {
            out.push(Primitive::Quad(Quad {
                rect: rect(x, i as f32 * 8.0, 64.0, 8.0),
                color,
                radius: 0.0,
                border: Border::NONE,
            }));
        }
        out.push(Primitive::LayerEnd);
        out
    };
    let red = Rgba::new(1.0, 0.0, 0.0, 1.0);
    let blue = Rgba::new(0.0, 0.0, 1.0, 1.0);

    r.upload(&mut gpu, &scene(0.0, red, 4.0, 2));
    assert_eq!(
        r.frame_stats().render_graph_compiles,
        1,
        "the first frame has no cached plan"
    );

    for (label, primitives) in [
        ("identical", scene(0.0, red, 4.0, 2)),
        ("moved", scene(32.0, red, 4.0, 2)),
        ("recolored", scene(32.0, blue, 4.0, 2)),
        ("sigma nudged within a tier", scene(32.0, blue, 6.0, 2)),
    ] {
        r.upload(&mut gpu, &primitives);
        assert_eq!(
            r.frame_stats().render_graph_compiles,
            0,
            "{label} must reuse the cached plan"
        );
    }

    // A resize changes every extent and no pass.
    r.set_surface_size([512.0, 512.0]);
    r.upload(&mut gpu, &scene(32.0, blue, 6.0, 2));
    assert_eq!(
        r.frame_stats().render_graph_compiles,
        0,
        "a resize is not a topology change"
    );

    // Crossing a blur tier changes the rung count, which is topology.
    r.upload(&mut gpu, &scene(32.0, blue, 24.0, 2));
    assert_eq!(r.frame_stats().render_graph_compiles, 1);

    // Adding a layer adds a pass, which is topology.
    let mut two_layers = scene(32.0, blue, 24.0, 2);
    two_layers.extend(scene(128.0, blue, 24.0, 2));
    r.upload(&mut gpu, &two_layers);
    assert_eq!(r.frame_stats().render_graph_compiles, 1);
    r.upload(&mut gpu, &two_layers);
    assert_eq!(r.frame_stats().render_graph_compiles, 0);
}

/// A real frame merges and culls nothing: every pass writes a distinct
/// attachment that something downstream samples. Nonzero merges/culls in this
/// shape would mean the renderer planned work it did not need.
#[test]
fn a_real_frame_plans_exactly_the_passes_it_needs() {
    let (mut gpu, mut r) = renderer(128, 128);
    r.upload(
        &mut gpu,
        &[
            blurred(rect(0.0, 0.0, 64.0, 64.0), 4.0),
            quad(rect(0.0, 0.0, 64.0, 64.0)),
            Primitive::LayerEnd,
            layer(rect(64.0, 0.0, 64.0, 64.0), 0.5),
            quad(rect(64.0, 0.0, 64.0, 64.0)),
            Primitive::LayerEnd,
        ],
    );
    let stats = r.frame_stats();
    assert_eq!(stats.offscreen_passes, 2);
    assert_eq!(stats.blur_passes, 2);
    assert_eq!(
        stats.render_passes,
        stats.offscreen_passes + stats.blur_passes as usize + 1
    );
    assert_eq!(stats.render_pass_merges, 0);
    assert_eq!(stats.culled_render_passes, 0);
}

/// `TargetId` is an opaque index handle, not a texture: the pool decides which
/// physical backs it, and the same id can back different textures across frames.
#[test]
fn target_ids_are_opaque_indices() {
    let mut pool = TransientTargets::new();
    let a: TargetId = pool.declare(desc(40, 40), 0);
    let b: TargetId = pool.declare(desc(40, 40), 1);
    assert_eq!(a.index(), 0);
    assert_eq!(b.index(), 1);
    assert_ne!(a, b);
}
