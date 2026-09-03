//! Section-2 tests: the UI IR lowering pass and the property → dirty-class table.
//!
//! These drive a real `ui!` fragment through parse → typed AST → `lower_fragment_items`
//! and assert the retained-tree template it produces: node kinds resolved from
//! type names, static style folded from compile-time-constant properties
//! (`Dp`/`Px`/`%` dimensions, `row`/`column` axes), reactive properties
//! recorded as pending bindings rather than silently dropped, and control-flow
//! regions (`if`/`for`/`match`) preserved with their head spans. A separate set
//! covers the dirty-class table directly, since its bit assignments are
//! load-bearing for the emitter that decomposes them back into runtime constants.

use viso_dsl::ast::{AstNode, ViewFragment};
use viso_dsl::ir::{
    AxisIr, DirtyClass, LengthIr, NodeKind, UiItem, UiNode, lower_fragment_items,
    property_dirty_class,
};
use viso_dsl::syntax::grammar::{Entry, parse_entry};
use viso_dsl::syntax::{SyntaxNode, tokenize};

/// Parses `src` as a `ui!` view fragment and lowers its items to a UI tree's items.
fn lower(src: &str) -> Vec<UiItem> {
    let root = SyntaxNode::new_root(parse_entry(&tokenize(src), src, Entry::ViewFragment).root);
    let fragment = ViewFragment::cast(root).expect("a ViewFragment root");
    lower_fragment_items(fragment.items()).items
}

/// The sole node at the top level of a lowered fragment.
fn only_node(src: &str) -> UiNode {
    let items = lower(src);
    assert_eq!(items.len(), 1, "one top-level item");
    match items.into_iter().next().unwrap() {
        UiItem::Node(n) => n,
        other => panic!("expected a node, got {other:?}"),
    }
}

#[test]
fn column_with_text_lowers_to_flex_over_leaf() {
    let column = only_node("Column { Text { } }");
    assert_eq!(column.type_name, "Column");
    assert_eq!(column.kind, NodeKind::Flex, "Column maps to a flex builder");
    assert_eq!(
        column.style.axis,
        Some(AxisIr::Column),
        "the Column type implies a column axis"
    );

    assert_eq!(column.children.len(), 1, "one child");
    let child = match &column.children[0] {
        UiItem::Node(n) => n,
        other => panic!("expected a child node, got {other:?}"),
    };
    assert_eq!(child.type_name, "Text");
    assert_eq!(child.kind, NodeKind::Leaf, "Text maps to a leaf builder");
    assert!(child.children.is_empty(), "Text has no children");
}

#[test]
fn row_and_containers_resolve_their_kind_and_axis() {
    assert_eq!(only_node("Row { }").style.axis, Some(AxisIr::Row));
    assert_eq!(only_node("Grid { }").kind, NodeKind::Grid);
    assert_eq!(only_node("Scroll { }").kind, NodeKind::Scroll);
    assert_eq!(only_node("List { }").kind, NodeKind::VirtualList);
    // An unknown type falls back to a leaf until component resolution lands.
    assert_eq!(only_node("Sparkline { }").kind, NodeKind::Leaf);
}

#[test]
fn static_dimensions_fold_into_style() {
    // `dp`/`px` and bare numbers fold to a fixed extent.
    let node = only_node("Leaf { width: 12dp; height: 34px; gap: 8; }");
    assert_eq!(node.style.width, Some(LengthIr::Fixed(12.0)));
    assert_eq!(node.style.height, Some(LengthIr::Fixed(34.0)));
    assert_eq!(node.style.gap, Some(8.0));
    assert!(node.pending.is_empty(), "constants folded, nothing pending");

    // `%` folds to a fill weight.
    let pct = only_node("Leaf { width: 50%; height: 25%; }");
    assert_eq!(pct.style.width, Some(LengthIr::Fill { weight: 0.5 }));
    assert_eq!(pct.style.height, Some(LengthIr::Fill { weight: 0.25 }));
}

#[test]
fn explicit_axis_property_folds_over_the_type_default() {
    let node = only_node("Flex { axis: row; }");
    assert_eq!(node.style.axis, Some(AxisIr::Row));
    assert!(node.pending.is_empty(), "axis folded, nothing pending");
}

#[test]
fn reactive_property_becomes_pending_not_dropped() {
    // A non-constant value (a path to state) is not folded; it is recorded as a
    // pending binding carrying the property's dirty class.
    let node = only_node("Text { text: label; }");
    assert!(node.style.is_empty(), "no static style folded");
    assert_eq!(node.pending.len(), 1, "one pending binding");
    let p = &node.pending[0];
    assert_eq!(p.name, "text");
    assert_eq!(
        p.dirty,
        DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS,
        "text content invalidates measure/layout/paint/semantics"
    );
    // The recorded span points at the value expression `label`.
    let span = &src_slice("Text { text: label; }", p.value);
    assert_eq!(span, "label");
}

/// The source substring a pending property's value span covers.
fn src_slice(src: &str, range: viso_dsl::syntax::TextRange) -> String {
    src[std::ops::Range::<usize>::from(range)].to_string()
}

