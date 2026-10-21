//! Binding IR — the compiled `source -> (node, dirty class)` edges (AGENTS sections 10.2, 10.3).
//!
//! The UI IR ([`crate::ir::ui_ir`]) folds compile-time-constant properties into
//! static style and leaves every reactive property as a [`PendingProperty`]. This
//! pass turns each pending property into a *binding edge*: the reactive sources it
//! reads, the template node it targets, and the dirty class a write invalidates.
//! These edges are the compiled fast path — at mount the emitter replays them into
//! `viso_ui::BindingTable::bind`, so a state write becomes a direct
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
use crate::hir::{DerivedReads, ReadEnv, WithDerived, collect_reads};
use crate::ir::dirty_map::{DirtyClass, property_dirty_class};
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

/// The expressions and references of each source unit a lowered view's
/// spans come from, by the unit's index in the library it was lowered with:
/// a span recovers its expression in the unit of the instance whose view
/// holds it.
pub struct ViewUnits<'r> {
    exprs: HashMap<(u32, TextRange), Expr>,
    refs: Vec<&'r [ResolvedRef]>,
}

impl<'r> ViewUnits<'r> {
    /// The units `units`, each its tree and the references its names resolve
    /// by; `None` holds the index of a unit no view's spans come from.
    pub fn new(units: impl IntoIterator<Item = Option<(SyntaxNode, &'r [ResolvedRef])>>) -> Self {
        let mut exprs = HashMap::new();
        let mut refs = Vec::new();
        for (unit, source) in units.into_iter().enumerate() {
            match source {
                Some((root, unit_refs)) => {
                    index_exprs(unit as u32, &root, &mut exprs);
                    refs.push(unit_refs);
                }
                None => refs.push(&[][..]),
            }
        }
        ViewUnits { exprs, refs }
    }

    /// The expression at `at` in unit `unit`.
    pub(crate) fn expr(&self, unit: u32, at: TextRange) -> Option<&Expr> {
        self.exprs.get(&(unit, at))
    }

    /// The references of unit `unit`.
    pub(crate) fn refs(&self, unit: u32) -> &'r [ResolvedRef] {
        self.refs.get(unit as usize).copied().unwrap_or(&[])
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
pub fn lower_bindings(tree: &UiTree, units: &ViewUnits<'_>, env: &dyn ReadEnv) -> BindingIr {
    lower_view_reactivity(tree, units, env, &DerivedReads::default(), &[]).0
}

/// The reactive sources of one inlined instance of a [`UiTree`]: what its
/// view's reads of its component's states and inputs stand for in the mounted
/// component.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstanceSources {
    /// Each state of the component and the hidden state of the mounted
    /// component that keeps it for the instance.
    pub states: Vec<(SymbolId, SymbolId)>,
    /// Each input of the component, by slot.
    pub inputs: Vec<Option<SymbolId>>,
}

/// What a write to each reactive source of a view invalidates: the dirty
/// classes of every property it binds, a view-driven control's value it
/// feeds included, and `STRUCTURE` where it decides an `if`, `for` or `match`
/// region. Sources the view never reads are absent.
pub fn source_invalidations(
    tree: &UiTree,
    units: &ViewUnits<'_>,
    env: &dyn ReadEnv,
    derived: &DerivedReads,
    instances: &[InstanceSources],
) -> HashMap<SymbolId, DirtyClass> {
    lower_view_reactivity(tree, units, env, derived, instances).1
}

/// [`lower_bindings`] for a view that inlines component instances, `instances`
/// holding each one's sources in [`UiTree::instances`] order, with
/// [`source_invalidations`] from the same walk. A read in an instance's view of
/// one of its component's states binds the hidden state keeping it; a read of
/// an input binds whatever the caller's argument reads, through the caller's
/// own instance in turn. Any other read of an inlined view binds nothing. A
/// read of a `computed` or a call of a `fn` binds the states and inputs beneath
/// it, as `derived` records them.
pub fn lower_view_reactivity(
    tree: &UiTree,
    units: &ViewUnits<'_>,
    env: &dyn ReadEnv,
    derived: &DerivedReads,
    instances: &[InstanceSources],
) -> (BindingIr, HashMap<SymbolId, DirtyClass>) {
    let is_derived = |symbol: SymbolId| derived.is_derived(symbol);
    let with = WithDerived {
        env,
        derived: &is_derived,
    };
    let mut ctx = LowerCtx::new(tree, units, &with, derived, instances);
    let mut out: HashMap<SymbolId, DirtyClass> = HashMap::new();
    for item in &tree.items {
        ctx.lower_item(item);
        ctx.invalidations(item, &mut out);
    }
    for edge in &ctx.ir.edges {
        *out.entry(edge.source).or_insert(DirtyClass::EMPTY) |= edge.class;
    }
    ctx.ir.dynamic_fallback_nodes = ctx.dynamic_nodes.len() as u32;
    (ctx.ir, out)
}

/// What the reads `reads` in the view of `instance` bind in the mounted
/// component, deduplicated in order.
fn substitute(
    substitutions: &[HashMap<SymbolId, Vec<SymbolId>>],
    instance: u32,
    reads: impl IntoIterator<Item = SymbolId>,
) -> Vec<SymbolId> {
    let Some(index) = instance.checked_sub(1) else {
        return reads.into_iter().collect();
    };
    let Some(map) = substitutions.get(index as usize) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for read in reads {
        for &source in map.get(&read).into_iter().flatten() {
            if !out.contains(&source) {
                out.push(source);
            }
        }
    }
    out
}

/// The mutable state threaded through the pre-order lowering walk.
struct LowerCtx<'a> {
    /// Every `Expr` of the view's units, so a pending property's value range
    /// resolves back to the expression `collect_reads` walks.
    units: &'a ViewUnits<'a>,
    /// The source unit of each instance's view, by instance.
    unit_of: Vec<u32>,
    env: &'a dyn ReadEnv,
    /// The base reads of each derived member.
    derived: &'a DerivedReads,
    /// What each inlined instance's reads bind, by instance minus one.
    substitutions: Vec<HashMap<SymbolId, Vec<SymbolId>>>,
    ir: BindingIr,
    next_key: u32,
    /// Template nodes that took a dynamic edge, counted distinctly.
    dynamic_nodes: BTreeSet<u32>,
}

