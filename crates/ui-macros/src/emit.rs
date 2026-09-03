//! UI IR + Binding IR -> `viso_ui` builder `TokenStream` (AGENTS section 21.5, 59).
//!
//! The shared DSL frontend (driven from [`crate`]) turns a `ui! { ... }` body into
//! a static [`UiTree`] template plus a [`BindingIr`] of compiled
//! `StateId -> (node, DirtyClass)` edges. This module lowers those *data*
//! structures into the Rust tokens the macro expands to: a single builder closure
//! `|cx: &mut ::viso_ui::BuildCx<'_>| -> ::viso_ui::Handle` that mounts the retained
//! tree once and records each reactive binding against the retained node it targets.
//!
//! No runtime parse, no per-frame rebuild (section 59): every node is a direct
//! `cx.flex` / `cx.leaf` / … call, and every static [`BindingEdge`] becomes one
//! `cx.bind(<state>, <handle>, <DirtyClass>)` call — the compiled binding metadata
//! of section 10.2 that feeds the retained `BindingTable` static fast path.
//!
//! The walk replicates the Binding IR's pre-order [`NodeKey`] numbering exactly, so
//! the `Handle` captured for each builder call aligns with the `NodeKey` each edge
//! targets. Control-flow regions (`if`/`for`/`match`) are recorded in the IR but
//! their runtime reconciliation belongs to a consuming slice; rather than mount them
//! wrong (which would desync the NodeKey numbering and misroute every later
//! binding), the emitter surfaces an explicit `compile_error!` for this first cut,
//! preserving the "no silent wrong lowering" invariant.

use std::collections::HashMap;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;

use viso_dsl::ir::binding_ir::{BindingEdge, BindingIr, NodeKey};
use viso_dsl::ir::dirty_map::DirtyClass;
use viso_dsl::ir::ui_ir::{AxisIr, LengthIr, NodeKind, StyleIr, UiItem, UiNode, UiTree};
use viso_dsl::resolve::SymbolId;

/// A control-flow region encountered by the emitter, named for the diagnostic.
struct ControlFlow {
    kind: &'static str,
}

/// Lowers a fragment's [`UiTree`] + [`BindingIr`] to the builder closure tokens.
///
/// `sources` maps each reactive-source [`SymbolId`] the frontend minted back to the
/// user's in-scope Rust `StateId` identifier; Rust hygiene resolves that identifier
/// at the macro call site (the caller named the source, the frontend minted the id).
///
/// Returns `Err` with a rendered `compile_error!` payload span/message if the
/// fragment contains a control-flow region (deferred to a consuming slice) or a
/// binding whose source the caller did not name.
pub fn emit_fragment(
    tree: &UiTree,
    bindings: &BindingIr,
    sources: &HashMap<SymbolId, Ident>,
) -> Result<TokenStream, String> {
    // A fragment mounts one root today; a bare multi-root fragment has no single
    // Handle to return. Reject it explicitly rather than silently drop siblings.
    if tree.items.len() != 1 {
        return Err(format!(
            "a `ui!` fragment must have exactly one root node, found {}",
            tree.items.len()
        ));
    }

    // Index static binding edges by the node they target, so each node emits its
    // binds right after its Handle is captured. Dynamic edges are not part of this
    // first static cut; their presence is a frontend concern (the dynamic escape
    // hatch trips `dynamic_fallback_nodes`), not something the static emitter mounts.
    let mut edges_by_node: HashMap<NodeKey, Vec<&BindingEdge>> = HashMap::new();
    for edge in bindings.static_edges() {
        edges_by_node.entry(edge.node).or_default().push(edge);
    }

    let mut ctx = Emit {
        edges_by_node,
        sources,
        next_key: 0,
        control_flow: None,
        missing_source: None,
    };
    let root = ctx.emit_item(&tree.items[0]);

    if let Some(cf) = ctx.control_flow {
        return Err(format!(
            "`ui!` does not yet mount a `{}` region; control-flow reconciliation \
             lands in a later slice. Lift the branch into Rust for now.",
            cf.kind
        ));
    }
    if let Some(id) = ctx.missing_source {
        return Err(format!(
            "internal: a binding references source symbol {id:?} that was not among \
             the captured reactive sources"
        ));
    }

    Ok(quote! {
        |cx: &mut ::viso_ui::BuildCx<'_>| -> ::viso_ui::Handle {
            #root
        }
    })
}

/// The state threaded through the pre-order emit walk.
struct Emit<'a> {
    edges_by_node: HashMap<NodeKey, Vec<&'a BindingEdge>>,
    sources: &'a HashMap<SymbolId, Ident>,
    next_key: u32,
    control_flow: Option<ControlFlow>,
    missing_source: Option<SymbolId>,
}

