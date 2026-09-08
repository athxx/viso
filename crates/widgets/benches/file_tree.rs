//! Microbench for the `FileTree` control: the cost of authoring a large tree, the
//! pure reflatten a toggle triggers, and — the interactive hot path — one full
//! expand/collapse reconcile frame through the public [`FileTreeHandle`].
//!
//! This is the section 71 microbench slot for the Tier 5 file browser, alongside
//! `dock.rs`. The file tree eats the Tier 3 virtual list (architecture section
//! 12.4), so its cost story is not "how many nodes does a 100k-file tree mount"
//! (the answer is: only a visible window) but "what does a toggle cost": edit the
//! open set, reflatten the forest into the visible-row list, and drive the keyed
//! list's item count so the substrate diffs the new key set. Four phases:
//!
//! - `file_tree/build_wide` authors a wide, one-level-deep tree (one root over
//!   1000 collapsed children) through the public [`file_tree`] builder, so the
//!   build-walk cost — the keyed virtual-list authoring, the shared warm cells — is
//!   measured at a realistic fan-out. The tree is collapsed, so only the visible
//!   window mounts however many children exist; this times the authoring, not a
//!   per-child mount.
//! - `file_tree/toggle_reconcile_frame` times the exact work one live toggle frame
//!   does: push a toggle intent, then run [`FileTreeHandle::reconcile`], which
//!   drains the intent, edits the open set, reflattens, and calls `set_item_count`.
//!   This is the interactive hot path, driven through the same public handle an
//!   application uses.
//!
//! It drives only `viso-ui` + `viso-widgets`, so it needs no facade or render
//! dev-dependency and adds no dependency edge. Run release
//! (`CARGO_TARGET_DIR=/tmp/rust_tmp cargo bench -p viso-widgets --bench file_tree`);
//! debug timing is not a perf result (AGENTS section 36).

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_ui::{
    BindingTable, BuildCx, Component, NodeStore, SemanticProjector, StateStore, TextEdits,
    VirtualLists,
};
use viso_widgets::{FileTree, FileTreeHandle, FileTreeHandleSlot, NodeKey, TreeNode, file_tree};

/// The number of children under the single root in the wide fixtures — a realistic
/// large directory listing.
const WIDE: u64 = 1000;

/// The reactive stores a `with_reactive` `BuildCx` needs, kept alive alongside the
/// node store so the built tree's binding/state references stay valid.
struct Reactive {
    states: StateStore,
    bindings: BindingTable,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
}

impl Reactive {
    fn new() -> Self {
        Reactive {
            states: StateStore::new(),
            bindings: BindingTable::new(),
            lists: VirtualLists::new(),
            text_edits: TextEdits::new(),
            projectors: SemanticProjector::new(),
        }
    }
}

/// A single root directory (key 0) over `WIDE` file children (keys `1..=WIDE`) — a
/// large flat directory listing, the shape a toggle grows and shrinks.
fn wide_roots() -> Vec<TreeNode> {
    let children: Vec<TreeNode> = (1..=WIDE)
        .map(|k| TreeNode::file(NodeKey(k), "f"))
        .collect();
    vec![TreeNode::dir(NodeKey(0), "root", children)]
}

/// A `FileTree` over the wide fixture with a handle slot wired, so a bench can drive
/// the public reconcile. The tree starts open so a toggle collapses the whole
/// listing (the reflatten shrinks from `WIDE + 1` rows to one).
fn wide_tree(slot: &FileTreeHandleSlot) -> FileTree {
    file_tree(wide_roots())
        .open(NodeKey(0))
        .handle(FileTreeHandleSlot::clone(slot))
}

/// Build a `FileTree` into a fresh store, returning the store, the reactive stores
/// (kept alive so list state stays valid), and the filled handle.
fn build_tree_scene() -> (NodeStore, Reactive, FileTreeHandle) {
    let mut store = NodeStore::new();
    let mut r = Reactive::new();
    let slot: FileTreeHandleSlot = FileTreeHandleSlot::default();
    {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut r.states,
            &mut r.bindings,
            &mut r.lists,
            &mut r.text_edits,
            &mut r.projectors,
        );
        wide_tree(&slot).build(&mut cx);
    }
    let handle = slot.borrow().clone().expect("build filled the handle slot");
    (store, r, handle)
}

fn bench_file_tree(c: &mut Criterion) {
    // build_wide: author a 1000-child (collapsed) tree into a fresh NodeStore. The
    // tree is collapsed here so the walk authors the viewport + warm cells without
    // mounting a child per file — the virtualization makes the mount lazy.
    c.bench_function("file_tree/build_wide", |b| {
        b.iter(|| {
            let mut store = NodeStore::new();
            let mut r = Reactive::new();
            let slot: FileTreeHandleSlot = FileTreeHandleSlot::default();
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut r.states,
                &mut r.bindings,
                &mut r.lists,
                &mut r.text_edits,
                &mut r.projectors,
            );
            file_tree(wide_roots())
                .handle(FileTreeHandleSlot::clone(&slot))
                .build(&mut cx);
            black_box(&store);
        });
    });

    // toggle_reconcile_frame: one live toggle frame — push the intent, then run the
    // public reconcile (drain -> edit open -> reflatten -> set_item_count). The
    // fixture starts open, so successive toggles alternate collapse/expand, each
    // reflattening between one row and WIDE + 1 rows.
    c.bench_function("file_tree/toggle_reconcile_frame", |b| {
        let (mut store, mut r, handle) = build_tree_scene();
        let ev = still_pointer();
        b.iter(|| {
            // Toggle the root each iteration so the open set — and thus the visible
            // row count — genuinely changes. The `EventCx` borrows only the state and
            // binding stores; the reconcile then takes the store and the (disjoint)
            // list store, so the two phases never overlap a borrow.
            {
                let mut cx = viso_ui::EventCx::__new_pointer(&mut r.states, &r.bindings, &ev);
                handle.toggle(&mut cx, NodeKey(0));
            }
            handle.reconcile(&mut store, &mut r.lists);
            black_box(&store);
        });
    });
}

/// A still (no-button) pointer sample for driving a command that reads no pointer.
fn still_pointer() -> viso_ui::PointerEvent {
    viso_ui::PointerEvent {
        x: 0.0,
        y: 0.0,
        phase: viso_ui::PointerPhase::Move,
        buttons: viso_ui::PointerButtons::NONE,
        modifiers: viso_ui::Modifiers::default(),
    }
}

criterion_group!(benches, bench_file_tree);
criterion_main!(benches);
