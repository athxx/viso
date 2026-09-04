//! Structural diff — the second, purely functional stage of the hot reload
//! transaction (architecture section 42; AGENTS 21.7).
//!
//! A hot reload is not a rebuild. Given the last-good template and a freshly
//! compiled candidate template, this stage computes the *minimal directed patch*
//! that turns one into the other, keyed by the same template-local [`NodeKey`]
//! pre-order numbering the Binding IR and key analysis use. Reproducing that
//! numbering exactly is the whole point: a `NodeKey` a `keep` decision names is
//! the same node the recompiled [`crate::ir::binding_ir::BindingEdge`]s target, so
//! the commit stage can reuse the live instance and rebind it without a runtime
//! search.
//!
//! The decision rule takes the migration reference's semantics — same identity →
//! reuse the instance (so its runtime state, focus, scroll, and animation survive
//! untouched); identity changed → rebuild the subtree (its state is lost) — and
//! sharpens them into an explicit directed patch rather than an implicit
//! over-apply. A node's identity here is its `(type_name, NodeKind)`: a `Text`
//! staying a `Text` at the same key is a keep; a `Text` becoming a `Button` is a
//! replace; a trailing node with no counterpart is an insert or a remove.
//!
//! This is a pure function of two [`UiTree`]s. It allocates only the patch it
//! returns and never touches the live UI runtime — a failure earlier in the
//! pipeline short-circuits before commit, so the live tree stays at last-good with
//! no snapshot (the keep-last-good invariant; see the module docs).

use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{NodeKind, UiItem, UiNode, UiTree};

/// How a node identity compares between the last-good tree and the candidate:
/// the `(type_name, NodeKind)` pair the diff treats as a node's stable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    /// The authored type name (`Text`, `Button`, …).
    type_name: String,
    /// The structural kind the type lowers to (container discipline / leaf).
    kind: NodeKind,
}

impl Identity {
    /// The identity of one lowered node.
    fn of(node: &UiNode) -> Self {
        Identity {
            type_name: node.type_name.clone(),
            kind: node.kind,
        }
    }
}

/// One node that survives the reload unchanged in identity: the same template
/// slot in both trees, so the commit reuses the live instance and its runtime
/// state, focus, scroll, and animation carry over. The key is shared between the
/// two trees because both walks assign it in the same pre-order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptNode {
    /// The template node key, identical in old and new (same pre-order slot).
    pub key: NodeKey,
}

/// One node whose identity changed at a slot present in both trees: the old
/// instance is torn down and a fresh one built, so the subtree's runtime state is
/// lost. The migration stage records this as a reset notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacedNode {
    /// The template slot, identical in old and new.
    pub key: NodeKey,
    /// The identity that was there (for the reset notice).
    pub old_type: String,
    /// The identity replacing it.
    pub new_type: String,
}

/// One node the candidate adds beyond the last-good tree: a slot present in the
/// new walk with no old counterpart. Built fresh; carries no prior state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertedNode {
    /// The new template key (in the candidate's pre-order numbering).
    pub key: NodeKey,
    /// The identity being introduced.
    pub new_type: String,
}

/// One node the candidate drops: a slot present in the old walk with no new
/// counterpart. Its instance and subtree state are freed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedNode {
    /// The old template key (in the last-good pre-order numbering).
    pub key: NodeKey,
    /// The identity being removed.
    pub old_type: String,
}

/// The directed minimal patch from the last-good template to the candidate,
/// classified per aligned [`NodeKey`]. Pure data: the commit stage consumes it to
/// reuse, rebuild, add, or free instances; nothing here is applied.
///
/// `keep` and `replace` cover slots present in both trees (aligned by pre-order
/// key); `insert` covers the candidate's extra trailing slots; `remove` covers the
/// last-good tree's dropped trailing slots. The three key spaces do not overlap: a
/// key in `keep`/`replace` names the same slot in both trees, an `insert` key is a
/// candidate-only slot, a `remove` key is an old-only slot.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuralPatch {
    /// Slots whose identity is unchanged — reuse the live instance.
    pub keep: Vec<KeptNode>,
    /// Slots whose identity changed — rebuild, state lost.
    pub replace: Vec<ReplacedNode>,
    /// Candidate-only slots — build fresh.
    pub insert: Vec<InsertedNode>,
    /// Old-only slots — free.
    pub remove: Vec<RemovedNode>,
}

