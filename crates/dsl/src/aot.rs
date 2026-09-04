//! The build-time release AOT emitter: lower a compiled [`CandidatePlan`] into a
//! compact [`viso_ui::aot::AotPackage`] and encode it (architecture section 41;
//! AGENTS 21.6, 60).
//!
//! This is the third lowering target of the one shared frontend. Slice N lowers the
//! IR to `viso_ui` builder tokens through the proc-macro; Slice O commits it into a
//! live tree at hot-reload time (`commit.rs`); this emitter serializes it into the
//! compact, dependency-free package a release app instantiates at startup with **no
//! DSL compiler present**. All three consume the same [`plan`]/frontend and walk the
//! same static-node subset in the same pre-order, so they build structurally
//! identical trees — this emitter just writes that tree out as bytes instead of
//! mounting it.
//!
//! The walk is a byte-for-byte twin of `commit.rs::build_tree`/`build_item`/
//! `build_node`: pre-order, control-flow regions rejected (the release package is the
//! static-node subset, exactly as Slice N/O), the same `NodeKey` numbering, and the
//! same folded-style semantics. That shared numbering is what lets a binding edge
//! reference a node by its pre-order index alone — the package needs no node names.
//!
//! What the package deliberately drops is every form of developer-only metadata
//! (section 60): a node's `type_name`/`local_name`, a property's name, source spans.
//! A binding needs only the durable [`StateKey`] identity (the `(hi, lo)` layout-twin
//! of the compiler's `SymbolId`) and the [`DirtyClass`] bits it invalidates — so the
//! property name is diagnostics-only and never enters the package. This is the
//! load-bearing exit criterion of Slice P made concrete: the release asset carries
//! none of the compiler's metadata, and the loader that reads it links with the
//! compiler absent.

use viso_ende::Encode;
use viso_ui::aot::{AotAxis, AotEdge, AotLength, AotNode, AotNodeKind, AotPackage, AotStyle};

use crate::diag::Diagnostic;
use crate::hotreload::plan::{CandidatePlan, plan};
use crate::ir::ui_ir::{AxisIr, LengthIr, NodeKind, StyleIr, UiItem, UiNode, UiTree};

/// Compile a fragment source straight into an encoded release package blob, or the
/// fatal diagnostics that make it uncompilable.
///
/// This is "build time: the same frontend that hot reload drives lowers the source
/// into a compact UI IR embedded as an asset" (section 41) made into one call:
/// [`plan`] runs the shared frontend and short-circuits on any fatal diagnostic, then
/// [`emit_package`] lowers the validated plan and [`AotPackage::encode_to_vec`] frames
/// it. The bytes are what an app embeds and the loader decodes — nothing here is
/// needed at run time.
pub fn build_package(source: &str) -> Result<Vec<u8>, Vec<Diagnostic>> {
    let plan = plan(source)?;
    Ok(emit_package(&plan).encode_to_vec())
}

/// Lower a compiled, validated [`CandidatePlan`] into a compact [`AotPackage`].
///
/// The node table is authored in the same pre-order the commit's `build_tree` walks,
/// so a node's index in [`AotPackage::nodes`] is its [`NodeKey`] — the identity the
/// binding edges reference. Control-flow regions are not part of the static-node
/// subset and are skipped (the plan's own edges never reference them; a
/// control-flow-bearing source is rejected before it reaches a release package, the
/// same boundary Slice N/O enforce).
pub fn emit_package(plan: &CandidatePlan) -> AotPackage {
    let mut nodes: Vec<AotNode> = Vec::new();
    emit_tree(&plan.tree, &mut nodes);

    // Each static edge wires a source `SymbolId` to a template `NodeKey` with a set of
    // dirty classes. The `SymbolId` is bridged to the runtime `StateKey` by its
    // `(hi, lo)` parts (the two share a `#[repr(C)]` layout, so no compiler type
    // crosses into the package), the `NodeKey` is its pre-order index, and the class
    // is the raw bitset byte the loader rebuilds from named constants. The property
    // name the edge came from is dropped — the runtime never consults it.
    let mut edges: Vec<AotEdge> = Vec::with_capacity(plan.bindings.static_edges().count());
    for edge in plan.bindings.static_edges() {
        edges.push(AotEdge {
            state: viso_ui::state::StateKey::from_parts(edge.source.hi, edge.source.lo),
            node: edge.node.0,
            class: edge.class.bits(),
        });
    }

    AotPackage { nodes, edges }
}

/// Author the whole tree into `nodes`, mirroring the commit's `build_tree`: each
/// top-level item is walked in source order, and its pre-order position in `nodes` is
/// its `NodeKey`.
fn emit_tree(tree: &UiTree, nodes: &mut Vec<AotNode>) {
    for item in &tree.items {
        emit_item(item, nodes);
    }
}

/// Author one item, mirroring the commit's `build_item`: only a mounted node produces
/// a package node; a control-flow region is out of the static-node subset and is
/// skipped without authoring anything (so no `NodeKey` is consumed for it, exactly as
/// the commit does not author one).
fn emit_item(item: &UiItem, nodes: &mut Vec<AotNode>) {
    let UiItem::Node(node) = item else {
        return;
    };
    emit_node(node, nodes);
}

