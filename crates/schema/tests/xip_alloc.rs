//! Reading a package in place: validating an AOT or view package and walking
//! every record it holds touches no heap, however many nodes, edges, states,
//! handlers and assets it carries, and what it reads borrows the blob.
//!
//! A counting global allocator, armed per thread, counts what the reads
//! allocate; the counter is thread-local, so the tests of this binary running
//! in parallel do not see each other's allocations.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

use viso_ende::{Aligned, Encode, SECTION_ALIGN};
use viso_schema::aot::{AotEdge, AotNode, AotNodeKind, AotPackage, AotStyle, AotView};
use viso_schema::layout::{BoxRules, GridPlacement, Placement, Span, SpanInset};
use viso_schema::view::{
    EventRoute, Route, ViewAsset, ViewHandler, ViewPackage, ViewPackageView, ViewShader, ViewState,
};
use viso_schema::{
    AssetBytes, DirtyClass, LengthTerms, NodeLengths, ResourceId, StateKey, StateValue,
};

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

/// The allocations `f` makes on this thread.
fn allocations<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCS.with(|n| n.set(0));
    ARMED.with(|a| a.set(true));
    let out = f();
    ARMED.with(|a| a.set(false));
    (out, ALLOCS.with(Cell::get))
}

/// A row of `leaves` leaves, each with lengths and an edge, every other one
/// with box rules and every third one placed.
fn tree(leaves: u32) -> AotPackage {
    let mut nodes = vec![AotNode {
        kind: AotNodeKind::Flex,
        style: AotStyle {
            gap: Some(4.0),
            ..AotStyle::default()
        },
        lengths: None,
        rules: None,
        grid: None,
        child_count: leaves,
    }];
    let mut edges = Vec::new();
    for i in 0..leaves {
        nodes.push(AotNode {
            kind: AotNodeKind::Leaf,
            style: AotStyle {
                place: (i % 3 == 0).then_some(Placement::Grid(GridPlacement {
                    column: Some(i as u16),
                    column_span: 2,
                    ..GridPlacement::default()
                })),
                ..AotStyle::default()
            },
            lengths: Some(Box::new(NodeLengths {
                width: Some(LengthTerms {
                    dp: i as f32,
                    ..LengthTerms::default()
                }),
                ..NodeLengths::default()
            })),
            rules: (i % 2 == 0).then(|| {
                Box::new(BoxRules {
                    min_width: Span::dp(i as f32),
                    margin: SpanInset {
                        top: Span::pct(5.0),
                        ..SpanInset::ZERO
                    },
                    ..BoxRules::NONE
                })
            }),
            grid: None,
            child_count: 0,
        });
        edges.push(AotEdge {
            state: StateKey::from_parts(1, u64::from(i)),
            node: i + 1,
            class: DirtyClass::PAINT.bits(),
        });
    }
    AotPackage { nodes, edges }
}

/// A view of `leaves` leaves, each with a state, a handler and an asset.
fn view(leaves: u32) -> ViewPackage {
    let n = leaves as usize;
    ViewPackage {
        ui: tree(leaves),
        behavior: vec![7; 64],
        component: "Counter".into(),
        states: (0..leaves)
            .map(|i| ViewState {
                key: StateKey::from_parts(1, u64::from(i)),
                slot: Some(i),
                tracked: i % 2 == 0,
                initial: StateValue::Int(i as i32),
            })
            .collect(),
        handlers: (0..leaves)
            .map(|i| ViewHandler {
                node: i + 1,
                route: Route {
                    event: EventRoute::Click,
                    handler: i,
                    capture: false,
                },
            })
            .collect(),
        assets: (0..n)
            .map(|i| ViewAsset {
                id: ResourceId(i as u64),
                bytes: AssetBytes::Shared(Arc::from(vec![i as u8; 1 + i % 13])),
            })
            .collect(),
        shaders: vec![ViewShader::new(vec![3u8; 21])],
        ..ViewPackage::default()
    }
}

/// `bytes`, at an aligned address for the rest of the process.
fn leak(bytes: Vec<u8>) -> &'static [u8] {
    let aligned: Vec<Aligned<[u8; SECTION_ALIGN]>> = bytes
        .chunks(SECTION_ALIGN)
        .map(|chunk| {
            let mut word = [0; SECTION_ALIGN];
            word[..chunk.len()].copy_from_slice(chunk);
            Aligned(word)
        })
        .collect();
    let words = Vec::leak(aligned);
    // SAFETY: `Aligned<[u8; N]>` is `repr(C)` around `[u8; N]` with no
    // padding (its size is a multiple of its alignment), so the leaked words
    // are `words.len() * N` initialized bytes, valid for `'static`; the
    // slice covers the `bytes.len()` of them the blob is.
    unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), bytes.len()) }
}

#[test]
fn an_aot_package_reads_in_place_without_allocating() {
    for leaves in [10, 1000] {
        let package = tree(leaves);
        let blob = leak(package.encode_to_vec());
        let ((nodes, lengths, edges), allocs) = allocations(|| {
            let view = AotView::new(blob).expect("valid");
            let nodes = view.nodes().count();
            let lengths = view.nodes().filter(|n| n.lengths().is_some()).count();
            let rules = view.nodes().filter_map(|n| n.rules()).count();
            assert_eq!(rules, leaves.div_ceil(2) as usize);
            let edges = view.edges().map(|e| e.node as usize).sum::<usize>();
            (nodes, lengths, edges)
        });
        assert_eq!(allocs, 0, "{leaves} leaves");
        assert_eq!((nodes, lengths), (leaves as usize + 1, leaves as usize));
        assert_eq!(edges, (1..=leaves as usize).sum::<usize>());
        let view = AotView::new(blob).unwrap();
        assert_eq!(view.to_package(), package, "the owned form is the same");
    }
}

#[test]
fn a_view_package_reads_in_place_without_allocating() {
    for leaves in [10, 1000] {
        let package = view(leaves);
        let blob = leak(package.encode_to_vec());
        let range = blob.as_ptr_range();
        let (lent, allocs) = allocations(|| {
            let view = ViewPackageView::new(blob).expect("valid");
            let states = view.states().filter(|s| s.tracked).count();
            let handlers = view.handlers().map(|h| h.route.handler).max();
            let assets = view
                .assets()
                .filter(|(_, bytes)| range.contains(&bytes.as_ptr()))
                .count();
            let shaders = view.shaders().all(|s| range.contains(&s.as_ptr()));
            let behavior = range.contains(&view.behavior.as_ptr());
            let component = range.contains(&view.component.as_ptr());
            (states, handlers, assets, shaders && behavior && component)
        });
        assert_eq!(allocs, 0, "{leaves} leaves");
        let (states, handlers, assets, borrowed) = lent;
        assert_eq!(states, leaves.div_ceil(2) as usize);
        assert_eq!(handlers, Some(leaves - 1));
        assert!(
            borrowed,
            "the module, component and shaders borrow the blob"
        );
        assert_eq!(assets, leaves as usize, "every asset borrows the blob");
        for (_, bytes) in ViewPackageView::new(blob).unwrap().assets() {
            assert_eq!(
                bytes.as_ptr().addr() % SECTION_ALIGN,
                0,
                "assets start aligned"
            );
        }
    }
}
