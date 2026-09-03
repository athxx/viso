//! Binding IR — the compiled `source -> (node, dirty class)` edges (AGENTS sections 10.2, 10.3).
//!
//! The UI IR ([`crate::ir::ui_ir`]) folds compile-time-constant properties into
//! static style and leaves every reactive property as a [`PendingProperty`]. This
//! pass turns each pending property into a *binding edge*: the reactive sources it
//! reads, the template node it targets, and the dirty class a write invalidates.
//! These edges are the compiled fast path — at mount the emitter replays them into
//! [`viso_ui::BindingTable::bind`], so a state write becomes a direct
//! `StateId -> (node, DirtyClass)` invalidation with no per-signal subscriber list
//! and no runtime dependency discovery.
//!
//! A compiler-known typed binding must never silently fall back to runtime dynamic
//! tracking (section 10.3): a pending property whose value reads at least one
//! reactive source becomes one or more [`BindingKind::Static`] edges. A property
//! that reads *no* reactive source is not a binding at all — it is a constant the
//! UI IR should have folded, or an unresolved name the frontend already
//! diagnosed — so it produces no edge rather than a silent dynamic subscription.
//! The [`BindingKind::Dynamic`] edge exists only for an explicit `dynamic`
//! construct, and every dynamic edge is counted so a strict typed example can be
//! asserted to have zero dynamic fallback.
//!
//! Node identity here is a **template-local** [`NodeKey`] — a stable pre-order
//! index into the UI tree, assigned once by [`lower_bindings`]. It is not a runtime
//! `NodeId`; the emitter maps each `NodeKey` to the `Handle` its builder call
//! returns at mount time. Keeping identity template-local means the Binding IR is a
//! pure compile-time artifact with no dependency on the UI runtime.

use std::collections::BTreeSet;

use std::collections::HashMap;

use crate::ast::{AstNode, Expr};
use crate::hir::{ReadEnv, collect_reads};
use crate::ir::dirty_map::DirtyClass;
use crate::ir::ui_ir::{PendingProperty, UiItem, UiNode, UiTree};
use crate::resolve::{ResolvedRef, SymbolId};
use crate::syntax::SyntaxNode;
use crate::syntax::span::TextRange;

/// A template-local node identity: the pre-order index of a [`UiNode`] within a
/// lowered [`UiTree`]. Stable for one lowering; the emitter binds it to the runtime
/// `Handle` the node's builder call produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeKey(pub u32);

/// Whether an edge rides the compiled static fast path or the explicit dynamic
/// fallback (section 10.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    /// A compiler-known typed binding: a property reads a reactive source resolved
    /// at compile time. The emitter replays it into the static `BindingTable`.
    Static,
    /// An explicit `dynamic` escape hatch. Never produced by a typed binding
    /// silently falling back; only by an explicit dynamic construct.
    Dynamic,
}

/// One compiled reactive edge: when `source` changes, mark the template node `node`
/// dirty with `class`. A property that reads several reactive sources produces one
/// edge per source (each state independently invalidates the same node/class).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingEdge {
    /// The reactive source (`state`/`input`/`computed` symbol) whose change fires
    /// this edge.
    pub source: SymbolId,
    /// The template node this edge invalidates.
    pub node: NodeKey,
    /// The bound property's leading name, for diagnostics and the emitter.
    pub property: String,
    /// The dirty classes a write to that property invalidates (section 11).
    pub class: DirtyClass,
    /// Static fast path or explicit dynamic fallback.
    pub kind: BindingKind,
}

/// The Binding IR for a lowered view: the compiled edges plus the observability
/// counts a strict typed example asserts against (section 10.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindingIr {
    /// Every compiled edge, in deterministic order (pre-order node, then source).
    pub edges: Vec<BindingEdge>,
    /// Distinct template nodes that took a [`BindingKind::Dynamic`] edge — the
    /// fallback surface a strict typed example keeps at zero.
    pub dynamic_fallback_nodes: u32,
}

impl BindingIr {
    /// The static edges, in order.
    pub fn static_edges(&self) -> impl Iterator<Item = &BindingEdge> {
        self.edges.iter().filter(|e| e.kind == BindingKind::Static)
    }

    /// The dynamic edges, in order.
    pub fn dynamic_edges(&self) -> impl Iterator<Item = &BindingEdge> {
        self.edges.iter().filter(|e| e.kind == BindingKind::Dynamic)
    }
}

