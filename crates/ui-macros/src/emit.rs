//! UI IR + Binding IR -> `viso_ui` builder `TokenStream` (AGENTS section 21.5, 59).
//!
//! The shared DSL frontend turns a view — a `ui!` fragment, or the `view` block of
//! a `component!` or a `.vs` component — into a static [`UiTree`] template plus a
//! [`BindingIr`] of compiled `StateId -> (node, DirtyClass)` edges. This module
//! lowers those *data* structures into the builder expression every entry point
//! expands around: it mounts the retained tree once through a `cx` of type
//! `&mut ::viso_ui::BuildCx<'_>`, records each reactive binding against the
//! retained node it targets, and evaluates to the root's `::viso_ui::Handle`.
//!
//! No runtime parse, no per-frame rebuild (section 59): every node is a direct
//! `cx.flex` / `cx.leaf` / … call, and every static [`BindingEdge`] becomes one
//! `cx.bind(<state>, <handle>, <DirtyClass>)` call — the compiled binding metadata
//! of section 10.2 that feeds the retained `BindingTable` static fast path.
//!
//! The walk replicates the Binding IR's pre-order [`NodeKey`] numbering exactly, so
//! the `Handle` captured for each builder call aligns with the `NodeKey` each edge
//! targets. The emitter authors only the *static* nodes, those outside every
//! control-flow region (`if`/`for`/`match`): it numbers a region's content
//! without building it, records each static node's id by its static pre-order
//! index, and after the build hands those ids to the view runtime, which mounts
//! the regions from the templates the expansion embeds.

use std::collections::HashMap;

use proc_macro2::{Literal, Span, TokenStream};
use quote::quote;
use syn::Ident;

use viso_dsl::ir::binding_ir::{BindingEdge, BindingIr, NodeKey};
use viso_dsl::ir::dirty_map::DirtyClass;
use viso_dsl::ir::ui_ir::{
    Avoid, AxisIr, LengthIr, LengthsIr, NodeKind, StyleIr, TermsIr, UiItem, UiNode, UiTree,
};
use viso_dsl::resolve::SymbolId;
use viso_dsl::view_behavior::{Control, ViewBehavior};

