//! Loading a release AOT package allocates for the tree it builds, never per
//! record it reads: [`AotView`] decodes one node at a time on the stack, so a
//! package of four times the nodes costs only the node stores' amortized
//! growth more, not an allocation per node.
//!
//! A counting global allocator, armed per thread, counts what the load
//! allocates; the counter is thread-local, so the tests of this binary
//! running in parallel do not see each other's allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use viso_ende::Encode;
use viso_ui::aot::{AotNode, AotNodeKind, AotPackage, AotStyle, AotView, instantiate};
use viso_ui::length::{LengthTerms, NodeLengths};
use viso_ui::{BindingTable, NodeStore, StateStore, VirtualLists};

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every call forwards to the system allocator unchanged; the only
// added behavior is a thread-local counter bump while the thread is armed,
// which allocates nothing (the cells have `const` initializers).
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.with(Cell::get) {
            ALLOCS.with(|n| n.set(n.get() + 1));
        }
        // SAFETY: the caller's contract for `alloc` is passed on as is.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from `System` through this allocator.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.with(Cell::get) {
            ALLOCS.with(|n| n.set(n.get() + 1));
        }
        // SAFETY: `ptr` came from `System` through this allocator, and the
        // caller's contract for `realloc` is passed on as is.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A column of `rows` rows of four leaves, every leaf with lengths.
fn blob(rows: u32) -> &'static [u8] {
    let mut nodes = vec![AotNode {
        kind: AotNodeKind::Flex,
        style: AotStyle::default(),
        lengths: None,
        child_count: rows,
    }];
    for row in 0..rows {
        nodes.push(AotNode {
            kind: AotNodeKind::Flex,
            style: AotStyle {
                gap: Some(2.0),
                ..AotStyle::default()
            },
            lengths: None,
            child_count: 4,
        });
        for leaf in 0..4 {
            nodes.push(AotNode {
                kind: AotNodeKind::Leaf,
                style: AotStyle::default(),
                lengths: Some(Box::new(NodeLengths {
                    width: Some(LengthTerms {
                        dp: (row * 4 + leaf) as f32,
                        ..LengthTerms::default()
                    }),
                    ..NodeLengths::default()
                })),
                child_count: 0,
            });
        }
    }
    let package = AotPackage {
        nodes,
        edges: Vec::new(),
    };
    Vec::leak(package.encode_to_vec())
}

/// The allocations validating and instantiating `blob` makes.
fn load(blob: &'static [u8]) -> usize {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    ALLOCS.with(|n| n.set(0));
    ARMED.with(|a| a.set(true));
    let view = AotView::new(blob).expect("valid");
    let root = instantiate(&view, &mut store, &mut states, &mut bindings, &mut lists);
    ARMED.with(|a| a.set(false));
    assert!(root.is_some());
    ALLOCS.with(Cell::get)
}

#[test]
fn a_load_does_not_allocate_per_node() {
    let (small, large) = (blob(250), blob(1000));
    let (few, many) = (load(small), load(large));
    let nodes = 3750;
    assert!(
        many - few < nodes / 50,
        "{few} allocations for 1251 nodes, {many} for 5001: a load allocates per node"
    );
}
