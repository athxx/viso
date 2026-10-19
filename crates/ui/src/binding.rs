//! The binding table: the compiled edges from a [`StateId`] to the nodes it
//! invalidates and the dirty classes each edge carries (§18.1).
//!
//! This is the static fast path of the reactive model. An edge is registered
//! once (at build/reconcile time) and read every flush; it turns "state `count`
//! changed" into "mark node `label` MEASURE|LAYOUT|PAINT, mark node `badge`
//! PAINT" without any per-signal subscriber list, closure boxing, or string
//! lookup.
//!
//! Edges live at three levels:
//!
//! - **Template edges.** Structure mounted again and again — the rows of a
//!   virtual list, the items of a `for`, the arms of an `if` — is built as
//!   *instances* of a [`Template`]. An instance's nodes fill a block of
//!   consecutive arena slots and its local cells a block of consecutive state
//!   slots, so each of its edges is `(cell offset, node offset, classes)`,
//!   identical for every instance: the template stores them once, and an
//!   instance adds only its [`InstanceBase`]. The first instance records the
//!   template; a later one that binds differently keeps its own edges.
//!   A cell outside the instance (a list's shared selection) is an *import*:
//!   the template stores its edges once too, and a change fans out to every
//!   instance.
//! - **Absolute edges**, `(cell, node, classes)`: edges bound outside every
//!   instance, and the edges of an instance that is not a regular instance of
//!   its template. An instance's absolute edges die with it.
//! - **Dynamic edges**, registered at runtime (by a dynamic script or a
//!   `Computed` re-eval), stored apart so the dynamic fallback never defines
//!   the cost of the static path.
//!
//! An instance lives while one of its nodes does. Freeing its last node queues
//! it; [`BindingTable::collect`] then drops it, releasing its node and cell
//! blocks for the next instance of the same size and freeing its cells and
//! edges, so recycled rows leak neither.

use core::cell::Cell;
use std::collections::HashMap;

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::node::NodeId;
use crate::state::{ScopeSlots, StateId, StateStore};

/// The reactive-binding observability counters (AGENTS section 10.3).
///
/// A compiler-known typed binding must never silently fall back to runtime
/// dynamic tracking, and the dynamic path must never define the cost of the
/// static fast path — so the two paths are counted apart. These are strippable
/// observability counters (an inspector/CI reads them to confirm a strict typed
/// example does zero dynamic fallback); they never gate correctness and never
/// ride a hot inner loop beyond a single increment per edge evaluated.
///
/// The evaluation counters are `Cell`s so a `&BindingTable` flush can bump them
/// while the table stays shared-borrowed.
#[derive(Debug, Default)]
pub struct ReactiveCounters {
    /// Static edges evaluated (a compiled `for_state` edge walked on flush).
    static_binding_eval: Cell<u64>,
    /// Dynamic edges evaluated (a runtime `dynamic_for_state` edge walked).
    dynamic_binding_eval: Cell<u64>,
    /// Runtime dynamic subscriptions registered (each `bind_dynamic` call).
    dynamic_subscribe: Cell<u64>,
    /// Distinct nodes that took a dynamic edge — the fallback surface a strict
    /// typed example must keep at zero.
    dynamic_fallback_nodes: Cell<u64>,
}

impl ReactiveCounters {
    /// Static edges evaluated since the last reset.
    #[inline]
    pub fn static_binding_eval(&self) -> u64 {
        self.static_binding_eval.get()
    }
    /// Dynamic edges evaluated since the last reset.
    #[inline]
    pub fn dynamic_binding_eval(&self) -> u64 {
        self.dynamic_binding_eval.get()
    }
    /// Runtime dynamic subscriptions registered since the last reset.
    #[inline]
    pub fn dynamic_subscribe(&self) -> u64 {
        self.dynamic_subscribe.get()
    }
    /// Distinct nodes that took a dynamic edge since the last reset.
    #[inline]
    pub fn dynamic_fallback_nodes(&self) -> u64 {
        self.dynamic_fallback_nodes.get()
    }

    /// Zero every counter — call at frame or benchmark-iteration boundaries so a
    /// count reflects one measured window.
    pub fn reset(&self) {
        self.static_binding_eval.set(0);
        self.dynamic_binding_eval.set(0);
        self.dynamic_subscribe.set(0);
        self.dynamic_fallback_nodes.set(0);
    }

    /// Record `n` static edges as evaluated. Called at the flush edge-walk site
    /// (each state's `for_state` slice length), so the count reflects one flush's
    /// compiled-path work without threading a `&mut` counter through the shared
    /// `flush_state_transactions` signature.
    #[inline]
    pub fn record_static_eval(&self, n: u64) {
        self.static_binding_eval
            .set(self.static_binding_eval.get() + n);
    }
    /// Record `n` dynamic edges as evaluated, the dynamic-path counterpart of
    /// [`Self::record_static_eval`].
    #[inline]
    pub fn record_dynamic_eval(&self, n: u64) {
        self.dynamic_binding_eval
            .set(self.dynamic_binding_eval.get() + n);
    }
}

/// One reactive edge: when the source state changes, mark `node` dirty with
/// `class`. A single state may have many bindings (one per dependent node /
/// property), so the flush marks each in turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    /// The node this edge invalidates.
    pub node: NodeId,
    /// The dirty classes the change contributes to that node (e.g. a bound
    /// width is `MEASURE | LAYOUT`; a bound color is `PAINT`).
    pub class: DirtyClass,
}

/// Where a template edge's source cell is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateRef {
    /// The instance's local cell at this offset from its cell base.
    Local(u32),
    /// A cell outside the instance, the same for every instance.
    Import(StateId),
}

/// One edge of a template: a change of `state` marks `dirty` on the
/// instance's node at offset `target` from its node base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TemplateBinding {
    pub state: StateRef,
    pub target: u32,
    pub dirty: DirtyClass,
}

/// What an instance adds to the binding table: its template and where its
/// node and cell blocks begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstanceBase {
    pub template: u32,
    pub node_base: u32,
    pub state_base: u32,
}

/// The template of structure an owner mounts again and again: the first
/// instance records it, and every later instance that binds the same way
/// shares it. An owner keeps one per kind of structure it mounts (a list keeps
/// one for its rows, a region one per arm); a template no instance uses any
/// more is dropped, and the next instance records it afresh.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Template {
    key: Option<(u32, u32)>,
}

impl Template {
    /// A template no instance has recorded yet.
    pub const fn new() -> Template {
        Template { key: None }
    }
}

