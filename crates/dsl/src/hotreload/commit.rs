//! Atomic commit — the fourth stage of the hot reload transaction and the only
//! one that touches the live UI tree (architecture section 42; AGENTS 21.7).
//!
//! Every earlier stage (`plan`, `diff`, `migrate`) is a pure function that
//! produces plain data. By the time control reaches the commit, all fallible work
//! is done: a compile error, an unsupported template, or an incompatible migration
//! has already short-circuited the pipeline with an `Err`, before anything mutated.
//! So the commit is an *infallible applier* — it cannot leave the tree half-changed
//! because there is nothing left that can fail. That is the keep-last-good
//! invariant without a snapshot: a rejected reload never reaches here, and the live
//! tree stays exactly at its last-good state (see the module docs and ADR 0015).
//!
//! The commit applies the decisions in a fixed order:
//!
//! 1. **Structural patch** — a structure-preserving edit reuses every live
//!    node in place, so all its runtime state stays untouched. Any other edit
//!    rebuilds the view's subtree, and each kept node's migratable state —
//!    focus, a viewport's scroll offset, a text field's edit buffer — moves from
//!    its old node to the node that rebuilt it; focus and scroll that do not
//!    carry are reported lost.
//! 2. **State migration** — for each surviving reactive source, migrate its live
//!    cell by durable [`SymbolId`] identity (bridged to the runtime `StateKey`);
//!    allocate fresh cells for new sources.
//! 3. **Rebind** — replace the view's static edges with the recompiled ones,
//!    mapping each edge's template [`NodeKey`] to the live node it now names.
//! 4. **Targeted dirty + flush** — mark exactly the rebound nodes dirty and flush
//!    the migrated state set, so only the changed nodes recompute.
//! 5. **Handlers and regions** — create the view's host, or reload it keeping
//!    each state by name, link its states to the migrated cells, reinstall every
//!    static node's handler routes, and mount the view's control-flow regions
//!    under the static nodes; a view with no behavior drops its host and every
//!    handler.
//!
//! The commit authors the *static* nodes of a template (those outside every
//! region) the way the `ui!` emitter does; the regions' content is mounted by
//! the view runtime from the same templates the emitter and the release package
//! embed. A reload into or out of a template with regions takes the rebuild
//! path: region-mounted nodes have no template slot the fast path could reuse
//! them by.
//!
//! The commit touches only the view's own subtree. A rebuild frees the view's
//! root and builds the new one under the same parent at the same sibling
//! position; the rebind replaces only the edges of the view's nodes; and the
//! view's region and value hooks are the only hooks it removes. A window
//! holding other content around the view keeps it untouched.

use std::cell::RefCell;
use std::rc::Rc;

use crate::aot::node_lengths;
use crate::hotreload::diff::StructuralPatch;
use crate::hotreload::migrate::{MigrationPlan, NodeMigration};
use crate::hotreload::plan::CandidatePlan;
use crate::ir::binding_ir::NodeKey;
use crate::ir::dirty_map::DirtyClass as IrDirtyClass;
use crate::ir::ui_ir::{AxisIr, LengthIr, NodeKind, StyleIr, UiItem, UiNode, UiTree};
use crate::resolve::SymbolId;
use crate::view_regions::{StaticNodes, has_regions};

use viso_view::{
    HostError, Scope, ViewHost, attach_node, cell_value, mount_regions, mount_values, vm_value,
};

use crate::diag::Diagnostic;
use crate::hotreload::compat::Retyping;
use crate::hotreload::migrate::{Retype, StateAction};

use viso_behavior::Fault;
use viso_behavior::native::MigratableState;
use viso_ui::state::{StateKey, StateMigration};
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    Axis, BindingTable, Buffer, BuildCx, DirtyClass, EffectStore, FlexStyle, GridStyle, Handle,
    LeafStyle, Length, NodeId, NodeStore, ScrollStyle, SemanticProjector, Size, StateId,
    StateStore, StateValue, StructureCx, TextEdits, Vec2,
};

/// What the commit did to the live runtime, for introspection and tests
/// (AGENTS 34 / 62). Pure counts and flags — the commit itself is infallible, so
/// `diagnostics` is only ever populated by the stage boundary that decides a
/// template is out of this slice's scope before commit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HotReloadReport {
    /// Reactive source cells that survived the reload keeping their live value,
    /// verbatim or converted into the state's new type.
    pub migrated: u32,
    /// Reactive source cells reset to a fresh initializer (a new source, or a
    /// live value that does not convert into the state's new type).
    pub reset: u32,
    /// An `E5101` warning for each state whose live value was reset because it
    /// does not convert into the state's new type.
    pub notices: Vec<Diagnostic>,
    /// Whether a previously focused node lost focus because its template slot did
    /// not survive the reload.
    pub focus_lost: bool,
    /// Scroll containers whose offset could not be restored because their slot did
    /// not survive.
    pub scroll_lost: u32,
    /// Whether the view's handlers were dropped because its recompiled behavior
    /// did not mount.
    pub handlers_lost: bool,
}

