//! Structural diff — the second, purely functional stage of the hot reload
//! transaction (architecture section 42; AGENTS 21.7).
//!
//! A hot reload is not a rebuild. Given the last-good template and a freshly
//! compiled candidate template, this stage computes the directed patch that
//! turns one into the other: which nodes survive (`keep`), which change type
//! (`replace`) and which appear or disappear (`insert` / `remove`). Every node
//! is named by the template-local [`NodeKey`] its own tree numbers it with — the
//! pre-order numbering the Binding IR and key analysis use — so a kept node
//! pairs its last-good key with its candidate key, and the commit maps each to
//! the live node it names without a search.
//!
//! Siblings are aligned per parent, not by global position: the children of two
//! matched nodes are matched by the longest common subsequence of their
//! identities, so inserting a node before its siblings keeps every sibling. A
//! node's identity is its type, its `node name:` and the retained node it lowers
//! to; a region (`if` / `for` / `match`) matches a region of the same form with
//! as many arms, and its arms align in turn. Between two matched siblings, the
//! unmatched nodes pair up in order as replaces — the commit builds them fresh,
//! but their children still align, so a `Row` turned `Column` keeps its children
//! — and any node left over is inserted or removed with its subtree.
//!
//! This is a pure function of two [`UiTree`]s. It allocates only the patch it
//! returns and never touches the live UI runtime — a failure earlier in the
//! pipeline short-circuits before commit, so the live tree stays at last-good with
//! no snapshot (the keep-last-good invariant; see the module docs).

use viso_behavior::native::MigratableState;

use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{UiItem, UiNode, UiTree};

/// One node that survives the reload: the same identity in both trees, so
/// the live state its widget schema marks migratable carries from the node at
/// `old` to the node at `new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptNode {
    /// The node's key in the last-good tree.
    pub old: NodeKey,
    /// The node's key in the candidate.
    pub new: NodeKey,
    /// The live state its widget schema carries.
    pub migratable: MigratableState,
}

/// One node whose identity changed in place: the old instance is torn down and
/// a fresh one built, so its own runtime state is lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplacedNode {
    /// The node's key in the last-good tree.
    pub old: NodeKey,
    /// The key of the node replacing it in the candidate.
    pub new: NodeKey,
    /// The type that was there.
    pub old_type: String,
    /// The type replacing it.
    pub new_type: String,
}

/// One node the candidate adds: built fresh, it carries no prior state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertedNode {
    /// The node's key in the candidate.
    pub key: NodeKey,
    /// Its type.
    pub new_type: String,
}

/// One node the candidate drops: its instance and state are freed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedNode {
    /// The node's key in the last-good tree.
    pub key: NodeKey,
    /// Its type.
    pub old_type: String,
}

/// The directed patch from the last-good template to the candidate. Pure data:
/// the commit stage consumes it; nothing here is applied.
///
/// Every node of either tree is in exactly one list: `keep` and `replace` pair
/// a last-good node with a candidate node, `insert` names a candidate-only
/// node and `remove` a last-good-only one. `keep` and `replace` ascend by
/// candidate key, `insert` and `remove` by their own key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuralPatch {
    /// Nodes whose identity is unchanged.
    pub keep: Vec<KeptNode>,
    /// Nodes whose identity changed.
    pub replace: Vec<ReplacedNode>,
    /// Candidate-only nodes.
    pub insert: Vec<InsertedNode>,
    /// Last-good-only nodes.
    pub remove: Vec<RemovedNode>,
}

impl StructuralPatch {
    /// Whether the candidate is structurally identical to the last-good tree:
    /// every node kept and every region matched, so each node keeps its key. A
    /// property-only edit produces such a patch, and the commit only rebinds —
    /// the common fast path.
    pub fn is_structure_preserving(&self) -> bool {
        self.replace.is_empty() && self.insert.is_empty() && self.remove.is_empty()
    }
}

/// One item of a sibling list, with each node keyed in its tree's pre-order.
enum Entry<'a> {
    Node {
        key: NodeKey,
        node: &'a UiNode,
        children: Vec<Entry<'a>>,
    },
    Region {
        form: RegionForm,
        arms: Vec<Vec<Entry<'a>>>,
    },
}

/// Which control-flow region an entry is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RegionForm {
    If,
    For,
    Match,
}