/// The sizes of the binding table, for a footprint test or an inspector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BindingFootprint {
    /// Live templates.
    pub templates: usize,
    /// Edges the live templates store.
    pub template_edges: usize,
    /// Live instances.
    pub instances: usize,
    /// Absolute edges.
    pub absolute_edges: usize,
    /// Dynamic edges.
    pub dynamic_edges: usize,
}

const NONE: u32 = u32::MAX;

/// A template edge as stored: the target's offset and the classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edge {
    target: u32,
    class: DirtyClass,
}

/// A contiguous slice `[start, start+len)` into an edge array.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Run {
    start: u32,
    len: u32,
}

/// The edges of one imported cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImportRun {
    state: StateId,
    run: Run,
}

/// A template's edges in their canonical form: local edges grouped by cell
/// offset, import edges grouped by cell in order of first use, each run in
/// bind order with repeated targets folded.
#[derive(Debug, Default, PartialEq, Eq)]
struct Edges {
    runs: Vec<Run>,
    local: Vec<Edge>,
    imports: Vec<ImportRun>,
    imported: Vec<Edge>,
}

impl Edges {
    fn clear(&mut self) {
        self.runs.clear();
        self.local.clear();
        self.imports.clear();
        self.imported.clear();
    }

    fn len(&self) -> usize {
        self.local.len() + self.imported.len()
    }

    fn local(&self, offset: u32) -> &[Edge] {
        match self.runs.get(offset as usize) {
            Some(run) => &self.local[run.start as usize..(run.start + run.len) as usize],
            None => &[],
        }
    }

    fn imported(&self, run: Run) -> &[Edge] {
        &self.imported[run.start as usize..(run.start + run.len) as usize]
    }
}

#[derive(Debug, Default)]
struct TemplateData {
    generation: u32,
    nodes: u32,
    states: u32,
    edges: Edges,
    /// The regular instances, for an import's fan-out.
    instances: Vec<u32>,
    /// Every live instance naming the template.
    users: u32,
}

/// The cold half of an instance: what its death releases.
#[derive(Debug, Default)]
struct InstanceLife {
    /// Its live nodes.
    live: u32,
    /// Its node block, base and length (`0` for none).
    node_base: u32,
    nodes: u32,
    /// Its cell block, base and length.
    state_base: u32,
    states: u32,
    /// Its ordinary cells, freed with it.
    loose: Option<Box<[StateId]>>,
    /// Its first absolute edge, chained through `AbsEdge::owner_next`.
    owned: u32,
    /// Its position in its template's `instances`, `NONE` for an instance
    /// that is not regular.
    slot: u32,
    /// Its template, `NONE` for none.
    template: u32,
}

/// An absolute edge, linked into its cell's list and its owner's chain.
#[derive(Debug, Clone, Copy)]
struct AbsEdge {
    state: StateId,
    node: NodeId,
    class: DirtyClass,
    /// In its cell's list; the free list's link for a free edge.
    prev: u32,
    next: u32,
    /// The next edge of the same owning instance.
    owner_next: u32,
    owner: u32,
    /// Still in its cell's list (an owned edge a retain dropped waits in its
    /// owner's chain until the owner dies).
    linked: bool,
}

/// A cell's absolute edges: the first and last.
#[derive(Debug, Clone, Copy)]
struct List {
    head: u32,
    tail: u32,
}

impl Default for List {
    fn default() -> Self {
        List {
            head: NONE,
            tail: NONE,
        }
    }
}

/// An instance under construction.
#[derive(Debug, Clone, Copy)]
struct BindScope {
    instance: u32,
    /// Where its binds begin in `bind_log`.
    log: u32,
}

/// The compiled reactive edges: template edges, absolute edges and the
/// dynamic overflow (see the module docs).
#[derive(Default)]
pub struct BindingTable {
    abs: Vec<AbsEdge>,
    /// The first free edge plus one; `0` for none.
    abs_free: u32,
    abs_live: usize,
    /// Each cell's absolute edges, by state slot.
    lists: Vec<List>,
    /// Runtime-registered edges (dynamic scripts, `Computed`), grouped by
    /// state slot through `dynamic_runs`.
    dynamic: Vec<Binding>,
    dynamic_runs: Vec<Run>,
    templates: Vec<TemplateData>,
    template_free: Vec<u32>,
    /// Each instance's base, by instance.
    bases: Vec<InstanceBase>,
    lives: Vec<InstanceLife>,
    instance_free: Vec<u32>,
    instances_live: usize,
    /// The regular instance whose block holds each cell, by state slot.
    state_owner: Vec<u32>,
    /// The templates importing each cell, by state slot: `(template, import)`.
    importers: HashMap<u32, Vec<(u32, u32)>>,
    scopes: Vec<BindScope>,
    bind_log: Vec<(StateId, NodeId, DirtyClass)>,
    canon: Edges,
    external: Vec<u32>,
    freed: Vec<u32>,
    /// Reactive-path observability (section 10.3). Bumped at bind time (dynamic
    /// subscriptions / fallback nodes) and at evaluation time (static/dynamic
    /// edges walked). Strippable; never gates correctness.
    counters: ReactiveCounters,
}