/// The live runtime the commit mutates, gathered as a bundle of borrows so the
/// commit is driver-agnostic and drives cleanly from a headless test. The commit
/// owns none of these; it reuses the caller's stores and scratch exactly as a frame
/// does, so it allocates nothing beyond the small key-to-node map a reload needs.
pub struct LiveRuntime<'a> {
    /// The retained node tree the reload patches.
    pub store: &'a mut NodeStore,
    /// The reactive state cells migrated by identity.
    pub states: &'a mut StateStore,
    /// The static binding table rebuilt from the recompiled edges.
    pub bindings: &'a mut BindingTable,
    /// Effect cleanups run when a re-typed or removed subtree is freed.
    pub effects: &'a mut EffectStore,
    /// The virtual-list registry, needed to author any freshly built subtree.
    pub lists: &'a mut VirtualLists,
    /// The edit-buffer registry, needed to author any freshly built `text_input`
    /// subtree (its retained [`Buffer`](viso_ui::Buffer) registers here).
    pub text_edits: &'a mut TextEdits,
    /// The semantic-state projection registry, needed to author any freshly built
    /// stateful subtree (a control's projection registers here).
    pub projectors: &'a mut SemanticProjector,
    /// The view's root node, produced by the last-good build. A rebuild replaces
    /// it in place under its parent (or as a new parentless root) and updates
    /// this field.
    pub root: Option<NodeId>,
    /// The live node of each of the view's static template slots, ascending by
    /// [`NodeKey`]. The commit maintains it; a caller adopting a tree it mounted
    /// itself seeds it with [`static_nodes`] or from the ids its mount recorded.
    pub nodes: &'a mut Vec<(NodeKey, NodeId)>,
    /// Caller-owned scratch reused across subtree frees so the commit allocates no
    /// per-node stack.
    pub scratch: &'a mut Vec<NodeId>,
    /// The host the view's handlers dispatch into, `None` while the view declares
    /// none. The commit creates, reloads or drops it.
    pub view: &'a mut Option<Rc<RefCell<ViewHost>>>,
}

/// Apply a validated reload candidate to the live runtime and report what happened.
///
/// Infallible by construction: `plan`/`diff`/`migrate` have already run and
/// succeeded, so every decision here is a plain application. The order is the one
/// the module docs fix; each step reads only the plan data and the live runtime.
///
/// `plan` is the recompiled candidate (template + binding edges + reactive-source
/// identities); `patch` aligns the last-good and candidate templates by
/// [`NodeKey`]; `migration` carries the per-cell decisions and the node state
/// each kept node carries.
pub fn commit(
    rt: &mut LiveRuntime<'_>,
    plan: &CandidatePlan,
    patch: &StructuralPatch,
    migration: &MigrationPlan,
) -> HotReloadReport {
    let mut report = HotReloadReport::default();

    // Step 1 — structural patch. Reuse the live nodes in place, or rebuild the
    // view carrying each kept node's migratable state. The key-to-node map
    // records, per template NodeKey, the live node it now names, so the rebind
    // step can map an edge's NodeKey to a runtime NodeId without a search.
    let key_to_node = apply_structural(rt, &plan.tree, patch, migration, &mut report);

    // Step 2 — state migration by durable identity. A kept symbol carries its live
    // value across (the reload preserves running state); a new symbol allocates a
    // fresh neutral cell. The returned map lets the rebind step turn an edge's
    // SymbolId into the live StateId it drives.
    // The recompiled behavior's host is created first, so the migration can set
    // each carried state into it while the prior host still holds the old
    // values.
    let mut next = plan
        .view
        .as_ref()
        .map(|view| ViewHost::new(Rc::clone(&view.module), &view.component));
    let symbol_to_state = {
        let prior = rt.view.as_ref().map(|host| host.borrow());
        migrate_states(
            rt.states,
            prior.as_deref(),
            next.as_mut().and_then(|next| next.as_mut().ok()),
            plan,
            migration,
            &mut report,
        )
    };

    // Step 3 — rebind. Replace the view's static edges with the recompiled ones,
    // mapping each edge's NodeKey to its live node and each source SymbolId to
    // its migrated cell.
    let mut dirty_marks: Vec<(NodeId, DirtyClass)> = Vec::new();
    rebind_static(
        rt.store,
        rt.bindings,
        plan,
        &key_to_node,
        &symbol_to_state,
        &mut dirty_marks,
    );

    // Step 4 — targeted dirty + flush. Mark exactly the rebound nodes dirty, then
    // flush the migrated state set so only the changed nodes recompute. The commit
    // touches nothing beyond these nodes.
    for (node, class) in &dirty_marks {
        rt.store.mark_dirty(*node, *class);
    }
    let changed: Vec<StateId> = symbol_to_state.iter().map(|&(_, id)| id).collect();
    rt.store.flush_state_transactions(&changed, rt.bindings);

    // Step 5 — handlers and regions, against the nodes and cells the reload now
    // names.
    mount_behavior(rt, plan, next, &key_to_node, &symbol_to_state, &mut report);

    *rt.nodes = key_to_node;
    report
}

