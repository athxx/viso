//! Keyed-list identity — stable keys for repeated view content (AGENTS section 21.8).
//!
//! A `for` region mounts a body per iterated item. When that body carries
//! per-item *identity* — an event handler bound to the item, or a reactive
//! property bound to the item's fields — reordering or removing items must move
//! the retained node that holds that state, not rebuild it against a new item.
//! That requires a stable key: an expression, evaluated per item, that names the
//! item's identity independent of its position (`item.id`, not the loop index).
//!
//! The grammar already makes `key` mandatory on every `for` and hard-errors
//! (`E3401`) when it is missing, so a well-formed source always carries a key.
//! This pass covers the *recovered* case — a `for` the parser continued past
//! without a key — and the strict-mode contract of section 21.8: a keyless `for`
//! whose body is *stateful* is a distinct strict finding (`E3402`, a warning),
//! surfaced at the IR level so the emitter and strict CI see a semantic result
//! rather than only the syntactic parse error. A keyless `for` whose body is
//! purely static (no handlers, no bindings) carries no identity to preserve, so
//! it is not flagged.
//!
//! For a keyed `for`, the pass recovers the key expression by span (the UI IR
//! stores only the span, like [`crate::ir::binding_ir`]) and records the reactive
//! sources the key reads, so the emitter can build the stable-key call and the
//! reactive graph knows the key's dependencies. A key that reads no reactive
//! source (a constant, or the loop-local binding the resolver treats as a local)
//! is still a valid key — its identity is positional-free by construction — so it
//! contributes no reactive edge, mirroring the binding pass.

use std::collections::{BTreeSet, HashMap};

use crate::ast::Expr;
use crate::diag::Diagnostic;
use crate::hir::{ReadEnv, collect_reads};
use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{UiFor, UiItem, UiNode, UiTree};
use crate::resolve::{ResolvedRef, SymbolId};
use crate::syntax::SyntaxNode;
use crate::syntax::span::TextRange;

/// The strict-mode diagnostic code for a stateful `for` body without a stable
/// key (section 21.8). Distinct from the parser's mandatory-`key` error
/// (`E3401`): this is the *semantic* strict finding, a warning the emitter and CI
/// read even when the parse recovered.
pub const KEYLESS_STATEFUL_FOR: &str = "E3402";

/// One resolved keyed repetition: the template node the `for` mounts under, the
/// reactive sources its key expression reads, and whether it carried a key at
/// all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedFor {
    /// The template node this `for` region occupies (pre-order, shared with the
    /// Binding IR's [`NodeKey`] numbering).
    pub node: NodeKey,
    /// The reactive sources the key expression reads, deterministically ordered.
    /// Empty when the key is a constant / loop-local, or when there is no key.
    pub key_reads: BTreeSet<SymbolId>,
    /// Whether a `key` clause was present. `false` is the recovered / keyless
    /// case section 21.8 governs.
    pub keyed: bool,
    /// Whether the body carries per-item identity (a handler or a binding) — the
    /// condition under which a missing key is a strict finding.
    pub stateful: bool,
}

/// The keyed-list analysis for a lowered view: one entry per `for` region plus
/// the strict findings a keyless stateful `for` produces.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyIr {
    /// Every `for` region, in pre-order.
    pub fors: Vec<KeyedFor>,
    /// Strict-mode findings (section 21.8): a warning per keyless stateful `for`.
    pub diagnostics: Vec<Diagnostic>,
}

/// Analyzes a [`UiTree`]'s `for` regions for stable-key identity, given the view's
/// parsed root (to recover key expressions by span), the resolver's references,
/// and a [`ReadEnv`] classifying reactive sources.
///
/// The walk shares the Binding IR's pre-order [`NodeKey`] numbering so a `KeyedFor`
/// and a `BindingEdge` refer to the same template node. Each keyed `for` records
/// its key's reactive reads; each keyless `for` with a stateful body produces one
/// [`KEYLESS_STATEFUL_FOR`] warning.
pub fn analyze_keys(
    tree: &UiTree,
    root: &SyntaxNode,
    refs: &[ResolvedRef],
    env: &dyn ReadEnv,
) -> KeyIr {
    let mut ctx = KeyCtx {
        exprs: index_exprs(root),
        refs,
        env,
        ir: KeyIr::default(),
        next_key: 0,
    };
    for item in &tree.items {
        ctx.walk_item(item);
    }
    ctx.ir
}