impl BindingTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a static reactive edge: when `state` changes, mark `node`
    /// dirty with `class`.
    ///
    /// Intended for build/reconcile time. Inside an instance build the edge is
    /// the instance's, shared through its template when the instance is
    /// regular. Elsewhere it is an absolute edge; one that repeats the last
    /// edge of its cell to the same node folds its classes into it.
    pub fn bind(&mut self, state: StateId, node: NodeId, class: DirtyClass) {
        if !self.scopes.is_empty() {
            self.bind_log.push((state, node, class));
            return;
        }
        self.insert_abs(state, node, class, NONE);
    }

    /// The absolute edges of a state: those bound outside every instance, and
    /// those of an instance that is not a regular instance of its template.
    pub fn for_state(&self, state: StateId) -> impl Iterator<Item = Binding> + '_ {
        let mut at = self
            .lists
            .get(state.index() as usize)
            .map_or(NONE, |list| list.head);
        core::iter::from_fn(move || {
            while at != NONE {
                let edge = &self.abs[at as usize];
                at = edge.next;
                if edge.state == state {
                    return Some(Binding {
                        node: edge.node,
                        class: edge.class,
                    });
                }
            }
            None
        })
    }

    /// Call `mark` with every node a change of `state` marks and the classes
    /// it marks: its template edges as node slots (`Ok`), its absolute edges
    /// as handles (`Err`). Returns how many edges it walked.
    pub(crate) fn targets(
        &self,
        state: StateId,
        mut mark: impl FnMut(Result<u32, NodeId>, DirtyClass),
    ) -> u32 {
        let mut walked = 0;
        let slot = state.index() as usize;
        if let Some(&owner) = self.state_owner.get(slot)
            && owner != NONE
        {
            let base = self.bases[owner as usize];
            let edges = &self.templates[base.template as usize].edges;
            for edge in edges.local(state.index() - base.state_base) {
                mark(Ok(base.node_base + edge.target), edge.class);
                walked += 1;
            }
        }
        if let Some(users) = self.importers.get(&state.index()) {
            for &(template, import) in users {
                let data = &self.templates[template as usize];
                let run = data.edges.imports[import as usize];
                if run.state != state {
                    continue;
                }
                for &instance in &data.instances {
                    let node_base = self.bases[instance as usize].node_base;
                    for edge in data.edges.imported(run.run) {
                        mark(Ok(node_base + edge.target), edge.class);
                        walked += 1;
                    }
                }
            }
        }
        let mut at = self.lists.get(slot).map_or(NONE, |list| list.head);
        while at != NONE {
            let edge = &self.abs[at as usize];
            at = edge.next;
            if edge.state == state {
                mark(Err(edge.node), edge.class);
                walked += 1;
            }
        }
        walked
    }

    /// Register a dynamic (runtime) reactive edge. Shares [`Binding`] shape with
    /// the static edges but lives in the separate dynamic region so it never
    /// perturbs the compiled layout. Used by dynamic scripts and, in a later
    /// slice, by `Computed` dependency registration.
    pub fn bind_dynamic(&mut self, state: StateId, node: NodeId, class: DirtyClass) {
        let idx = state.index() as usize;
        if idx >= self.dynamic_runs.len() {
            self.dynamic_runs.resize(idx + 1, Run::default());
        }
        // A subscription is registered on every dynamic bind; a *new* node edge
        // (not a fold into an existing one) is a fresh fallback node.
        self.counters
            .dynamic_subscribe
            .set(self.counters.dynamic_subscribe.get() + 1);
        let before = self.dynamic.len();
        insert_dynamic(&mut self.dynamic, &mut self.dynamic_runs, idx, node, class);
        if self.dynamic.len() > before {
            self.counters
                .dynamic_fallback_nodes
                .set(self.counters.dynamic_fallback_nodes.get() + 1);
        }
    }

    /// The dynamic bindings for a state.
    #[inline]
    pub fn dynamic_for_state(&self, state: StateId) -> &[Binding] {
        match self.dynamic_runs.get(state.index() as usize) {
            Some(run) if run.len > 0 => {
                &self.dynamic[run.start as usize..(run.start + run.len) as usize]
            }
            _ => &[],
        }
    }

    /// Drop every absolute edge bound outside an instance, leaving instances,
    /// templates, the dynamic region and the counters intact.
    ///
    /// A transactional hot reload replaces the compiled binding set wholesale:
    /// the recompiled Binding IR is the new source of truth, so the prior edges
    /// must be voided before the new ones are installed (there is no unbind — a
    /// compiled edge set is rebuilt, not patched). The dynamic region is left
    /// untouched because it is an explicit runtime escape hatch (section 10.3),
    /// not part of the compiled template; an instance's edges go with the
    /// instance. Cold path (reload only).
    pub fn clear_static(&mut self) {
        for at in 0..self.abs.len() as u32 {
            let edge = self.abs[at as usize];
            if edge.linked && edge.owner == NONE {
                self.unlink(at);
                self.free_abs(at);
            }
        }
    }

    /// Drop every absolute edge whose node `keep` rejects, keeping the others
    /// in their order. Cold path (structural change only).
    pub fn retain_static(&mut self, mut keep: impl FnMut(NodeId) -> bool) {
        for at in 0..self.abs.len() as u32 {
            let edge = self.abs[at as usize];
            if edge.linked && !keep(edge.node) {
                self.unlink(at);
                if edge.owner == NONE {
                    self.free_abs(at);
                }
            }
        }
    }

    /// Replace the edges bound outside every instance with a fresh compiled
    /// edge set: [`Self::clear_static`] followed by a [`Self::bind`] for each
    /// edge. Cold path (reload only).
    pub fn rebuild_static(&mut self, edges: impl IntoIterator<Item = (StateId, Binding)>) {
        self.clear_static();
        for (state, binding) in edges {
            self.insert_abs(state, binding.node, binding.class, NONE);
        }
    }

    /// Every edge into `node`, static ones first: the dense index of the state
    /// it comes from, its classes, and whether it is dynamic. Cold path: it
    /// scans the whole table, for an inspector asking about one node.
    pub fn edges_into(
        &self,
        store: &NodeStore,
        node: NodeId,
        out: &mut Vec<(u32, DirtyClass, bool)>,
    ) {
        out.clear();
        if let Some(instance) = store.instance_of(node)
            && let Some(life) = self.lives.get(instance as usize)
            && life.slot != NONE
        {
            let base = self.bases[instance as usize];
            let offset = node.index() - base.node_base;
            let edges = &self.templates[base.template as usize].edges;
            for (cell, run) in edges.runs.iter().enumerate() {
                let start = run.start as usize;
                for edge in &edges.local[start..start + run.len as usize] {
                    if edge.target == offset {
                        out.push((base.state_base + cell as u32, edge.class, false));
                    }
                }
            }
            for import in &edges.imports {
                for edge in edges.imported(import.run) {
                    if edge.target == offset {
                        out.push((import.state.index(), edge.class, false));
                    }
                }
            }
        }
        for edge in &self.abs {
            if edge.linked && edge.node == node {
                out.push((edge.state.index(), edge.class, false));
            }
        }
        for (state, run) in self.dynamic_runs.iter().enumerate() {
            let start = run.start as usize;
            for edge in &self.dynamic[start..start + run.len as usize] {
                if edge.node == node {
                    out.push((state as u32, edge.class, true));
                }
            }
        }
    }

    /// The reactive-path counters (section 10.3), for an inspector, a test, or a
    /// benchmark comparing the static / mixed / dynamic paths.
    #[inline]
    pub fn counters(&self) -> &ReactiveCounters {
        &self.counters
    }

    /// How large the table is.
    pub fn footprint(&self) -> BindingFootprint {
        let live = |t: &&TemplateData| t.users > 0;
        BindingFootprint {
            templates: self.templates.iter().filter(live).count(),
            template_edges: self
                .templates
                .iter()
                .filter(live)
                .map(|t| t.edges.len())
                .sum(),
            instances: self.instances_live,
            absolute_edges: self.abs_live,
            dynamic_edges: self.dynamic.len(),
        }
    }

    /// The base of live instance `instance`, while it is a regular instance
    /// of its template.
    pub fn instance_base(&self, instance: u32) -> Option<InstanceBase> {
        let life = self.lives.get(instance as usize)?;
        (life.slot != NONE).then(|| self.bases[instance as usize])
    }

    /// The edges of the template `template` names, while it is live.
    pub fn template_bindings(&self, template: &Template) -> Option<Vec<TemplateBinding>> {
        let index = self.live_template(template)?;
        let edges = &self.templates[index as usize].edges;
        let mut out = Vec::with_capacity(edges.len());
        for (cell, run) in edges.runs.iter().enumerate() {
            let start = run.start as usize;
            for edge in &edges.local[start..start + run.len as usize] {
                out.push(TemplateBinding {
                    state: StateRef::Local(cell as u32),
                    target: edge.target,
                    dirty: edge.class,
                });
            }
        }
        for import in &edges.imports {
            for edge in edges.imported(import.run) {
                out.push(TemplateBinding {
                    state: StateRef::Import(import.state),
                    target: edge.target,
                    dirty: edge.class,
                });
            }
        }
        Some(out)
    }

    /// Start building an instance of `template`: until the matching
    /// [`end_instance`](Self::end_instance), the nodes `store` allocates and
    /// the cells [`StateStore::alloc_local`] allocates are the instance's, and
    /// the edges bound are its edges. A recorded template sizes the instance's
    /// blocks; a template not recorded yet lets the build allocate freely.
    pub fn begin_instance(
        &mut self,
        template: &Template,
        store: &mut NodeStore,
        states: &mut StateStore,
    ) {
        let instance = match self.instance_free.pop() {
            Some(instance) => instance,
            None => {
                self.bases.push(InstanceBase {
                    template: NONE,
                    node_base: 0,
                    state_base: 0,
                });
                self.lives.push(InstanceLife {
                    slot: NONE,
                    template: NONE,
                    owned: NONE,
                    ..InstanceLife::default()
                });
                self.bases.len() as u32 - 1
            }
        };
        let sizes = self
            .live_template(template)
            .map(|t| &self.templates[t as usize])
            .map(|t| (t.nodes, t.states));
        store.open_scope(instance, sizes.map(|(n, _)| n).filter(|&n| n > 0));
        states.open_scope(sizes.map(|(_, s)| s).filter(|&s| s > 0));
        self.scopes.push(BindScope {
            instance,
            log: self.bind_log.len() as u32,
        });
    }

    /// Finish the instance the innermost [`begin_instance`](Self::begin_instance)
    /// started. An instance whose nodes and cells fill consecutive slots and
    /// whose edges are the template's is a regular instance and stores no
    /// edge of its own; the first such instance records the template. Any
    /// other keeps its edges as absolute edges, freed with it.
    ///
    /// An instance that built no node has nothing to die with: its cells
    /// become ordinary cells and its edges ordinary absolute edges.
    pub fn end_instance(
        &mut self,
        template: &mut Template,
        store: &mut NodeStore,
        states: &mut StateStore,
    ) {
        let scope = self.scopes.pop().expect("an open instance");
        let instance = scope.instance;
        let nodes = store.close_scope();
        let cells = states.close_scope();
        let loose_nodes = store.scope_log(nodes.log);
        let live = nodes.filled + loose_nodes.len() as u32;
        let node_block = block_of(nodes, loose_nodes.iter().map(|id| id.index()));
        let cell_block = block_of(
            cells,
            states.scope_log(cells.log).iter().map(|id| id.index()),
        );
        let binds = scope.log as usize..self.bind_log.len();

        if live == 0 {
            if nodes.len > 0 {
                store.release_block(nodes.base, nodes.len);
            }
            if cells.len > 0 {
                states.unkeep_block(cells.base, cells.len);
            }
            for i in binds {
                let (state, node, class) = self.bind_log[i];
                self.insert_abs(state, node, class, NONE);
            }
            store.drop_scope_log(nodes.log);
            states.drop_scope_log(cells.log);
            self.bind_log.truncate(scope.log as usize);
            self.lives[instance as usize] = InstanceLife {
                slot: NONE,
                template: NONE,
                owned: NONE,
                ..InstanceLife::default()
            };
            self.instance_free.push(instance);
            return;
        }

        let mut regular = None;
        if let (Some(node_block), Some(cell_block)) = (node_block, cell_block) {
            canonical(
                &self.bind_log[binds.clone()],
                node_block,
                cell_block,
                &mut self.canon,
                &mut self.external,
            );
            let canon = &self.canon;
            let matches = |t: &TemplateData| {
                t.nodes == node_block.1 && t.states == cell_block.1 && t.edges == *canon
            };
            regular = match self.live_template(template) {
                Some(t) if matches(&self.templates[t as usize]) => Some(t),
                Some(_) => None,
                None => {
                    let t = self.record_template(node_block.1, cell_block.1);
                    template.key = Some((t, self.templates[t as usize].generation));
                    Some(t)
                }
            };
        }

        let mut life = InstanceLife {
            live,
            node_base: nodes.base,
            nodes: nodes.len,
            state_base: cells.base,
            states: cells.len,
            loose: None,
            owned: NONE,
            slot: NONE,
            template: NONE,
        };
        match regular {
            Some(t) => {
                let (node_base, node_len) = node_block.unwrap_or_default();
                let (state_base, state_len) = cell_block.unwrap_or_default();
                if nodes.len == 0 && node_len > 0 {
                    store.keep_block(instance, node_base, node_len);
                }
                if cells.len == 0 && state_len > 0 {
                    states.keep_block(state_base, state_len);
                }
                life.node_base = node_base;
                life.nodes = node_len;
                life.state_base = state_base;
                life.states = state_len;
                life.template = t;
                self.bases[instance as usize] = InstanceBase {
                    template: t,
                    node_base,
                    state_base,
                };
                if state_len > 0 {
                    let end = (state_base + state_len) as usize;
                    if self.state_owner.len() < end {
                        self.state_owner.resize(end, NONE);
                    }
                    self.state_owner[state_base as usize..end].fill(instance);
                }
                let data = &mut self.templates[t as usize];
                life.slot = data.instances.len() as u32;
                data.instances.push(instance);
                data.users += 1;
            }
            None => {
                let loose = states.scope_log(cells.log);
                if !loose.is_empty() {
                    life.loose = Some(Box::from(loose));
                }
            }
        }
        if regular.is_some() && cells.len > 0 {
            // A filled block took no ordinary cell; nothing else to free.
            life.loose = None;
        }
        self.lives[instance as usize] = life;
        if regular.is_some() {
            for k in 0..self.external.len() {
                let (state, node, class) = self.bind_log[self.external[k] as usize];
                self.insert_abs(state, node, class, instance);
            }
        } else {
            for i in binds {
                let (state, node, class) = self.bind_log[i];
                self.insert_abs(state, node, class, instance);
            }
        }
        store.drop_scope_log(nodes.log);
        states.drop_scope_log(cells.log);
        self.bind_log.truncate(scope.log as usize);
        self.instances_live += 1;
    }

    /// Drop every instance whose last node was freed since the last call:
    /// release its blocks for the next instance of the same size, and free its
    /// cells and edges. Returns how many instances it dropped.
    pub fn collect(&mut self, store: &mut NodeStore, states: &mut StateStore) -> u32 {
        let mut freed = core::mem::take(&mut self.freed);
        store.take_freed_owners(&mut freed);
        let mut dropped = 0;
        for &instance in &freed {
            let life = &mut self.lives[instance as usize];
            life.live -= 1;
            if life.live == 0 {
                self.drop_instance(instance, store, states);
                dropped += 1;
            }
        }
        freed.clear();
        self.freed = freed;
        dropped
    }

    /// Drop instance `instance`, whose nodes are all freed.
    fn drop_instance(&mut self, instance: u32, store: &mut NodeStore, states: &mut StateStore) {
        let life = core::mem::replace(
            &mut self.lives[instance as usize],
            InstanceLife {
                slot: NONE,
                template: NONE,
                owned: NONE,
                ..InstanceLife::default()
            },
        );
        if life.nodes > 0 {
            store.release_block(life.node_base, life.nodes);
        }
        if life.states > 0 {
            states.release_block(life.state_base, life.states);
        }
        for &cell in life.loose.iter().flatten() {
            states.free(cell);
        }
        let mut at = life.owned;
        while at != NONE {
            let next = self.abs[at as usize].owner_next;
            if self.abs[at as usize].linked {
                self.unlink(at);
            }
            self.free_abs(at);
            at = next;
        }
        if life.template != NONE {
            let end = (life.state_base + life.states) as usize;
            if let Some(owners) = self.state_owner.get_mut(life.state_base as usize..end) {
                owners.fill(NONE);
            }
            let data = &mut self.templates[life.template as usize];
            let slot = life.slot as usize;
            data.instances.swap_remove(slot);
            if let Some(&moved) = data.instances.get(slot) {
                self.lives[moved as usize].slot = slot as u32;
            }
            data.users -= 1;
            if data.users == 0 {
                self.drop_template(life.template);
            }
        }
        self.bases[instance as usize] = InstanceBase {
            template: NONE,
            node_base: 0,
            state_base: 0,
        };
        self.instance_free.push(instance);
        self.instances_live -= 1;
    }

    fn live_template(&self, template: &Template) -> Option<u32> {
        let (index, generation) = template.key?;
        let data = self.templates.get(index as usize)?;
        (data.generation == generation && data.users > 0).then_some(index)
    }

    /// A new template holding the canonical edges just computed.
    fn record_template(&mut self, nodes: u32, states: u32) -> u32 {
        let index = match self.template_free.pop() {
            Some(index) => index,
            None => {
                self.templates.push(TemplateData::default());
                self.templates.len() as u32 - 1
            }
        };
        let data = &mut self.templates[index as usize];
        data.nodes = nodes;
        data.states = states;
        core::mem::swap(&mut data.edges, &mut self.canon);
        self.canon.clear();
        for (import, run) in data.edges.imports.iter().enumerate() {
            self.importers
                .entry(run.state.index())
                .or_default()
                .push((index, import as u32));
        }
        index
    }

    fn drop_template(&mut self, index: u32) {
        let data = &mut self.templates[index as usize];
        for run in &data.edges.imports {
            if let Some(users) = self.importers.get_mut(&run.state.index()) {
                users.retain(|&(t, _)| t != index);
                if users.is_empty() {
                    self.importers.remove(&run.state.index());
                }
            }
        }
        data.generation = data.generation.wrapping_add(1);
        data.edges.clear();
        data.instances.clear();
        self.template_free.push(index);
    }

    fn insert_abs(&mut self, state: StateId, node: NodeId, class: DirtyClass, owner: u32) {
        let slot = state.index() as usize;
        if slot >= self.lists.len() {
            self.lists.resize(slot + 1, List::default());
        }
        let tail = self.lists[slot].tail;
        if owner == NONE && tail != NONE {
            let last = &mut self.abs[tail as usize];
            if last.state == state && last.node == node && last.owner == NONE {
                last.class |= class;
                return;
            }
        }
        let edge = AbsEdge {
            state,
            node,
            class,
            prev: tail,
            next: NONE,
            owner_next: NONE,
            owner,
            linked: true,
        };
        let at = if self.abs_free != 0 {
            let at = self.abs_free - 1;
            self.abs_free = self.abs[at as usize].next;
            self.abs[at as usize] = edge;
            at
        } else {
            self.abs.push(edge);
            self.abs.len() as u32 - 1
        };
        match tail {
            NONE => self.lists[slot].head = at,
            tail => self.abs[tail as usize].next = at,
        }
        self.lists[slot].tail = at;
        if owner != NONE {
            let life = &mut self.lives[owner as usize];
            self.abs[at as usize].owner_next = life.owned;
            life.owned = at;
        }
        self.abs_live += 1;
    }

    fn unlink(&mut self, at: u32) {
        let edge = self.abs[at as usize];
        let list = &mut self.lists[edge.state.index() as usize];
        match edge.prev {
            NONE => list.head = edge.next,
            prev => self.abs[prev as usize].next = edge.next,
        }
        match edge.next {
            NONE => list.tail = edge.prev,
            next => self.abs[next as usize].prev = edge.prev,
        }
        self.abs[at as usize].linked = false;
        self.abs_live -= 1;
    }

    fn free_abs(&mut self, at: u32) {
        let edge = &mut self.abs[at as usize];
        edge.linked = false;
        edge.owner = NONE;
        edge.next = self.abs_free;
        self.abs_free = at + 1;
    }
}