/// Installs `next`, the recompiled behavior's host the migration carried each
/// kept state into, in place of the prior one; links its states to the migrated
/// cells,
/// reinstalls every static node's handler routes, replacing the prior ones,
/// mounts the view's regions under the static nodes, and delivers the values
/// the static nodes show, replacing the hook that re-delivered the prior ones.
///
/// A state a cell holds is mirrored; one no cell holds (a string, a list) is
/// tracked through its integer revision cell, which is what a region reading it
/// depends on.
///
/// A recompiled module that does not mount keeps no host and no handler, and is
/// counted in [`HotReloadReport::handlers_lost`]: the candidate's behavior was
/// verified and linked while planning, so this is a defensive path.
fn mount_behavior(
    rt: &mut LiveRuntime<'_>,
    plan: &CandidatePlan,
    next: Option<Result<ViewHost, HostError>>,
    key_to_node: &[(NodeKey, NodeId)],
    symbol_to_state: &[(SymbolId, StateId)],
    report: &mut HotReloadReport,
) {
    if plan.view.is_none()
        && let Some(host) = rt.view.take()
    {
        let mut host = host.borrow_mut();
        host.release_regions(rt.store, rt.states);
        host.release_values(rt.store);
    }
    let host = plan.view.as_ref().zip(next).and_then(|(view, next)| {
        let mounted = match (rt.view.take(), next) {
            (Some(host), Ok(next)) => {
                host.borrow_mut().reload(next);
                Some(host)
            }
            (None, Ok(next)) => Some(Rc::new(RefCell::new(next))),
            (Some(host), Err(_)) => {
                let mut host = host.borrow_mut();
                host.release_regions(rt.store, rt.states);
                host.release_values(rt.store);
                None
            }
            (None, Err(_)) => None,
        };
        let Some(host) = mounted else {
            report.handlers_lost = true;
            return None;
        };
        {
            let mut mirrored = host.borrow_mut();
            for &(symbol, slot) in &view.slots {
                let Some(id) = lookup_state(symbol_to_state, symbol) else {
                    continue;
                };
                if plan.initial(symbol).is_some() {
                    mirrored.mirror(slot as usize, id);
                } else {
                    mirrored.track(slot as usize, id);
                }
            }
        }
        Some((view, host))
    });
    for &(key, node) in key_to_node {
        match &host {
            Some((view, host)) => attach_node(
                rt.store,
                host,
                node,
                view.routes(key),
                view.control(key),
                &Scope::EMPTY,
            ),
            None => rt.store.clear_event_handlers(node),
        }
    }
    if let Some((view, host)) = &host
        && !view.regions.is_empty()
    {
        let statics = StaticNodes::of(&plan.tree);
        let mut nodes = vec![None; statics.len()];
        for &(key, node) in key_to_node {
            if let Some(ordinal) = statics.ordinal(key) {
                nodes[ordinal as usize] = Some(node);
            }
        }
        let cells: Vec<(StateKey, StateId)> = symbol_to_state
            .iter()
            .map(|&(symbol, id)| (StateKey::from_parts(symbol.hi, symbol.lo), id))
            .collect();
        let mut cx = StructureCx {
            store: rt.store,
            states: rt.states,
            bindings: rt.bindings,
            effects: rt.effects,
        };
        mount_regions(&mut cx, Rc::new(view.regions.clone()), host, &nodes, &cells);
    }
    if let Some((view, host)) = &host {
        let shown: Vec<_> = key_to_node
            .iter()
            .filter_map(|&(key, node)| Some((node, view.control(key)?)))
            .collect();
        let mut cx = StructureCx {
            store: rt.store,
            states: rt.states,
            bindings: rt.bindings,
            effects: rt.effects,
        };
        mount_values(&mut cx, host, &shown);
    }
    *rt.view = host.map(|(_, host)| host);
}

