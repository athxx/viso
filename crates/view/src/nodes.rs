//! The named nodes a view's actions and handlers reach as a `NodeRef`, and
//! the requests they make of them (U7.6).
//!
//! A `NodeRef` holds a key: the inlined instance whose view names the node in
//! its high 16 bits, the node's place among that component's named nodes in
//! the low 16. A node method queues a request on the VM; the host takes the
//! requests a committed call queued, with the scope it ran in, and raises its
//! pulse cell. The view's node hook depends on the cell, so it runs once the
//! transaction's writes have settled the view's structure — after the region
//! hooks, which register first — and a node the same action mounts is
//! reached. The hook finds each node by its key and applies the request; a
//! node no longer mounted takes none.
//!
//! The static nodes, and those a region mounts outside any `for` item, are
//! found in the host's table. A `for` item mounting component instances keeps
//! a table of its own, which its content's scope carries, so each item's
//! instances reach their own nodes.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::{NodeAction, NodeRequest, Value};
use viso_ui::{
    NodeId, NodeStore, ScrollRequest, StateId, StateStore, StateValue, StructureCx, Vec2,
    blur_node, focus_quietly,
};

use crate::host::{StateCells, ViewHost};
use crate::layout_values::length;
use crate::scope::Scope;

/// The nodes a view, or one `for` item of it, reaches as a `NodeRef`: each
/// key and its node, ascending by key.
#[derive(Debug, Default)]
pub(crate) struct NamedNodes(RefCell<Vec<(u32, NodeId)>>);

impl NamedNodes {
    /// The node of `key`.
    pub(crate) fn get(&self, key: u32) -> Option<NodeId> {
        let entries = self.0.borrow();
        let at = entries.binary_search_by_key(&key, |&(k, _)| k).ok()?;
        Some(entries[at].1)
    }

    /// Makes `node` the node of `key`.
    pub(crate) fn insert(&self, key: u32, node: NodeId) {
        let mut entries = self.0.borrow_mut();
        match entries.binary_search_by_key(&key, |&(k, _)| k) {
            Ok(at) => entries[at].1 = node,
            Err(at) => entries.insert(at, (key, node)),
        }
    }

    /// Replaces every entry with `entries`.
    pub(crate) fn set(&self, entries: impl IntoIterator<Item = (u32, NodeId)>) {
        let mut table = self.0.borrow_mut();
        table.clear();
        table.extend(entries);
        table.sort_unstable_by_key(|&(key, _)| key);
        table.dedup_by_key(|&mut (key, _)| key);
    }
}

/// A request a committed call queued, with the `for` item tables of the scope
/// it ran in, outermost first.
#[derive(Debug)]
pub(crate) struct PendingRequest {
    request: NodeRequest,
    items: Vec<Rc<NamedNodes>>,
}

/// What a view keeps for its `NodeRef`s.
#[derive(Debug, Default)]
pub(crate) struct NodeRefs {
    /// The static nodes and those regions mount outside any `for` item.
    pub(crate) named: NamedNodes,
    /// The requests waiting for the hook.
    pending: Vec<PendingRequest>,
    /// The cell raised when a request is queued, which the hook depends on.
    pulse: Option<StateId>,
    /// The hook applying the requests.
    hook: Option<viso_ui::StructureHookId>,
}

impl ViewHost {
    /// Keeps the requests a committed call in `scope` queued and raises the
    /// view's pulse so its node hook applies them.
    pub(crate) fn queue_nodes(
        &mut self,
        requests: Vec<NodeRequest>,
        scope: &Scope,
        cells: &mut dyn StateCells,
    ) {
        if requests.is_empty() {
            return;
        }
        let refs = &mut self.nodes;
        refs.pending
            .extend(requests.into_iter().map(|request| PendingRequest {
                request,
                items: scope.named.clone(),
            }));
        if let Some(pulse) = refs.pulse
            && let Some(StateValue::Int(revision)) = cells.get(pulse)
        {
            cells.set(pulse, StateValue::Int(revision.wrapping_add(1)));
        }
    }

    /// The node of `key` as a call in a scope with the `for` item tables
    /// `items` reaches it: the innermost item's first, then the view's.
    fn named_node(&self, key: u32, items: &[Rc<NamedNodes>]) -> Option<NodeId> {
        items
            .iter()
            .rev()
            .find_map(|table| table.get(key))
            .or_else(|| self.nodes.named.get(key))
    }

    /// Makes `node` the node of `key` a region mounts, in the innermost
    /// `for` item table of `scope`, else in the view's. A node the region
    /// frees or keeps switched away stays named until another takes its key;
    /// the hook skips it, no longer mounted.
    pub(crate) fn name_node(&self, key: u32, node: NodeId, scope: &Scope) {
        match scope.named.last() {
            Some(table) => table.insert(key, node),
            None => self.nodes.named.insert(key, node),
        }
    }