/// The consecutive slots a closed scope's allocations fill, as `(base, len)`:
/// its whole block when it filled it and took nothing past it, or its
/// ordinary slots when they happen to ascend one by one. `None` otherwise.
fn block_of(
    scope: ScopeSlots,
    mut loose: impl ExactSizeIterator<Item = u32>,
) -> Option<(u32, u32)> {
    if scope.len > 0 {
        return (scope.filled == scope.len && loose.len() == 0).then_some((scope.base, scope.len));
    }
    let len = loose.len() as u32;
    let Some(first) = loose.next() else {
        return Some((0, 0));
    };
    (first + 1..)
        .zip(loose)
        .all(|(expect, index)| index == expect)
        .then_some((first, len))
}

/// Put an instance's binds in the canonical form of a template edge set,
/// listing in `external` the binds whose target is outside the instance.
fn canonical(
    binds: &[(StateId, NodeId, DirtyClass)],
    (node_base, nodes): (u32, u32),
    (state_base, states): (u32, u32),
    out: &mut Edges,
    external: &mut Vec<u32>,
) {
    out.clear();
    external.clear();
    out.runs.resize(states as usize, Run::default());
    let local = |state: StateId| {
        let at = state.index().wrapping_sub(state_base);
        (at < states).then_some(at)
    };
    let target = |node: NodeId| {
        let at = node.index().wrapping_sub(node_base);
        (at < nodes).then_some(at)
    };
    for (i, &(state, node, _)) in binds.iter().enumerate() {
        if target(node).is_none() {
            external.push(i as u32);
        } else if let Some(at) = local(state) {
            out.runs[at as usize].len += 1;
        } else if !out.imports.iter().any(|run| run.state == state) {
            out.imports.push(ImportRun {
                state,
                run: Run::default(),
            });
        }
    }
    let mut start = 0;
    for run in &mut out.runs {
        run.start = start;
        start += run.len;
        run.len = 0;
    }
    out.local.resize(
        start as usize,
        Edge {
            target: 0,
            class: DirtyClass::EMPTY,
        },
    );
    for &(state, node, class) in binds {
        let (Some(at), Some(target)) = (local(state), target(node)) else {
            continue;
        };
        let run = &mut out.runs[at as usize];
        let edges = &mut out.local[run.start as usize..(run.start + run.len) as usize];
        if let Some(edge) = edges.iter_mut().find(|e| e.target == target) {
            edge.class |= class;
        } else {
            out.local[(run.start + run.len) as usize] = Edge { target, class };
            run.len += 1;
        }
    }
    // Close the gaps folding left.
    let mut write = 0;
    for run in &mut out.runs {
        let read = run.start as usize;
        out.local.copy_within(read..read + run.len as usize, write);
        run.start = write as u32;
        write += run.len as usize;
    }
    out.local.truncate(write);
    for i in 0..out.imports.len() {
        let state = out.imports[i].state;
        let start = out.imported.len() as u32;
        for &(s, node, class) in binds {
            let Some(target) = target(node) else {
                continue;
            };
            if s != state || local(s).is_some() {
                continue;
            }
            let edges = &mut out.imported[start as usize..];
            if let Some(edge) = edges.iter_mut().find(|e| e.target == target) {
                edge.class |= class;
            } else {
                out.imported.push(Edge { target, class });
            }
        }
        out.imports[i].run = Run {
            start,
            len: out.imported.len() as u32 - start,
        };
    }
}