/// Apply the structural patch and return the template `NodeKey` → live
/// `NodeId` map the rebind step keys against, ascending by key.
///
/// A structure-preserving edit of a region-free view reuses every live node:
/// the candidate numbers its nodes exactly as the last-good tree did, so each
/// key keeps naming its node and all its runtime state stays in place. Any
/// other edit frees the view's subtree and builds the candidate in its place
/// through the same builder the `ui!` emitter targets; kept state cells survive
/// by identity in the state store, which the node rebuild does not touch, and
/// each kept node's migratable node state moves to the node that rebuilt it.
fn apply_structural(
    rt: &mut LiveRuntime<'_>,
    tree: &UiTree,
    patch: &StructuralPatch,
    migration: &MigrationPlan,
    report: &mut HotReloadReport,
) -> Vec<(NodeKey, NodeId)> {
    let regions = has_regions(tree)
        || rt
            .view
            .as_ref()
            .is_some_and(|host| host.borrow().has_regions());
    if patch.is_structure_preserving() && !regions && rt.root.is_some() {
        let map = std::mem::take(rt.nodes);
        let mut next: u32 = 0;
        for item in &tree.items {
            restyle_item(rt.store, item, &mut next, &map);
        }
        return map;
    }

    let carried = carry_out(rt, &migration.nodes);
    let (focused, scrolled) = rt.root.map_or((false, 0), |root| census(rt, root));
    if let Some(host) = rt.view.as_ref() {
        let mut host = host.borrow_mut();
        host.release_regions(rt.store, rt.states);
        host.release_values(rt.store);
    }
    let mut placement = None;
    if let Some(old) = rt.root.take() {
        let before = rt.store.arena().links(old).and_then(|l| l.next_sibling);
        placement = rt.store.parent(old).map(|parent| (parent, before));
        rt.store.free_tree(old, rt.effects, rt.scratch);
    }
    let arena = rt.store.arena();
    rt.bindings.retain_static(|node| arena.is_live(node));
    rt.nodes.clear();
    let mut map = Vec::new();
    let new_root = build_tree(rt, tree, &mut map);
    if let (Some((parent, before)), Some(root)) = (placement, new_root) {
        rt.store.arena_insert_before(parent, root, before);
    }
    rt.root = new_root;
    map.sort_unstable_by_key(|&(key, _)| key);

    let (refocused, restored) = carry_in(rt, carried, &map);
    if focused && !refocused {
        rt.store.set_focused(None);
        report.focus_lost = true;
    }
    report.scroll_lost = scrolled - restored;
    map
}

/// The node state one kept node carries across a rebuild.
struct Carried {
    /// The node's key in the candidate.
    to: NodeKey,
    /// Whether it held focus.
    focus: bool,
    /// Its nonzero scroll offset.
    scroll: Option<Vec2>,
    /// Its edit buffer: text, caret, selection and composition.
    buffer: Option<Box<Buffer>>,
}

/// Lift from the live nodes of the last-good tree the state each of `nodes`
/// carries, before the tree is freed.
fn carry_out(rt: &mut LiveRuntime<'_>, nodes: &[NodeMigration]) -> Vec<Carried> {
    let focused = rt.store.focused();
    let mut out = Vec::new();
    for migration in nodes {
        let Some(old) = lookup_node(rt.nodes, migration.from) else {
            continue;
        };
        let carries = |state| migration.carries.contains(state);
        let offset = rt.store.scroll(old);
        let carried = Carried {
            to: migration.to,
            focus: carries(MigratableState::FOCUS) && focused == Some(old),
            scroll: (carries(MigratableState::SCROLL) && offset != Vec2::ZERO).then_some(offset),
            buffer: carries(MigratableState::SELECTION)
                .then(|| rt.text_edits.take(old))
                .flatten(),
        };
        if carried.focus || carried.scroll.is_some() || carried.buffer.is_some() {
            out.push(carried);
        }
    }
    out
}

/// Whether the subtree at `root` holds the focus, and how many of its nodes
/// hold a nonzero scroll offset.
fn census(rt: &mut LiveRuntime<'_>, root: NodeId) -> (bool, u32) {
    let focused = rt.store.focused();
    let (mut focus, mut scrolled) = (false, 0);
    let stack = &mut *rt.scratch;
    stack.clear();
    stack.push(root);
    while let Some(node) = stack.pop() {
        focus |= focused == Some(node);
        scrolled += u32::from(rt.store.scroll(node) != Vec2::ZERO);
        let mut child = rt.store.arena().links(node).and_then(|l| l.first_child);
        while let Some(id) = child {
            stack.push(id);
            child = rt.store.arena().links(id).and_then(|l| l.next_sibling);
        }
    }
    (focus, scrolled)
}

/// Settle each carried state on the node that rebuilt its kept node, and
/// return whether the focus moved and how many scroll offsets did.
fn carry_in(
    rt: &mut LiveRuntime<'_>,
    carried: Vec<Carried>,
    map: &[(NodeKey, NodeId)],
) -> (bool, u32) {
    let (mut refocused, mut restored) = (false, 0);
    for carried in carried {
        let Some(node) = lookup_node(map, carried.to) else {
            continue;
        };
        if carried.focus {
            rt.store.set_focused(Some(node));
            refocused = true;
        }
        if let Some(offset) = carried.scroll {
            rt.store.restore_scroll(node, offset);
            restored += 1;
        }
        if let Some(buffer) = carried.buffer {
            rt.store.set_text_request(node, buffer.request());
            rt.text_edits.register(node, buffer);
        }
    }
    (refocused, restored)
}

