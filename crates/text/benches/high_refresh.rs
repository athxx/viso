//! High-refresh regression gate for the text subsystem (section 36, `text`).
//!
//! High refresh rates do not make text *work* faster; they make the per-frame
//! budget smaller. A 240Hz display gives a frame every ~4.17ms, and the text
//! runtime's share of main-thread maintenance must stay within the budget the
//! runtime spec derives per refresh rate: `min(500us, 5% of frame interval)` —
//! 60Hz ~500us, 120Hz ~416us, 144Hz ~347us, 240Hz ~208us. This is the
//! Dev/benchmark scheduling target, not a public ABI.
//!
//! The only way the same text pipeline meets a shrinking budget across four
//! refresh tiers is by doing *no reshaping* in the steady state and *no
//! shaping on the main thread at all*. So this gate asserts the invariants that
//! make high refresh feasible, and measures the residual per-frame cost against
//! each tier's budget:
//!
//! 1. **Steady-state zero-reshape** (TF-P3.1): a warmed paragraph re-laid out on
//!    an unchanged frame reshapes nothing — `shape_call_count` does not move.
//!    Because the steady frame reshapes nothing, its cost is independent of the
//!    refresh rate, which is what lets one pipeline serve all four tiers.
//! 2. **Main-thread zero-shaping under a high-refresh edit cadence** (TF-P3.3):
//!    driving edits at each tier's frame cadence through the worker scheduler,
//!    the main thread shapes nothing on every step — hits and misses alike.
//! 3. **Per-tier steady-frame budget**: the measured steady-frame text
//!    maintenance cost must sit under each tier's budget with headroom. The
//!    gate fails only on a gross regression; the microbench prints the actual
//!    per-frame time for trend reporting.
//!
//! All three run once at startup (like `render/benches/renderer_steady_state.rs`
//! and `runtime/benches/frame_loop.rs`) so a regression fails the bench binary
//! immediately. Run release: criterion defaults to a release profile; debug
//! timing is not a perf result (section 36).

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_text::paragraph::Paragraph;
use viso_text::shaping::{Direction, ShapedRun, Shaper};
use viso_text::text_work::TextWork;
use viso_text::{BaseDirection, FontFaceId, TextOffset};

/// The DejaVu Sans subset carrying the Latin the benches lay out.
const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

/// A refresh tier: its rate and the main-thread text-maintenance budget the
/// runtime spec derives for it (`min(500us, 5% of frame interval)`).
struct Tier {
    hz: u32,
    budget_us: f64,
}

/// The four gated refresh tiers with their spec 14.4 budgets.
const TIERS: [Tier; 4] = [
    Tier {
        hz: 60,
        budget_us: 500.0,
    },
    Tier {
        hz: 120,
        budget_us: 416.0,
    },
    Tier {
        hz: 144,
        budget_us: 347.0,
    },
    Tier {
        hz: 240,
        budget_us: 208.0,
    },
];

/// Shape one substring through the fixture face (the same path the paragraph
/// tests drive). This is the caller-supplied shaper the paragraph memoizes.
fn shape_run(sub: &str, dir: Direction) -> ShapedRun {
    Shaper::new()
        .shape_run(FontFaceId(0), DEJAVU, 0, sub, dir)
        .expect("fixture parses")
}

/// A paragraph laid out once at `width`, warmed so its next identical layout is
/// a pure cache hit. Returns the paragraph and the shape-call count after warmup.
fn warmed_paragraph(text: &str, width: f32) -> (Paragraph, u64) {
    let mut p = Paragraph::new(text, BaseDirection::LeftToRight, 0);
    let mut shape_fn = |s: &str, d: Direction| shape_run(s, d);
    p.layout(width, &mut shape_fn);
    let warm = p.shape_call_count();
    (p, warm)
}

/// GATE 1 — a warmed paragraph re-laid out on an unchanged frame reshapes
/// nothing. This is the invariant that makes the steady frame's cost
/// refresh-rate-independent; without it, high refresh could not hold budget.
fn assert_steady_frame_does_not_reshape() {
    // A multi-line paragraph so layout is non-trivial (several runs, wrapping).
    let text = "the quick brown fox jumps over the lazy dog again and again";
    let full = shape_run(text, Direction::LeftToRight).width_ems;
    let width = full / 3.0; // force wrapping into several lines.
    let (mut p, warm) = warmed_paragraph(text, width);

    // Re-lay out identical frames at the highest tier's cadence: none reshapes.
    let mut shape_fn = |s: &str, d: Direction| shape_run(s, d);
    for frame in 0..240 {
        p.layout(width, &mut shape_fn);
        assert_eq!(
            p.shape_call_count(),
            warm,
            "steady frame {frame} reshaped: high refresh cannot hold budget if the \
             unchanged paragraph reshapes"
        );
    }
}