impl<'a> LowerCtx<'a> {
    /// The walk over `tree`, each inlined instance's input reads standing for
    /// what its caller's arguments read.
    fn new(
        tree: &UiTree,
        units: &'a ViewUnits<'a>,
        env: &'a dyn ReadEnv,
        derived: &'a DerivedReads,
        instances: &[InstanceSources],
    ) -> Self {
        let unit_of: Vec<u32> = (0..=tree.instances.len() as u32)
            .map(|instance| tree.unit_of(instance))
            .collect();
        let mut substitutions: Vec<HashMap<SymbolId, Vec<SymbolId>>> = Vec::new();
        for (instance, sources) in tree.instances.iter().zip(instances) {
            let mut map: HashMap<SymbolId, Vec<SymbolId>> = sources
                .states
                .iter()
                .map(|&(state, hidden)| (state, vec![hidden]))
                .collect();
            // An argument is an expression of the caller's view.
            let unit = unit_of[instance.parent as usize];
            for &(slot, at) in &instance.args {
                let Some(Some(input)) = sources.inputs.get(slot as usize) else {
                    continue;
                };
                let Some(expr) = units.expr(unit, at) else {
                    continue;
                };
                let reads = derived.flatten(collect_reads(units.refs(unit), env, expr));
                let reads = substitute(&substitutions, instance.parent, reads);
                map.insert(*input, reads);
            }
            substitutions.push(map);
        }
        Self {
            units,
            unit_of,
            env,
            derived,
            substitutions,
            ir: BindingIr::default(),
            next_key: 0,
            dynamic_nodes: BTreeSet::new(),
        }
    }

    /// The sources the expression at `at`, in the view of `instance`, reads.
    fn reads_at(&self, at: TextRange, instance: u32) -> Vec<SymbolId> {
        let unit = self.unit_of.get(instance as usize).copied().unwrap_or(0);
        self.reads_in(unit, at, instance)
    }

    /// The reactive sources the value at `at` of unit `unit` reads, as the
    /// instance `instance` sees them.
    fn reads_in(&self, unit: u32, at: TextRange, instance: u32) -> Vec<SymbolId> {
        let Some(expr) = self.units.expr(unit, at) else {
            return Vec::new();
        };
        let reads = self
            .derived
            .flatten(collect_reads(self.units.refs(unit), self.env, expr));
        substitute(&self.substitutions, instance, reads)
    }

    /// Adds what the regions and control values under `item` invalidate.
    fn invalidations(&self, item: &UiItem, out: &mut HashMap<SymbolId, DirtyClass>) {
        let mut add = |at: Option<TextRange>, instance: u32, class: DirtyClass| {
            for source in at.map(|at| self.reads_at(at, instance)).unwrap_or_default() {
                *out.entry(source).or_insert(DirtyClass::EMPTY) |= class;
            }
        };
        let children: Vec<&UiItem> = match item {
            UiItem::Node(node) => {
                for (name, at, instance) in &node.control_reads {
                    add(Some(*at), *instance, property_dirty_class(name));
                }
                node.children.iter().collect()
            }
            UiItem::If(region) => {
                for arm in &region.arms {
                    add(arm.condition, region.instance, DirtyClass::STRUCTURE);
                }
                region.arms.iter().flat_map(|arm| &arm.items).collect()
            }
            UiItem::For(region) => {
                add(region.iterable, region.instance, DirtyClass::STRUCTURE);
                add(region.key, region.instance, DirtyClass::STRUCTURE);
                region.body.iter().collect()
            }
            UiItem::Match(region) => {
                add(region.scrutinee, region.instance, DirtyClass::STRUCTURE);
                for arm in &region.arms {
                    add(arm.guard, region.instance, DirtyClass::STRUCTURE);
                }
                region.arms.iter().flat_map(|arm| &arm.items).collect()
            }
        };
        for child in children {
            self.invalidations(child, out);
        }
    }

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
        let unit = pending.unit.unwrap_or_else(|| {
            self.unit_of
                .get(pending.instance as usize)
                .copied()
                .unwrap_or(0)
        });
        for source in self.reads_in(unit, pending.value, pending.instance) {
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

/// Indexes every `Expr` under `root`, the tree of unit `unit`, by its span, so
/// a pending property's recorded value range recovers the expression to run
/// `collect_reads` over. Spans are unique per node within a unit, so the map is
/// one-to-one. This is a cold compile-time walk, so it simply asks
/// [`Expr::cast`] itself which nodes are expressions rather than duplicating
/// the AST's expression-kind set.
fn index_exprs(unit: u32, root: &SyntaxNode, map: &mut HashMap<(u32, TextRange), Expr>) {
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        if let Some(expr) = Expr::cast(node.clone()) {
            map.insert((unit, node.text_range()), expr);
        }
        stack.extend(node.children());
    }
}