/// Author one node and its subtree, mirroring the commit's `build_node`: the builder
/// kind is selected the same way (a `VirtualList` collapses to a leaf, its `for` body
/// being a rejected control-flow region), the style is folded the same way, and the
/// node reserves its own pre-order slot *before* its children are authored so the
/// index a binding edge references is stable regardless of subtree size.
fn emit_node(node: &UiNode, nodes: &mut Vec<AotNode>) {
    let kind = aot_kind(node.kind);
    // Reserve this node's pre-order slot now; its `child_count` is patched in after the
    // children are authored, so the index stays this node's `NodeKey` even as the
    // children push higher indices.
    let index = nodes.len();
    nodes.push(AotNode {
        kind,
        style: aot_style(&node.style),
        child_count: 0,
    });

    // A container descends into exactly its children; a leaf (or a collapsed
    // VirtualList) authors none, matching the commit's builder-closure recursion.
    let mut child_count = 0u32;
    if kind.is_container() {
        for child in &node.children {
            let before = nodes.len();
            emit_item(child, nodes);
            // Only items that actually authored a node count as immediate children —
            // a skipped control-flow region contributes none, keeping `child_count` in
            // step with what the loader descends into.
            if nodes.len() > before {
                child_count += 1;
            }
        }
    }
    nodes[index].child_count = child_count;
}

/// The package builder kind for a lowered [`NodeKind`], mirroring the commit's
/// builder selection: `VirtualList` collapses to a leaf (its body is a rejected
/// control-flow region), everything else maps one-to-one.
fn aot_kind(kind: NodeKind) -> AotNodeKind {
    match kind {
        NodeKind::Flex => AotNodeKind::Flex,
        NodeKind::Grid => AotNodeKind::Grid,
        NodeKind::Scroll => AotNodeKind::Scroll,
        NodeKind::VirtualList | NodeKind::Leaf => AotNodeKind::Leaf,
    }
}

/// Fold a lowered [`StyleIr`] into the package's [`AotStyle`], carrying each optional
/// scalar across verbatim. An unset field stays `None` so the loader applies the same
/// runtime builder default the commit's `flex_style`/`scroll_style`/`leaf_style` do.
fn aot_style(style: &StyleIr) -> AotStyle {
    AotStyle {
        axis: style.axis.map(aot_axis),
        width: style.width.map(aot_length),
        height: style.height.map(aot_length),
        gap: style.gap,
    }
}

/// One lowered axis to the package axis.
fn aot_axis(axis: AxisIr) -> AotAxis {
    match axis {
        AxisIr::Row => AotAxis::Row,
        AxisIr::Column => AotAxis::Column,
    }
}

/// One lowered length to the package length; the variants map one-to-one.
fn aot_length(len: LengthIr) -> AotLength {
    match len {
        LengthIr::Fixed(px) => AotLength::Fixed(px),
        LengthIr::Fill { weight } => AotLength::Fill { weight },
        LengthIr::Fit => AotLength::Fit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ende::Decode;

    #[test]
    fn emits_a_pre_order_node_table_with_child_counts() {
        // Row { Text {} Text {} } → a flex root with two leaf children.
        let pkg = emit_package(&plan("Row { Text {} Text {} }").expect("compiles"));
        assert_eq!(pkg.nodes.len(), 3, "root + two leaves");
        assert_eq!(pkg.nodes[0].kind, AotNodeKind::Flex);
        assert_eq!(pkg.nodes[0].child_count, 2);
        assert_eq!(pkg.nodes[1].kind, AotNodeKind::Leaf);
        assert_eq!(pkg.nodes[1].child_count, 0);
        assert_eq!(pkg.nodes[2].kind, AotNodeKind::Leaf);
    }

    #[test]
    fn a_bound_property_becomes_an_edge_by_identity_not_name() {
        // The single node is index 0; its `text` binds to the `label` source.
        let candidate = plan("Text { text: label; }").expect("compiles");
        let pkg = emit_package(&candidate);
        assert_eq!(pkg.edges.len(), 1, "one static binding edge");
        let edge = pkg.edges[0];
        assert_eq!(edge.node, 0, "edge references the node's pre-order index");

        // The edge's state key is the `SymbolId` of `label`, bridged by its parts —
        // no name string enters the package.
        let sym = candidate
            .symbol_for_name("label")
            .expect("label is a source");
        assert_eq!(
            edge.state,
            viso_ui::state::StateKey::from_parts(sym.hi, sym.lo)
        );
    }

    #[test]
    fn node_indices_are_the_binding_keys() {
        // A nested tree: the deep leaf's edge must reference its own pre-order index,
        // proving the emitter's numbering matches the binding IR's `NodeKey` walk.
        let candidate = plan("Column { Row { Text { text: label; } } }").expect("compiles");
        let pkg = emit_package(&candidate);
        // pre-order: Column(0) Row(1) Text(2).
        assert_eq!(pkg.nodes.len(), 3);
        assert_eq!(pkg.edges.len(), 1);
        assert_eq!(
            pkg.edges[0].node, 2,
            "the deepest leaf is pre-order index 2"
        );
        // And that is exactly the NodeKey the binding IR assigned.
        let edge = candidate.bindings.static_edges().next().expect("one edge");
        assert_eq!(edge.node.0, pkg.edges[0].node);
    }

    #[test]
    fn build_package_round_trips_through_the_loader_format() {
        // The end of the emitter is bytes the loader decodes; the DSL side only has to
        // produce a well-formed blob (the full load closure is exercised in the
        // headless integration test, which imports only viso-ui).
        let blob = build_package("Row { Text { text: label; } }").expect("compiles");
        let pkg = viso_ui::aot::AotPackage::decode_from_slice(&blob).expect("decodes");
        assert_eq!(pkg.nodes.len(), 2, "Row + Text");
        assert_eq!(pkg.edges.len(), 1);
    }

    #[test]
    fn a_malformed_source_yields_diagnostics_not_a_package() {
        let err = build_package("Text { text: ;;; }").expect_err("malformed is fatal");
        assert!(!err.is_empty(), "carries the fatal diagnostics");
    }
}