/// Lowers a [`UiTree`]'s pending properties into Binding IR, given the view's
/// parsed root (to recover each pending value's expression by span), the resolver's
/// references, and a [`ReadEnv`] classifying which symbols are reactive sources.
///
/// The walk is pre-order so `NodeKey`s are assigned in the same order the emitter
/// mounts nodes. Each pending property's value expression is looked up by its
/// recorded span and run through `collect_reads`; every reactive source it reads
/// becomes a static edge carrying the property's dirty class. A property reading no
/// reactive source contributes no edge — a typed binding never silently becomes a
/// dynamic subscription.
pub fn lower_bindings(
    tree: &UiTree,
    root: &SyntaxNode,
    refs: &[ResolvedRef],
    env: &dyn ReadEnv,
) -> BindingIr {
    let mut ctx = LowerCtx {
        exprs: index_exprs(root),
        refs,
        env,
        ir: BindingIr::default(),
        next_key: 0,
        dynamic_nodes: BTreeSet::new(),
    };
    for item in &tree.items {
        ctx.lower_item(item);
    }
    ctx.ir.dynamic_fallback_nodes = ctx.dynamic_nodes.len() as u32;
    ctx.ir
}

/// The mutable state threaded through the pre-order lowering walk.
struct LowerCtx<'a> {
    /// Every `Expr` in the view, keyed by its span, so a pending property's value
    /// range resolves back to the expression `collect_reads` walks.
    exprs: HashMap<TextRange, Expr>,
    refs: &'a [ResolvedRef],
    env: &'a dyn ReadEnv,
    ir: BindingIr,
    next_key: u32,
    /// Template nodes that took a dynamic edge, counted distinctly.
    dynamic_nodes: BTreeSet<u32>,
}

impl LowerCtx<'_> {
    /// Assigns the next pre-order [`NodeKey`].
    fn take_key(&mut self) -> NodeKey {
        let key = NodeKey(self.next_key);
        self.next_key += 1;
        key
    }

    /// Lowers one UI item, descending in the same pre-order the emitter mounts.
    fn lower_item(&mut self, item: &UiItem) {
        match item {
            UiItem::Node(node) => self.lower_node(node),
            UiItem::If(vi) => {
                for arm in &vi.arms {
                    for item in &arm.items {
                        self.lower_item(item);
                    }
                }
            }
            UiItem::For(vf) => {
                for item in &vf.body {
                    self.lower_item(item);
                }
            }
            UiItem::Match(vm) => {
                for arm in &vm.arms {
                    for item in &arm.items {
                        self.lower_item(item);
                    }
                }
            }
        }
    }

    /// Lowers one node: assigns its key, emits an edge per reactive read of each
    /// pending property, then descends into its children.
    fn lower_node(&mut self, node: &UiNode) {
        let key = self.take_key();
        for pending in &node.pending {
            self.lower_pending(key, pending);
        }
        for child in &node.children {
            self.lower_item(child);
        }
    }

    /// Turns one pending property into static edges — one per reactive source its
    /// value reads. A value that reads no reactive source is left unbound (a
    /// constant the UI IR should have folded, or a name the frontend diagnosed):
    /// a typed binding never silently becomes a dynamic subscription.
    fn lower_pending(&mut self, node: NodeKey, pending: &PendingProperty) {
        let Some(expr) = self.exprs.get(&pending.value) else {
            return;
        };
        let reads = collect_reads(self.refs, self.env, expr);
        for source in reads {
            self.ir.edges.push(BindingEdge {
                source,
                node,
                property: pending.name.clone(),
                class: pending.dirty,
                kind: BindingKind::Static,
            });
        }
    }
}

/// Indexes every `Expr` in the view by its span, so a pending property's recorded
/// value range recovers the expression to run `collect_reads` over. Spans are
/// unique per node, so the map is one-to-one. This is a cold compile-time walk, so
/// it simply asks [`Expr::cast`] itself which nodes are expressions rather than
/// duplicating the AST's expression-kind set.
fn index_exprs(root: &SyntaxNode) -> HashMap<TextRange, Expr> {
    let mut map = HashMap::new();
    collect_exprs(root, &mut map);
    map
}

/// Recursively records each node that casts to an [`Expr`], keyed by its span.
fn collect_exprs(node: &SyntaxNode, map: &mut HashMap<TextRange, Expr>) {
    if let Some(expr) = Expr::cast(node.clone()) {
        map.insert(node.text_range(), expr);
    }
    for child in node.children() {
        collect_exprs(&child, map);
    }
}