/// The entries of `items`, numbered from `next` in the shared pre-order: a
/// node takes one key then numbers its children; a region takes none and
/// numbers every arm in turn (a `for`'s body as its one arm). Any drift from
/// the Binding IR's order would make a key name a different node than its
/// binding edges do, so this walk is the contract.
fn entries<'a>(items: &'a [UiItem], next: &mut u32) -> Vec<Entry<'a>> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let entry = match item {
            UiItem::Node(node) => {
                let key = NodeKey(*next);
                *next += 1;
                Entry::Node {
                    key,
                    node,
                    children: entries(&node.children, next),
                }
            }
            UiItem::If(region) => Entry::Region {
                form: RegionForm::If,
                arms: region
                    .arms
                    .iter()
                    .map(|arm| entries(&arm.items, next))
                    .collect(),
            },
            UiItem::For(region) => Entry::Region {
                form: RegionForm::For,
                arms: vec![entries(&region.body, next)],
            },
            UiItem::Match(region) => Entry::Region {
                form: RegionForm::Match,
                arms: region
                    .arms
                    .iter()
                    .map(|arm| entries(&arm.items, next))
                    .collect(),
            },
        };
        out.push(entry);
    }
    out
}

/// Whether two nodes are the same node across the edit.
fn same_node(old: &UiNode, new: &UiNode) -> bool {
    old.type_name == new.type_name && old.local_name == new.local_name && old.kind == new.kind
}

/// Whether two entries align: nodes of one identity, or regions of one form
/// and arm count.
fn aligns(old: &Entry<'_>, new: &Entry<'_>) -> bool {
    match (old, new) {
        (Entry::Node { node: a, .. }, Entry::Node { node: b, .. }) => same_node(a, b),
        (Entry::Region { form: a, arms: x }, Entry::Region { form: b, arms: y }) => {
            a == b && x.len() == y.len()
        }
        _ => false,
    }
}

/// Compute the directed structural patch from the last-good template `old` to
/// the candidate template `new`.
///
/// Pure: it reads only the two trees and allocates only the returned patch and
/// the per-list alignment tables, each the product of two sibling counts.
pub fn diff(old: &UiTree, new: &UiTree) -> StructuralPatch {
    let old = entries(&old.items, &mut 0);
    let new = entries(&new.items, &mut 0);
    let mut patch = StructuralPatch::default();
    align(&old, &new, &mut patch);
    patch.keep.sort_unstable_by_key(|k| k.new);
    patch.replace.sort_unstable_by_key(|r| r.new);
    patch.insert.sort_unstable_by_key(|i| i.key);
    patch.remove.sort_unstable_by_key(|r| r.key);
    patch
}

