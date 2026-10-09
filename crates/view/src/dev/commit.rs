//! The commit of a typed UI patch: the only stage of a hot reload that
//! touches the live tree (`Viso_Hot_Reload.md` §12, §13, §38, §41; AGENTS
//! 21.7).
//!
//! The host compiles, validates and plans every edit before it sends one, so
//! by the time a [`ViewPatch`](super::wire::ViewPatch) reaches a running app
//! there is nothing left that can fail: the candidate's behavior module is
//! decoded and verified when the patch is staged ([`Candidate::load`]), and
//! the commit is an infallible applier. A patch the runtime refuses never
//! reaches here, and the live tree stays exactly at its last-good state
//! without a snapshot to restore.
//!
//! The commit applies the plan in a fixed order:
//!
//! 1. **Structural patch** — a structure-preserving edit of a view without
//!    regions reuses every live node in place and restyles it, so all its
//!    runtime state stays untouched. Any other edit rebuilds the view's
//!    subtree from the candidate package, and each kept node's migratable
//!    state — focus, a viewport's scroll offset, a text field's edit buffer,
//!    a look transition in flight — moves from its old node to the node that
//!    rebuilt it. A node a region mounts moves to the node the remounted
//!    region builds from the same item for the same `for` item keys, once
//!    step 5 has mounted it; focus and scroll that do not carry are reported
//!    lost.
//! 2. **State migration** — each state cell migrates by its durable
//!    [`StateKey`]: a kept state keeps its value, a converted one carries it
//!    into its new type, a reset or new one starts from its initializer; the
//!    recompiled behavior's host takes each kept state's VM value.
//! 3. **Rebind** — the view's static edges are replaced by the candidate's,
//!    and exactly the edges the view did not have before are marked: a
//!    property edit that changes no edge marks nothing here.
//! 4. **Changed cells** — the nodes bound to a cell whose value changed
//!    (converted, reset or new) are marked by its edges.
//! 5. **Handlers and regions** — the view's host is created, reloaded or
//!    dropped; its states link to the migrated cells, every static node's
//!    handler routes are reinstalled, the regions mount under the static
//!    nodes and the values the nodes show are delivered, a value equal to
//!    the one a node shows delivering nothing. The prior module's effects are
//!    cancelled first, each cleanup running against the code that created
//!    it, and the candidate's effects mount on the root to run in the next
//!    flush.
//!
//! The commit touches only the view's own subtree. A rebuild frees the view's
//! root and builds the new one under the same parent at the same sibling
//! position; the rebind replaces only the edges of the view's nodes; and the
//! view's region and value hooks are the only hooks it removes. A window
//! holding other content around the view keeps it untouched.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::native::MigratableState;
use viso_behavior::{Fault, Module};
use viso_ui::aot::{build_nodes, restyle_aot_node};
use viso_ui::state::{StateKey, StateMigration};
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, Buffer, BuildCx, DirtyClass, EffectStore, LookTransition, NodeId, NodeStore,
    SemanticProjector, StateId, StateStore, StateValue, StructureCx, TextEdits, Vec2,
};

use super::wire::{
    DirtyCounts, NodeCarry, NodeRef, ReloadPlan, RetypePlan, StateAction, StatePlan, StructuralOp,
};
use crate::attach::{Route, attach_node};
use crate::control::Control;
use crate::effects::{mount_effects, release_effects};
use crate::host::{HostError, ViewHost, cell_value, vm_value};
use crate::regions::{ItemKey, mount_regions};
use crate::scope::Scope;
use crate::tasks::release_tasks;
use crate::values::mount_values;
use crate::{ViewPackage, ViewState};

/// A candidate view, ready to commit: its package, and its behavior module
/// decoded and verified once for every mount of its file.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub package: ViewPackage,
    module: Option<Rc<Module>>,
}

impl Candidate {
    /// Decodes and verifies `package`'s behavior module and checks it holds
    /// the component the view mounts.
    ///
    /// # Errors
    ///
    /// A [`HostError`] when the module does not load or lacks the component.
    pub fn load(package: ViewPackage) -> Result<Candidate, HostError> {
        let module = if package.behavior.is_empty() {
            None
        } else {
            let module = Module::decode(&package.behavior).map_err(HostError::Load)?;
            if module.component(&package.component).is_none() {
                return Err(HostError::NoComponent(package.component.clone()));
            }
            Some(Rc::new(module))
        };
        Ok(Candidate { package, module })
    }

