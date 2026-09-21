//! D0 frozen-contract pin: the frame counter roster (§30/§61) and the effect
//! cost classification (§7.5/§62) the whole draw layer above D0 reports through.
//!
//! `FrameStats` is the renderer's public performance surface — Studio, the
//! inspector, the steady-state bench, and every later layer's own gate read it
//! by field. `EffectCost` is the per-drawable cost class the inspector shows and
//! the planner will branch on. D0 freezes both: the set of counters and the set
//! of cost classes are the contract D1→A0 extend but never silently reshape.
//!
//! The steady-state bench already exercises the counters against a live data
//! path (one-slot hover, transform-only scroll), but a bench is not part of
//! `cargo test --workspace`; this integration test is the `cargo test` gate that
//! fails loudly the moment a counter is dropped/renamed or a cost class moves.
//! Two halves:
//!   1. the counter *roster* — every §30 counter is present, named, and zero by
//!      default (a construction with all fields named breaks to compile if one is
//!      dropped or renamed);
//!   2. the cost *classification* — the seven classes, their cheapest-first
//!      order, their labels, and the "dominating = max link" rule.

use viso_render::{EffectCost, FrameStats};

/// The §30/§61 counter roster is frozen: `FrameStats` carries exactly these
/// fields, each an integer counter that defaults to zero. Naming every field in
/// a struct literal means dropping or renaming any counter fails to compile —
/// this is the single place that has to change (deliberately) when the roster
/// grows, so a silent shift is impossible.
#[test]
fn frame_stats_roster_is_frozen() {
    // Construct with every field named and non-default, then read each back. A
    // dropped field → missing initializer (compile error); a renamed field →
    // unknown field (compile error); an added field → missing initializer here
    // (forces a deliberate update). The values are arbitrary distinct markers.
    let s = FrameStats {
        // CPU dispatch shape.
        draw_calls: 1,
        instances: 2,
        batches: 3,
        render_chunks: 4,
        pipeline_switches: 5,
        texture_binding_switches: 6,
        // Retained-scene ingest tallies (§8.4/§61).
        visible_primitives: 7,
        culled_primitives: 8,
        dirty_primitives: 9,
        quad_instances: 10,
        glyph_instances: 11,
        path_tessellations: 12,
        // Data-path / upload discipline (§9.1/§9.3/§30).
        uploaded_ranges: 13,
        instance_rebuilds: 14,
        clip_mask_builds: 15,
        gpu_upload_bytes: 16,
        // Offscreen / pipeline lifetime (§30).
        offscreen_passes: 17,
        transient_target_bytes: 18,
        blur_passes: 19,
        blur_target_bytes: 20,
        backdrop_captures: 30,
        backdrop_capture_pixels: 31,
        shader_pipeline_creations: 21,
        // Transient render-target pool occupancy (§16.4/§30).
        transient_targets: 22,
        transient_peak_bytes: 23,
        transient_pool_bytes: 24,
        transient_target_allocations: 25,
        // Compiled pass plan (§16.1/§25).
        render_passes: 26,
        render_pass_merges: 27,
        culled_render_passes: 28,
        render_graph_compiles: 29,
    };

    // Every field reads back what it was set to — a plain integer counter, no
    // derivation, no aliasing between fields.
    assert_eq!(s.draw_calls, 1);
    assert_eq!(s.instances, 2);
    assert_eq!(s.batches, 3);
    assert_eq!(s.render_chunks, 4);
    assert_eq!(s.pipeline_switches, 5);
    assert_eq!(s.texture_binding_switches, 6);
    assert_eq!(s.visible_primitives, 7);
    assert_eq!(s.culled_primitives, 8);
    assert_eq!(s.dirty_primitives, 9);
    assert_eq!(s.quad_instances, 10);
    assert_eq!(s.glyph_instances, 11);
    assert_eq!(s.path_tessellations, 12);
    assert_eq!(s.uploaded_ranges, 13);
    assert_eq!(s.instance_rebuilds, 14);
    assert_eq!(s.clip_mask_builds, 15);
    assert_eq!(s.gpu_upload_bytes, 16);
    assert_eq!(s.offscreen_passes, 17);
    assert_eq!(s.transient_target_bytes, 18);
    assert_eq!(s.blur_passes, 19);
    assert_eq!(s.blur_target_bytes, 20);
    assert_eq!(s.backdrop_captures, 30);
    assert_eq!(s.backdrop_capture_pixels, 31);
    assert_eq!(s.shader_pipeline_creations, 21);
    assert_eq!(s.transient_targets, 22);
    assert_eq!(s.transient_peak_bytes, 23);
    assert_eq!(s.transient_pool_bytes, 24);
    assert_eq!(s.transient_target_allocations, 25);
    assert_eq!(s.render_passes, 26);
    assert_eq!(s.render_pass_merges, 27);
    assert_eq!(s.culled_render_passes, 28);
    assert_eq!(s.render_graph_compiles, 29);

    // The default is the all-zero frame: a renderer that drew nothing reports
    // every counter at zero, so a steady frame's deltas are meaningful.
    let z = FrameStats::default();
    assert_eq!(z.draw_calls, 0);
    assert_eq!(z.instances, 0);
    assert_eq!(z.batches, 0);
    assert_eq!(z.render_chunks, 0);
    assert_eq!(z.pipeline_switches, 0);
    assert_eq!(z.texture_binding_switches, 0);
    assert_eq!(z.visible_primitives, 0);
    assert_eq!(z.culled_primitives, 0);
    assert_eq!(z.dirty_primitives, 0);
    assert_eq!(z.quad_instances, 0);
    assert_eq!(z.glyph_instances, 0);
    assert_eq!(z.path_tessellations, 0);
    assert_eq!(z.uploaded_ranges, 0);
    assert_eq!(z.instance_rebuilds, 0);
    assert_eq!(z.clip_mask_builds, 0);
    assert_eq!(z.gpu_upload_bytes, 0);
    assert_eq!(z.offscreen_passes, 0);
    assert_eq!(z.transient_target_bytes, 0);
    assert_eq!(z.blur_passes, 0);
    assert_eq!(z.blur_target_bytes, 0);
    assert_eq!(z.backdrop_captures, 0);
    assert_eq!(z.backdrop_capture_pixels, 0);
    assert_eq!(z.shader_pipeline_creations, 0);
    assert_eq!(z.transient_targets, 0);
    assert_eq!(z.transient_peak_bytes, 0);
    assert_eq!(z.transient_pool_bytes, 0);
    assert_eq!(z.transient_target_allocations, 0);
    assert_eq!(z.render_passes, 0);
    assert_eq!(z.render_pass_merges, 0);
    assert_eq!(z.culled_render_passes, 0);
    assert_eq!(z.render_graph_compiles, 0);
}