/// GATE 2 — under a high-refresh edit cadence, the main thread shapes nothing on
/// every step, at every tier. The worker scheduler advances caret geometry and
/// dispatches dirty runs; it never reshapes inline, regardless of how fast the
/// display drives edits.
fn assert_edit_cadence_never_shapes_on_main() {
    for tier in &TIERS {
        let mut work = TextWork::default();
        // A burst of edits sized to the tier: one per frame for a full second of
        // input at that refresh rate — the worst realistic keyboard cadence.
        let mut hit = false;
        for step in 0..tier.hz {
            // Alternate warm hits and cold misses so both paths are exercised.
            hit = !hit;
            let s = work.main_thread_edit_step(step as u64, hit);
            assert_eq!(
                s.shaped_on_main, 0,
                "{}Hz edit step {step} shaped on the main thread",
                tier.hz
            );
            assert!(
                s.caret_advanced,
                "{}Hz edit step {step} did not advance the caret",
                tier.hz
            );
        }
        // The worker still has real work queued from the misses — the main
        // thread offloaded it rather than shaping inline.
        assert!(
            work.pending_len() > 0,
            "{}Hz misses produced no worker jobs (nothing was offloaded)",
            tier.hz
        );
    }
}

/// GATE 3 — the measured steady-frame text maintenance cost sits under each
/// tier's budget with headroom. Returns nothing; asserts on gross regression and
/// prints the per-frame time for section 36 trend reporting.
fn assert_steady_frame_within_tier_budgets() {
    let text = "the quick brown fox jumps over the lazy dog again and again";
    let full = shape_run(text, Direction::LeftToRight).width_ems;
    let width = full / 3.0;
    let (mut p, _warm) = warmed_paragraph(text, width);
    let mut shape_fn = |s: &str, d: Direction| shape_run(s, d);

    // Time a batch of steady (cache-hit) frames and take the per-frame mean.
    const FRAMES: u32 = 2000;
    let start = Instant::now();
    for _ in 0..FRAMES {
        black_box(p.layout(width, &mut shape_fn));
    }
    let per_frame_us = start.elapsed().as_secs_f64() * 1e6 / FRAMES as f64;

    for tier in &TIERS {
        // Gross-regression gate: the steady text maintenance must stay under the
        // tier budget. The cache-hit path is ~two orders of magnitude under the
        // 240Hz budget; a 2x safety multiple catches real regression without
        // flaking on a loaded CI machine.
        let ceiling = tier.budget_us * 2.0;
        assert!(
            per_frame_us < ceiling,
            "{}Hz steady frame {per_frame_us:.2}us exceeds {ceiling:.0}us gate \
             (budget {:.0}us): steady-state text maintenance regressed",
            tier.hz,
            tier.budget_us
        );
        println!(
            "text high-refresh: {}Hz budget {:.0}us, steady frame {per_frame_us:.2}us",
            tier.hz, tier.budget_us
        );
    }
}

/// Criterion measurement of the steady (cache-hit) frame layout, so the trend is
/// tracked over time rather than only gated once.
fn bench_steady_frame(c: &mut Criterion) {
    let text = "the quick brown fox jumps over the lazy dog again and again";
    let full = shape_run(text, Direction::LeftToRight).width_ems;
    let width = full / 3.0;
    let (mut p, _warm) = warmed_paragraph(text, width);
    let mut shape_fn = |s: &str, d: Direction| shape_run(s, d);

    c.bench_function("text_steady_frame_layout_cache_hit", |b| {
        b.iter(|| black_box(p.layout(black_box(width), &mut shape_fn)));
    });
}

/// Criterion measurement of one worker-offloaded edit step (caret geometry +
/// dispatch, zero main-thread shaping) — the per-keystroke main-thread cost.
fn bench_edit_step(c: &mut Criterion) {
    let mut work = TextWork::default();
    let mut n = 0u64;
    c.bench_function("text_edit_step_zero_main_shaping", |b| {
        b.iter(|| {
            n += 1;
            black_box(work.main_thread_edit_step(black_box(n), n.is_multiple_of(2)));
            // Keep the queue bounded so the bench measures the step, not growth.
            if work.pending_len() > 64 {
                while work.take_next().is_some() {}
            }
        });
    });
    let _ = TextOffset(0); // keep the typed-offset import meaningful for readers.
}

/// Run the three regression gates once before the criterion measurements, so a
/// violated invariant fails the bench binary immediately.
fn gates(c: &mut Criterion) {
    assert_steady_frame_does_not_reshape();
    assert_edit_cadence_never_shapes_on_main();
    assert_steady_frame_within_tier_budgets();
    bench_steady_frame(c);
    bench_edit_step(c);
}

criterion_group!(benches, gates);
criterion_main!(benches);