    /// A candidate whose behavior module an in-process host already
    /// verified: `module` is `package.behavior` decoded.
    pub fn verified(package: ViewPackage, module: Option<Rc<Module>>) -> Candidate {
        Candidate { package, module }
    }

    /// The number of static nodes of the view.
    pub fn static_count(&self) -> usize {
        self.package.ui.nodes.len()
    }
}

/// What the commit did to one mount, for the ACK and for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitReport {
    /// State cells that kept their live value, verbatim or converted into
    /// the state's new type.
    pub migrated: u32,
    /// State cells that started from their initializer: new states, and kept
    /// ones whose live value does not convert.
    pub reset: u32,
    /// The reset of each kept state whose live value did not convert.
    pub notices: Vec<ResetNotice>,
    /// Whether a focused node of the view lost focus because its node did
    /// not survive.
    pub focus_lost: bool,
    /// Scroll offsets of the view whose node did not survive.
    pub scroll_lost: u32,
    /// Whether the view's handlers were dropped because the candidate's
    /// behavior did not link.
    pub handlers_lost: bool,
    /// How many of the view's static nodes carry each [`DirtyClass`] once
    /// the commit finished — what the next frame's layout and paint have to
    /// redo, named for the diagnostic line (§53) and the ACK (§37).
    pub dirty: DirtyCounts,
}

impl DirtyCounts {
    /// `nodes`' counts, one node at a time.
    fn of(store: &NodeStore, nodes: &[Option<NodeId>]) -> DirtyCounts {
        let mut counts = DirtyCounts::default();
        for &node in nodes.iter().flatten() {
            let dirty = store.dirty(node);
            if dirty.intersects(DirtyClass::STRUCTURE) {
                counts.structure += 1;
            }
            if dirty.intersects(DirtyClass::STYLE) {
                counts.style += 1;
            }
            if dirty.intersects(DirtyClass::MEASURE) {
                counts.measure += 1;
            }
            if dirty.intersects(DirtyClass::LAYOUT) {
                counts.layout += 1;
            }
            if dirty.intersects(DirtyClass::TRANSFORM) {
                counts.transform += 1;
            }
            if dirty.intersects(DirtyClass::PAINT) {
                counts.paint += 1;
            }
            if dirty.intersects(DirtyClass::HIT_TEST) {
                counts.hit_test += 1;
            }
            if dirty.intersects(DirtyClass::SEMANTICS) {
                counts.semantics += 1;
            }
        }
        counts
    }
}

/// A kept state the commit reset because its live value did not convert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetNotice {
    /// The byte range of the state's declaration in the candidate.
    pub start: u32,
    pub end: u32,
    pub message: String,
}

/// The live runtime one mount's commit mutates, as a bundle of borrows so
/// the commit drives cleanly from an app window or a headless test. It reuses
/// the caller's stores and scratch exactly as a frame does.
pub struct LiveRuntime<'a> {
    pub store: &'a mut NodeStore,
    pub states: &'a mut StateStore,
    pub bindings: &'a mut BindingTable,
    /// Effect cleanups run when a subtree is freed.
    pub effects: &'a mut EffectStore,
    /// The registries a freshly built subtree registers in.
    pub lists: &'a mut VirtualLists,
    pub text_edits: &'a mut TextEdits,
    pub projectors: &'a mut SemanticProjector,
    /// The view's root. A rebuild replaces it in place under its parent (or
    /// as a new parentless root) and updates this field.
    pub root: Option<NodeId>,
    /// The live node of each of the view's static nodes, by static index.
    /// The commit maintains it; a caller adopting a mount seeds it from the
    /// ids the mount recorded or with [`static_nodes`].
    pub nodes: &'a mut Vec<Option<NodeId>>,
    /// Scratch reused across subtree frees.
    pub scratch: &'a mut Vec<NodeId>,
    /// The host the view's handlers dispatch into, `None` while the view
    /// declares none. The commit creates, reloads or drops it.
    pub view: &'a mut Option<Rc<RefCell<ViewHost>>>,
}

/// The static nodes of a view without regions mounted at `root`, by static
/// index: the live tree walked in pre-order with `shape`, the number of
/// static children of each static node in pre-order. A view's static
/// children are exactly a container's live children; a node that authors no
/// children (a leaf, a `VirtualList` mounting its own items) has none.
pub fn static_nodes(store: &NodeStore, root: NodeId, shape: &[u32]) -> Vec<Option<NodeId>> {
    let mut out = vec![None; shape.len()];
    let mut cursor = 0;
    while cursor < shape.len() {
        let live = (cursor == 0).then_some(root);
        walk_static(store, shape, live, &mut cursor, &mut out);
    }
    out
}