/// The seven effect cost classes are frozen in cheapest-first order (§7.5): the
/// discriminant is a monotonic cost rank, `default()` is the cheapest class, and
/// `Ord` agrees with realization cost. A later layer that adds a class must place
/// it at the right rank and update this list deliberately.
#[test]
fn effect_cost_classes_are_frozen() {
    let order = [
        EffectCost::Local,
        EffectCost::Analytic,
        EffectCost::NeedsMask,
        EffectCost::NeedsOffscreen,
        EffectCost::NeedsBackdrop,
        EffectCost::DestinationRead,
        EffectCost::ComputePreferred,
    ];
    for pair in order.windows(2) {
        assert!(pair[0] < pair[1], "{:?} ranks below {:?}", pair[0], pair[1]);
    }
    assert_eq!(
        EffectCost::default(),
        EffectCost::Local,
        "the cheapest class is the default"
    );

    // The label roster is the inspector/overlay contract (§62): a stable,
    // distinct lowercase name per class.
    let labels = [
        (EffectCost::Local, "local"),
        (EffectCost::Analytic, "analytic"),
        (EffectCost::NeedsMask, "needs-mask"),
        (EffectCost::NeedsOffscreen, "needs-offscreen"),
        (EffectCost::NeedsBackdrop, "needs-backdrop"),
        (EffectCost::DestinationRead, "destination-read"),
        (EffectCost::ComputePreferred, "compute-preferred"),
    ];
    for (class, label) in labels {
        assert_eq!(class.label(), label, "{class:?} label is frozen");
    }
}

/// The cost-class predicates and the dominating rule are frozen: they are what
/// the planner reads to decide a frame's mask/offscreen/backdrop resources, and
/// a chain's cost is the maximum (cheapest-first) over its links (§7.5).
#[test]
fn effect_cost_semantics_are_frozen() {
    // Resource predicates map each class to the frame resource it forces.
    assert!(EffectCost::NeedsMask.needs_mask());
    assert!(EffectCost::NeedsOffscreen.needs_offscreen());
    assert!(EffectCost::NeedsBackdrop.needs_offscreen());
    assert!(EffectCost::NeedsBackdrop.needs_backdrop());
    assert!(EffectCost::DestinationRead.reads_destination());
    assert!(!EffectCost::Local.needs_mask());
    assert!(!EffectCost::Local.needs_offscreen());
    assert!(!EffectCost::Local.reads_destination());

    // A chain's dominating cost is its most expensive link; an empty chain is
    // the cheapest class.
    assert_eq!(EffectCost::dominating([]), EffectCost::Local);
    assert_eq!(
        EffectCost::dominating([
            EffectCost::Local,
            EffectCost::NeedsMask,
            EffectCost::Analytic,
        ]),
        EffectCost::NeedsMask,
    );
}