/// Insert `(node, class)` into the dynamic run owned by dense state index
/// `idx`, folding into an existing edge to the same node, else appending at
/// the end of the run and shifting every later run right by one.
fn insert_dynamic(
    edges: &mut Vec<Binding>,
    runs: &mut [Run],
    idx: usize,
    node: NodeId,
    class: DirtyClass,
) {
    // An empty run has no meaningful start yet — its first edge appends at
    // the end of the highest existing run below it.
    if runs[idx].len == 0 {
        let anchor = runs[..idx]
            .iter()
            .filter(|r| r.len > 0)
            .map(|r| r.start + r.len)
            .max()
            .unwrap_or(0);
        runs[idx].start = anchor;
    }
    let run = runs[idx];
    let start = run.start as usize;
    let end = start + run.len as usize;
    for edge in &mut edges[start..end] {
        if edge.node == node {
            edge.class |= class;
            return;
        }
    }
    edges.insert(end, Binding { node, class });
    runs[idx].len += 1;
    for later in &mut runs[idx + 1..] {
        if later.len > 0 && later.start as usize >= end {
            later.start += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::NodeArena;
    use crate::state::{StateStore, StateValue};

    // The binding table never dereferences its ids; it only groups by
    // `StateId::index()` and hands back `NodeId`s, so allocating real ids from
    // the stores is enough to exercise it.
    fn state(store: &mut StateStore) -> StateId {
        store.alloc(StateValue::Int(0))
    }
    fn node(arena: &mut NodeArena) -> NodeId {
        arena.alloc()
    }

    #[test]
    fn retain_static_drops_the_rejected_nodes_edges() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();
        let (s, t) = (state(&mut states), state(&mut states));
        let (a, b) = (node(&mut arena), node(&mut arena));
        table.bind(s, a, DirtyClass::PAINT);
        table.bind(s, b, DirtyClass::PAINT);
        table.bind(t, b, DirtyClass::LAYOUT);
        table.bind(t, a, DirtyClass::LAYOUT);
        table.retain_static(|n| n != b);
        assert_eq!(
            table.for_state(s).collect::<Vec<_>>(),
            [Binding {
                node: a,
                class: DirtyClass::PAINT
            }]
        );
        assert_eq!(
            table.for_state(t).collect::<Vec<_>>(),
            [Binding {
                node: a,
                class: DirtyClass::LAYOUT
            }]
        );
        table.bind(s, b, DirtyClass::PAINT);
        assert_eq!(table.for_state(s).collect::<Vec<_>>().len(), 2);
        assert_eq!(table.for_state(t).collect::<Vec<_>>().len(), 1);
    }

    #[test]
    fn bind_and_lookup_single_state() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        let b = node(&mut arena);

        table.bind(s, a, DirtyClass::MEASURE | DirtyClass::LAYOUT);
        table.bind(s, b, DirtyClass::PAINT);

        let edges = table.for_state(s).collect::<Vec<_>>();
        assert_eq!(edges.len(), 2);
        assert_eq!(edges[0].node, a);
        assert!(
            edges[0]
                .class
                .contains(DirtyClass::MEASURE | DirtyClass::LAYOUT)
        );
        assert_eq!(edges[1].node, b);
        assert!(edges[1].class.contains(DirtyClass::PAINT));
    }

    #[test]
    fn repeated_bind_same_node_folds_class() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        table.bind(s, a, DirtyClass::MEASURE);
        table.bind(s, a, DirtyClass::PAINT);

        let edges = table.for_state(s).collect::<Vec<_>>();
        assert_eq!(edges.len(), 1, "same node stays one edge");
        assert!(
            edges[0]
                .class
                .contains(DirtyClass::MEASURE | DirtyClass::PAINT)
        );
    }

    #[test]
    fn multiple_states_keep_contiguous_runs() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s0 = state(&mut states);
        let s1 = state(&mut states);
        let a = node(&mut arena);
        let b = node(&mut arena);
        let c = node(&mut arena);

        // Interleave registration order to prove grouping is by state, not order.
        table.bind(s0, a, DirtyClass::PAINT);
        table.bind(s1, b, DirtyClass::LAYOUT);
        table.bind(s0, c, DirtyClass::PAINT);

        let e0 = table.for_state(s0).collect::<Vec<_>>();
        assert_eq!(e0.len(), 2);
        assert_eq!(e0[0].node, a);
        assert_eq!(e0[1].node, c);

        let e1 = table.for_state(s1).collect::<Vec<_>>();
        assert_eq!(e1.len(), 1);
        assert_eq!(e1[0].node, b);
    }

    #[test]
    fn unbound_state_has_no_edges() {
        let mut states = StateStore::new();
        let table = BindingTable::new();
        let s = state(&mut states);
        assert!(table.for_state(s).collect::<Vec<_>>().is_empty());
    }

    #[test]
    fn bind_dynamic_counts_subscribe_and_fallback_nodes() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        let b = node(&mut arena);

        table.bind_dynamic(s, a, DirtyClass::PAINT);
        // A second edge to a *new* node is a fresh fallback node.
        table.bind_dynamic(s, b, DirtyClass::LAYOUT);
        // Re-binding an existing node folds its class — a subscription, not a
        // new fallback node.
        table.bind_dynamic(s, a, DirtyClass::MEASURE);

        let c = table.counters();
        assert_eq!(c.dynamic_subscribe(), 3, "one per bind_dynamic call");
        assert_eq!(c.dynamic_fallback_nodes(), 2, "two distinct dynamic nodes");
    }

    #[test]
    fn counters_reset_zeroes_all() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        table.bind_dynamic(s, a, DirtyClass::PAINT);
        table.counters().record_static_eval(4);
        table.counters().record_dynamic_eval(2);

        table.counters().reset();
        let c = table.counters();
        assert_eq!(c.static_binding_eval(), 0);
        assert_eq!(c.dynamic_binding_eval(), 0);
        assert_eq!(c.dynamic_subscribe(), 0);
        assert_eq!(c.dynamic_fallback_nodes(), 0);
    }

    #[test]
    fn record_eval_accumulates_each_path_separately() {
        let table = BindingTable::new();
        let c = table.counters();
        c.record_static_eval(3);
        c.record_static_eval(2);
        c.record_dynamic_eval(1);
        assert_eq!(c.static_binding_eval(), 5, "static edges accumulate");
        assert_eq!(c.dynamic_binding_eval(), 1, "counted apart from static");
    }

    #[test]
    fn dynamic_region_is_separate() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        let b = node(&mut arena);
        table.bind(s, a, DirtyClass::PAINT);
        table.bind_dynamic(s, b, DirtyClass::LAYOUT);

        assert_eq!(table.for_state(s).collect::<Vec<_>>().len(), 1);
        assert_eq!(table.for_state(s).collect::<Vec<_>>()[0].node, a);
        assert_eq!(table.dynamic_for_state(s).len(), 1);
        assert_eq!(table.dynamic_for_state(s)[0].node, b);
    }

    #[test]
    fn rebuild_static_replaces_edges_and_keeps_dynamic() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let old = node(&mut arena);
        let dynamic_node = node(&mut arena);
        table.bind(s, old, DirtyClass::PAINT);
        table.bind_dynamic(s, dynamic_node, DirtyClass::LAYOUT);

        // A reload installs a fresh compiled edge set for the same state.
        let fresh = node(&mut arena);
        table.rebuild_static([(
            s,
            Binding {
                node: fresh,
                class: DirtyClass::MEASURE | DirtyClass::LAYOUT,
            },
        )]);

        let edges = table.for_state(s).collect::<Vec<_>>();
        assert_eq!(edges.len(), 1, "old static edge dropped");
        assert_eq!(edges[0].node, fresh);
        assert!(
            edges[0]
                .class
                .contains(DirtyClass::MEASURE | DirtyClass::LAYOUT)
        );
        // Dynamic region is untouched by a static rebuild.
        assert_eq!(table.dynamic_for_state(s).len(), 1);
        assert_eq!(table.dynamic_for_state(s)[0].node, dynamic_node);
    }

    #[test]
    fn clear_static_leaves_no_edges() {
        let mut states = StateStore::new();
        let mut arena = NodeArena::new();
        let mut table = BindingTable::new();

        let s = state(&mut states);
        let a = node(&mut arena);
        table.bind(s, a, DirtyClass::PAINT);
        table.clear_static();
        assert!(table.for_state(s).collect::<Vec<_>>().is_empty());
    }
}