fn walk_static(
    store: &NodeStore,
    shape: &[u32],
    live: Option<NodeId>,
    cursor: &mut usize,
    out: &mut [Option<NodeId>],
) {
    let index = *cursor;
    *cursor += 1;
    out[index] = live;
    let links = |id: NodeId| store.arena().links(id);
    let mut child = live.and_then(|id| links(id)?.first_child);
    for _ in 0..shape[index] {
        if *cursor >= shape.len() {
            return;
        }
        let at = child;
        child = at.and_then(|id| links(id)?.next_sibling);
        walk_static(store, shape, at, cursor, out);
    }
}

/// Commits `candidate` to the mount `rt` by `plan`, and reports what it kept
/// and lost. Infallible: everything that can fail ran before the patch was
/// staged.
pub fn commit(rt: &mut LiveRuntime<'_>, candidate: &Candidate, plan: &ReloadPlan) -> CommitReport {
    let mut report = CommitReport::default();
    let package = &candidate.package;

    // Step 0 — cancel the running tasks, each `cancelled` handler running the
    // code that started it while its instance still lives, so what it writes
    // migrates with the rest.
    if let Some(host) = rt.view.as_ref() {
        release_tasks(host, rt.store, rt.states);
    }

    // Step 1 — reuse the live nodes in place, or rebuild the view carrying
    // each kept node's migratable state.
    let (nodes, pending) = apply_structural(rt, package, plan);

    // Step 2 — the recompiled behavior's host is created first, so the
    // migration sets each carried state into it while the prior host still
    // holds the old values. It keeps the prior host's grant: an edit cannot
    // widen it.
    let mut next = candidate.module.as_ref().map(|module| match &rt.view {
        Some(prior) => {
            let prior = prior.borrow();
            let grant: Vec<&str> = prior.capabilities().iter().map(|c| &**c).collect();
            ViewHost::with_capabilities(Rc::clone(module), &package.component, &grant)
        }
        None => ViewHost::new(Rc::clone(module), &package.component),
    });
    let mut changed = Vec::new();
    let cells = {
        let prior = rt.view.as_ref().map(|host| host.borrow());
        migrate_states(
            rt.states,
            prior.as_deref(),
            next.as_mut().and_then(|next| next.as_mut().ok()),
            plan,
            &mut changed,
            &mut report,
        )
    };

    // Step 3 — rebind the view's static edges, marking the ones it adds.
    rebind_static(rt, package, &nodes, &cells);

    // Step 4 — the nodes bound to a cell whose value changed.
    rt.store.flush_state_transactions(&changed, rt.bindings);

    // Step 5 — handlers and regions, against the nodes and cells the reload
    // now names.
    mount_behavior(rt, candidate, next, &nodes, &cells, &mut report);
    if let Some(pending) = pending {
        settle_carried(rt, pending, &mut report);
    }

    report.dirty = DirtyCounts::of(rt.store, &nodes);
    *rt.nodes = nodes;
    report
}

/// Applies the structural patch and returns the live node of each static
/// node of the candidate, and after a rebuild what still waits for the
/// regions to mount.
fn apply_structural(
    rt: &mut LiveRuntime<'_>,
    package: &ViewPackage,
    plan: &ReloadPlan,
) -> (Vec<Option<NodeId>>, Option<Pending>) {
    let regions = !package.regions.is_empty()
        || rt
            .view
            .as_ref()
            .is_some_and(|host| host.borrow().has_regions());
    if plan.preserving && !regions && rt.root.is_some() && rt.nodes.len() == package.ui.nodes.len()
    {
        let nodes = std::mem::take(rt.nodes);
        for (node, live) in package.ui.nodes.iter().zip(&nodes) {
            if let Some(live) = *live {
                restyle_aot_node(rt.store, rt.states, node, live);
            }
        }
        return (nodes, None);
    }
    if !regions && !plan.structural.is_empty() && rt.root.is_some() {
        return apply_structural_ops(rt, package, plan);
    }
    let carried = carry_out(rt, &plan.nodes);
    let regional = carry_out_regions(rt, &plan.nodes);
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
    let mut nodes = Vec::new();
    let new_root = {
        let mut cx = BuildCx::with_reactive(
            rt.store,
            rt.states,
            rt.bindings,
            rt.lists,
            rt.text_edits,
            rt.projectors,
        );
        build_nodes(&mut cx, &package.ui.nodes, &mut nodes);
        cx.root()
    };
    if let (Some((parent, before)), Some(root)) = (placement, new_root) {
        rt.store.arena_insert_before(parent, root, before);
    }
    rt.root = new_root;

    let mut moving = Vec::new();
    let (refocused, restored) = carry_in(
        rt,
        carried,
        |to, _| match to {
            NodeRef::Static(index) => nodes.get(index as usize).copied().flatten(),
            NodeRef::Region { .. } => None,
        },
        &mut moving,
    );
    let pending = Pending {
        regional,
        focused,
        scrolled,
        refocused,
        restored,
        moving,
    };
    (nodes, Some(pending))
}