impl StructuralPatch {
    /// Whether the candidate is structurally identical to the last-good tree: no
    /// slot was added, removed, or re-typed. A property-only edit produces such a
    /// patch (every node kept), so the commit does no structural work and only
    /// rebinds — the common fast path.
    pub fn is_structure_preserving(&self) -> bool {
        self.replace.is_empty() && self.insert.is_empty() && self.remove.is_empty()
    }
}

/// Flatten a [`UiTree`] into its pre-order node sequence, assigning each node the
/// same [`NodeKey`] the Binding IR and key analysis assign it.
///
/// The numbering discipline is copied exactly from `lower_bindings` / `analyze_keys`
/// (see [`crate::ir::binding_ir`], [`crate::ir::keys`]): a node consumes one key
/// then descends into its children; a `for` region consumes one key then descends
/// into its body; `if`/`match` regions consume no key of their own and descend into
/// every arm's items. Any drift from that order would make a diff key name a
/// different runtime node than the binding edges do, so this walk is the contract.
fn flatten(tree: &UiTree) -> Vec<(NodeKey, Identity)> {
    let mut out = Vec::new();
    let mut next: u32 = 0;
    for item in &tree.items {
        flatten_item(item, &mut next, &mut out);
    }
    out
}

/// Pre-order walk of one item, mirroring the shared numbering. `for` regions are
/// treated as a keyed slot (they consume a key) whose identity is a synthetic
/// container marker, so an old `for` aligns with a new `for` at the same slot; a
/// `for` becoming a plain node (or vice versa) reads as a replace, exactly as a
/// type change does.
fn flatten_item(item: &UiItem, next: &mut u32, out: &mut Vec<(NodeKey, Identity)>) {
    match item {
        UiItem::Node(node) => {
            let key = take_key(next);
            out.push((key, Identity::of(node)));
            for child in &node.children {
                flatten_item(child, next, out);
            }
        }
        UiItem::If(vi) => {
            for arm in &vi.arms {
                for item in &arm.items {
                    flatten_item(item, next, out);
                }
            }
        }
        UiItem::For(vf) => {
            let key = take_key(next);
            out.push((key, for_identity()));
            for item in &vf.body {
                flatten_item(item, next, out);
            }
        }
        UiItem::Match(vm) => {
            for arm in &vm.arms {
                for item in &arm.items {
                    flatten_item(item, next, out);
                }
            }
        }
    }
}

/// Take the next pre-order key, matching `LowerCtx::take_key` / `KeyCtx::take_key`.
fn take_key(next: &mut u32) -> NodeKey {
    let key = NodeKey(*next);
    *next += 1;
    key
}

/// The synthetic identity of a `for` region's own slot. A `for` occupies one key
/// in the shared numbering but has no authored type name; giving it a stable
/// reserved identity lets two `for` regions at the same slot compare equal (kept)
/// while a `for`-vs-node mismatch compares unequal (replace).
fn for_identity() -> Identity {
    Identity {
        type_name: "<for>".to_string(),
        kind: NodeKind::Leaf,
    }
}

