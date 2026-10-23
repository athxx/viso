//! The `node_arena` benchmark category: create, traverse,
//! and remove a 100k-node retained tree over the generational [`NodeArena`].
//! This is the identity/tree substrate every subsystem sits on, so its
//! create/traverse/remove costs bound the ceiling of any structural churn a
//! frame can trigger.
//!
//! Run release (`cargo bench -p viso-ui`); criterion defaults to a release
//! profile. Debug timing is not a performance result.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{NodeArena, NodeId, NodeIndex};

const NODE_COUNT: usize = 100_000;
/// Children per interior node: a bushy tree (fanout 16) is closer to real UI
/// than a linked list, and stresses the sibling-chain wiring in `append_child`.
const FANOUT: usize = 16;

/// Allocate `NODE_COUNT` nodes and wire them into a fanout-`FANOUT` tree. Nodes
/// are attached in allocation order: node `i`'s parent is `(i - 1) / FANOUT`,
/// which yields a balanced tree with the root at index 0. Returns the arena and
/// every id in allocation order (index 0 is the root).
fn build_tree() -> (NodeArena, Vec<NodeId>) {
    let mut arena = NodeArena::new();
    let mut ids = Vec::with_capacity(NODE_COUNT);
    let root = arena.alloc();
    ids.push(root);
    for i in 1..NODE_COUNT {
        let child = arena.alloc();
        let parent = ids[(i - 1) / FANOUT];
        arena.append_child(parent, child);
        ids.push(child);
    }
    (arena, ids)
}

/// Depth-first walk from the root over the bare index links, summing raw
/// indices so the traversal is not optimized away. Uses an explicit stack (no
/// recursion) — the same shape the layout/paint/hit-test walks use.
fn traverse(arena: &NodeArena, root: NodeIndex, stack: &mut Vec<NodeIndex>) -> u64 {
    stack.clear();
    stack.push(root);
    let mut acc: u64 = 0;
    while let Some(index) = stack.pop() {
        acc = acc.wrapping_add(u64::from(index.get()));
        stack.extend(arena.children_of(index));
    }
    acc
}

/// The same walk through live handles, each link paired with its generation:
/// the cost of the checked external API, for comparison with [`traverse`].
fn traverse_checked(arena: &NodeArena, root: NodeId, stack: &mut Vec<NodeId>) -> u64 {
    stack.clear();
    stack.push(root);
    let mut acc: u64 = 0;
    while let Some(id) = stack.pop() {
        acc = acc.wrapping_add(u64::from(id.index()));
        stack.extend(arena.children(id));
    }
    acc
}

fn create(c: &mut Criterion) {
    c.bench_function("node_arena_create_100k", |b| {
        b.iter(|| {
            let (arena, ids) = build_tree();
            black_box((arena, ids));
        });
    });
    c.bench_function("node_arena_create_100k_fixed", |b| {
        b.iter(|| {
            let mut arena = NodeArena::with_fixed_capacity(NODE_COUNT as u32);
            let root = arena.alloc();
            let mut parents = Vec::with_capacity(NODE_COUNT);
            parents.push(root);
            for i in 1..NODE_COUNT {
                let child = arena.alloc();
                arena.append_child(parents[(i - 1) / FANOUT], child);
                parents.push(child);
            }
            black_box((arena, parents));
        });
    });
}

fn traverse_bench(c: &mut Criterion) {
    let (arena, ids) = build_tree();
    let root = ids[0].node_index();
    let mut stack: Vec<NodeIndex> = Vec::new();

    // Startup zero-growth assertion: repeated traversal reuses one stack and must
    // not grow it per walk — a scratch-realloc regression fails the bench binary.
    let sum = traverse(&arena, root, &mut stack);
    let cap = stack.capacity();
    for _ in 0..16 {
        assert_eq!(
            traverse(&arena, root, &mut stack),
            sum,
            "traversal is stable"
        );
    }
    assert_eq!(
        stack.capacity(),
        cap,
        "traversal stack must not grow per walk"
    );

    c.bench_function("node_arena_traverse_100k", |b| {
        b.iter(|| {
            black_box(traverse(&arena, black_box(root), &mut stack));
        });
    });

    let mut checked: Vec<NodeId> = Vec::new();
    assert_eq!(traverse_checked(&arena, ids[0], &mut checked), sum);
    c.bench_function("node_arena_traverse_100k_checked", |b| {
        b.iter(|| {
            black_box(traverse_checked(&arena, black_box(ids[0]), &mut checked));
        });
    });
}

fn remove(c: &mut Criterion) {
    c.bench_function("node_arena_remove_100k", |b| {
        // Rebuild the tree each iteration so every run removes a full 100k tree
        // from the same starting state; only the detach+free work is timed-plus
        // -rebuild, which is the honest cost of structural teardown.
        b.iter_batched(
            build_tree,
            |(mut arena, ids)| {
                // Remove leaves-first (reverse allocation order): each node is a
                // leaf by the time it is reached, so detach only touches the
                // parent's endpoints and the sibling chain, never orphaned links.
                for &id in ids.iter().rev() {
                    arena.detach_child(id);
                    arena.free(id);
                }
                black_box(&arena);
            },
            criterion::BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, create, traverse_bench, remove);
criterion_main!(benches);