/// Applies `plan.structural`'s node-level ops to the live tree instead of
/// freeing and rebuilding the view's whole root: a node no op names keeps its
/// live [`NodeId`], so only the subtree an op actually touches loses runtime
/// state, and every untouched sibling — a `Tabs` instance, anything else
/// around the edit — is never freed. Every op's [`NodeRef`] addresses the
/// last-good tree, resolved against `old`, the pristine array this function
/// takes and never mutates again: an insert's anchor is always a kept node,
/// so its live id never moves mid-batch and the order these run in does not
/// matter beyond each op's own left-to-right emission order. Only a
/// region-free mount reaches here — `apply_structural` keeps the whole-root
/// rebuild for a view with a region.
fn apply_structural_ops(
    rt: &mut LiveRuntime<'_>,
    package: &ViewPackage,
    plan: &ReloadPlan,
) -> (Vec<Option<NodeId>>, Option<Pending>) {
    let old = std::mem::take(rt.nodes);
    let resolve = |node: NodeRef| match node {
        NodeRef::Static(index) => old.get(index as usize).copied().flatten(),
        NodeRef::Region { .. } => None,
    };

    // Every kept node's unchanged live identity moves to its new static
    // ordinal — the same remap `migrate.rs` documents `plan.nodes` for, used
    // here instead of a rebuild's full re-authoring.
    let mut nodes = vec![None; package.ui.nodes.len()];
    for carry in &plan.nodes {
        if let (NodeRef::Static(from), NodeRef::Static(to)) = (carry.from, carry.to) {
            nodes[to as usize] = old.get(from as usize).copied().flatten();
        }
    }

    let (mut focused, mut scrolled) = (false, 0);
    let (mut refocused, mut restored) = (false, 0);
    let mut moving = Vec::new();

    for op in &plan.structural {
        match op {
            StructuralOp::Remove { node } => {
                let Some(live) = resolve(*node) else { continue };
                let (f, s) = census(rt, live);
                focused |= f;
                scrolled += s;
                rt.store.free_tree(live, rt.effects, rt.scratch);
            }
            StructuralOp::Replace {
                node,
                start,
                subtree,
            } => {
                let Some(live) = resolve(*node) else { continue };
                // The kept descendants a replace's subtree still carries
                // (`KeptNode::under_replace`): lift their state before the
                // old subtree is freed, by the candidate ordinal `plan.nodes`
                // already resolved into this op's own range.
                let end = *start + subtree.len() as u32;
                let mut carried = Vec::new();
                for carry in &plan.nodes {
                    let NodeRef::Static(to) = carry.to else {
                        continue;
                    };
                    if to < *start || to >= end {
                        continue;
                    }
                    let NodeRef::Static(from) = carry.from else {
                        continue;
                    };
                    if let Some(old_live) = old.get(from as usize).copied().flatten() {
                        carried.extend(lift(rt, old_live, carry, None));
                    }
                }
                let (f, s) = census(rt, live);
                focused |= f;
                scrolled += s;

                let is_root = rt.root == Some(live);
                let parent = rt.store.parent(live);
                let before = rt.store.arena().links(live).and_then(|l| l.next_sibling);
                rt.store.free_tree(live, rt.effects, rt.scratch);

                let mut built = Vec::new();
                let new_root = {
                    let mut cx = BuildCx::with_reactive(
                        rt.store,
                        rt.states,
                        rt.bindings,
                        rt.lists,
                        rt.text_edits,
                        rt.projectors,
                    );
                    build_nodes(&mut cx, subtree, &mut built);
                    cx.root()
                };
                if is_root {
                    rt.root = new_root;
                } else if let (Some(parent), Some(root)) = (parent, new_root) {
                    rt.store.arena_insert_before(parent, root, before);
                }
                for (offset, id) in built.into_iter().enumerate() {
                    nodes[*start as usize + offset] = id;
                }
                let (rf, rs) = carry_in(
                    rt,
                    carried,
                    |to, _| match to {
                        NodeRef::Static(index) => nodes.get(index as usize).copied().flatten(),
                        NodeRef::Region { .. } => None,
                    },
                    &mut moving,
                );
                refocused |= rf;
                restored += rs;
            }
            StructuralOp::Insert {
                parent,
                before,
                start,
                subtree,
            } => {
                let Some(parent) = resolve(*parent) else {
                    continue;
                };
                let before = before.and_then(resolve);
                let mut built = Vec::new();
                let new_root = {
                    let mut cx = BuildCx::with_reactive(
                        rt.store,
                        rt.states,
                        rt.bindings,
                        rt.lists,
                        rt.text_edits,
                        rt.projectors,
                    );
                    build_nodes(&mut cx, subtree, &mut built);
                    cx.root()
                };
                if let Some(root) = new_root {
                    rt.store.arena_insert_before(parent, root, before);
                }
                for (offset, id) in built.into_iter().enumerate() {
                    nodes[*start as usize + offset] = id;
                }
            }
        }
    }

    let pending = Pending {
        regional: Vec::new(),
        focused,
        scrolled,
        refocused,
        restored,
        moving,
    };
    (nodes, Some(pending))
}

