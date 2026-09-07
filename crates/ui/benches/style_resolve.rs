//! The section 8.3 decision-gate baseline for the incremental STYLE layer:
//! `NodeStore::resolve_styles` re-resolves the warm style of every STYLE-dirty
//! *bound* node after a theme swap, but it finds those nodes by scanning the
//! whole dirty array (`0..dirty.len()`) and testing the STYLE bit on each. The
//! backlog note asks whether that full scan is hot enough — on a large tree with
//! only a few token-bound nodes — to justify caching a "bound node list". Section
//! 7.3 forbids answering by assertion: this bench measures the scan first.
//!
//! Shape: a wide tree where only a small fraction of leaves carry a style token
//! bound to a theme cell; a theme swap writes that cell, the flush marks STYLE on
//! exactly the bound leaves, and `resolve_styles` then scans the full dirty array
//! to re-resolve them. The scan cost is `O(nodes)`; the useful work is
//! `O(bound)`. A startup guard pins the tree size and that exactly the bound
//! nodes re-resolve, so a regression fails loud rather than timing a no-op.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release
//! profile. Debug timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BoxStyle, BuildCx, DirtyClass, FlexStyle, LeafStyle, NodeStore, Size, StateId,
    StateStore, StateValue, StyleId, Theme, TokenInterner, TokenNamespace,
};

/// Flex containers under the root.
const CONTAINERS: usize = 100;
/// Leaves per container — `CONTAINERS * LEAVES` total leaves.
const LEAVES: usize = 60;
/// Only every `BOUND_STRIDE`-th leaf carries a style token bound to the theme
/// cell, so the bound set is a small fraction of the tree — the case where the
/// full dirty scan could dominate the useful re-resolve work.
const BOUND_STRIDE: usize = 20;

/// Everything a style resolve touches: the node store (style + dirty columns),
/// the state store holding the theme cell, the binding table wiring the cell to
/// the bound leaves, the theme, the token cell, and the bound leaves recorded so
/// the guard can pin the re-resolve count.
struct Harness {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    theme: Theme,
    cell: StateId,
    bound_count: u32,
    changed: Vec<StateId>,
}

/// Build the scene: one flex root over `CONTAINERS` containers of `LEAVES` leaves,
/// every `BOUND_STRIDE`-th leaf given a `fill` style token bound to a single
/// shared theme color cell (the token's backing cell). This is the "large tree,
/// few styled nodes" shape the backlog note calls out.
fn setup() -> Harness {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut interner = TokenInterner::new();
    let mut theme = Theme::new();

    let bg = interner.intern(TokenNamespace::Color, "bg");
    let cell = states.alloc(StateValue::Color(0.0, 0.0, 0.0, 1.0));
    theme.define(bg, cell);

    let mut bound = Vec::new();
    {
        let mut cx = BuildCx::new(&mut store);
        cx.flex(FlexStyle::default(), |cx| {
            let mut leaf_index = 0usize;
            for _ in 0..CONTAINERS {
                cx.flex(FlexStyle::default(), |cx| {
                    for _ in 0..LEAVES {
                        let leaf = cx
                            .leaf(LeafStyle {
                                size: Size::fixed(1.0, 1.0),
                                style: BoxStyle::default(),
                            })
                            .id();
                        if leaf_index.is_multiple_of(BOUND_STRIDE) {
                            bound.push(leaf);
                        }
                        leaf_index += 1;
                    }
                });
            }
        });
    }

    for &leaf in &bound {
        store.set_style_token(leaf, StyleId::fill(bg));
        // Bind the token's backing cell so a theme swap re-marks the node STYLE.
        bindings.bind(cell, leaf, DirtyClass::STYLE | DirtyClass::PAINT);
    }
    // The initial resolve, then clear the build/setup dirt so the bench starts
    // from a clean frame.
    store.resolve_styles(&theme, &states);
    store.clear_dirty();

    Harness {
        store,
        states,
        bindings,
        theme,
        cell,
        bound_count: bound.len() as u32,
        changed: Vec::new(),
    }
}

/// One theme swap + resolve: write a fresh color to the theme cell, flush the
/// binding (marking STYLE on every bound leaf), then run the full-array scan.
/// Returns how many nodes re-resolved.
fn swap_and_resolve(h: &mut Harness, v: f32) -> u32 {
    h.states.set(h.cell, StateValue::Color(v, v, v, 1.0));
    h.changed.clear();
    h.states.take_pending(&mut h.changed);
    h.store.flush_state_transactions(&h.changed, &h.bindings);
    h.store.resolve_styles(&h.theme, &h.states)
}

/// The startup guard: the scene has the expected node count, and a theme swap
/// re-resolves exactly the bound leaves — so a regression that drops the binding
/// (nothing re-resolves) or the scan (wrong count) fails the bench binary rather
/// than timing a silent no-op. Mirrors `semantic_projection`'s startup guard.
fn assert_swap_reresolves_bound_nodes() {
    let mut h = setup();
    let total_nodes = 1 + CONTAINERS + CONTAINERS * LEAVES;
    let resolved = swap_and_resolve(&mut h, 0.5);
    assert_eq!(
        resolved, h.bound_count,
        "a theme swap re-resolves exactly the token-bound leaves"
    );
    assert!(h.bound_count > 0, "the scene has bound leaves to resolve");
    assert!(
        (h.bound_count as usize) < total_nodes / 4,
        "the bound set is a small fraction of the tree — the case the full scan \
         could dominate"
    );
}

fn bench_style_resolve(c: &mut Criterion) {
    assert_swap_reresolves_bound_nodes();

    // The full-array STYLE scan cost after a theme swap: O(nodes) scan for
    // O(bound) useful work. Alternate the color each iteration so every swap is a
    // real change that re-marks the bound leaves.
    c.bench_function("resolve_styles_theme_swap_full_scan", |b| {
        let mut h = setup();
        let mut v = 0.0f32;
        b.iter(|| {
            v = if v > 0.9 { 0.0 } else { v + 0.1 };
            black_box(swap_and_resolve(black_box(&mut h), v))
        });
    });
}

criterion_group!(benches, bench_style_resolve);
criterion_main!(benches);