#[test]
fn handlers_are_recorded_on_the_node() {
    let node = only_node("Button { on click { } }");
    assert_eq!(node.handlers.len(), 1);
    assert_eq!(node.handlers[0].event, "click");
}

#[test]
fn if_region_flattens_its_arms() {
    let items = lower("if ready { Text { } } else { Spinner { } }");
    let vi = match &items[0] {
        UiItem::If(vi) => vi,
        other => panic!("expected an if region, got {other:?}"),
    };
    assert_eq!(vi.arms.len(), 2, "then + else");
    assert!(
        vi.arms[0].condition.is_some(),
        "the then arm has a condition"
    );
    assert_eq!(vi.arms[0].items.len(), 1, "the then arm mounts one node");
    assert!(
        vi.arms[1].condition.is_none(),
        "the else arm has no condition"
    );

    // `else if` chains into a third arm with its own condition.
    let chained = lower("if a { X { } } else if b { Y { } } else { Z { } }");
    let ci = match &chained[0] {
        UiItem::If(vi) => vi,
        other => panic!("expected an if region, got {other:?}"),
    };
    assert_eq!(ci.arms.len(), 3, "if / else if / else");
    assert!(
        ci.arms[1].condition.is_some(),
        "the else-if arm has a condition"
    );
    assert!(ci.arms[2].condition.is_none(), "the trailing else has none");
}

#[test]
fn for_region_records_binding_iterable_and_key() {
    let items = lower("for item in items key item.id { Row { } }");
    let vf = match &items[0] {
        UiItem::For(vf) => vf,
        other => panic!("expected a for region, got {other:?}"),
    };
    assert_eq!(vf.binding.as_deref(), Some("item"));
    assert!(vf.iterable.is_some(), "the iterable span is recorded");
    assert!(vf.key.is_some(), "the key span is recorded");
    assert_eq!(vf.body.len(), 1, "the body mounts one node");
}

#[test]
fn match_region_records_scrutinee_and_arms() {
    let items = lower("match status { Active => { Text { } }, Idle => { Spinner { } } }");
    let vm = match &items[0] {
        UiItem::Match(vm) => vm,
        other => panic!("expected a match region, got {other:?}"),
    };
    assert!(vm.scrutinee.is_some(), "the scrutinee span is recorded");
    assert_eq!(vm.arms.len(), 2, "two arms");
    assert!(vm.arms[0].pattern.is_some(), "the first arm has a pattern");
    assert_eq!(vm.arms[0].items.len(), 1, "the first arm mounts one node");
}

// --- dirty-class table -------------------------------------------------------

#[test]
fn dirty_class_table_maps_property_semantics() {
    use DirtyClass as D;
    // Text content: the widest common invalidation.
    assert_eq!(
        property_dirty_class("text"),
        D::MEASURE | D::LAYOUT | D::PAINT | D::SEMANTICS
    );
    // Pure paint.
    assert_eq!(property_dirty_class("color"), D::PAINT);
    assert_eq!(property_dirty_class("background"), D::PAINT);
    // Dimensions re-measure and re-lay-out.
    assert_eq!(property_dirty_class("width"), D::MEASURE | D::LAYOUT);
    // Transform-only: no measure/layout.
    assert_eq!(
        property_dirty_class("transform"),
        D::TRANSFORM | D::HIT_TEST | D::PAINT
    );
    let t = property_dirty_class("transform");
    assert!(
        !t.contains(D::MEASURE) && !t.contains(D::LAYOUT),
        "a transform change never forces re-measure or re-layout"
    );
    // Accessibility-only.
    assert_eq!(property_dirty_class("aria_label"), D::SEMANTICS);
    // Unknown property: the conservative, never-empty default.
    let unknown = property_dirty_class("wobble");
    assert!(!unknown.is_empty(), "an unknown property is never a no-op");
    assert_eq!(unknown, D::MEASURE | D::LAYOUT | D::PAINT);
}

#[test]
fn dirty_class_bit_positions_are_stable() {
    // These bit positions are load-bearing: the emitter decomposes them back into
    // the runtime `viso_ui::DirtyClass` constants, so a shift here would misroute
    // invalidation. Pin them.
    assert_eq!(DirtyClass::EMPTY.bits(), 0);
    assert_eq!(DirtyClass::STRUCTURE.bits(), 1 << 0);
    assert_eq!(DirtyClass::STYLE.bits(), 1 << 1);
    assert_eq!(DirtyClass::MEASURE.bits(), 1 << 2);
    assert_eq!(DirtyClass::LAYOUT.bits(), 1 << 3);
    assert_eq!(DirtyClass::TRANSFORM.bits(), 1 << 4);
    assert_eq!(DirtyClass::PAINT.bits(), 1 << 5);
    assert_eq!(DirtyClass::HIT_TEST.bits(), 1 << 6);
    assert_eq!(DirtyClass::SEMANTICS.bits(), 1 << 7);
    // `contains` and `|` compose as a bitset.
    let set = DirtyClass::MEASURE | DirtyClass::PAINT;
    assert!(set.contains(DirtyClass::MEASURE));
    assert!(set.contains(DirtyClass::PAINT));
    assert!(!set.contains(DirtyClass::LAYOUT));
}