/// Align two sibling lists by the longest common subsequence of their
/// entries, then settle each run of unmatched entries between two matches.
fn align(old: &[Entry<'_>], new: &[Entry<'_>], patch: &mut StructuralPatch) {
    let width = new.len() + 1;
    // `lcs[i * width + j]` is the common subsequence length of `old[i..]` and
    // `new[j..]`.
    let mut lcs = vec![0u32; (old.len() + 1) * width];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            lcs[i * width + j] = if aligns(&old[i], &new[j]) {
                lcs[(i + 1) * width + j + 1] + 1
            } else {
                lcs[(i + 1) * width + j].max(lcs[i * width + j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let (mut old_run, mut new_run) = (0, 0);
    while i < old.len() && j < new.len() {
        if aligns(&old[i], &new[j]) && lcs[i * width + j] == lcs[(i + 1) * width + j + 1] + 1 {
            settle(&old[old_run..i], &new[new_run..j], patch);
            matched(&old[i], &new[j], patch);
            i += 1;
            j += 1;
            (old_run, new_run) = (i, j);
        } else if lcs[(i + 1) * width + j] >= lcs[i * width + j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    settle(&old[old_run..], &new[new_run..], patch);
}

/// Two aligned entries: a kept node and its aligned children, or a region
/// whose arms align pairwise.
fn matched(old: &Entry<'_>, new: &Entry<'_>, patch: &mut StructuralPatch) {
    match (old, new) {
        (
            Entry::Node {
                key: from,
                children: old_children,
                ..
            },
            Entry::Node {
                key: to,
                node,
                children: new_children,
            },
        ) => {
            patch.keep.push(KeptNode {
                old: *from,
                new: *to,
                migratable: node.migratable,
            });
            align(old_children, new_children, patch);
        }
        (Entry::Region { arms: old_arms, .. }, Entry::Region { arms: new_arms, .. }) => {
            for (old_arm, new_arm) in old_arms.iter().zip(new_arms) {
                align(old_arm, new_arm, patch);
            }
        }
        _ => unreachable!("only aligned entries match"),
    }
}

/// A run of unmatched entries between two matches: its nodes pair up in order
/// as replaces whose children still align, and what is left is removed or
/// inserted whole.
fn settle(old: &[Entry<'_>], new: &[Entry<'_>], patch: &mut StructuralPatch) {
    let mut old_nodes = old.iter().filter(|e| matches!(e, Entry::Node { .. }));
    let mut new_nodes = new.iter().filter(|e| matches!(e, Entry::Node { .. }));
    let mut paired = 0;
    while let (
        Some(Entry::Node {
            key: from,
            node: was,
            children: old_children,
        }),
        Some(Entry::Node {
            key: to,
            node: now,
            children: new_children,
        }),
    ) = (old_nodes.next(), new_nodes.next())
    {
        patch.replace.push(ReplacedNode {
            old: *from,
            new: *to,
            old_type: was.type_name.clone(),
            new_type: now.type_name.clone(),
        });
        align(old_children, new_children, patch);
        paired += 1;
    }
    let mut node_at = 0;
    for entry in old {
        if matches!(entry, Entry::Node { .. }) {
            node_at += 1;
            if node_at <= paired {
                continue;
            }
        }
        each_node(entry, &mut |key, node| {
            patch.remove.push(RemovedNode {
                key,
                old_type: node.type_name.clone(),
            });
        });
    }
    let mut node_at = 0;
    for entry in new {
        if matches!(entry, Entry::Node { .. }) {
            node_at += 1;
            if node_at <= paired {
                continue;
            }
        }
        each_node(entry, &mut |key, node| {
            patch.insert.push(InsertedNode {
                key,
                new_type: node.type_name.clone(),
            });
        });
    }
}

/// Every node of `entry`'s subtree, with its key.
fn each_node<'a>(entry: &Entry<'a>, f: &mut impl FnMut(NodeKey, &'a UiNode)) {
    match entry {
        Entry::Node {
            key,
            node,
            children,
        } => {
            f(*key, node);
            for child in children {
                each_node(child, f);
            }
        }
        Entry::Region { arms, .. } => {
            for arm in arms {
                for item in arm {
                    each_node(item, f);
                }
            }
        }
    }
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

    /// The UI IR of a component whose view is `view`, holding the state `flag`.
    fn view_of(view: &str) -> UiTree {
        let source = format!("component C {{ state flag = true; view {{ {view} }} }}");
        let origin = crate::frontend::Origin {
            package: "app".into(),
            module: vec!["c".into()],
            language: None,
        };
        crate::hotreload::plan::plan_view(&source, &origin)
            .expect("component compiles")
            .tree
    }

    /// The `(old, new)` key pairs of the kept nodes.
    fn kept(patch: &StructuralPatch) -> Vec<(u32, u32)> {
        patch.keep.iter().map(|k| (k.old.0, k.new.0)).collect()
    }

    #[test]
    fn identical_trees_keep_every_node() {
        let a = tree_of("Row { Text { text: label; } }");
        let b = tree_of("Row { Text { text: label; } }");
        let patch = diff(&a, &b);
        assert!(patch.is_structure_preserving());
        assert_eq!(kept(&patch), [(0, 0), (1, 1)]);
    }

    #[test]
    fn property_only_edit_keeps_structure() {
        let a = tree_of("Text { text: label; }");
        let b = tree_of("Text { text: other; color: label; }");
        let patch = diff(&a, &b);
        assert!(patch.is_structure_preserving());
        assert_eq!(patch.keep.len(), 1);
    }

    #[test]
    fn a_kept_node_carries_what_its_schema_marks() {
        let a = tree_of("Scroll { TextInput { } }");
        let patch = diff(&a, &a.clone());
        let marks: Vec<_> = patch.keep.iter().map(|k| k.migratable).collect();
        assert!(marks[0].contains(MigratableState::SCROLL.with(MigratableState::FOCUS)));
        assert!(marks[1].contains(MigratableState::SELECTION.with(MigratableState::FOCUS)));
        assert!(!marks[0].contains(MigratableState::SELECTION));
    }

    #[test]
    fn type_change_in_place_is_a_replace() {
        let a = tree_of("Text { text: label; }");
        let b = tree_of("Button { text: label; }");
        let patch = diff(&a, &b);
        assert!(patch.keep.is_empty());
        assert_eq!(
            patch.replace,
            [ReplacedNode {
                old: NodeKey(0),
                new: NodeKey(0),
                old_type: "Text".into(),
                new_type: "Button".into(),
            }]
        );
    }

    #[test]
    fn a_renamed_node_is_a_replace() {
        let a = tree_of("Row { node a: Text { } }");
        let b = tree_of("Row { node b: Text { } }");
        let patch = diff(&a, &b);
        assert_eq!(kept(&patch), [(0, 0)]);
        assert_eq!(patch.replace.len(), 1);
    }

    #[test]
    fn appended_child_is_an_insert() {
        let a = tree_of("Row { Text { text: a; } }");
        let b = tree_of("Row { Text { text: a; } Text { text: b; } }");
        let patch = diff(&a, &b);
        assert_eq!(kept(&patch), [(0, 0), (1, 1)]);
        assert_eq!(patch.insert.len(), 1);
        assert_eq!(patch.insert[0].key, NodeKey(2));
        assert!(patch.remove.is_empty());
    }

    #[test]
    fn removed_child_is_a_remove() {
        let a = tree_of("Row { Text { text: a; } Text { text: b; } }");
        let b = tree_of("Row { Text { text: a; } }");
        let patch = diff(&a, &b);
        assert_eq!(kept(&patch), [(0, 0), (1, 1)]);
        assert!(patch.insert.is_empty());
        assert_eq!(patch.remove.len(), 1);
        assert_eq!(patch.remove[0].key, NodeKey(2));
    }

    #[test]
    fn a_sibling_inserted_before_keeps_the_rest() {
        let a = tree_of("Column { Scroll { Text { } } TextInput { } }");
        let b = tree_of("Column { Button { } Scroll { Text { } } TextInput { } }");
        let patch = diff(&a, &b);
        assert_eq!(kept(&patch), [(0, 0), (1, 2), (2, 3), (3, 4)]);
        assert_eq!(patch.insert.len(), 1);
        assert_eq!(patch.insert[0].key, NodeKey(1));
        assert!(patch.replace.is_empty() && patch.remove.is_empty());
    }

    #[test]
    fn a_removed_subtree_is_removed_whole() {
        let a = tree_of("Column { Row { Text { } Text { } } TextInput { } }");
        let b = tree_of("Column { TextInput { } }");
        let patch = diff(&a, &b);
        assert_eq!(kept(&patch), [(0, 0), (4, 1)]);
        let removed: Vec<_> = patch.remove.iter().map(|r| r.key.0).collect();
        assert_eq!(removed, [1, 2, 3]);
    }

    #[test]
    fn a_replaced_container_keeps_its_children() {
        let a = tree_of("Row { Text { } TextInput { } }");
        let b = tree_of("Column { Text { } TextInput { } }");
        let patch = diff(&a, &b);
        assert_eq!(patch.replace.len(), 1);
        assert_eq!(kept(&patch), [(1, 1), (2, 2)]);
    }

    #[test]
    fn swapped_siblings_keep_one_and_rebuild_the_other() {
        let a = tree_of("Row { Text { text: a; } Button { text: b; } }");
        let b = tree_of("Row { Button { text: a; } Text { text: b; } }");
        let patch = diff(&a, &b);
        assert_eq!(patch.keep.len(), 2, "Row and one child");
        assert_eq!((patch.insert.len(), patch.remove.len()), (1, 1));
    }

    #[test]
    fn regions_align_by_form_and_arms() {
        let a = view_of("Column { if flag { Text { } } TextInput { } }");
        let b = view_of("Column { Button { } if flag { Text { } } TextInput { } }");
        let patch = diff(&a, &b);
        assert_eq!(kept(&patch), [(0, 0), (1, 2), (2, 3)]);
        assert_eq!(patch.insert.len(), 1);

        let c = view_of("Column { if flag { Text { } } else { Text { } } TextInput { } }");
        let patch = diff(&a, &c);
        assert_eq!(kept(&patch), [(0, 0), (2, 3)], "the arm count changed");
        assert_eq!((patch.insert.len(), patch.remove.len()), (2, 1));
    }
}