/// A rebuild's node state that waits for the candidate's regions to mount.
struct Pending {
    /// What the nodes the last-good regions mounted carry.
    regional: Vec<Carried>,
    /// Whether the last-good subtree held the focus, and how many of its
    /// nodes a scroll offset.
    focused: bool,
    scrolled: u32,
    /// Whether the focus, and how many offsets, the static nodes took.
    refocused: bool,
    restored: u32,
    /// The look transitions in flight the static nodes take once their
    /// values are delivered.
    moving: Vec<(NodeId, LookTransition)>,
}

/// Settles what the last-good regions' nodes carry on the nodes the
/// candidate's regions mounted, resumes every carried transition on the
/// value its node now shows, then reports the focus and the offsets no node
/// took.
fn settle_carried(rt: &mut LiveRuntime<'_>, mut pending: Pending, report: &mut CommitReport) {
    let mut mounted = Vec::new();
    if !pending.regional.is_empty()
        && let Some(host) = rt.view.as_ref()
    {
        mounted = host.borrow().region_nodes(rt.store);
    }
    let (refocused, restored) = carry_in(
        rt,
        pending.regional,
        |to, path| {
            let NodeRef::Region { region, arm, item } = to else {
                return None;
            };
            mounted
                .iter()
                .find(|node| {
                    (node.region, node.arm, node.item) == (region, arm, item)
                        && Some(node.path.as_slice()) == path
                })
                .map(|node| node.node)
        },
        &mut pending.moving,
    );
    for (node, moving) in pending.moving {
        rt.store.resume_transition(node, moving);
    }
    if pending.focused && !(pending.refocused || refocused) {
        rt.store.set_focused(None);
        report.focus_lost = true;
    }
    report.scroll_lost = pending.scrolled.saturating_sub(pending.restored + restored);
}

/// The node state one kept node carries across a rebuild.
struct Carried {
    /// The node in the candidate.
    to: NodeRef,
    /// For a node a region mounts, the keys of the `for` items it sits in.
    path: Option<Vec<ItemKey>>,
    /// Whether it held focus.
    focus: bool,
    /// Its nonzero scroll offset.
    scroll: Option<Vec2>,
    /// Its edit buffer: text, caret, selection and composition.
    buffer: Option<Box<Buffer>>,
    /// Its look transitions in flight.
    moving: Vec<LookTransition>,
}

/// Lifts from the static nodes of the last-good view the state each of
/// `carries` names, before the tree is freed.
fn carry_out(rt: &mut LiveRuntime<'_>, carries: &[NodeCarry]) -> Vec<Carried> {
    let mut out = Vec::new();
    for carry in carries {
        let NodeRef::Static(index) = carry.from else {
            continue;
        };
        if let Some(old) = rt.nodes.get(index as usize).copied().flatten() {
            out.extend(lift(rt, old, carry, None));
        }
    }
    out
}