/// Compute the directed structural patch from the last-good template `old` to the
/// candidate template `new`, aligned by the shared pre-order [`NodeKey`].
///
/// Pure: it reads only the two trees and allocates only the returned patch. Slots
/// present in both (up to the shorter length) are classified keep vs replace by
/// identity; the longer tree's trailing slots become inserts (new longer) or
/// removes (old longer). This is the minimal directed patch under positional
/// alignment — the same discipline the runtime uses to map keys to live handles,
/// so the commit stage never has to search for a node.
pub fn diff(old: &UiTree, new: &UiTree) -> StructuralPatch {
    let old_seq = flatten(old);
    let new_seq = flatten(new);

    let mut patch = StructuralPatch::default();
    let common = old_seq.len().min(new_seq.len());

    for i in 0..common {
        let (old_key, old_id) = &old_seq[i];
        let (_new_key, new_id) = &new_seq[i];
        debug_assert_eq!(
            *old_key, new_seq[i].0,
            "aligned slots share the same pre-order key"
        );
        if old_id == new_id {
            patch.keep.push(KeptNode { key: *old_key });
        } else {
            patch.replace.push(ReplacedNode {
                key: *old_key,
                old_type: old_id.type_name.clone(),
                new_type: new_id.type_name.clone(),
            });
        }
    }

    // Candidate has extra trailing slots → build them fresh.
    for (key, id) in new_seq.iter().skip(common) {
        patch.insert.push(InsertedNode {
            key: *key,
            new_type: id.type_name.clone(),
        });
    }

    // Last-good has extra trailing slots → free them.
    for (key, id) in old_seq.iter().skip(common) {
        patch.remove.push(RemovedNode {
            key: *key,
            old_type: id.type_name.clone(),
        });
    }

    patch
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotreload::plan::plan;

    /// Compile a fragment through the real frontend and return its UI IR, so the
    /// diff tests run against exactly the trees the transaction diffs at runtime.
    fn tree_of(source: &str) -> UiTree {
        plan(source).expect("fragment compiles").tree
    }

    #[test]
    fn identical_trees_keep_every_node() {
        let a = tree_of("Row { Text { text: label; } }");
        let b = tree_of("Row { Text { text: label; } }");
        let patch = diff(&a, &b);
        assert!(
            patch.is_structure_preserving(),
            "no structural change between identical trees"
        );
        assert_eq!(patch.keep.len(), 2, "Row and Text both kept");
        assert_eq!(patch.keep[0].key, NodeKey(0), "Row is pre-order key 0");
        assert_eq!(patch.keep[1].key, NodeKey(1), "Text is pre-order key 1");
    }

    #[test]
    fn property_only_edit_keeps_structure() {
        // Changing a bound property must not alter identity: every node is kept,
        // so the commit does zero structural work and only rebinds.
        let a = tree_of("Text { text: label; }");
        let b = tree_of("Text { text: other; color: label; }");
        let patch = diff(&a, &b);
        assert!(patch.is_structure_preserving());
        assert_eq!(patch.keep.len(), 1);
    }

    #[test]
    fn type_change_at_a_slot_is_a_replace() {
        let a = tree_of("Text { text: label; }");
        let b = tree_of("Button { text: label; }");
        let patch = diff(&a, &b);
        assert_eq!(patch.keep.len(), 0, "identity changed → not kept");
        assert_eq!(patch.replace.len(), 1);
        assert_eq!(patch.replace[0].key, NodeKey(0));
        assert_eq!(patch.replace[0].old_type, "Text");
        assert_eq!(patch.replace[0].new_type, "Button");
    }

    #[test]
    fn appended_child_is_an_insert() {
        let a = tree_of("Row { Text { text: a; } }");
        let b = tree_of("Row { Text { text: a; } Text { text: b; } }");
        let patch = diff(&a, &b);
        // Row (key 0) and first Text (key 1) kept; second Text (key 2) inserted.
        assert_eq!(patch.keep.len(), 2);
        assert_eq!(patch.insert.len(), 1);
        assert_eq!(patch.insert[0].key, NodeKey(2));
        assert_eq!(patch.insert[0].new_type, "Text");
        assert!(patch.remove.is_empty());
    }

    #[test]
    fn removed_child_is_a_remove() {
        let a = tree_of("Row { Text { text: a; } Text { text: b; } }");
        let b = tree_of("Row { Text { text: a; } }");
        let patch = diff(&a, &b);
        assert_eq!(patch.keep.len(), 2, "Row and first Text kept");
        assert!(patch.insert.is_empty());
        assert_eq!(patch.remove.len(), 1);
        assert_eq!(patch.remove[0].key, NodeKey(2));
        assert_eq!(patch.remove[0].old_type, "Text");
    }

    #[test]
    fn nested_reorder_shifts_alignment_into_replaces() {
        // Positional alignment: swapping sibling types at the same slots reads as
        // replaces, matching the reference's "identity changed → rebuild" rule.
        let a = tree_of("Row { Text { text: a; } Button { text: b; } }");
        let b = tree_of("Row { Button { text: a; } Text { text: b; } }");
        let patch = diff(&a, &b);
        assert_eq!(patch.keep.len(), 1, "only Row (key 0) keeps identity");
        assert_eq!(
            patch.replace.len(),
            2,
            "both children re-typed at their slots"
        );
    }
}