/// The static template slots of a region-free view built from `tree` and
/// mounted at `root`, by ascending [`NodeKey`]: the template and the live tree
/// walked together in the pre-order the emitter numbers nodes in. A node that
/// authors no children (a leaf, a `VirtualList` mounting its own items) numbers
/// its template children without naming a live node for them. A view with
/// regions records its static nodes at mount instead, since region content
/// interleaves with them.
pub fn static_nodes(store: &NodeStore, root: NodeId, tree: &UiTree) -> Vec<(NodeKey, NodeId)> {
    let mut out = Vec::new();
    let mut next = 0;
    if let Some(UiItem::Node(node)) = tree.items.first() {
        static_walk(store, node, Some(root), &mut next, &mut out);
    }
    out
}

fn static_walk(
    store: &NodeStore,
    node: &UiNode,
    live: Option<NodeId>,
    next: &mut u32,
    out: &mut Vec<(NodeKey, NodeId)>,
) {
    out.extend(live.map(|id| (NodeKey(*next), id)));
    *next += 1;
    let authors = matches!(
        node.kind,
        NodeKind::Flex | NodeKind::Grid | NodeKind::Scroll
    );
    let links = |id: NodeId| store.arena().links(id);
    let mut child = live
        .filter(|_| authors)
        .and_then(|id| links(id)?.first_child);
    for item in &node.children {
        match item {
            UiItem::Node(template) => {
                let at = child;
                child = at.and_then(|id| links(id)?.next_sibling);
                static_walk(store, template, at, next, out);
            }
            region => skip_item(region, next),
        }
    }
}

/// Re-apply one template item's static style to the live node at its key: the
/// built size and gap, and the bound environment lengths. A kept node keeps its
/// runtime state; only a request that moved is rewritten and dirtied. An axis
/// bound to environment lengths keeps its folded value until its terms change.
fn restyle_item(store: &mut NodeStore, item: &UiItem, next: &mut u32, map: &[(NodeKey, NodeId)]) {
    let UiItem::Node(node) = item else {
        skip_item(item, next);
        return;
    };
    let key = NodeKey(*next);
    *next += 1;
    let live = map
        .binary_search_by_key(&key, |(k, _)| *k)
        .ok()
        .map(|at| map[at].1);
    match node.kind {
        NodeKind::Flex | NodeKind::Grid | NodeKind::Scroll => {
            for child in &node.children {
                restyle_item(store, child, next, map);
            }
        }
        NodeKind::VirtualList | NodeKind::Leaf | NodeKind::Component => {
            for child in &node.children {
                skip_item(child, next);
            }
        }
    }
    let Some(live) = live else {
        return;
    };
    let style = &node.style;
    let (mut size, gap) = match node.kind {
        NodeKind::Flex => {
            let built = flex_style(style);
            (built.size, Some(built.gap))
        }
        NodeKind::Grid => {
            let built = GridStyle::default();
            (built.size, Some(built.column_gap))
        }
        NodeKind::Scroll => (scroll_style(style).size, None),
        NodeKind::VirtualList | NodeKind::Leaf | NodeKind::Component => {
            (leaf_style(style).size, None)
        }
    };
    if let Some(current) = store.size_request(live) {
        if style.lengths().width.is_some() {
            size.width = current.width;
        }
        if style.lengths().height.is_some() {
            size.height = current.height;
        }
    }
    store.set_fixed_size(live, size);
    if let Some(gap) = gap
        && style.lengths().gap.is_none()
    {
        store.set_gap(live, gap);
    }
    match node_lengths(style.lengths()) {
        Some(lengths) => store.bind_lengths(live, lengths),
        None => store.unbind_lengths(live),
    }
}

/// Build the candidate template's static nodes into the (cleared) live store
/// through a reactive [`BuildCx`], recording each node's template [`NodeKey`] as
/// it is authored. Returns the new root, if any.
///
/// This is the runtime twin of the compile-time emitter (`ui-macros`): it issues
/// the same `flex` / `grid` / `scroll` / `leaf` builder calls in the same order,
/// and numbers the nodes it does not author (a region's content, the children of
/// a node that authors none) without building them, so each key matches the
/// binding edges' numbering exactly.
fn build_tree(
    rt: &mut LiveRuntime<'_>,
    tree: &UiTree,
    map: &mut Vec<(NodeKey, NodeId)>,
) -> Option<NodeId> {
    let mut cx = BuildCx::with_reactive(
        rt.store,
        rt.states,
        rt.bindings,
        rt.lists,
        rt.text_edits,
        rt.projectors,
    );
    let mut next: u32 = 0;
    for item in &tree.items {
        build_item(&mut cx, item, &mut next, map);
    }
    cx.root()
}

