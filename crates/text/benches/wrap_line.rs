//! Width-aware line-breaking microbenchmarks and the wrapping-path allocation
//! invariant (§7.3, §20, §36 `text`).
//!
//! Two things are measured here, both through the public API (benches are an
//! external crate and cannot touch `pub(crate)` internals, so we drive
//! [`layout`] directly with the fixture font):
//!
//! 1. The per-call cost of laying out a paragraph with soft wrapping enabled
//!    (`Some(width)`) against the unwrapped baseline (`None`), so the wrapping
//!    overhead — one whole-line measure shape plus one shape per accepted row —
//!    has a tracked number and a regression is visible.
//! 2. An assertion that the wrapping path shapes each byte a bounded number of
//!    times, not once per break candidate: allocations for wrapping a paragraph
//!    stay within a small constant factor of the unwrapped layout of the same
//!    text (§20 "shape once for measure, once per row"). A per-candidate reshape
//!    would scale with the number of break opportunities and blow this budget.
//!
//! Run release (`cargo bench -p viso-text`); criterion defaults to a release
//! profile. Debug timing is not a perf result (§36).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use viso_text::{FontId, FontStore, layout};

/// A global allocator that counts heap allocations while `ARMED`, so the
/// wrapping path's allocation behavior can be asserted directly. Off by default
/// so criterion's own allocations are never counted.
struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static ARMED: AtomicBool = AtomicBool::new(false);

// SAFETY: forwards every call to the system allocator unchanged; the only added
// behavior is a relaxed counter increment on allocation while armed.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// The embedded ASCII-subset test font, the same fixture the integration tests
/// use — kept in-tree so the bench needs no system font and stays deterministic.
const FONT: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

/// A store with the fixture loaded as its primary face.
fn store() -> (FontStore, FontId) {
    let mut store = FontStore::new();
    let id = store.load(FONT.to_vec(), 0).expect("fixture font parses");
    (store, id)
}

/// A multi-word paragraph long enough to wrap into many rows at the bench width,
/// exercising the word-boundary break path repeatedly (the common case).
const PARAGRAPH: &str = "the quick brown fox jumps over the lazy dog while a \
    sphinx of black quartz judges the vow and five boxing wizards jump quickly";

/// Font size and wrap width for the measured layouts. The width fits a handful
/// of words per row, so the paragraph breaks into roughly a dozen rows.
const SIZE: f32 = 16.0;
const WRAP: f32 = 120.0;

fn wrap_line(c: &mut Criterion) {
    assert_wrapping_allocation_is_bounded();

    let (store, font) = store();

    // Baseline: the width-unaware path (one row, no break search).
    c.bench_function("layout_no_wrap", |b| {
        b.iter(|| {
            let out = layout(
                black_box(&store),
                black_box(font),
                black_box(PARAGRAPH),
                black_box(SIZE),
                None,
            );
            black_box(out);
        });
    });

    // Wrapped: split into rows no wider than WRAP.
    c.bench_function("layout_wrap", |b| {
        b.iter(|| {
            let out = layout(
                black_box(&store),
                black_box(font),
                black_box(PARAGRAPH),
                black_box(SIZE),
                Some(black_box(WRAP)),
            );
            black_box(out);
        });
    });
}

/// The allocation invariant: wrapping shapes each byte a bounded number of times
/// (one measure shape + one shape per row), so its allocation count stays within
/// a small constant factor of the unwrapped layout of the same paragraph. This
/// runs once at startup so a per-candidate-reshape regression fails the bench
/// binary immediately, mirroring `render/benches/renderer_steady_state.rs`.
fn assert_wrapping_allocation_is_bounded() {
    let (store, font) = store();

    // Warm any lazy one-time state (face parse caches) before arming, so the
    // measured counts reflect only the layout work, not first-touch setup.
    let _ = layout(&store, font, PARAGRAPH, SIZE, None);
    let _ = layout(&store, font, PARAGRAPH, SIZE, Some(WRAP));

    ARMED.store(true, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    let unwrapped = layout(&store, font, PARAGRAPH, SIZE, None);
    let base = ALLOCS.load(Ordering::Relaxed);

    ALLOCS.store(0, Ordering::Relaxed);
    let wrapped = layout(&store, font, PARAGRAPH, SIZE, Some(WRAP));
    let wrap_allocs = ALLOCS.load(Ordering::Relaxed);
    ARMED.store(false, Ordering::Relaxed);

    // Wrapping produces more rows (hence more glyphs and per-row shape buffers)
    // than the single unwrapped line, but the factor is bounded by the row count,
    // not the break-candidate count. An 8x ceiling over the unwrapped baseline
    // catches a regression to per-candidate reshaping (which scales with the
    // ~two dozen word boundaries) while leaving headroom for the legitimate
    // per-row allocations.
    let ceiling = base * 8 + 64;
    assert!(
        wrap_allocs <= ceiling,
        "wrapping allocated {wrap_allocs}, expected <= {ceiling} \
         (unwrapped baseline {base}); a per-candidate reshape regressed the \
         shape-once-per-row invariant (§20)"
    );

    // Sanity: wrapping really did split the paragraph into multiple rows, so the
    // bound above is meaningful and not trivially satisfied by a no-op path.
    let rows = wrapped
        .iter()
        .map(|g| g.origin_px[1].to_bits())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert!(
        rows > 1,
        "the paragraph wrapped into multiple rows, got {rows}"
    );
    assert!(
        !unwrapped.is_empty() && !wrapped.is_empty(),
        "both layouts placed glyphs"
    );
}

criterion_group!(benches, wrap_line);
criterion_main!(benches);