/// The state threaded through the pre-order key walk.
struct KeyCtx<'a> {
    exprs: HashMap<TextRange, Expr>,
    refs: &'a [ResolvedRef],
    env: &'a dyn ReadEnv,
    ir: KeyIr,
    next_key: u32,
}

impl KeyCtx<'_> {
    /// Assigns the next pre-order [`NodeKey`], matching the Binding IR numbering.
    fn take_key(&mut self) -> NodeKey {
        let key = NodeKey(self.next_key);
        self.next_key += 1;
        key
    }

    /// Walks one item in pre-order. A node consumes one key then descends; a
    /// control-flow region descends into its branches (its own body items each
    /// take keys as nodes do), and a `for` region records a [`KeyedFor`].
    fn walk_item(&mut self, item: &UiItem) {
        match item {
            UiItem::Node(node) => self.walk_node(node),
            UiItem::If(vi) => {
                for arm in &vi.arms {
                    for item in &arm.items {
                        self.walk_item(item);
                    }
                }
            }
            UiItem::For(vf) => self.walk_for(vf),
            UiItem::Match(vm) => {
                for arm in &vm.arms {
                    for item in &arm.items {
                        self.walk_item(item);
                    }
                }
            }
        }
    }

    /// Walks a node: consumes its key, then descends into its children.
    fn walk_node(&mut self, node: &UiNode) {
        let _ = self.take_key();
        for child in &node.children {
            self.walk_item(child);
        }
    }

    /// Records a `for` region: recovers its key reads (if keyed), decides whether
    /// its body is stateful, emits the strict finding for a keyless stateful body,
    /// then descends into the body so its nodes keep the shared numbering.
    fn walk_for(&mut self, vf: &UiFor) {
        let node = self.take_key();
        let keyed = vf.key.is_some();
        let key_reads = match vf.key.and_then(|span| self.exprs.get(&span)) {
            Some(expr) => collect_reads(self.refs, self.env, expr),
            None => BTreeSet::new(),
        };
        let stateful = body_is_stateful(&vf.body);
        if !keyed && stateful {
            self.ir.diagnostics.push(Diagnostic::warning(
                KEYLESS_STATEFUL_FOR,
                vf.origin,
                "repeated content with per-item state has no stable `key`; \
                 reordering will rebuild item state instead of moving it",
            ));
        }
        self.ir.fors.push(KeyedFor {
            node,
            key_reads,
            keyed,
            stateful,
        });
        for item in &vf.body {
            self.walk_item(item);
        }
    }
}

/// Whether a `for` body carries per-item identity worth preserving across
/// reorders: any node with an event handler or a pending binding, anywhere in the
/// body subtree. A purely static body (no handlers, no bindings) has no state to
/// move, so a missing key on it is not a finding.
fn body_is_stateful(body: &[UiItem]) -> bool {
    body.iter().any(item_is_stateful)
}

/// Whether one item's subtree carries per-item identity.
fn item_is_stateful(item: &UiItem) -> bool {
    match item {
        UiItem::Node(node) => {
            !node.handlers.is_empty()
                || !node.pending.is_empty()
                || node.children.iter().any(item_is_stateful)
        }
        UiItem::If(vi) => vi
            .arms
            .iter()
            .any(|arm| arm.items.iter().any(item_is_stateful)),
        // A nested keyed repetition is itself stateful content.
        UiItem::For(_) => true,
        UiItem::Match(vm) => vm
            .arms
            .iter()
            .any(|arm| arm.items.iter().any(item_is_stateful)),
    }
}

/// Indexes every `Expr` in the view by its span, so a `for`'s recorded key span
/// recovers the expression `collect_reads` walks. Shares the cold compile-time
/// strategy of [`crate::ir::binding_ir`]: cast every node, key by span.
fn index_exprs(root: &SyntaxNode) -> HashMap<TextRange, Expr> {
    use crate::ast::AstNode;
    let mut map = HashMap::new();
    let mut stack = vec![root.clone()];
    while let Some(node) = stack.pop() {
        if let Some(expr) = Expr::cast(node.clone()) {
            map.insert(node.text_range(), expr);
        }
        stack.extend(node.children());
    }
    map
}
