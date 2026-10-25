//! UI IR + Binding IR -> `viso_ui` builder `TokenStream`.
//!
//! The shared DSL frontend turns a view — a `ui!` fragment, or the `view` block of
//! a `component!` or a `.vs` component — into a static [`UiTree`] template plus a
//! [`BindingIr`] of compiled `StateId -> (node, DirtyClass)` edges. This module
//! lowers those *data* structures into the builder expression every entry point
//! expands around: it mounts the retained tree once through a `cx` of type
//! `&mut ::viso::__private::viso_ui::BuildCx<'_>`, records each reactive binding against the
//! retained node it targets, and evaluates to the root's `::viso::__private::viso_ui::Handle`.
//!
//! No runtime parse, no per-frame rebuild: every node is a direct
//! `cx.flex` / `cx.leaf` / … call, and every static [`BindingEdge`] becomes one
//! `cx.bind(<state>, <handle>, <DirtyClass>)` call — the compiled binding metadata
//! that feeds the retained `BindingTable` static fast path.
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
use viso_dsl::ir::ui_ir::Span as LayoutSpan;
use viso_dsl::ir::ui_ir::{
    AdaptiveColumns, Align, AlignItems, Alignment2D, AotSheet, AotSnap, AutoRepeat, Avoid, AxisIr,
    BoxRules, EdgeTermsIr, Inset, InsetIr, InsetTermsIr, Justify, LayoutDirection, LengthIr,
    LengthsIr, NodeKind, Placement, ScrollIr, SheetDetent, SheetEdge, StyleIr, TermsIr, TrackMax,
    TrackSizing, UiItem, UiNode, UiTree,
};
use viso_dsl::resolve::SymbolId;
use viso_dsl::view_behavior::{Control, GameQuery, Switched, ViewBehavior, When};

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
        record_ids: regions.is_some()
            || behavior
                .is_some_and(|b| !b.env.is_empty() || !b.named.is_empty() || !b.game.is_empty()),
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
            quote! { ::viso::__private::viso_view::__link_env(cx, &__viso_host, &__viso_ids, &[#(#reads),*]); }
        };
        let game = (!behavior.game.is_empty()).then(|| {
            let reads = behavior.game.iter().map(|read| {
                let slot = read.slot as usize;
                let (tag, system, function) = match &read.query {
                    GameQuery::Paused => (0u8, "", ""),
                    GameQuery::Tick => (1u8, "", ""),
                    GameQuery::System { system, function } => (2u8, &**system, &**function),
                };
                quote! { (#slot, #tag, #system, #function) }
            });
            quote! {
                ::viso::__private::viso_view::__link_game(cx, &__viso_host, __viso_root.id(), &[#(#reads),*]);
            }
        });
        let mount = regions.map(|behavior| {
            let bytes = aligned_bytes(&behavior.region_bytes());
            let mut cells: Vec<(&SymbolId, &Ident)> = sources.iter().collect();
            cells.sort_unstable_by_key(|(symbol, _)| (symbol.hi, symbol.lo));
            let cells = cells.into_iter().map(|(symbol, local)| {
                let (hi, lo) = (symbol.hi, symbol.lo);
                quote! { (::viso::__private::viso_ui::state::StateKey::from_parts(#hi, #lo), #local) }
            });
            quote! {
                ::viso::__private::viso_view::__mount_embedded(
                    cx,
                    #bytes,
                    &__viso_host,
                    &__viso_ids,
                    &[#(#cells),*],
                );
            }
        });
        let nodes = (!behavior.named.is_empty()).then(|| {
            let named = behavior.named.iter().filter_map(|&(key, node)| {
                let ordinal = ctx.static_keys.iter().position(|&k| k == node.0)?;
                Some(quote! { (#key, #ordinal) })
            });
            quote! {
                ::viso::__private::viso_view::__mount_nodes(cx, &__viso_host, &__viso_ids, &[#(#named),*]);
            }
        });
        let mounted = record.take().map(|record| {
            quote! {
                ::viso::__private::viso_view::__record_mount! {
                    root: __viso_root.id(),
                    #record
                    nodes: __viso_ids.to_vec(),
                }
            }
        });
        root = quote! {
            {
                let mut __viso_ids: [::core::option::Option<::viso::__private::viso_ui::NodeId>; #count] =
                    [::core::option::Option::None; #count];
                let __viso_root = #root;
                #link
                #game
                #mount
                #nodes
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
                let mut __viso_shown: [::core::option::Option<::viso::__private::viso_ui::NodeId>; #count] =
                    [::core::option::Option::None; #count];
                let __viso_root = #root;
                ::viso::__private::viso_view::__mount_values(cx, &__viso_host, &__viso_shown, &[#(#controls),*]);
                __viso_root
            }
        };
    }
    if behavior.is_some_and(ViewBehavior::mounts_on_root) {
        root = quote! {
            {
                let __viso_root = #root;
                ::viso::__private::viso_view::__mount_effects(cx, &__viso_host, __viso_root.id());
                __viso_root
            }
        };
    }
    if let Some(record) = record {
        root = quote! {
            {
                let __viso_root = #root;
                ::viso::__private::viso_view::__record_mount! {
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

        // A node that authors no children (a leaf) numbers them without
        // building them.
        let authors_children = node.kind.is_container();
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
        let rules = rules_tokens(node.style.rules.as_deref().copied(), &handle_ident);
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
        let place = node.style.place.map(|place| {
            let place = place_tokens(place);
            quote! { cx.place(#place); }
        });
        quote! {
            {
                #place
                let #handle_ident = #build_call;
                #rules
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
                        let direction = match scope.direction {
                            Some(direction) => {
                                let direction = match direction {
                                    LayoutDirection::Ltr => quote! { Ltr },
                                    LayoutDirection::Rtl => quote! { Rtl },
                                };
                                quote! {
                                    ::core::option::Option::Some(
                                        ::viso::__private::viso_ui::adaptive::LayoutDirection::#direction,
                                    )
                                }
                            }
                            None => quote! { ::core::option::Option::None },
                        };
                        quote! { cx.adaptive_scope(#basis, #direction, #style, |cx| { #child_block }) }
                    }
                    None => match node.style.avoid {
                        Some(avoid) => {
                            let avoid = match avoid {
                                Avoid::SafeArea => quote! { SafeArea },
                                Avoid::Keyboard => quote! { Keyboard },
                            };
                            quote! {
                                cx.avoiding(
                                    ::viso::__private::viso_ui::adaptive::Avoid::#avoid,
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
                let padding = node.style.padding.map(inset_tokens).map(|padding| {
                    quote! { padding: #padding, }
                });
                let size = has_size(&node.style).then(|| {
                    let size = size_tokens(&node.style);
                    quote! { size: #size, }
                });
                let template = node.style.grid.as_deref().map(|grid| {
                    let tracks = |tracks: &[TrackSizing]| {
                        let tracks = tracks.iter().copied().map(track_tokens);
                        quote! { ::std::vec![#(#tracks),*] }
                    };
                    let columns = (!grid.columns.is_empty()).then(|| {
                        let columns = tracks(&grid.columns);
                        quote! { columns: #columns, }
                    });
                    let rows = (!grid.rows.is_empty()).then(|| {
                        let rows = tracks(&grid.rows);
                        quote! { rows: #rows, }
                    });
                    let auto_rows = grid.auto_rows.map(|track| {
                        let track = track_tokens(track);
                        quote! { auto_rows: #track, }
                    });
                    let column_gap = grid.column_gap.map(|gap| quote! { column_gap: #gap, });
                    let row_gap = grid.row_gap.map(|gap| quote! { row_gap: #gap, });
                    let align_items = grid.align_items.map(|align| {
                        let align = align_items_tokens(align);
                        quote! { align_items: #align, }
                    });
                    let adaptive = grid.adaptive_columns.map(|adaptive| {
                        let adaptive = adaptive_tokens(adaptive);
                        quote! { adaptive_columns: ::core::option::Option::Some(#adaptive), }
                    });
                    quote! { #columns #rows #auto_rows #column_gap #row_gap #align_items #adaptive }
                });
                quote! {
                    cx.grid(
                        ::viso::__private::viso_ui::grid::GridStyle {
                            #template
                            #padding
                            #size
                            ..::core::default::Default::default()
                        },
                        |cx| { #child_block },
                    )
                }
            }
            NodeKind::Stack | NodeKind::Absolute => {
                let padding = node.style.padding.map(inset_tokens).map(|padding| {
                    quote! { padding: #padding, }
                });
                let size = has_size(&node.style).then(|| {
                    let size = size_tokens(&node.style);
                    quote! { size: #size, }
                });
                if node.kind == NodeKind::Stack {
                    let align = node.style.content_align.map(|align| {
                        let align = alignment_tokens(align);
                        quote! { align: #align, }
                    });
                    let stack = quote! {
                        cx.stack(
                            ::viso::__private::viso_ui::StackStyle {
                                #align
                                #padding
                                #size
                                ..::core::default::Default::default()
                            },
                            |cx| { #child_block },
                        )
                    };
                    match node.style.sheet {
                        Some(sheet) => {
                            let spec = sheet_tokens(sheet);
                            quote! {{
                                let sheet = #stack;
                                cx.sheet(sheet, #spec, ::core::option::Option::None)
                            }}
                        }
                        None => stack,
                    }
                } else {
                    quote! {
                        cx.absolute(
                            ::viso::__private::viso_ui::AbsoluteStyle {
                                #padding
                                #size
                                ..::core::default::Default::default()
                            },
                            |cx| { #child_block },
                        )
                    }
                }
            }
            NodeKind::Scroll => {
                let axis = axis_tokens(node.style.axis.unwrap_or(AxisIr::Column));
                let size = size_tokens(&node.style);
                let scroll = node.style.scroll.unwrap_or_default();
                let bars = if scroll.bars {
                    quote!(::viso::__private::viso_ui::ScrollBars::Auto)
                } else {
                    quote!(::viso::__private::viso_ui::ScrollBars::Hidden)
                };
                let tail = scroll.tail;
                let snap = snap_tokens(scroll);
                let dots = if scroll.dots {
                    quote!(::core::option::Option::Some(
                        ::viso::__private::viso_ui::PageDotsStyle::default()
                    ))
                } else {
                    quote!(::core::option::Option::None)
                };
                quote! {
                    cx.scroll(
                        ::viso::__private::viso_ui::ScrollStyle {
                            axis: #axis,
                            size: #size,
                            bars: #bars,
                            tail: #tail,
                            snap: #snap,
                            dots: #dots,
                            ..::core::default::Default::default()
                        },
                        |cx| { #child_block },
                    )
                }
            }
            // The scroll viewport its item template makes a list of as the
            // regions mount.
            NodeKind::VirtualList => {
                let axis = axis_tokens(node.style.axis.unwrap_or(AxisIr::Column));
                let size = size_tokens(&node.style);
                let scroll = node.style.scroll.unwrap_or_default();
                let bars = if scroll.bars {
                    quote!(::viso::__private::viso_ui::ScrollBars::Auto)
                } else {
                    quote!(::viso::__private::viso_ui::ScrollBars::Hidden)
                };
                let tail = scroll.tail;
                let snap = snap_tokens(scroll);
                let dots = if scroll.dots {
                    quote!(::core::option::Option::Some(
                        ::viso::__private::viso_ui::PageDotsStyle::default()
                    ))
                } else {
                    quote!(::core::option::Option::None)
                };
                quote! {
                    cx.scroll(
                        ::viso::__private::viso_ui::ScrollStyle {
                            axis: #axis,
                            size: #size,
                            bars: #bars,
                            tail: #tail,
                            snap: #snap,
                            dots: #dots,
                            ..::core::default::Default::default()
                        },
                        |cx| { #child_block },
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
    /// The `::viso::__private::viso_view::attach` call installing `key`'s handler routes and
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
        let routes = routes.iter().map(|route| {
            let variant = Ident::new(route.event.variant(), Span::call_site());
            let (handler, capture) = (route.handler, route.capture);
            quote! {
                ::viso::__private::viso_view::Route {
                    event: ::viso::__private::viso_view::EventRoute::#variant,
                    handler: #handler,
                    capture: #capture,
                }
            }
        });
        let control = match control {
            Some(control) => {
                let control = control_tokens(control);
                quote! { ::core::option::Option::Some(#control) }
            }
            None => quote! { ::core::option::Option::None },
        };
        quote! {
            ::viso::__private::viso_view::attach(cx, &__viso_host, #handle_ident, &[#(#routes),*], #control, &::viso::__private::viso_view::Scope::EMPTY);
        }
    }
}

/// The `::viso::__private::viso_view::Control` expression of `control`.
/// The `ScrollSnap` a viewport's `snap` and `snap_inset` say, if any.
fn snap_tokens(scroll: ScrollIr) -> TokenStream {
    let Some(snap) = scroll.snap else {
        return quote!(::core::option::Option::None);
    };
    let ui = quote!(::viso::__private::viso_ui);
    let base = match snap {
        AotSnap::Start => quote!(#ui::ScrollSnap::START),
        AotSnap::Center => quote!(#ui::ScrollSnap::CENTER),
        AotSnap::End => quote!(#ui::ScrollSnap {
            align: #ui::SnapAlign::End,
            ..#ui::ScrollSnap::START
        }),
        AotSnap::Pages => quote!(#ui::ScrollSnap::PAGES),
    };
    let inset = scroll.snap_inset;
    quote!(::core::option::Option::Some(#ui::ScrollSnap {
        inset: #inset,
        ..#base
    }))
}

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
    let (fit, tint, shader) = (
        entry(control.fit),
        entry(control.tint),
        entry(control.shader),
    );
    let (placeholder, secure, multiline, caption) = (
        entry(control.placeholder),
        entry(control.secure),
        entry(control.multiline),
        entry(control.caption),
    );
    let (input_kind, max_length, precision, page_transition, pan_margin) = (
        entry(control.input_kind),
        entry(control.max_length),
        entry(control.precision),
        entry(control.page_transition),
        entry(control.pan_margin),
    );
    let url = entry(control.url);
    let removable = entry(control.removable);
    let wrap = entry(control.wrap);
    let fill_origin = entry(control.fill_origin);
    let marks = entry(control.marks);
    let axis = entry(control.axis);
    let alpha = entry(control.alpha);
    let wheel = entry(control.wheel);
    let options = entry(control.options);
    let free_text = entry(control.free_text);
    let empty_text = entry(control.empty_text);
    let debounce = entry(control.debounce);
    let animate = entry(control.animate);
    let playing = entry(control.playing);
    let looping = entry(control.looping);
    let muted = entry(control.muted);
    let volume = entry(control.volume);
    let rate = entry(control.rate);
    let controls = entry(control.controls);
    let fullscreen = entry(control.fullscreen);
    let mirror = entry(control.mirror);
    let semantics = control.semantics;
    let role = match semantics.role {
        Some(role) => {
            let role = Ident::new(&format!("{role:?}"), Span::call_site());
            quote! { ::core::option::Option::Some(::viso::__private::viso_ui::Role::#role) }
        }
        None => quote! { ::core::option::Option::None },
    };
    let live = Ident::new(&format!("{:?}", semantics.live), Span::call_site());
    let (label, hint, spoken) = (
        entry(semantics.label),
        entry(semantics.hint),
        entry(semantics.value),
    );
    let (hidden, heading_level) = (entry(semantics.hidden), entry(semantics.heading_level));
    let (visible, focusable) = (entry(control.node.visible), entry(control.node.focusable));
    let (enabled, autofocus) = (entry(control.node.enabled), entry(control.node.autofocus));
    let (clip, cursor) = (entry(control.node.clip), entry(control.node.cursor));
    let (refreshing, sticky, hero) = (
        entry(control.node.refreshing),
        entry(control.node.sticky),
        entry(control.node.hero),
    );
    let focus_ring = entry(control.node.focus_ring);
    let access_key = entry(control.node.access_key);
    let drag_axis = entry(control.node.drag_axis);
    let (drag, drag_kind) = (entry(control.node.drag), entry(control.node.drag_kind));
    let (drop_accepts, drop_operation) = (
        entry(control.node.drop_accepts),
        entry(control.node.drop_operation),
    );
    let text = control.text;
    let (text_color, soft_wrap) = (entry(text.color), entry(text.soft_wrap));
    let (font_weight, font_family) = (entry(text.font_weight), entry(text.font_family));
    let (line_height, max_lines) = (entry(text.line_height), entry(text.max_lines));
    let (overflow, text_align) = (entry(text.overflow), entry(text.align));
    let (font_style, spans) = (entry(text.font_style), entry(text.spans));
    let selectable = entry(text.selectable);
    let look = control.look;
    let (background, opacity) = (entry(look.background), entry(look.opacity));
    let (corner_radius, border) = (entry(look.corner_radius), entry(look.border));
    let (background_transition, opacity_transition) = (
        entry(look.background_transition),
        entry(look.opacity_transition),
    );
    let (translate, translate_transition) =
        (entry(look.translate), entry(look.translate_transition));
    let (corner_radius_transition, color_transition) = (
        entry(look.corner_radius_transition),
        entry(look.color_transition),
    );
    let (width_transition, height_transition) =
        (entry(look.width_transition), entry(look.height_transition));
    let (scale, scale_transition) = (entry(look.scale), entry(look.scale_transition));
    let (rotation, rotation_transition) = (entry(look.rotation), entry(look.rotation_transition));
    let transform_origin = entry(look.transform_origin);
    let shadow = entry(look.shadow);
    let backdrop = entry(look.backdrop);
    let layout = control.layout.iter().map(|&(slot, entry)| {
        let slot = Ident::new(slot.variant(), Span::call_site());
        quote! { (::viso::__private::viso_view::LayoutSlot::#slot, #entry) }
    });
    let arms = control.arms.iter().map(|arm| {
        let target = match arm.target {
            Switched::Input(input) => {
                let input = Ident::new(&format!("{input:?}"), Span::call_site());
                quote! {
                    ::viso::__private::viso_view::Switched::Input(
                        ::viso::__private::viso_view::ControlInput::#input
                    )
                }
            }
            Switched::Layout(slot) => {
                let slot = Ident::new(slot.variant(), Span::call_site());
                quote! {
                    ::viso::__private::viso_view::Switched::Layout(
                        ::viso::__private::viso_view::LayoutSlot::#slot
                    )
                }
            }
        };
        let when = arm.when.iter().map(|step| match *step {
            When::State(state) => {
                let state = Ident::new(&format!("{state:?}"), Span::call_site());
                quote! { ::viso::__private::viso_view::When::State(::viso::__private::viso_ui::Interaction::#state) }
            }
            When::Entry(entry) => quote! { ::viso::__private::viso_view::When::Entry(#entry) },
            When::Not => quote! { ::viso::__private::viso_view::When::Not },
            When::And => quote! { ::viso::__private::viso_view::When::And },
            When::Or => quote! { ::viso::__private::viso_view::When::Or },
        });
        let entry = arm.entry;
        quote! {
            ::viso::__private::viso_view::LookArm {
                target: #target,
                when: ::std::boxed::Box::new([#(#when),*]),
                entry: #entry,
            }
        }
    });
    quote! {
        ::viso::__private::viso_view::Control {
            kind: ::viso::__private::viso_view::ControlKind::#kind,
            value: #value,
            min: #min,
            max: #max,
            step: #step,
            fit: #fit,
            tint: #tint,
            placeholder: #placeholder,
            secure: #secure,
            multiline: #multiline,
            input_kind: #input_kind,
            max_length: #max_length,
            precision: #precision,
            caption: #caption,
            page_transition: #page_transition,
            pan_margin: #pan_margin,
            url: #url,
            removable: #removable,
            wrap: #wrap,
            fill_origin: #fill_origin,
            marks: #marks,
            axis: #axis,
            alpha: #alpha,
            wheel: #wheel,
            options: #options,
            free_text: #free_text,
            empty_text: #empty_text,
            debounce: #debounce,
            animate: #animate,
            playing: #playing,
            looping: #looping,
            muted: #muted,
            volume: #volume,
            rate: #rate,
            controls: #controls,
            fullscreen: #fullscreen,
            mirror: #mirror,
            shader: #shader,
            look: ::viso::__private::viso_view::Look {
                background: #background,
                opacity: #opacity,
                corner_radius: #corner_radius,
                border: #border,
                background_transition: #background_transition,
                opacity_transition: #opacity_transition,
                translate: #translate,
                translate_transition: #translate_transition,
                corner_radius_transition: #corner_radius_transition,
                color_transition: #color_transition,
                width_transition: #width_transition,
                height_transition: #height_transition,
                scale: #scale,
                scale_transition: #scale_transition,
                rotation: #rotation,
                rotation_transition: #rotation_transition,
                transform_origin: #transform_origin,
                shadow: #shadow,
                backdrop: #backdrop,
            },
            arms: ::std::boxed::Box::new([#(#arms),*]),
            node: ::viso::__private::viso_view::NodeEntries {
                visible: #visible,
                focusable: #focusable,
                enabled: #enabled,
                autofocus: #autofocus,
                clip: #clip,
                cursor: #cursor,
                refreshing: #refreshing,
                sticky: #sticky,
                hero: #hero,
                focus_ring: #focus_ring,
                access_key: #access_key,
                drag_axis: #drag_axis,
                drag: #drag,
                drag_kind: #drag_kind,
                drop_accepts: #drop_accepts,
                drop_operation: #drop_operation,
            },
            text: ::viso::__private::viso_view::TextEntries {
                color: #text_color,
                soft_wrap: #soft_wrap,
                font_weight: #font_weight,
                font_family: #font_family,
                line_height: #line_height,
                max_lines: #max_lines,
                overflow: #overflow,
                align: #text_align,
                font_style: #font_style,
                spans: #spans,
                selectable: #selectable,
            },

            semantics: ::viso::__private::viso_view::SemanticEntries {
                role: #role,
                live: ::viso::__private::viso_ui::LiveRegion::#live,
                label: #label,
                hint: #hint,
                value: #spoken,
                hidden: #hidden,
                heading_level: #heading_level,
            },
            layout: ::std::boxed::Box::new([#(#layout),*]),
        }
    }
}

/// The per-node `Handle` binding identifier, unique by pre-order key
/// (`__viso_n0`, `__viso_n1`, …). Hygiene keeps these from clashing with user names.
fn node_handle_ident(key: NodeKey) -> Ident {
    Ident::new(&format!("__viso_n{}", key.0), Span::call_site())
}

/// `::viso::__private::viso_ui::FlexStyle { axis, gap?, size?, .. }` from a folded [`StyleIr`].
fn flex_style_tokens(style: &StyleIr) -> TokenStream {
    let mut fields = Vec::new();
    if let Some(axis) = style.axis {
        let axis = axis_tokens(axis);
        fields.push(quote! { axis: #axis });
    }
    if let Some(justify) = style.justify {
        let justify = match justify {
            Justify::Start => quote! { Start },
            Justify::Center => quote! { Center },
            Justify::End => quote! { End },
        };
        fields.push(quote! { justify: ::viso::__private::viso_ui::Justify::#justify });
    }
    if let Some(align) = style.align {
        let align = match align {
            Align::Start => quote! { Start },
            Align::Center => quote! { Center },
            Align::End => quote! { End },
            Align::Stretch => quote! { Stretch },
        };
        fields.push(quote! { align: ::viso::__private::viso_ui::Align::#align });
    }
    if let Some(gap) = style.gap {
        fields.push(quote! { gap: #gap });
    }
    if let Some(padding) = style.padding {
        let padding = inset_tokens(padding);
        fields.push(quote! { padding: #padding });
    }
    if has_size(style) {
        let size = size_tokens(style);
        fields.push(quote! { size: #size });
    }
    quote! {
        ::viso::__private::viso_ui::FlexStyle {
            #( #fields, )*
            ..::core::default::Default::default()
        }
    }
}

/// `::viso::__private::viso_ui::LeafStyle { size, .. }` from a folded [`StyleIr`]:
/// `Fit` on an axis the node did not size, so a leaf measures to its content.
fn leaf_style_tokens(style: &StyleIr) -> TokenStream {
    let size = size_tokens(style);
    quote! {
        ::viso::__private::viso_ui::LeafStyle {
            size: #size,
            ..::core::default::Default::default()
        }
    }
}

/// Whether the node authored a width or a height, folded or environment-bound.
fn has_size(style: &StyleIr) -> bool {
    style.width.is_some()
        || style.height.is_some()
        || style.lengths().width.is_some()
        || style.lengths().height.is_some()
}

/// `cx.bind_lengths(handle, ::viso::__private::viso_ui::NodeLengths { .. });` for a node with
/// environment lengths, and nothing otherwise.
fn lengths_tokens(lengths: &LengthsIr, handle: &Ident) -> TokenStream {
    if lengths.is_empty() {
        return quote! {};
    }
    let width = terms_tokens(lengths.width);
    let height = terms_tokens(lengths.height);
    let gap = terms_tokens(lengths.gap);
    let font_size = terms_tokens(lengths.font_size);
    let padding = edges_tokens(lengths.padding);
    let min_width = terms_tokens(lengths.min_width);
    let max_width = terms_tokens(lengths.max_width);
    let min_height = terms_tokens(lengths.min_height);
    let max_height = terms_tokens(lengths.max_height);
    let margin = edges_tokens(lengths.margin);
    let row_gap = terms_tokens(lengths.row_gap);
    let insets = insets_tokens(lengths.insets);
    quote! {
        cx.bind_lengths(#handle, ::viso::__private::viso_ui::NodeLengths {
            width: #width,
            height: #height,
            gap: #gap,
            font_size: #font_size,
            padding: #padding,
            min_width: #min_width,
            max_width: #max_width,
            min_height: #min_height,
            max_height: #max_height,
            margin: #margin,
            row_gap: #row_gap,
            insets: #insets,
            corner_radius: ::core::option::Option::None,
        });
    }
}

/// `::viso::__private::viso_ui::grid::TrackSizing::…` of `track`.
fn track_tokens(track: TrackSizing) -> TokenStream {
    let sizing = quote! { ::viso::__private::viso_ui::grid::TrackSizing };
    match track {
        TrackSizing::Fixed(v) => quote! { #sizing::Fixed(#v) },
        TrackSizing::Fr(v) => quote! { #sizing::Fr(#v) },
        TrackSizing::Auto => quote! { #sizing::Auto },
        TrackSizing::Percent(v) => quote! { #sizing::Percent(#v) },
        TrackSizing::Minmax(min, max) => quote! { #sizing::Minmax(#min, #max) },
        TrackSizing::FitContent(v) => quote! { #sizing::FitContent(#v) },
        TrackSizing::FlexMin(min, fr) => quote! { #sizing::FlexMin(#min, #fr) },
        TrackSizing::Relative { fixed, pct } => {
            quote! { #sizing::Relative { fixed: #fixed, pct: #pct } }
        }
        TrackSizing::Length(t) => {
            let terms = one_terms_tokens(TermsIr {
                dp: t.dp,
                px: t.px,
                sp: t.sp,
                em: t.em,
                pct: t.pct,
            });
            quote! { #sizing::Length(#terms) }
        }
    }
}

/// `::viso::__private::viso_ui::AlignItems::…` of `align`.
fn align_items_tokens(align: AlignItems) -> TokenStream {
    let variant = match align {
        AlignItems::Stretch => quote! { Stretch },
        AlignItems::Start => quote! { Start },
        AlignItems::Center => quote! { Center },
        AlignItems::End => quote! { End },
        AlignItems::Baseline => quote! { Baseline },
    };
    quote! { ::viso::__private::viso_ui::AlignItems::#variant }
}

/// `::viso::__private::viso_ui::grid::AdaptiveColumns { .. }` of `adaptive`.
fn adaptive_tokens(adaptive: AdaptiveColumns) -> TokenStream {
    let grid = quote! { ::viso::__private::viso_ui::grid };
    let mode = match adaptive.mode {
        AutoRepeat::Fill => quote! { #grid::AutoRepeat::Fill },
        AutoRepeat::Fit => quote! { #grid::AutoRepeat::Fit },
    };
    let min = adaptive.min;
    let max = match adaptive.max {
        TrackMax::Px(px) => quote! { #grid::TrackMax::Px(#px) },
        TrackMax::Fr(fr) => quote! { #grid::TrackMax::Fr(#fr) },
    };
    quote! { #grid::AdaptiveColumns { mode: #mode, min: #min, max: #max } }
}

/// The `::viso::__private::viso_ui::Placement` of `place`.
fn place_tokens(place: Placement) -> TokenStream {
    let ui = quote! { ::viso::__private::viso_ui };
    match place {
        Placement::Grid(cell) => {
            let index = |at: Option<u16>| match at {
                Some(at) => quote! { ::core::option::Option::Some(#at) },
                None => quote! { ::core::option::Option::None },
            };
            let (column, row) = (index(cell.column), index(cell.row));
            let (column_span, row_span) = (cell.column_span, cell.row_span);
            quote! {
                #ui::Placement::Grid(#ui::grid::GridPlacement {
                    column: #column,
                    row: #row,
                    column_span: #column_span,
                    row_span: #row_span,
                })
            }
        }
        Placement::Stack(stacked) => {
            let align = match stacked.align {
                Some(align) => {
                    let align = alignment_tokens(align);
                    quote! { ::core::option::Option::Some(#align) }
                }
                None => quote! { ::core::option::Option::None },
            };
            let layer = stacked.layer;
            quote! {
                #ui::Placement::Stack(#ui::StackPlacement { align: #align, layer: #layer })
            }
        }
        Placement::Absolute(insets) => {
            let edge = |span: Option<LayoutSpan>| match span {
                Some(LayoutSpan { fixed, pct }) => {
                    quote! { ::core::option::Option::Some(#ui::Span { fixed: #fixed, pct: #pct }) }
                }
                None => quote! { ::core::option::Option::None },
            };
            let [top, end, bottom, start] =
                [insets.top, insets.end, insets.bottom, insets.start].map(edge);
            quote! {
                #ui::Placement::Absolute(#ui::AbsoluteInsets {
                    top: #top, end: #end, bottom: #bottom, start: #start,
                })
            }
        }
    }
}

/// A `::viso::__private::viso_ui::SheetSpec` of `sheet`.
fn sheet_tokens(sheet: AotSheet) -> TokenStream {
    let edge = match sheet.edge {
        SheetEdge::Bottom => quote! { Bottom },
        SheetEdge::Top => quote! { Top },
        SheetEdge::Leading => quote! { Leading },
        SheetEdge::Trailing => quote! { Trailing },
    };
    let detents = sheet.detents().iter().map(|detent| match *detent {
        SheetDetent::Full => quote! { ::viso::__private::viso_ui::SheetDetent::Full },
        SheetDetent::Fraction(f) => {
            quote! { ::viso::__private::viso_ui::SheetDetent::Fraction(#f) }
        }
        SheetDetent::Fixed(v) => quote! { ::viso::__private::viso_ui::SheetDetent::Fixed(#v) },
    });
    let dismissible = sheet.dismissible;
    quote! {
        ::viso::__private::viso_ui::SheetSpec {
            edge: ::viso::__private::viso_ui::SheetEdge::#edge,
            detents: ::std::vec![#(#detents),*],
            dismissible: #dismissible,
        }
    }
}

/// `::viso::__private::viso_ui::Alignment2D::…` of `align`.
fn alignment_tokens(align: Alignment2D) -> TokenStream {
    let variant = match align {
        Alignment2D::TopStart => quote! { TopStart },
        Alignment2D::Top => quote! { Top },
        Alignment2D::TopEnd => quote! { TopEnd },
        Alignment2D::Start => quote! { Start },
        Alignment2D::Center => quote! { Center },
        Alignment2D::End => quote! { End },
        Alignment2D::BottomStart => quote! { BottomStart },
        Alignment2D::Bottom => quote! { Bottom },
        Alignment2D::BottomEnd => quote! { BottomEnd },
    };
    quote! { ::viso::__private::viso_ui::Alignment2D::#variant }
}

/// `Option<::viso::__private::viso_ui::InsetTerms>` from optional
/// [`InsetTermsIr`].
fn insets_tokens(insets: Option<InsetTermsIr>) -> TokenStream {
    match insets {
        Some(InsetTermsIr {
            top,
            end,
            bottom,
            start,
        }) => {
            let [top, end, bottom, start] = [top, end, bottom, start].map(terms_tokens);
            quote! {
                ::core::option::Option::Some(::viso::__private::viso_ui::InsetTerms {
                    top: #top, end: #end, bottom: #bottom, start: #start,
                })
            }
        }
        None => quote! { ::core::option::Option::None },
    }
}

/// `Option<::viso::__private::viso_ui::EdgeTerms>` from optional [`EdgeTermsIr`].
fn edges_tokens(edges: Option<EdgeTermsIr>) -> TokenStream {
    match edges {
        Some(EdgeTermsIr {
            top,
            end,
            bottom,
            start,
        }) => {
            let [top, end, bottom, start] = [top, end, bottom, start].map(one_terms_tokens);
            quote! {
                ::core::option::Option::Some(::viso::__private::viso_ui::EdgeTerms {
                    top: #top, end: #end, bottom: #bottom, start: #start,
                })
            }
        }
        None => quote! { ::core::option::Option::None },
    }
}

/// `cx.set_box_rules(..)` for a node's constant box rules, nothing for none.
fn rules_tokens(rules: Option<BoxRules>, handle: &Ident) -> TokenStream {
    let Some(BoxRules {
        min_width,
        max_width,
        min_height,
        max_height,
        margin,
        padding_pct,
    }) = rules
    else {
        return quote! {};
    };
    let [min_width, max_width, min_height, max_height] =
        [min_width, max_width, min_height, max_height].map(span_tokens);
    let [ml, mt, mr, mb] = [margin.left, margin.top, margin.right, margin.bottom].map(span_tokens);
    let Inset {
        left,
        top,
        right,
        bottom,
    } = padding_pct;
    quote! {
        cx.set_box_rules(#handle, ::viso::__private::viso_ui::BoxRules {
            min_width: #min_width,
            max_width: #max_width,
            min_height: #min_height,
            max_height: #max_height,
            margin: ::viso::__private::viso_ui::SpanInset {
                left: #ml, top: #mt, right: #mr, bottom: #mb,
            },
            padding_pct: ::viso::__private::viso_ui::Inset {
                left: #left, top: #top, right: #right, bottom: #bottom,
            },
        });
    }
}

/// `::viso::__private::viso_ui::Span { .. }`, an unbounded one as such.
fn span_tokens(span: LayoutSpan) -> TokenStream {
    if span == LayoutSpan::UNBOUNDED {
        return quote! { ::viso::__private::viso_ui::Span::UNBOUNDED };
    }
    let LayoutSpan { fixed, pct } = span;
    quote! { ::viso::__private::viso_ui::Span { fixed: #fixed, pct: #pct } }
}

/// `::viso::__private::viso_ui::Inset { .. }` from a folded [`InsetIr`].
fn inset_tokens(inset: InsetIr) -> TokenStream {
    let InsetIr {
        left,
        top,
        right,
        bottom,
    } = inset;
    quote! { ::viso::__private::viso_ui::Inset { left: #left, top: #top, right: #right, bottom: #bottom } }
}

/// `::viso::__private::viso_ui::LengthTerms { .. }` from [`TermsIr`].
fn one_terms_tokens(terms: TermsIr) -> TokenStream {
    let TermsIr {
        dp,
        px,
        sp,
        em,
        pct,
    } = terms;
    quote! { ::viso::__private::viso_ui::LengthTerms { dp: #dp, px: #px, sp: #sp, em: #em, pct: #pct } }
}

/// `Option<::viso::__private::viso_ui::LengthTerms>` from optional [`TermsIr`].
fn terms_tokens(terms: Option<TermsIr>) -> TokenStream {
    match terms {
        Some(terms) => {
            let terms = one_terms_tokens(terms);
            quote! { ::core::option::Option::Some(#terms) }
        }
        None => quote! { ::core::option::Option::None },
    }
}

/// `::viso::__private::viso_ui::Size { width, height }` from a folded [`StyleIr`]. A missing axis
/// length defaults to `Length::Fit` (shrink to natural size), the neutral request.
fn size_tokens(style: &StyleIr) -> TokenStream {
    let width = length_tokens(style.width);
    let height = length_tokens(style.height);
    quote! { ::viso::__private::viso_ui::Size { width: #width, height: #height } }
}

/// `::viso::__private::viso_ui::Length::…` from an optional folded [`LengthIr`]; `None` -> `Fit`.
fn length_tokens(length: Option<LengthIr>) -> TokenStream {
    match length {
        Some(LengthIr::Fixed(dp)) => quote! { ::viso::__private::viso_ui::Length::Fixed(#dp) },
        Some(LengthIr::Relative { fixed, pct }) => {
            quote! { ::viso::__private::viso_ui::Length::Relative { fixed: #fixed, pct: #pct } }
        }
        Some(LengthIr::Fill { weight }) => {
            quote! { ::viso::__private::viso_ui::Length::Fill { weight: #weight } }
        }
        Some(LengthIr::Fit) | None => quote! { ::viso::__private::viso_ui::Length::Fit },
        Some(LengthIr::MinContent) => quote! { ::viso::__private::viso_ui::Length::MinContent },
        Some(LengthIr::MaxContent) => quote! { ::viso::__private::viso_ui::Length::MaxContent },
    }
}

/// `::viso::__private::viso_ui::Axis::…` from a folded [`AxisIr`].
fn axis_tokens(axis: AxisIr) -> TokenStream {
    match axis {
        AxisIr::Row => quote! { ::viso::__private::viso_ui::Axis::Row },
        AxisIr::Column => quote! { ::viso::__private::viso_ui::Axis::Column },
    }
}

/// An expression of `bytes` as a `&'static [u8]` at an 8-aligned address,
/// which a blob's aligned sections are read in place at: a `static` only the
/// expression names, so a branch that compiles the expression out leaves no
/// bytes in the binary.
pub(crate) fn aligned_bytes(bytes: &[u8]) -> TokenStream {
    let len = bytes.len();
    let literal = Literal::byte_string(bytes);
    quote! {{
        static BYTES: ::viso::__private::viso_ende::Aligned<[u8; #len]> =
            ::viso::__private::viso_ende::Aligned(*#literal);
        BYTES.as_slice()
    }}
}

/// [`aligned_bytes`] of the file at `path`, embedded with `include_bytes!`,
/// so editing it rebuilds the crate.
pub(crate) fn aligned_file(path: &str) -> TokenStream {
    quote! {{
        static BYTES: ::viso::__private::viso_ende::Aligned<
            [u8; ::core::include_bytes!(#path).len()],
        > = ::viso::__private::viso_ende::Aligned(*::core::include_bytes!(#path));
        BYTES.as_slice()
    }}
}

/// The item making the expansion depend on [`OVERLAY_ENV`]: switching a
/// build between converted and source assets rebuilds the crate.
///
/// [`OVERLAY_ENV`]: viso_dsl::assets::OVERLAY_ENV
pub(crate) fn overlay_dependency() -> TokenStream {
    let env = viso_dsl::assets::OVERLAY_ENV;
    quote! { const _: ::core::option::Option<&str> = ::core::option_env!(#env); }
}

/// Decomposes a [`DirtyClass`] bitset into a `::viso::__private::viso_ui::DirtyClass` `|` chain.
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
            terms.push(quote! { ::viso::__private::viso_ui::DirtyClass::#ident });
        }
    }

    if terms.is_empty() {
        // property_dirty_class never returns EMPTY, but be explicit rather than emit
        // an empty expression if a future class is all-zero.
        quote! { ::viso::__private::viso_ui::DirtyClass::EMPTY }
    } else {
        quote! { #( #terms )|* }
    }
}