#[cfg(test)]
mod instance_tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle};
    use crate::reactive::EffectStore;
    use crate::state::StateValue;

    struct World {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        root: NodeId,
        shared: StateId,
    }

    impl World {
        fn new() -> World {
            let mut store = NodeStore::new();
            let root = BuildCx::new(&mut store)
                .flex(FlexStyle::default(), |_| {})
                .id();
            let mut states = StateStore::new();
            let shared = states.alloc(StateValue::Int(0));
            World {
                store,
                states,
                bindings: BindingTable::new(),
                root,
                shared,
            }
        }

        /// One row: a local count bound to its label, the shared selection
        /// bound to its frame. Returns `(count, frame, label)`.
        fn row(&mut self, template: &mut Template) -> (StateId, NodeId, NodeId) {
            let shared = self.shared;
            let mut cx = BuildCx::with_parent(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                self.root,
            );
            cx.instance(template, |cx| {
                let count = cx.state(StateValue::Int(0));
                let mut label = None;
                let frame = cx.flex(FlexStyle::default(), |cx| {
                    label = Some(cx.leaf(LeafStyle::default()));
                });
                let label = label.unwrap();
                cx.bind(count, label, DirtyClass::PAINT);
                cx.bind(shared, frame, DirtyClass::LAYOUT);
                (count, frame.id(), label.id())
            })
        }

        fn flush(&mut self, state: StateId) {
            self.store.clear_dirty();
            self.states.set(
                state,
                StateValue::Int(
                    1 + self.states.get(state).map_or(0, |v| match v {
                        StateValue::Int(n) => n,
                        _ => 0,
                    }),
                ),
            );
            let mut pending = Vec::new();
            self.states.take_pending(&mut pending);
            self.store
                .flush_state_transactions(&pending, &self.bindings);
        }

        fn free(&mut self, node: NodeId) {
            self.store
                .free_tree(node, &mut EffectStore::default(), &mut Vec::new());
            self.bindings.collect(&mut self.store, &mut self.states);
        }
    }

    #[test]
    fn instances_of_one_template_add_only_their_base() {
        let mut w = World::new();
        let mut template = Template::new();
        w.row(&mut template);
        let one = w.bindings.footprint();
        assert_eq!(one.templates, 1);
        assert_eq!(one.template_edges, 2);
        assert_eq!(one.absolute_edges, 0);
        for _ in 0..99 {
            w.row(&mut template);
        }
        let hundred = w.bindings.footprint();
        assert_eq!(hundred.templates, 1);
        assert_eq!(hundred.template_edges, 2, "the edges are stored once");
        assert_eq!(hundred.absolute_edges, 0, "no row keeps an edge of its own");
        assert_eq!(hundred.instances, 100);
        assert_eq!(core::mem::size_of::<InstanceBase>(), 12);
        assert_eq!(
            w.bindings.template_bindings(&template).unwrap(),
            [
                TemplateBinding {
                    state: StateRef::Local(0),
                    target: 1,
                    dirty: DirtyClass::PAINT,
                },
                TemplateBinding {
                    state: StateRef::Import(w.shared),
                    target: 0,
                    dirty: DirtyClass::LAYOUT,
                },
            ]
        );
    }

    #[test]
    fn a_local_change_marks_its_own_row_and_a_shared_one_every_row() {
        let mut w = World::new();
        let mut template = Template::new();
        let rows: Vec<_> = (0..3).map(|_| w.row(&mut template)).collect();

        w.flush(rows[1].0);
        for (i, &(_, frame, label)) in rows.iter().enumerate() {
            assert_eq!(w.store.dirty(label).contains(DirtyClass::PAINT), i == 1);
            assert!(!w.store.dirty(frame).intersects(DirtyClass::LAYOUT));
        }

        w.flush(w.shared);
        for &(_, frame, label) in &rows {
            assert!(w.store.dirty(frame).contains(DirtyClass::LAYOUT));
            assert!(!w.store.dirty(label).intersects(DirtyClass::PAINT));
        }
    }

    #[test]
    fn a_row_that_binds_differently_keeps_its_own_edges() {
        let mut w = World::new();
        let mut template = Template::new();
        w.row(&mut template);
        let shared = w.shared;
        let odd = {
            let mut cx = BuildCx::with_parent(&mut w.store, &mut w.states, &mut w.bindings, w.root);
            cx.instance(&mut template, |cx| {
                let count = cx.state(StateValue::Int(0));
                let frame = cx.flex(FlexStyle::default(), |cx| {
                    cx.leaf(LeafStyle::default());
                });
                cx.bind(count, frame, DirtyClass::STYLE);
                cx.bind(shared, frame, DirtyClass::LAYOUT);
                (count, frame.id())
            })
        };
        let footprint = w.bindings.footprint();
        assert_eq!(footprint.template_edges, 2);
        assert_eq!(footprint.absolute_edges, 2);
        w.flush(odd.0);
        assert!(w.store.dirty(odd.1).contains(DirtyClass::STYLE));
    }

    #[test]
    fn a_freed_row_frees_its_states_and_edges_and_the_next_takes_its_blocks() {
        let mut w = World::new();
        let mut template = Template::new();
        let first = w.row(&mut template);
        let second = w.row(&mut template);
        let slots = (w.store.arena().slot_count(), w.states.slot_count());

        w.free(second.1);
        assert!(!w.states.is_live(second.0), "the row's state went with it");
        assert_eq!(w.bindings.footprint().instances, 1);
        let third = w.row(&mut template);
        assert_eq!(
            (w.store.arena().slot_count(), w.states.slot_count()),
            slots,
            "the new row reused the freed row's blocks"
        );
        assert_eq!(third.1.index(), second.1.index());

        let mut current = third;
        for _ in 0..50 {
            w.free(current.1);
            current = w.row(&mut template);
            assert_eq!(current.1.index(), third.1.index());
        }
        assert_eq!(
            (w.store.arena().slot_count(), w.states.slot_count()),
            slots,
            "recycling rows grows nothing"
        );
        w.flush(first.0);
        assert!(w.store.dirty(first.2).contains(DirtyClass::PAINT));
    }

    #[test]
    fn the_last_row_dropping_drops_the_template() {
        let mut w = World::new();
        let mut template = Template::new();
        let row = w.row(&mut template);
        w.free(row.1);
        let footprint = w.bindings.footprint();
        assert_eq!((footprint.templates, footprint.instances), (0, 0));
        assert!(w.bindings.template_bindings(&template).is_none());
        let again = w.row(&mut template);
        assert_eq!(w.bindings.footprint().templates, 1, "recorded afresh");
        w.flush(again.0);
        assert!(w.store.dirty(again.2).contains(DirtyClass::PAINT));
    }

    #[test]
    fn a_freed_node_of_a_live_row_keeps_its_slot_from_strangers() {
        let mut w = World::new();
        let mut template = Template::new();
        w.row(&mut template);
        let row = w.row(&mut template);
        w.store
            .free_tree(row.2, &mut EffectStore::default(), &mut Vec::new());
        w.bindings.collect(&mut w.store, &mut w.states);
        let stranger = BuildCx::new(&mut w.store)
            .flex(FlexStyle::default(), |_| {})
            .id();
        assert_ne!(stranger.index(), row.2.index());
        w.flush(row.0);
        assert!(!w.store.dirty(stranger).intersects(DirtyClass::PAINT));
    }

    #[test]
    fn an_instance_without_nodes_leaves_ordinary_cells() {
        let mut w = World::new();
        let mut template = Template::new();
        let cell = {
            let mut cx = BuildCx::with_parent(&mut w.store, &mut w.states, &mut w.bindings, w.root);
            cx.instance(&mut template, |cx| cx.state(StateValue::Int(3)))
        };
        assert_eq!(w.states.get(cell), Some(StateValue::Int(3)));
        assert_eq!(w.bindings.footprint().instances, 0);
    }
}