/// Lowers a view's [`UiTree`] + [`BindingIr`] to the builder expression.
///
/// `sources` maps each reactive-source [`SymbolId`] to the Rust identifier of the
/// `StateId` in scope where the expression expands: the caller's own for a `ui!`
/// fragment, a local the expansion allocates for a component's `state`.
///
/// `behavior` is the view's handler table and regions: each node with routes
/// attaches them to the `__viso_host` in scope where the expression expands, and
/// the regions mount on it under the static nodes once the tree is built, and
/// then the values the static nodes show are delivered.
///
/// `record` is the mount record a `view!` hands the development session, every
/// field but the root and the static nodes, which the emitter supplies once the
/// tree is built; `None` for a form that records no mount.
///
/// Returns `Err` with the message if the view has other than one root, has a
/// region but no behavior to run it, or binds a source `sources` does not name.
pub fn emit_view(
    tree: &UiTree,
    bindings: &BindingIr,
    sources: &HashMap<SymbolId, Ident>,
    behavior: Option<&ViewBehavior>,
    record: Option<TokenStream>,
) -> Result<TokenStream, String> {
    // A view mounts one root; a multi-root view has no single Handle to return.
    // Reject it explicitly rather than silently drop siblings.
    if tree.items.len() != 1 {
        return Err(format!(
            "E3002: a view must have exactly one root node, found {}",
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

    let regions = behavior.filter(|behavior| !behavior.regions.is_empty());
    if regions.is_none() && viso_dsl::view_regions::has_regions(tree) {
        return Err("internal: a view with regions has no behavior to run them".into());
    }
    let mut ctx = Emit {
        edges_by_node,
        sources,
        behavior,
        next_key: 0,
        next_static: 0,
        record_ids: regions.is_some() || behavior.is_some_and(|b| !b.env.is_empty()),
        static_keys: Vec::new(),
        shown: Vec::new(),
        missing_source: None,
        error: None,
    };
    let mut root = ctx.emit_item(&tree.items[0]);
    let mut record = record;

    if let Some(behavior) = behavior.filter(|_| ctx.record_ids) {
        let count = ctx.next_static as usize;
        let reads = behavior.env.iter().map(|read| {
            let slot = read.slot as usize;
            let field = read.field.tag();
            let anchor = match ctx.static_keys.iter().position(|&key| key == read.anchor.0) {
                Some(ordinal) => quote! { ::core::option::Option::Some(#ordinal) },
                None => quote! { ::core::option::Option::None },
            };
            quote! { (#slot, #field, #anchor) }
        });
        let link = if behavior.env.is_empty() {
            quote! {}
        } else {
            quote! { ::viso_view::__link_env(cx, &__viso_host, &__viso_ids, &[#(#reads),*]); }
        };
        let mount = regions.map(|behavior| {
            let bytes = Literal::byte_string(&behavior.region_bytes());
            let mut cells: Vec<(&SymbolId, &Ident)> = sources.iter().collect();
            cells.sort_unstable_by_key(|(symbol, _)| (symbol.hi, symbol.lo));
            let cells = cells.into_iter().map(|(symbol, local)| {
                let (hi, lo) = (symbol.hi, symbol.lo);
                quote! { (::viso_ui::state::StateKey::from_parts(#hi, #lo), #local) }
            });
            quote! {
                ::viso_view::__mount_embedded(
                    cx,
                    #bytes,
                    &__viso_host,
                    &__viso_ids,
                    &[#(#cells),*],
                );
            }
        });
        let mounted = record.take().map(|record| {
            let keys = &ctx.static_keys;
            quote! {
                ::viso_view::__record_mount! {
                    root: __viso_root.id(),
                    #record
                    nodes: ::viso_view::__static_nodes(&[#(#keys),*], &__viso_ids),
                }
            }
        });
        root = quote! {
            {
                let mut __viso_ids: [::core::option::Option<::viso_ui::NodeId>; #count] =
                    [::core::option::Option::None; #count];
                let __viso_root = #root;
                #link
                #mount
                #mounted
                __viso_root
            }
        };
    }
    if !ctx.shown.is_empty() {
        let count = ctx.shown.len();
        let controls = &ctx.shown;
        root = quote! {
            {
                let mut __viso_shown: [::core::option::Option<::viso_ui::NodeId>; #count] =
                    [::core::option::Option::None; #count];
                let __viso_root = #root;
                ::viso_view::__mount_values(cx, &__viso_host, &__viso_shown, &[#(#controls),*]);
                __viso_root
            }
        };
    }
    if let Some(record) = record {
        root = quote! {
            {
                let __viso_root = #root;
                ::viso_view::__record_mount! {
                    root: __viso_root.id(),
                    #record
                    nodes: ::std::vec::Vec::new(),
                }
                __viso_root
            }
        };
    }
    if let Some(message) = ctx.error {
        return Err(message);
    }
    if let Some(id) = ctx.missing_source {
        return Err(format!(
            "internal: a binding references source symbol {id:?} that was not among \
             the captured reactive sources"
        ));
    }

    Ok(root)
}

/// The state threaded through the pre-order emit walk.
struct Emit<'a> {
    edges_by_node: HashMap<NodeKey, Vec<&'a BindingEdge>>,
    sources: &'a HashMap<SymbolId, Ident>,
    behavior: Option<&'a ViewBehavior>,
    next_key: u32,
    /// The next static pre-order index.
    next_static: u32,
    /// Whether each static node's id is recorded, for the regions to mount
    /// under and the `env` reads to anchor at.
    record_ids: bool,
    /// The template key of each recorded static node, by static ordinal.
    static_keys: Vec<u32>,
    /// The control of each static node showing a view value, in the order the
    /// nodes record their ids.
    shown: Vec<TokenStream>,
    missing_source: Option<SymbolId>,
    /// The first node that cannot be built.
    error: Option<String>,
}

impl Emit<'_> {
    /// Assigns the next pre-order [`NodeKey`], matching the Binding IR numbering.
    fn take_key(&mut self) -> NodeKey {
        let key = NodeKey(self.next_key);
        self.next_key += 1;
        key
    }

    /// Emits one item's tokens: a node is built; a region is numbered but not
    /// built, its content mounted by the view runtime.
    fn emit_item(&mut self, item: &UiItem) -> TokenStream {
        match item {
            UiItem::Node(node) => self.emit_node(node),
            _ => {
                self.skip_item(item);
                quote! {}
            }
        }
    }

    /// Advances the numbering past every node under `item`, building none.
    fn skip_item(&mut self, item: &UiItem) {
        match item {
            UiItem::Node(node) => {
                self.take_key();
                for child in &node.children {
                    self.skip_item(child);
                }
            }
            UiItem::If(region) => {
                for arm in &region.arms {
                    for item in &arm.items {
                        self.skip_item(item);
                    }
                }
            }
            UiItem::For(region) => {
                for item in &region.body {
                    self.skip_item(item);
                }
            }
            UiItem::Match(region) => {
                for arm in &region.arms {
                    for item in &arm.items {
                        self.skip_item(item);
                    }
                }
            }
        }
    }

    /// Emits a node: consumes its key, builds its children, invokes the matching
    /// builder, captures the returned `Handle`, then emits each binding edge that
    /// targets this node against that handle.
    fn emit_node(&mut self, node: &UiNode) -> TokenStream {
        let key = self.take_key();
        let ordinal = self.next_static as usize;
        self.next_static += 1;
        if self.record_ids {
            self.static_keys.push(key.0);
        }

        // A node that authors no children (a leaf, a `VirtualList`, which mounts
        // its own items) numbers them without building them.
        let authors_children = node.kind.is_container() && node.kind != NodeKind::VirtualList;
        if !authors_children {
            for child in &node.children {
                self.skip_item(child);
            }
        }

        // Children are executed as statements inside the builder closure, which
        // returns `()`. Each child item's block evaluates to its own `Handle`; as a
        // child that value is discarded, so terminate it with `;` to keep the closure
        // body a statement sequence rather than a trailing `Handle` expression.
        let children: Vec<TokenStream> = node
            .children
            .iter()
            .filter(|_| authors_children)
            .map(|c| {
                let item = self.emit_item(c);
                quote! { #item; }
            })
            .collect();
        let child_block = quote! { #( #children )* };

        let handle_ident = node_handle_ident(key);
        let build_call = match self.emit_builder_call(node, &child_block) {
            Ok(call) => call,
            Err(message) => {
                self.error.get_or_insert(message);
                quote! { ::core::unreachable!() }
            }
        };

        let lengths = lengths_tokens(node.style.lengths(), &handle_ident);
        let binds = self.emit_binds(key);
        let attach = self.emit_attach(key);
        let show = match self.behavior.and_then(|behavior| behavior.control(key)) {
            Some(control) => {
                let index = self.shown.len();
                self.shown.push(control_tokens(control));
                quote! { __viso_shown[#index] = ::core::option::Option::Some(#handle_ident.id()); }
            }
            None => quote! {},
        };
        let record = if self.record_ids {
            quote! { __viso_ids[#ordinal] = ::core::option::Option::Some(#handle_ident.id()); }
        } else {
            quote! {}
        };

        // A leaf's builder takes no closure, so its children (there are none for a
        // real leaf) are dropped by `emit_builder_call`. Containers thread the child
        // block through their `FnOnce`.
        quote! {
            {
                let #handle_ident = #build_call;
                #lengths
                #binds
                #attach
                #record
                #show
                #handle_ident
            }
        }
    }

    /// The `cx.<builder>(<style>, <children>)` (or `cx.leaf(<style>)`) call for a node.
    fn emit_builder_call(
        &self,
        node: &UiNode,
        child_block: &TokenStream,
    ) -> Result<TokenStream, String> {
        Ok(match node.kind {
            NodeKind::Flex => {
                let style = flex_style_tokens(&node.style);
                match node.style.scope {
                    Some(scope) => {
                        let basis = match scope.basis {
                            Some(basis) => quote! { ::core::option::Option::Some(#basis) },
                            None => quote! { ::core::option::Option::None },
                        };
                        quote! { cx.adaptive_scope(#basis, #style, |cx| { #child_block }) }
                    }
                    None => match node.style.avoid {
                        Some(avoid) => {
                            let avoid = match avoid {
                                Avoid::SafeArea => quote! { SafeArea },
                                Avoid::Keyboard => quote! { Keyboard },
                            };
                            quote! {
                                cx.avoiding(
                                    ::viso_ui::adaptive::Avoid::#avoid,
                                    #style,
                                    |cx| { #child_block },
                                )
                            }
                        }
                        None => quote! { cx.flex(#style, |cx| { #child_block }) },
                    },
                }
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
                        0,
                        |_, _| {},
                    )
                }
            }
            NodeKind::Leaf => {
                let style = leaf_style_tokens(&node.style);
                quote! { cx.leaf(#style) }
            }
            NodeKind::Component => {
                let path = syn::parse_str::<syn::Path>(&node.type_name)
                    .map_err(|_| format!("`{}` is not a Rust path", node.type_name))?;
                quote! { #path::build(cx).1 }
            }
        })
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

impl Emit<'_> {
    /// The `::viso_view::attach` call installing `key`'s handler routes and
    /// native control response, if it has any.
    fn emit_attach(&self, key: NodeKey) -> TokenStream {
        let Some(behavior) = self.behavior else {
            return quote! {};
        };
        let (routes, control) = (behavior.routes(key), behavior.control(key));
        if routes.is_empty() && control.is_none() {
            return quote! {};
        }
        let handle_ident = node_handle_ident(key);
        let routes = routes.iter().map(|(route, index)| {
            let variant = Ident::new(route.variant(), Span::call_site());
            quote! { (::viso_view::EventRoute::#variant, #index) }
        });
        let control = match control {
            Some(control) => {
                let control = control_tokens(control);
                quote! { ::core::option::Option::Some(#control) }
            }
            None => quote! { ::core::option::Option::None },
        };
        quote! {
            ::viso_view::attach(cx, &__viso_host, #handle_ident, &[#(#routes),*], #control, &::viso_view::Scope::EMPTY);
        }
    }
}

/// The `::viso_view::Control` expression of `control`.
fn control_tokens(control: Control) -> TokenStream {
    let kind = Ident::new(control.kind.variant(), Span::call_site());
    let entry = |entry: Option<u32>| match entry {
        Some(entry) => quote! { ::core::option::Option::Some(#entry) },
        None => quote! { ::core::option::Option::None },
    };
    let (value, min, max, step) = (
        entry(control.value),
        entry(control.min),
        entry(control.max),
        entry(control.step),
    );
    let look = control.look;
    let (background, opacity) = (entry(look.background), entry(look.opacity));
    let (background_transition, opacity_transition) = (
        entry(look.background_transition),
        entry(look.opacity_transition),
    );
    quote! {
        ::viso_view::Control {
            kind: ::viso_view::ControlKind::#kind,
            value: #value,
            min: #min,
            max: #max,
            step: #step,
            look: ::viso_view::Look {
                background: #background,
                opacity: #opacity,
                background_transition: #background_transition,
                opacity_transition: #opacity_transition,
            },
        }
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
    if has_size(style) {
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
    if has_size(style) {
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

/// Whether the node authored a width or a height, folded or environment-bound.
fn has_size(style: &StyleIr) -> bool {
    style.width.is_some()
        || style.height.is_some()
        || style.lengths().width.is_some()
        || style.lengths().height.is_some()
}

/// `cx.bind_lengths(handle, ::viso_ui::NodeLengths { .. });` for a node with
/// environment lengths, and nothing otherwise.
fn lengths_tokens(lengths: &LengthsIr, handle: &Ident) -> TokenStream {
    if lengths.is_empty() {
        return quote! {};
    }
    let width = terms_tokens(lengths.width);
    let height = terms_tokens(lengths.height);
    let gap = terms_tokens(lengths.gap);
    let font_size = terms_tokens(lengths.font_size);
    quote! {
        cx.bind_lengths(#handle, ::viso_ui::NodeLengths {
            width: #width,
            height: #height,
            gap: #gap,
            font_size: #font_size,
        });
    }
}

/// `Option<::viso_ui::LengthTerms>` from optional [`TermsIr`].
fn terms_tokens(terms: Option<TermsIr>) -> TokenStream {
    match terms {
        Some(TermsIr {
            dp,
            px,
            sp,
            em,
            pct,
        }) => quote! {
            ::core::option::Option::Some(::viso_ui::LengthTerms {
                dp: #dp, px: #px, sp: #sp, em: #em, pct: #pct,
            })
        },
        None => quote! { ::core::option::Option::None },
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
        Some(LengthIr::Fixed(dp)) => quote! { ::viso_ui::Length::Fixed(#dp) },
        Some(LengthIr::Relative { fixed, pct }) => {
            quote! { ::viso_ui::Length::Relative { fixed: #fixed, pct: #pct } }
        }
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