/// Author one template item, recording its NodeKey. Containers recurse into their
/// children inside the builder closure so the pre-order matches the emitter.
fn build_item(
    cx: &mut BuildCx<'_>,
    item: &UiItem,
    next: &mut u32,
    map: &mut Vec<(NodeKey, NodeId)>,
) {
    let UiItem::Node(node) = item else {
        // A region's content is mounted by the view runtime after the build.
        skip_item(item, next);
        return;
    };
    let key = NodeKey(*next);
    *next += 1;
    let handle = build_node(cx, node, next, map);
    if let Some(lengths) = node_lengths(node.style.lengths()) {
        cx.bind_lengths(handle, lengths);
    }
    map.push((key, handle.id()));
}

/// Author one node and its children, mirroring the emitter's builder selection.
fn build_node(
    cx: &mut BuildCx<'_>,
    node: &UiNode,
    next: &mut u32,
    map: &mut Vec<(NodeKey, NodeId)>,
) -> Handle {
    match node.kind {
        NodeKind::Flex => cx.flex(flex_style(&node.style), |cx| {
            for child in &node.children {
                build_item(cx, child, next, map);
            }
        }),
        NodeKind::Grid => cx.grid(Default::default(), |cx| {
            for child in &node.children {
                build_item(cx, child, next, map);
            }
        }),
        NodeKind::Scroll => cx.scroll(scroll_style(&node.style), |cx| {
            for child in &node.children {
                build_item(cx, child, next, map);
            }
        }),
        // A VirtualList mounts its own items, and a view gives it none. A
        // Rust-scope component never reaches a commit: `plan` rejects it.
        NodeKind::VirtualList | NodeKind::Leaf | NodeKind::Component => {
            for child in &node.children {
                skip_item(child, next);
            }
            cx.leaf(leaf_style(&node.style))
        }
    }
}

/// Advances `next` past every node under `item`, authoring none of them.
fn skip_item(item: &UiItem, next: &mut u32) {
    match item {
        UiItem::Node(node) => {
            *next += 1;
            for child in &node.children {
                skip_item(child, next);
            }
        }
        UiItem::If(region) => {
            for arm in &region.arms {
                for item in &arm.items {
                    skip_item(item, next);
                }
            }
        }
        UiItem::For(region) => {
            for item in &region.body {
                skip_item(item, next);
            }
        }
        UiItem::Match(region) => {
            for arm in &region.arms {
                for item in &arm.items {
                    skip_item(item, next);
                }
            }
        }
    }
}

/// The flex builder style for a lowered container, mirroring the emitter's
/// `flex_style_tokens`: axis and gap when set, an explicit size only when the node
/// authored a width or height, else the builder default.
fn flex_style(style: &StyleIr) -> FlexStyle {
    let mut out = FlexStyle::default();
    if let Some(axis) = style.axis {
        out.axis = axis_of(axis);
    }
    if let Some(gap) = style.gap {
        out.gap = gap;
    }
    if has_size(style) {
        out.size = size_of(style);
    }
    out
}

/// The scroll builder style, mirroring the emitter: the authored axis (default
/// column) and a size only when authored.
fn scroll_style(style: &StyleIr) -> ScrollStyle {
    let mut out = ScrollStyle::default();
    if let Some(axis) = style.axis {
        out.axis = axis_of(axis);
    }
    if has_size(style) {
        out.size = size_of(style);
    }
    out
}

/// The leaf builder style, mirroring the emitter: a size only when the node
/// authored a width or height, else the default.
fn leaf_style(style: &StyleIr) -> LeafStyle {
    let mut out = LeafStyle::default();
    if has_size(style) {
        out.size = size_of(style);
    }
    out
}

/// Whether the node authored a width or a height, static or bound to the
/// environment, mirroring the emitter's `has_size`.
fn has_size(style: &StyleIr) -> bool {
    style.width.is_some()
        || style.height.is_some()
        || style.lengths().width.is_some()
        || style.lengths().height.is_some()
}

/// The runtime [`Size`] for a lowered style, mirroring the emitter's `size_tokens`:
/// an unset dimension lowers to `Fit`.
fn size_of(style: &StyleIr) -> Size {
    Size {
        width: length_of(style.width),
        height: length_of(style.height),
    }
}

/// One lowered length to a runtime [`Length`]; `None` is `Fit`, matching the
/// emitter.
fn length_of(len: Option<LengthIr>) -> Length {
    match len {
        Some(LengthIr::Fixed(dp)) => Length::Fixed(dp),
        Some(LengthIr::Relative { fixed, pct }) => Length::Relative { fixed, pct },
        Some(LengthIr::Fill { weight }) => Length::Fill { weight },
        Some(LengthIr::Fit) | None => Length::Fit,
    }
}

/// One lowered axis to the runtime [`Axis`].
fn axis_of(axis: AxisIr) -> Axis {
    match axis {
        AxisIr::Row => Axis::Row,
        AxisIr::Column => Axis::Column,
    }
}