impl Emit<'_> {
    /// Assigns the next pre-order [`NodeKey`], matching the Binding IR numbering.
    fn take_key(&mut self) -> NodeKey {
        let key = NodeKey(self.next_key);
        self.next_key += 1;
        key
    }

    /// Emits one item's tokens. A node consumes one key then descends; a
    /// control-flow region records the first-seen kind (deferred) and still walks
    /// its branches so any node it contains keeps the shared numbering aligned with
    /// the Binding IR — the emitter aborts on the region afterward.
    fn emit_item(&mut self, item: &UiItem) -> TokenStream {
        match item {
            UiItem::Node(node) => self.emit_node(node),
            UiItem::If(vi) => {
                self.note_control_flow("if");
                for arm in &vi.arms {
                    for item in &arm.items {
                        let _ = self.emit_item(item);
                    }
                }
                quote! {}
            }
            UiItem::For(vf) => {
                self.note_control_flow("for");
                for item in &vf.body {
                    let _ = self.emit_item(item);
                }
                quote! {}
            }
            UiItem::Match(vm) => {
                self.note_control_flow("match");
                for arm in &vm.arms {
                    for item in &arm.items {
                        let _ = self.emit_item(item);
                    }
                }
                quote! {}
            }
        }
    }

    /// Records the first control-flow region seen (later ones are subsumed by the
    /// single diagnostic).
    fn note_control_flow(&mut self, kind: &'static str) {
        if self.control_flow.is_none() {
            self.control_flow = Some(ControlFlow { kind });
        }
    }

    /// Emits a node: consumes its key, builds its children, invokes the matching
    /// builder, captures the returned `Handle`, then emits each binding edge that
    /// targets this node against that handle.
    fn emit_node(&mut self, node: &UiNode) -> TokenStream {
        let key = self.take_key();

        // Children are executed as statements inside the builder closure, which
        // returns `()`. Each child item's block evaluates to its own `Handle`; as a
        // child that value is discarded, so terminate it with `;` to keep the closure
        // body a statement sequence rather than a trailing `Handle` expression.
        let children: Vec<TokenStream> = node
            .children
            .iter()
            .map(|c| {
                let item = self.emit_item(c);
                quote! { #item; }
            })
            .collect();
        let child_block = quote! { #( #children )* };

        let handle_ident = node_handle_ident(key);
        let build_call = self.emit_builder_call(node, &child_block);

        let binds = self.emit_binds(key);

        // A leaf's builder takes no closure, so its children (there are none for a
        // real leaf) are dropped by `emit_builder_call`. Containers thread the child
        // block through their `FnOnce`.
        quote! {
            {
                let #handle_ident = #build_call;
                #binds
                #handle_ident
            }
        }
    }

    /// The `cx.<builder>(<style>, <children>)` (or `cx.leaf(<style>)`) call for a node.
    fn emit_builder_call(&self, node: &UiNode, child_block: &TokenStream) -> TokenStream {
        match node.kind {
            NodeKind::Flex => {
                let style = flex_style_tokens(&node.style);
                quote! { cx.flex(#style, |cx| { #child_block }) }
            }
            NodeKind::Grid => {
                // Grid style beyond the shared axis/size seam is a consuming-slice
                // concern; the folded StyleIr carries no grid track sizing yet, so a
                // default GridStyle mounts the container and its children.
                quote! { cx.grid(::core::default::Default::default(), |cx| { #child_block }) }
            }
            NodeKind::Scroll => {
                let axis = axis_tokens(node.style.axis.unwrap_or(AxisIr::Column));
                let size = size_tokens(&node.style);
                quote! {
                    cx.scroll(
                        ::viso_ui::ScrollStyle {
                            axis: #axis,
                            size: #size,
                            ..::core::default::Default::default()
                        },
                        |cx| { #child_block },
                    )
                }
            }
            NodeKind::VirtualList => {
                let axis = axis_tokens(node.style.axis.unwrap_or(AxisIr::Column));
                let size = size_tokens(&node.style);
                quote! {
                    cx.virtual_list(
                        ::viso_ui::VirtualListStyle {
                            axis: #axis,
                            size: #size,
                            ..::core::default::Default::default()
                        },
                        |cx| { #child_block },
                    )
                }
            }
            NodeKind::Leaf => {
                let style = leaf_style_tokens(&node.style);
                // A leaf has no builder closure; a Text-like leaf carries no children.
                quote! { cx.leaf(#style) }
            }
        }
    }

    /// The `cx.bind(<state>, <handle>, <DirtyClass>)` calls for every static edge on
    /// `key`. A source the caller did not name is recorded as an internal error
    /// rather than emitting a dangling identifier.
    fn emit_binds(&mut self, key: NodeKey) -> TokenStream {
        let Some(edges) = self.edges_by_node.get(&key) else {
            return quote! {};
        };
        let handle_ident = node_handle_ident(key);
        let mut calls = Vec::with_capacity(edges.len());
        for edge in edges {
            let Some(state_ident) = self.sources.get(&edge.source) else {
                if self.missing_source.is_none() {
                    self.missing_source = Some(edge.source);
                }
                continue;
            };
            let class = dirty_class_tokens(edge.class);
            calls.push(quote! {
                cx.bind(#state_ident, #handle_ident, #class);
            });
        }
        quote! { #( #calls )* }
    }
}

/// The per-node `Handle` binding identifier, unique by pre-order key
/// (`__viso_n0`, `__viso_n1`, …). Hygiene keeps these from clashing with user names.
fn node_handle_ident(key: NodeKey) -> Ident {
    Ident::new(&format!("__viso_n{}", key.0), Span::call_site())
}

/// `::viso_ui::FlexStyle { axis, gap?, size?, .. }` from a folded [`StyleIr`].
fn flex_style_tokens(style: &StyleIr) -> TokenStream {
    let mut fields = Vec::new();
    if let Some(axis) = style.axis {
        let axis = axis_tokens(axis);
        fields.push(quote! { axis: #axis });
    }
    if let Some(gap) = style.gap {
        fields.push(quote! { gap: #gap });
    }
    if style.width.is_some() || style.height.is_some() {
        let size = size_tokens(style);
        fields.push(quote! { size: #size });
    }
    quote! {
        ::viso_ui::FlexStyle {
            #( #fields, )*
            ..::core::default::Default::default()
        }
    }
}

/// `::viso_ui::LeafStyle { size?, .. }` from a folded [`StyleIr`].
fn leaf_style_tokens(style: &StyleIr) -> TokenStream {
    if style.width.is_some() || style.height.is_some() {
        let size = size_tokens(style);
        quote! {
            ::viso_ui::LeafStyle {
                size: #size,
                ..::core::default::Default::default()
            }
        }
    } else {
        quote! { ::core::default::Default::default() }
    }
}

/// `::viso_ui::Size { width, height }` from a folded [`StyleIr`]. A missing axis
/// length defaults to `Length::Fit` (shrink to natural size), the neutral request.
fn size_tokens(style: &StyleIr) -> TokenStream {
    let width = length_tokens(style.width);
    let height = length_tokens(style.height);
    quote! { ::viso_ui::Size { width: #width, height: #height } }
}

/// `::viso_ui::Length::…` from an optional folded [`LengthIr`]; `None` -> `Fit`.
fn length_tokens(length: Option<LengthIr>) -> TokenStream {
    match length {
        Some(LengthIr::Fixed(px)) => quote! { ::viso_ui::Length::Fixed(#px) },
        Some(LengthIr::Fill { weight }) => quote! { ::viso_ui::Length::Fill { weight: #weight } },
        Some(LengthIr::Fit) | None => quote! { ::viso_ui::Length::Fit },
    }
}

/// `::viso_ui::Axis::…` from a folded [`AxisIr`].
fn axis_tokens(axis: AxisIr) -> TokenStream {
    match axis {
        AxisIr::Row => quote! { ::viso_ui::Axis::Row },
        AxisIr::Column => quote! { ::viso_ui::Axis::Column },
    }
}

/// Decomposes a [`DirtyClass`] bitset into a `::viso_ui::DirtyClass` `|` chain.
///
/// The dsl and runtime `DirtyClass` bit layouts are byte-identical (pinned by
/// `dirty_class_bit_positions_are_stable` on the dsl side and the runtime consts),
/// so each set bit maps to the same-named runtime constant. Emitting the OR chain of
/// named constants — rather than a raw `from_bits` — keeps the expansion readable and
/// independent of any private raw constructor.
fn dirty_class_tokens(class: DirtyClass) -> TokenStream {
    // (bit-const, runtime ident) in ascending bit order; every set bit contributes.
    const BITS: &[(DirtyClass, &str)] = &[
        (DirtyClass::STRUCTURE, "STRUCTURE"),
        (DirtyClass::STYLE, "STYLE"),
        (DirtyClass::MEASURE, "MEASURE"),
        (DirtyClass::LAYOUT, "LAYOUT"),
        (DirtyClass::TRANSFORM, "TRANSFORM"),
        (DirtyClass::PAINT, "PAINT"),
        (DirtyClass::HIT_TEST, "HIT_TEST"),
        (DirtyClass::SEMANTICS, "SEMANTICS"),
    ];

    let mut terms = Vec::new();
    for (bit, name) in BITS {
        if class.contains(*bit) {
            let ident = Ident::new(name, Span::call_site());
            terms.push(quote! { ::viso_ui::DirtyClass::#ident });
        }
    }

    if terms.is_empty() {
        // property_dirty_class never returns EMPTY, but be explicit rather than emit
        // an empty expression if a future class is all-zero.
        quote! { ::viso_ui::DirtyClass::EMPTY }
    } else {
        quote! { #( #terms )|* }
    }
}