    /// Drops the hook applying node requests and the requests waiting for it,
    /// as unmounting the view does.
    pub(crate) fn release_nodes(&mut self, store: &mut NodeStore) {
        if let Some(hook) = self.nodes.hook.take() {
            store.remove_structure_hook(hook);
        }
        self.nodes.pending.clear();
        self.nodes.named.set([]);
    }

    /// The pulse cell, allocated in `states` the first time.
    fn node_pulse(&mut self, states: &mut StateStore) -> StateId {
        match self.nodes.pulse {
            Some(pulse) if states.get(pulse).is_some() => pulse,
            _ => {
                let pulse = states.alloc(StateValue::Int(0));
                self.nodes.pulse = Some(pulse);
                self.adopt_pulse(pulse);
                pulse
            }
        }
    }
}

/// Mounts the view's `NodeRef`s: `named` are its static named nodes, by key,
/// and the hook applying the requests its calls queue replaces any earlier
/// one. Registered after the view's region hooks, so it runs after them.
pub fn mount_nodes(
    cx: &mut StructureCx<'_>,
    host: &Rc<RefCell<ViewHost>>,
    named: &[(u32, NodeId)],
) {
    let Ok(mut view) = host.try_borrow_mut() else {
        return;
    };
    view.nodes.named.set(named.iter().copied());
    if let Some(prior) = view.nodes.hook.take() {
        cx.store.remove_structure_hook(prior);
    }
    let pulse = view.node_pulse(cx.states);
    drop(view);
    let keep = Rc::clone(host);
    let hook = cx.store.add_structure_hook([pulse], move |cx, _| {
        let Ok(mut view) = keep.try_borrow_mut() else {
            return;
        };
        let pending = std::mem::take(&mut view.nodes.pending);
        for PendingRequest { request, items } in pending {
            if let Some(node) = view.named_node(request.node, &items)
                && mounted(cx.store, node, view.root)
            {
                apply(cx.store, node, &request);
            }
        }
    });
    host.borrow_mut().nodes.hook = Some(hook);
}

/// Whether a view whose static named nodes are `named` and whose regions are
/// `regions` reaches any node as a `NodeRef`, and so mounts a node hook.
pub(crate) fn reaches_nodes(named: usize, regions: &viso_schema::view::ViewRegions) -> bool {
    named > 0
        || regions
            .regions
            .iter()
            .flat_map(|region| &region.arms)
            .flat_map(|arm| &arm.items)
            .any(|item| {
                matches!(
                    item,
                    viso_schema::view::ItemTemplate::Node {
                        node_ref: Some(_),
                        ..
                    }
                )
            })
}

/// [`mount_nodes`] for the nodes a macro expansion built: each named node is
/// a key and the static ordinal of its node in `nodes`.
#[doc(hidden)]
pub fn __mount_nodes(
    cx: &mut viso_ui::BuildCx<'_>,
    host: &Rc<RefCell<ViewHost>>,
    nodes: &[Option<NodeId>],
    named: &[(u32, usize)],
) {
    let named: Vec<(u32, NodeId)> = named
        .iter()
        .filter_map(|&(key, ordinal)| Some((key, nodes.get(ordinal).copied().flatten()?)))
        .collect();
    cx.structure(|cx| mount_nodes(cx, host, &named));
}

/// Whether `node` is live and in the view's tree under `root`: a node a
/// `preserve` branch keeps switched away is not.
fn mounted(store: &NodeStore, node: NodeId, root: Option<NodeId>) -> bool {
    if !store.arena().is_live(node) {
        return false;
    }
    let Some(root) = root else {
        return true;
    };
    let mut at = Some(node);
    while let Some(id) = at {
        if id == root {
            return true;
        }
        at = store.parent(id);
    }
    false
}

/// Applies `request` to `node`.
fn apply(store: &mut NodeStore, node: NodeId, request: &NodeRequest) {
    match request.action {
        NodeAction::Focus => {
            focus_quietly(store, node);
        }
        NodeAction::Blur => {
            blur_node(store, node);
        }
        NodeAction::ScrollIntoView => store.request_scroll(node, ScrollRequest::Reveal),
        NodeAction::ScrollTo | NodeAction::ScrollBy => {
            let Some(offset) = request.args.first().and_then(|o| offset(store, node, o)) else {
                return;
            };
            let request = match request.action {
                NodeAction::ScrollTo => ScrollRequest::To(offset),
                _ => ScrollRequest::By(offset),
            };
            store.request_scroll(node, request);
        }
    }
}

/// The dp offset an `Offset` value folds to at `node`.
fn offset(store: &NodeStore, node: NodeId, value: &Value) -> Option<Vec2> {
    let Value::Agg(agg) = value else {
        return None;
    };
    let [x, y] = &*agg.fields else {
        return None;
    };
    Some(store.fold_offset(node, [length(x)?, length(y)?]))
}