/// Migrate each reactive source's live cell by durable identity, and carry each
/// kept state's live value into `next`, the recompiled behavior's host.
///
/// The migration key is the source's [`SymbolId`], bridged to the runtime
/// [`StateKey`] by its `(hi, lo)` parts — the two share a `#[repr(C)]` layout so
/// no UI-side dependency on the compiler is needed. A kept state keeps its value
/// verbatim; a converted one carries its value converted into the new type, each
/// new record field's default computed by `next`; a reset one, or one whose
/// value does not convert after all (a removed variant it holds, a default that
/// faults), restarts from its new initializer and is reported as an `E5101`.
///
/// A state a cell holds (`plan.initial` is `Some`) is written through its cell;
/// one tracked through an integer revision cell has its revision bumped, so each
/// region reading it re-reads the value `next` now holds.
fn migrate_states(
    states: &mut StateStore,
    prior: Option<&ViewHost>,
    mut next: Option<&mut ViewHost>,
    plan: &CandidatePlan,
    migration: &MigrationPlan,
    report: &mut HotReloadReport,
) -> Vec<(SymbolId, StateId)> {
    let slot_of = |symbol: SymbolId| {
        let view = plan.view.as_ref()?;
        view.slots
            .iter()
            .find(|&&(s, _)| s == symbol)
            .map(|&(_, slot)| slot as usize)
    };
    let mut out = Vec::new();
    for m in &migration.states {
        let key = StateKey::from_parts(m.symbol.hi, m.symbol.lo);
        match m.action {
            StateAction::Keep => {
                // The running value is preserved: the widen closure returns the
                // prior value unchanged, so the cell keeps its identity and
                // value. `new_initial` is only used if the key is somehow absent,
                // which a Keep guarantees it is not.
                let (id, outcome) =
                    states.migrate_state(key, StateValue::Int(0), |prior, _new| Some(prior));
                match outcome {
                    StateMigration::Kept | StateMigration::Widened => report.migrated += 1,
                    StateMigration::Reset => report.reset += 1,
                }
                if let (Some(slot), Some(prior), Some(next)) =
                    (migration.slot(m.symbol), prior, next.as_deref_mut())
                    && let Some(value) = prior.current(slot.from as usize, &*states)
                {
                    next.set_state(slot.to as usize, value);
                }
                out.push((m.symbol, id));
            }
            StateAction::Convert | StateAction::Reset => {
                let retype = migration.retype(m.symbol);
                let mut fault = None;
                let converted = retype.and_then(|retype| {
                    let old = match retype.from_slot {
                        Some(slot) => prior?.current(slot as usize, &*states)?,
                        None if retype.held => vm_value(states.get(states.id_for_key(key)?)?)?,
                        None => return None,
                    };
                    let mut convert = |conversion: &Retyping| {
                        conversion
                            .apply(&old, &mut |chunk| next.as_deref_mut()?.run(chunk, &[]).ok())
                    };
                    if let Some(value) = retype.conversion.as_ref().and_then(&mut convert) {
                        return Some(value);
                    }
                    let (conversion, chunk) = retype.migrator.as_ref()?;
                    let value = convert(conversion)?;
                    next.as_deref_mut()?
                        .run(*chunk, &[value])
                        .map_err(|error| fault = Some(error))
                        .ok()
                });
                let initial = plan.initial(m.symbol);
                let slot = slot_of(m.symbol);
                let written = converted.and_then(|value| {
                    let cell = match initial {
                        Some(initial) => Some(cell_value(&value, initial)?),
                        None => None,
                    };
                    if let (Some(slot), Some(next)) = (slot, next.as_deref_mut()) {
                        next.set_state(slot, value);
                    }
                    Some(cell)
                });
                let id = match written {
                    Some(cell) => {
                        let (id, _) = states.migrate_state(
                            key,
                            cell.unwrap_or(StateValue::Int(0)),
                            |prior, new| Some(cell.unwrap_or_else(|| revision(prior, new))),
                        );
                        report.migrated += 1;
                        id
                    }
                    None => {
                        let (id, _) = match initial {
                            Some(initial) => states.migrate_state(key, initial, |_, _| None),
                            None => states.migrate_state(key, StateValue::Int(0), |prior, new| {
                                Some(revision(prior, new))
                            }),
                        };
                        report.reset += 1;
                        if let Some(retype) = retype {
                            report.notices.push(reset_notice(retype, fault.as_ref()));
                        }
                        id
                    }
                };
                out.push((m.symbol, id));
            }
            StateAction::New => {
                // A brand-new source: allocate a cell keyed by identity, holding
                // the state's constant initializer, or a neutral value for a source
                // the reload does not initialize.
                let initial = plan.initial(m.symbol).unwrap_or(StateValue::Int(0));
                let (id, _outcome) = states.migrate_state(key, initial, |_prior, new| Some(new));
                report.reset += 1;
                out.push((m.symbol, id));
            }
            // A dropped source has no live counterpart to key against and no
            // binding will reference it after the rebind, so its cell is left inert
            // (the store has no explicit free; a stale cell is simply unbound).
            StateAction::Dropped => {}
        }
    }
    out
}

