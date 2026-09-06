//! The per-frame cost of the one-shot [`TimerRegistry`] hot seam: the `earliest`
//! query the scheduler runs every idle frame to compute its `WaitUntil` deadline,
//! and the `fire_due` pass the driver runs at each frame head.
//!
//! Section 36 lists no timer category (the timer store is new with Tier 4's
//! `Toast`), but the registry sits on two per-frame paths and section 28 forbids
//! hidden allocation on them, so it needs its own floor:
//!
//! - `earliest` is a flat min over live deadlines — consulted every idle frame to
//!   decide how long the loop blocks (section 7.1's zero-CPU-when-idle contract
//!   rides on it), so its cost is paid whenever any overlay is pending.
//! - `fire_due` is a reverse walk that liveness-checks each timer and fires the
//!   ones whose deadline has passed — run once at every frame head that observes a
//!   deadline, and (via a beat) the frame that a timer's deadline wakes.
//! - `arm` is the one-off push a control pays when it shows; not per-frame, but
//!   measured so a regression in the amortized grow path is visible.
//!
//! Like `animation_tick`, the timed `fire_due` loop uses a `now` *before* every
//! deadline: criterion runs the closure hundreds of millions of times, and any
//! `now` past a deadline would fire-and-remove timers until the registry drained,
//! understating the real cost. A `now` before all deadlines keeps every timer
//! permanently live while exercising the identical steady path (reverse walk,
//! liveness check, deadline compare) — only the never-taken fire branch differs,
//! and a fire is a one-shot outside this steady-cost category. `earliest` is a
//! pure read, so it needs no such guard.
//!
//! It drives only `viso-ui` and adds no dependency edge. Run release
//! (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-ui`); criterion defaults
//! to a release profile. Debug timing is not a perf result (AGENTS section 36).

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{BuildCx, LeafStyle, NodeId, NodeStore, Rect, Size, TimerRegistry};

/// The surface the anchor leaf lays out against — a timer must scope to a live
/// node, and `fire_due` liveness-checks that node every pass.
const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 200.0,
    h: 200.0,
};

/// The per-timer delay, spread so `earliest` walks distinct deadlines rather than
/// a single repeated value. Large enough that the `fire_due` `now` (the arming
/// instant) never crosses any of them, keeping the timed loop permanently
/// populated.
const DELAY: Duration = Duration::from_secs(3600);

/// A single 100x100 leaf laid out at the surface origin — the live node every
/// timer scopes to, so `fire_due`'s liveness check hits a real live handle.
fn one_leaf() -> (NodeStore, NodeId) {
    let mut store = NodeStore::new();
    let root = {
        let mut cx = BuildCx::new(&mut store);
        let h = cx.leaf(LeafStyle {
            size: Size::fixed(100.0, 100.0),
            ..Default::default()
        });
        h.id()
    };
    let mut scratch = Vec::new();
    store.layout(root, SURFACE, &mut scratch);
    (store, root)
}

/// A registry with `n` no-op timers armed on `node` at `now`, each a little
/// further out than the last. Paired with a `fire_due` `now` of the same `now`,
/// none is due, so every timer stays live across the whole timed loop.
fn registry_for(node: NodeId, now: Instant, n: usize) -> TimerRegistry {
    let mut reg = TimerRegistry::new();
    for i in 0..n {
        reg.arm(node, DELAY + Duration::from_millis(i as u64), now, |_| {});
    }
    reg
}

fn bench_timer(c: &mut Criterion) {
    // arm: the one-off push a control pays when it shows. Rebuilt fresh each
    // iteration so the timed cost is a single arm (grow amortized), not a
    // grow-without-bound. Uses `arm` with a fresh boxed no-op closure, matching
    // what `EventCx::request_timer` -> `arm_request` boxes on the show path.
    {
        let (_store, node) = one_leaf();
        let now = Instant::now();
        c.bench_function("timer/arm", |b| {
            b.iter(|| {
                let mut reg = TimerRegistry::new();
                reg.arm(node, DELAY, now, |_| {});
                black_box(&reg);
            });
        });
    }

    // earliest: the flat min the scheduler runs every idle frame to size its
    // `WaitUntil`. Measured at one and many live timers — the cost when a lone
    // overlay is pending and when a screenful are.
    for &n in &[1usize, 64, 1024] {
        let (_store, node) = one_leaf();
        let now = Instant::now();
        let reg = registry_for(node, now, n);
        assert_eq!(reg.len(), n);
        c.bench_function(&format!("timer/earliest/{n}"), |b| {
            b.iter(|| {
                black_box(reg.earliest());
            });
        });
    }

    // fire_due: the frame-head pass. `now` is the arming instant, before every
    // deadline, so nothing fires and the registry stays full across the loop —
    // measuring the steady reverse-walk + liveness-check + deadline-compare cost,
    // the floor every frame that runs `fire_due` pays. Measured at one and many
    // live timers.
    for &n in &[1usize, 64, 1024] {
        let (mut store, node) = one_leaf();
        let now = Instant::now();
        let mut reg = registry_for(node, now, n);
        assert_eq!(reg.len(), n);
        c.bench_function(&format!("timer/fire_due/{n}"), |b| {
            b.iter(|| {
                reg.fire_due(&mut store, now);
                black_box(&reg);
            });
        });
        // The guarded `now` fired nothing: every timer is still live afterward.
        assert_eq!(reg.len(), n, "no timer was due at the arming instant");
    }
}

criterion_group!(benches, bench_timer);
criterion_main!(benches);
