//! Paragraph-cache microbenchmarks and the cache-hit allocation invariant
//! (§7.3, §20, §36 `text`).
//!
//! Two things are measured, both through the public [`TextSystem::prepare`] API
//! (benches are an external crate and cannot touch `pub(crate)` internals):
//!
//! 1. The per-call cost of a *cache hit* — re-preparing an already-laid-out
//!    paragraph — against a *cache miss* that reshapes and re-linebreaks from
//!    scratch. The hit should be a small fraction of the miss (it clones an `Rc`
//!    to the cached glyph positions and only re-walks the atlas), so the win the
//!    cache buys has a tracked number and a regression is visible.
//! 2. An assertion that a cache hit does **not** reshape: its allocation count
//!    stays far below a forced miss of the same paragraph (§20 "text/width
//!    unchanged → no reshape/re-linebreak"). A regression that reshaped on every
//!    prepare — losing the cache — would blow this budget.
//!
//! To force a miss on demand the bench bumps the wrap width by one integer pixel
//! each iteration: a new width is a new cache key, so `layout` runs every time.
//! The hit path re-prepares one fixed paragraph, so after the first call every
//! subsequent one hits.
//!
//! Run release (`cargo bench -p viso-text`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use criterion::{Criterion, criterion_group, criterion_main};
use viso_text::{FontId, TextSystem};

/// A global allocator that counts heap allocations while `ARMED`, so the cache
/// hit's allocation behavior can be asserted directly. Off by default so
/// criterion's own allocations are never counted.
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
/// and the wrap bench use — kept in-tree so the bench needs no system font.
const FONT: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

/// A text system with the fixture loaded as its primary face, plus that id.
fn system() -> (TextSystem, FontId) {
    let mut sys = TextSystem::new();
    let id = sys
        .load_font(FONT.to_vec(), 0)
        .expect("fixture font parses");
    (sys, id)
}

/// A multi-word paragraph long enough to wrap into many rows, so a miss does real
/// shape + line-break work (the whole point the cache elides on a hit).
const PARAGRAPH: &str = "the quick brown fox jumps over the lazy dog while a \
    sphinx of black quartz judges the vow and five boxing wizards jump quickly";

const SIZE: f32 = 16.0;
const WRAP: f32 = 120.0;

fn paragraph_cache(c: &mut Criterion) {
    assert_cache_hit_does_not_reshape();

    let (mut sys, font) = system();
    // Warm the entry so the benched call is a hit.
    let _ = sys.prepare(font, PARAGRAPH, SIZE, Some(WRAP), 1.0, None);

    // Hit: the same paragraph, already laid out — no reshape.
    c.bench_function("prepare_cache_hit", |b| {
        b.iter(|| {
            let out = sys.prepare(
                black_box(font),
                black_box(PARAGRAPH),
                black_box(SIZE),
                black_box(Some(WRAP)),
                1.0,
                None,
            );
            black_box(out);
        });
    });

    // Miss: a fresh wrap width every iteration forces a reshape + re-linebreak.
    let mut w = 200.0f32;
    c.bench_function("prepare_cache_miss", |b| {
        b.iter(|| {
            w += 1.0;
            let out = sys.prepare(
                black_box(font),
                black_box(PARAGRAPH),
                black_box(SIZE),
                black_box(Some(w)),
                1.0,
                None,
            );
            black_box(out);
        });
    });
}

/// The cache-hit invariant: a hit does not reshape, so its allocation count is a
/// small fraction of a forced miss of the same paragraph. Runs once at startup
/// so a regression that reshaped on every prepare fails the bench binary
/// immediately, mirroring `wrap_line.rs`.
fn assert_cache_hit_does_not_reshape() {
    let (mut sys, font) = system();

    // Warm the fixture parse and the cache entry before arming, so the measured
    // counts reflect only the prepare work, not first-touch setup.
    let _ = sys.prepare(font, PARAGRAPH, SIZE, Some(WRAP), 1.0, None);
    let _ = sys.prepare(font, PARAGRAPH, SIZE, Some(WRAP), 1.0, None);

    // A cache hit: re-prepare the warmed paragraph.
    ARMED.store(true, Ordering::Relaxed);
    ALLOCS.store(0, Ordering::Relaxed);
    let hit = sys.prepare(font, PARAGRAPH, SIZE, Some(WRAP), 1.0, None);
    let hit_allocs = ALLOCS.load(Ordering::Relaxed);

    // A cache miss: a never-seen wrap width reshapes from scratch.
    ALLOCS.store(0, Ordering::Relaxed);
    let miss = sys.prepare(font, PARAGRAPH, SIZE, Some(WRAP + 37.0), 1.0, None);
    let miss_allocs = ALLOCS.load(Ordering::Relaxed);
    ARMED.store(false, Ordering::Relaxed);

    // The miss shapes the whole paragraph plus one shape per wrapped row and
    // builds the positioned-glyph vector; the hit skips all of that and only
    // clones the cached `Rc` and rebuilds the output quad vector from the cached
    // positions. The hit must therefore allocate strictly and substantially less
    // — half the miss is a generous ceiling that still catches a regression that
    // reshaped on every prepare (which would make the two counts comparable).
    assert!(
        hit_allocs * 2 < miss_allocs,
        "a cache hit allocated {hit_allocs}, expected well under half the miss's \
         {miss_allocs}; a hit that reshaped would lose the cache (§20)"
    );
    assert!(
        !hit.is_empty() && !miss.is_empty(),
        "both prepares produced quads, so the comparison is meaningful"
    );
}

criterion_group!(benches, paragraph_cache);
criterion_main!(benches);