/// The next value of a tracked state's revision cell: the prior revision plus
/// one, or `new` when the cell held a mirrored value before.
fn revision(prior: StateValue, new: StateValue) -> StateValue {
    match prior {
        StateValue::Int(n) => StateValue::Int(n.wrapping_add(1)),
        _ => new,
    }
}

/// The `E5101` warning that a state's live value was reset, its `@migrate`
/// function having raised `fault` when it did.
fn reset_notice(retype: &Retype, fault: Option<&Fault>) -> Diagnostic {
    let message = match fault {
        Some(fault) => format!(
            "the state `{}` was reset: its migration from `{}` to `{}` failed: {}",
            retype.name, retype.from, retype.to, fault.message
        ),
        None => format!(
            "the state `{}` was reset: its live value of type `{}` does not convert to `{}`",
            retype.name, retype.from, retype.to
        ),
    };
    Diagnostic::warning("E5101", retype.at, message)
}

/// Replace the static edges of the view's nodes with the recompiled edges,
/// mapping each edge's template [`NodeKey`] to its live [`NodeId`] and each
/// source [`SymbolId`] to its migrated [`StateId`]. The edges of every other
/// live node stay; those of a node the structural step freed are dropped.
/// Collects the (node, class) pairs to mark dirty so the rebound nodes
/// recompute once after the flush.
///
/// An edge whose node or source did not survive the migration is skipped — that
/// can only happen for a slot the structural patch removed or a symbol the plan
/// dropped, neither of which the candidate's own edges should reference, so it is a
/// defensive skip, not a normal path.
fn rebind_static(
    store: &NodeStore,
    bindings: &mut BindingTable,
    plan: &CandidatePlan,
    key_to_node: &[(NodeKey, NodeId)],
    symbol_to_state: &[(SymbolId, StateId)],
    dirty_marks: &mut Vec<(NodeId, DirtyClass)>,
) {
    let order = |id: &NodeId| (id.index(), id.generation());
    let mut statics: Vec<NodeId> = key_to_node.iter().map(|&(_, node)| node).collect();
    statics.sort_unstable_by_key(order);
    let arena = store.arena();
    bindings.retain_static(|node| {
        arena.is_live(node) && statics.binary_search_by_key(&order(&node), order).is_err()
    });
    for edge in plan.bindings.static_edges() {
        let Some(node) = lookup_node(key_to_node, edge.node) else {
            continue;
        };
        let Some(state) = lookup_state(symbol_to_state, edge.source) else {
            continue;
        };
        let class = to_runtime_class(edge.class);
        bindings.bind(state, node, class);
        dirty_marks.push((node, class));
    }
}

/// Find the live node a template [`NodeKey`] maps to in the ascending map.
fn lookup_node(map: &[(NodeKey, NodeId)], key: NodeKey) -> Option<NodeId> {
    map.binary_search_by_key(&key, |(k, _)| *k)
        .ok()
        .map(|at| map[at].1)
}

/// Find the live state cell a source [`SymbolId`] maps to. Linear over the small
/// per-template map; cold reload path.
fn lookup_state(map: &[(SymbolId, StateId)], symbol: SymbolId) -> Option<StateId> {
    map.iter().find(|(s, _)| *s == symbol).map(|(_, id)| *id)
}

/// Translate a DSL-side [`IrDirtyClass`] to the runtime [`DirtyClass`] by matching
/// each set bit to its named runtime constant, mirroring the emitter's
/// `dirty_class_tokens`. The two enums share a byte-identical bit layout, but the
/// commit never reinterprets the raw byte — it rebuilds the value from named
/// constants so a future divergence in either layout is a compile error, not a
/// silent miscompile.
fn to_runtime_class(class: IrDirtyClass) -> DirtyClass {
    const BITS: [(u8, DirtyClass); 8] = [
        (1 << 0, DirtyClass::STRUCTURE),
        (1 << 1, DirtyClass::STYLE),
        (1 << 2, DirtyClass::MEASURE),
        (1 << 3, DirtyClass::LAYOUT),
        (1 << 4, DirtyClass::TRANSFORM),
        (1 << 5, DirtyClass::PAINT),
        (1 << 6, DirtyClass::HIT_TEST),
        (1 << 7, DirtyClass::SEMANTICS),
    ];
    let raw = class.bits();
    let mut out = DirtyClass::EMPTY;
    for (bit, runtime) in BITS {
        if raw & bit != 0 {
            out |= runtime;
        }
    }
    out
}