/// Lifts the state each node the last-good regions mount carries, pairing it
/// with its `for` item keys.
fn carry_out_regions(rt: &mut LiveRuntime<'_>, carries: &[NodeCarry]) -> Vec<Carried> {
    let Some(host) = rt.view.as_ref().map(Rc::clone) else {
        return Vec::new();
    };
    if !carries
        .iter()
        .any(|carry| matches!(carry.from, NodeRef::Region { .. }))
    {
        return Vec::new();
    }
    let mounted = host.borrow().region_nodes(rt.store);
    let mut out = Vec::new();
    for node in mounted {
        let from = NodeRef::Region {
            region: node.region,
            arm: node.arm,
            item: node.item,
        };
        if let Some(carry) = carries.iter().find(|carry| carry.from == from) {
            out.extend(lift(rt, node.node, carry, Some(node.path)));
        }
    }
    out
}

/// The state the live node `old` carries as far as `carry` marks it, `None`
/// when it holds none.
fn lift(
    rt: &mut LiveRuntime<'_>,
    old: NodeId,
    carry: &NodeCarry,
    path: Option<Vec<ItemKey>>,
) -> Option<Carried> {
    let carries = |state| carry.carries.contains(state);
    let offset = rt.store.scroll(old);
    let mut moving = Vec::new();
    if carries(MigratableState::ANIMATION) {
        rt.store.lift_transitions(old, &mut moving);
    }
    let carried = Carried {
        to: carry.to,
        path,
        focus: carries(MigratableState::FOCUS) && rt.store.focused() == Some(old),
        scroll: (carries(MigratableState::SCROLL) && offset != Vec2::ZERO).then_some(offset),
        buffer: carries(MigratableState::SELECTION)
            .then(|| rt.text_edits.take(old))
            .flatten(),
        moving,
    };
    let holds = carried.focus
        || carried.scroll.is_some()
        || carried.buffer.is_some()
        || !carried.moving.is_empty();
    holds.then_some(carried)
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

/// Settles each carried state on the node `rebuilt` names for its candidate
/// node and `for` item keys, and returns whether the focus moved and how
/// many scroll offsets did. The transitions in flight wait in `moving` for
/// the values the reload delivers.
fn carry_in(
    rt: &mut LiveRuntime<'_>,
    carried: Vec<Carried>,
    rebuilt: impl Fn(NodeRef, Option<&[ItemKey]>) -> Option<NodeId>,
    moving: &mut Vec<(NodeId, LookTransition)>,
) -> (bool, u32) {
    let (mut refocused, mut restored) = (false, 0);
    for carried in carried {
        let Some(node) = rebuilt(carried.to, carried.path.as_deref()) else {
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
        moving.extend(carried.moving.into_iter().map(|m| (node, m)));
    }
    (refocused, restored)
}

/// Migrates each state cell of `plan` by its durable key, carries each kept
/// state's live value into `next`, the recompiled behavior's host, and
/// returns the cell of each key. A cell whose value changed goes to
/// `changed`.
///
/// A kept state keeps its value verbatim; a converted one carries its value
/// converted into the new type, each new record field's default computed by
/// `next`; a reset one, or one whose value does not convert after all (a
/// removed variant it holds, a default that faults), restarts from its new
/// initializer and is reported. A state a cell holds is written through its
/// cell; one tracked through an integer revision cell has its revision
/// bumped, so each region reading it re-reads the value `next` now holds.
fn migrate_states(
    states: &mut StateStore,
    prior: Option<&ViewHost>,
    mut next: Option<&mut ViewHost>,
    plan: &ReloadPlan,
    changed: &mut Vec<StateId>,
    report: &mut CommitReport,
) -> Vec<(StateKey, StateId)> {
    let mut out = Vec::with_capacity(plan.states.len());
    for state in &plan.states {
        let key = state.key;
        let id = match state.action {
            StateAction::Keep => {
                let (id, outcome) =
                    states.migrate_state(key, StateValue::Int(0), |prior, _| Some(prior));
                match outcome {
                    StateMigration::Kept => report.migrated += 1,
                    StateMigration::Widened => {
                        report.migrated += 1;
                        changed.push(id);
                    }
                    StateMigration::Reset => {
                        report.reset += 1;
                        changed.push(id);
                    }
                }
                if let (Some(from), Some(to), Some(prior), Some(next)) =
                    (state.from_slot, state.slot, prior, next.as_deref_mut())
                    && let Some(value) = prior.current(from as usize, &*states)
                {
                    next.set_state(to as usize, value);
                }
                id
            }
            StateAction::Convert | StateAction::Reset => {
                let id = retype_state(states, prior, next.as_deref_mut(), state, report);
                changed.push(id);
                id
            }
            StateAction::New => {
                let initial = state.initial.unwrap_or(StateValue::Int(0));
                let (id, _) = states.migrate_state(key, initial, |_, new| Some(new));
                report.reset += 1;
                changed.push(id);
                id
            }
        };
        out.push((key, id));
    }
    out.sort_unstable_by_key(|&(key, _)| (key.hi, key.lo));
    out
}

/// Converts the kept state `state` whose type or cell form changed into its
/// new type, or resets it and reports the reset.
fn retype_state(
    states: &mut StateStore,
    prior: Option<&ViewHost>,
    mut next: Option<&mut ViewHost>,
    state: &StatePlan,
    report: &mut CommitReport,
) -> StateId {
    let key = state.key;
    let retype = state.retype.as_deref();
    let mut fault = None;
    let converted = retype.and_then(|retype| {
        let old = match state.from_slot {
            Some(slot) => prior?.current(slot as usize, &*states)?,
            None if retype.held => vm_value(states.get(states.id_for_key(key)?)?)?,
            None => return None,
        };
        let mut convert = |conversion: &viso_behavior::retype::Retyping| {
            conversion.apply(&old, &mut |chunk| next.as_deref_mut()?.run(chunk, &[]).ok())
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
    let written = converted.and_then(|value| {
        let cell = match state.initial {
            Some(initial) => Some(cell_value(&value, initial)?),
            None => None,
        };
        if let (Some(slot), Some(next)) = (state.slot, next) {
            next.set_state(slot as usize, value);
        }
        Some(cell)
    });
    match written {
        Some(cell) => {
            let (id, _) =
                states.migrate_state(key, cell.unwrap_or(StateValue::Int(0)), |prior, new| {
                    Some(cell.unwrap_or_else(|| revision(prior, new)))
                });
            report.migrated += 1;
            id
        }
        None => {
            let (id, _) = match state.initial {
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
    }
}

/// The next value of a tracked state's revision cell: the prior revision
/// plus one, or `new` when the cell held a value before.
fn revision(prior: StateValue, new: StateValue) -> StateValue {
    match prior {
        StateValue::Int(n) => StateValue::Int(n.wrapping_add(1)),
        _ => new,
    }
}

/// The notice that a state's live value was reset, its `@migrate` function
/// having raised `fault` when it did.
fn reset_notice(retype: &RetypePlan, fault: Option<&Fault>) -> ResetNotice {
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
    ResetNotice {
        start: retype.at.0,
        end: retype.at.1,
        message,
    }
}

/// The cell of `key` among the migrated `cells`, ascending by key.
fn cell_of(cells: &[(StateKey, StateId)], key: StateKey) -> Option<StateId> {
    cells
        .binary_search_by_key(&(key.hi, key.lo), |&(k, _)| (k.hi, k.lo))
        .ok()
        .map(|at| cells[at].1)
}

/// Replaces the static edges of the view's nodes with the candidate's,
/// marking each edge the view did not have before. The edges of every other
/// live node stay; those of a node the structural step freed are dropped.
/// An edge whose node or cell does not exist is skipped.
fn rebind_static(
    rt: &mut LiveRuntime<'_>,
    package: &ViewPackage,
    nodes: &[Option<NodeId>],
    cells: &[(StateKey, StateId)],
) {
    let order = |id: &NodeId| (id.index(), id.generation());
    let mut statics: Vec<NodeId> = nodes.iter().flatten().copied().collect();
    statics.sort_unstable_by_key(order);
    let mut marks: Vec<(NodeId, DirtyClass)> = Vec::new();
    for edge in &package.ui.edges {
        let Some(node) = nodes.get(edge.node as usize).copied().flatten() else {
            continue;
        };
        let Some(state) = cell_of(cells, edge.state) else {
            continue;
        };
        let class = DirtyClass::from_bits(edge.class);
        let bound = rt
            .bindings
            .for_state(state)
            .iter()
            .any(|binding| binding.node == node && binding.class == class);
        if !bound {
            marks.push((node, class));
        }
    }
    let arena = rt.store.arena();
    rt.bindings.retain_static(|node| {
        arena.is_live(node) && statics.binary_search_by_key(&order(&node), order).is_err()
    });
    for edge in &package.ui.edges {
        let Some(node) = nodes.get(edge.node as usize).copied().flatten() else {
            continue;
        };
        let Some(state) = cell_of(cells, edge.state) else {
            continue;
        };
        rt.bindings
            .bind(state, node, DirtyClass::from_bits(edge.class));
    }
    for (node, class) in marks {
        rt.store.mark_dirty(node, class);
    }
}

/// The routes of static node `index`, from the handlers grouped by node.
fn routes_of(package: &ViewPackage, index: u32, out: &mut Vec<Route>) {
    out.clear();
    let handlers = &package.handlers;
    let start = handlers.partition_point(|h| h.node < index);
    out.extend(
        handlers[start..]
            .iter()
            .take_while(|h| h.node == index)
            .map(|h| h.route),
    );
}

/// The control of static node `index`.
fn control_of(package: &ViewPackage, index: u32) -> Option<Control> {
    let controls = &package.controls;
    controls
        .binary_search_by_key(&index, |c| c.node)
        .ok()
        .map(|at| controls[at].control.clone())
}

/// Installs `next`, the recompiled behavior's host the migration carried
/// each kept state into, in place of the prior one; links its states to the
/// migrated cells and its `env` slots to the environment, reinstalls every
/// static node's handler routes, mounts the view's regions under the static
/// nodes, and delivers the values the static nodes show, replacing the hook
/// that delivered the prior ones.
///
/// A state a cell holds is mirrored; one no cell holds (a string, a list) is
/// tracked through its integer revision cell, which is what a region reading
/// it depends on. A candidate whose behavior does not link keeps no host and
/// no handler, and is counted in [`CommitReport::handlers_lost`].
fn mount_behavior(
    rt: &mut LiveRuntime<'_>,
    candidate: &Candidate,
    next: Option<Result<ViewHost, HostError>>,
    nodes: &[Option<NodeId>],
    cells: &[(StateKey, StateId)],
    report: &mut CommitReport,
) {
    let package = &candidate.package;
    if let Some(host) = rt.view.as_ref() {
        release_effects(host, rt.effects);
    }
    if candidate.module.is_none()
        && let Some(host) = rt.view.take()
    {
        let mut host = host.borrow_mut();
        host.release_regions(rt.store, rt.states);
        host.release_values(rt.store);
        host.release_env(rt.states);
    }
    let host = next.and_then(|next| {
        let mounted = match (rt.view.take(), next) {
            (Some(host), Ok(next)) => {
                host.borrow_mut().reload(next);
                Some(host)
            }
            (None, Ok(next)) => Some(next.shared()),
            (Some(host), Err(_)) => {
                let mut host = host.borrow_mut();
                host.release_regions(rt.store, rt.states);
                host.release_values(rt.store);
                host.release_env(rt.states);
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
            for &ViewState {
                key, slot, tracked, ..
            } in &package.states
            {
                let (Some(slot), Some(id)) = (slot, cell_of(cells, key)) else {
                    continue;
                };
                if tracked {
                    mirrored.track(slot as usize, id);
                } else {
                    mirrored.mirror(slot as usize, id);
                }
            }
            for read in &package.env {
                let anchor = nodes.get(read.anchor as usize).copied().flatten();
                mirrored.link_env(read.slot as usize, read.field, anchor, rt.states);
            }
            mirrored.prune_env(rt.states);
        }
        crate::__mount_persisted(&host, rt.store, rt.states);
        Some(host)
    });
    let mut routes = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        let Some(node) = *node else {
            continue;
        };
        match &host {
            Some(host) => {
                routes_of(package, index as u32, &mut routes);
                attach_node(
                    rt.store,
                    host,
                    node,
                    &routes,
                    control_of(package, index as u32),
                    &Scope::EMPTY,
                );
            }
            None => rt.store.clear_event_handlers(node),
        }
    }
    if let Some(host) = &host {
        let mut cx = StructureCx {
            store: rt.store,
            states: rt.states,
            bindings: rt.bindings,
            effects: rt.effects,
        };
        if !package.regions.is_empty() {
            mount_regions(
                &mut cx,
                Rc::new(package.regions.clone()),
                host,
                nodes,
                cells,
            );
        }
        let shown: Vec<(NodeId, Control)> = package
            .controls
            .iter()
            .filter_map(|c| {
                Some((
                    nodes.get(c.node as usize).copied().flatten()?,
                    c.control.clone(),
                ))
            })
            .collect();
        mount_values(&mut cx, host, &shown);
        if let Some(root) = rt.root {
            mount_effects(rt.store, host, root);
        }
    }
    *rt.view = host;
}
