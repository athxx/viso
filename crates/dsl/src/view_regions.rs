//! A view's control-flow regions as every target mounts them: the static nodes
//! the targets author directly, and the [`ViewRegions`] templates the runtime
//! mounts, switches and reorders under them.
//!
//! The Binding IR numbers every node of a view in one pre-order, the nodes of
//! every region arm included. A target authors only the *static* nodes — those
//! outside every region — and names them by their pre-order among themselves,
//! the index [`StaticNodes`] maps a [`NodeKey`] to. Everything a region mounts
//! is a template: its nodes, their state edges and handler routes, the
//! handler-table entries that decide what it mounts, and the states of the
//! component instances each mount of an arm keeps.

use std::collections::{HashMap, HashSet};

use viso_behavior::ComponentEffect;
use viso_ui::state::StateKey;
use viso_view::{
    ArmTemplate, CellRef, Control, EffectTemplate, EnvTemplate, GroupTemplate, ItemTemplate,
    LocalTemplate, RegionKind, RegionTemplate, Route, SlotTemplate, StarterTemplate, ViewRegions,
};

use crate::aot::{aot_kind, aot_style};
use crate::behavior::ir::{ComponentLayout, FuncId, Inst, Program, Site};
use crate::frontend::{Compiled, SourceKind};
use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{NodeKind, UiItem, UiNode, UiTree};
use crate::resolve::SymbolId;
use crate::syntax::TextRange;
use crate::view_behavior::MountError;

/// Each node's static pre-order index, by [`NodeKey`]: `None` for a node a
/// region mounts, or one under a node that authors no children (a leaf, a
/// `VirtualList`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StaticNodes {
    ordinals: Vec<Option<u32>>,
    count: u32,
}

impl StaticNodes {
    /// The static numbering of `tree`.
    pub fn of(tree: &UiTree) -> Self {
        let mut nodes = StaticNodes::default();
        for item in &tree.items {
            nodes.walk(item, true);
        }
        nodes
    }

    /// The static index of the node `key`, `None` for one no target authors
    /// directly.
    pub fn ordinal(&self, key: NodeKey) -> Option<u32> {
        self.ordinals.get(key.0 as usize).copied().flatten()
    }

    /// The number of static nodes.
    pub fn len(&self) -> usize {
        self.count as usize
    }

    /// Whether the view has no static node.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn walk(&mut self, item: &UiItem, authored: bool) {
        match item {
            UiItem::Node(node) => {
                if authored {
                    self.ordinals.push(Some(self.count));
                    self.count += 1;
                } else {
                    self.ordinals.push(None);
                }
                let authored = authored && authors_children(node);
                for child in &node.children {
                    self.walk(child, authored);
                }
            }
            _ => {
                for items in arms(item) {
                    for item in items {
                        self.walk(item, false);
                    }
                }
            }
        }
    }
}

/// The [`NodeKey`] of every node a region of `tree` mounts, by the region,
/// arm and arm item [`view_regions`] lowers it to, ascending.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionKeys {
    keys: Vec<((u32, u32, u32), NodeKey)>,
}

impl RegionKeys {
    /// The region numbering of `tree`.
    pub fn of(tree: &UiTree) -> Self {
        let mut walk = RegionWalk::default();
        for item in &tree.items {
            walk.item(item);
        }
        walk.out.keys.sort_unstable_by_key(|&(at, _)| at);
        walk.out
    }

    /// The key of the node item `item` of arm `arm` of region `region` mounts.
    pub fn key(&self, region: u32, arm: u32, item: u32) -> Option<NodeKey> {
        self.keys
            .binary_search_by_key(&(region, arm, item), |&(at, _)| at)
            .ok()
            .map(|index| self.keys[index].1)
    }
}

/// The [`Builder`] walk's counters alone: the next key and region index.
#[derive(Default)]
struct RegionWalk {
    key: u32,
    regions: u32,
    out: RegionKeys,
}

impl RegionWalk {
    fn item(&mut self, item: &UiItem) {
        let UiItem::Node(node) = item else {
            self.region(item);
            return;
        };
        self.key += 1;
        if authors_children(node) {
            for child in &node.children {
                self.item(child);
            }
        } else {
            self.skip(&node.children);
        }
    }

    fn skip(&mut self, items: &[UiItem]) {
        for item in items {
            match item {
                UiItem::Node(node) => {
                    self.key += 1;
                    self.skip(&node.children);
                }
                _ => {
                    for items in arms(item) {
                        self.skip(items);
                    }
                }
            }
        }
    }

    fn region(&mut self, item: &UiItem) {
        let region = self.regions;
        self.regions += 1;
        for (arm, items) in arms(item).into_iter().enumerate() {
            let mut at = 0;
            for item in items {
                self.content(item, region, arm as u32, &mut at);
            }
        }
    }

    fn content(&mut self, item: &UiItem, region: u32, arm: u32, at: &mut u32) {
        let UiItem::Node(node) = item else {
            self.region(item);
            *at += 1;
            return;
        };
        self.out.keys.push(((region, arm, *at), NodeKey(self.key)));
        self.key += 1;
        *at += 1;
        if authors_children(node) {
            for child in &node.children {
                self.content(child, region, arm, at);
            }
        } else {
            self.skip(&node.children);
        }
    }
}

/// Whether `tree` has a control-flow region.
pub fn has_regions(tree: &UiTree) -> bool {
    fn any(items: &[UiItem]) -> bool {
        items.iter().any(|item| match item {
            UiItem::Node(node) => any(&node.children),
            _ => true,
        })
    }
    any(&tree.items)
}

/// The structural errors of `tree` no target mounts: a region at the root,
/// and anything under a `VirtualList`, whose items are not mounted from a view
/// yet.
pub fn structure_errors(tree: &UiTree) -> Vec<MountError> {
    fn walk(item: &UiItem, errors: &mut Vec<MountError>) {
        match item {
            UiItem::Node(node) => {
                if node.kind == NodeKind::VirtualList
                    && let Some(child) = node.children.first()
                {
                    errors.push(MountError::new(
                        Some(origin(child)),
                        "a `VirtualList` does not mount content from a view yet",
                    ));
                    return;
                }
                for child in &node.children {
                    walk(child, errors);
                }
            }
            _ => {
                for items in arms(item) {
                    for item in items {
                        walk(item, errors);
                    }
                }
            }
        }
    }
    let mut errors = Vec::new();
    for item in &tree.items {
        if !matches!(item, UiItem::Node(_)) {
            errors.push(MountError::new(
                Some(origin(item)),
                "a view's root must be a node, not a control-flow region",
            ));
        }
        walk(item, &mut errors);
    }
    errors
}

/// Every region of `compiled`'s view as the runtime mounts it, its entries
/// looked up in `layout` and its nodes' handlers in `routes`.
///
/// # Errors
///
/// Every region entry that does not run.
pub(crate) fn view_regions(
    compiled: &Compiled,
    layout: &ComponentLayout,
    routes: &[(NodeKey, Vec<Route>)],
    controls: &[(NodeKey, Control)],
) -> Result<ViewRegions, Vec<MountError>> {
    let mut edges: HashMap<NodeKey, Vec<(SymbolId, u8)>> = HashMap::new();
    for edge in compiled.bindings.static_edges() {
        edges
            .entry(edge.node)
            .or_default()
            .push((edge.source, edge.class.bits()));
    }
    let slots = layout
        .states
        .iter()
        .map(|name| {
            compiled
                .sources
                .iter()
                .find(|s| matches!(s.kind, SourceKind::State { .. }) && s.name == *name)
                .map(|s| s.symbol)
        })
        .collect();
    let regional = compiled
        .regional
        .iter()
        .filter_map(|s| {
            Some((
                s.symbol,
                layout.states.iter().position(|n| *n == s.name)? as u32,
            ))
        })
        .collect();
    let mut builder = Builder {
        program: &compiled.behavior,
        layout,
        routes,
        controls,
        edges,
        slots,
        regional,
        claimed: HashSet::new(),
        cells: HashMap::new(),
        out: ViewRegions::default(),
        errors: Vec::new(),
        key: 0,
        ordinal: 0,
    };
    for item in &compiled.tree.items {
        builder.item(item, None);
    }
    if builder.errors.is_empty() {
        Ok(builder.out)
    } else {
        Err(builder.errors)
    }
}

/// The region walk: the pre-order counters, the templates so far and the
/// errors.
struct Builder<'a> {
    program: &'a Program,
    layout: &'a ComponentLayout,
    routes: &'a [(NodeKey, Vec<Route>)],
    controls: &'a [(NodeKey, Control)],
    edges: HashMap<NodeKey, Vec<(SymbolId, u8)>>,
    /// The state source of each component state slot.
    slots: Vec<Option<SymbolId>>,
    /// The slot of each state source of an instance a region mounts.
    regional: HashMap<SymbolId, u32>,
    /// The instances whose states an arm already keeps.
    claimed: HashSet<u32>,
    /// Each state's index in [`ViewRegions::states`].
    cells: HashMap<SymbolId, u32>,
    out: ViewRegions,
    errors: Vec<MountError>,
    /// The next [`NodeKey`].
    key: u32,
    /// The next static index.
    ordinal: u32,
}

impl Builder<'_> {
    /// Walks a static item; `slots` collects the children of the enclosing
    /// static node when it has a region among them.
    fn item(&mut self, item: &UiItem, slots: Option<&mut Vec<SlotTemplate>>) {
        let UiItem::Node(node) = item else {
            let region = self.region(item);
            if let Some(slots) = slots {
                slots.push(SlotTemplate::Region(region));
            }
            return;
        };
        let ordinal = self.ordinal;
        self.ordinal += 1;
        self.key += 1;
        if let Some(slots) = slots {
            slots.push(SlotTemplate::Node(ordinal));
        }
        if !authors_children(node) {
            self.skip(&node.children);
            return;
        }
        if node.children.iter().all(|c| matches!(c, UiItem::Node(_))) {
            for child in &node.children {
                self.item(child, None);
            }
            return;
        }
        let mut children = Vec::with_capacity(node.children.len());
        for child in &node.children {
            self.item(child, Some(&mut children));
        }
        self.out.groups.push(GroupTemplate {
            parent: ordinal,
            slots: children,
        });
    }

    /// Advances the counters past items no target authors.
    fn skip(&mut self, items: &[UiItem]) {
        for item in items {
            match item {
                UiItem::Node(node) => {
                    self.key += 1;
                    self.skip(&node.children);
                }
                _ => {
                    for items in arms(item) {
                        self.skip(items);
                    }
                }
            }
        }
    }

    /// Lowers the region `item` and the regions nested in it, and returns its
    /// index.
    fn region(&mut self, item: &UiItem) -> u32 {
        let index = self.out.regions.len() as u32;
        let (kind, entries, preserves) = match item {
            UiItem::If(region) => {
                let select = self.entry(region.instance, Some(region.origin), region.origin);
                (
                    RegionKind::If { select },
                    vec![select],
                    region
                        .arms
                        .iter()
                        .map(|arm| arm.preserve.is_some())
                        .collect(),
                )
            }
            UiItem::Match(region) => {
                let scrutinee = self.entry(region.instance, region.scrutinee, region.origin);
                let select = self.entry(region.instance, Some(region.origin), region.origin);
                (
                    RegionKind::Match { scrutinee, select },
                    vec![scrutinee, select],
                    vec![false; region.arms.len()],
                )
            }
            UiItem::For(region) => {
                let items = self.entry(region.instance, region.iterable, region.origin);
                let key = region
                    .key
                    .map(|key| self.entry(region.instance, Some(key), region.origin));
                (
                    RegionKind::For { items, key },
                    [items].into_iter().chain(key).collect(),
                    vec![false],
                )
            }
            UiItem::Node(_) => unreachable!("a node is not a region"),
        };
        let deps = self.deps(&entries);
        self.out.regions.push(RegionTemplate {
            kind,
            deps,
            arms: Vec::new(),
        });
        let mut arms_out = Vec::with_capacity(preserves.len());
        for (items, preserve) in arms(item).into_iter().zip(preserves) {
            let mut out = Vec::new();
            let mut locals = Vec::new();
            let mut env = Vec::new();
            let mut effects = Vec::new();
            let mut starters = Vec::new();
            for item in items {
                self.content(
                    item,
                    &mut out,
                    &mut locals,
                    &mut env,
                    &mut effects,
                    &mut starters,
                );
            }
            locals.sort_unstable_by_key(|local: &LocalTemplate| local.slot);
            env.sort_unstable_by_key(|env: &EnvTemplate| env.slot);
            arms_out.push(ArmTemplate {
                preserve,
                locals,
                env,
                effects,
                starters,
                items: out,
            });
        }
        self.out.regions[index as usize].arms = arms_out;
        index
    }

    /// Flattens one item of a region arm into `out`, in pre-order, and
    /// collects into `locals` the states of the instances whose view first
    /// appears there, into `env` the `env` fields they read, anchored at the
    /// instance's root, into `effects` their effects, mounted on it, and into
    /// `starters` those that start tasks, owned by it.
    fn content(
        &mut self,
        item: &UiItem,
        out: &mut Vec<ItemTemplate>,
        locals: &mut Vec<LocalTemplate>,
        env: &mut Vec<EnvTemplate>,
        effects: &mut Vec<EffectTemplate>,
        starters: &mut Vec<StarterTemplate>,
    ) {
        let UiItem::Node(node) = item else {
            let region = self.region(item);
            out.push(ItemTemplate::Region(region));
            return;
        };
        if node.instance != 0 && self.claimed.insert(node.instance) {
            let anchor = out.len() as u32;
            if self.layout.starters.contains(&node.instance) {
                starters.push(StarterTemplate {
                    instance: node.instance,
                    anchor,
                });
            }
            effects.extend(
                self.layout
                    .effects
                    .iter()
                    .filter(|effect| effect.instance == node.instance)
                    .map(|effect| EffectTemplate {
                        effect: ComponentEffect {
                            deps: effect.deps,
                            body: effect.body,
                            run: effect.run,
                            resource: effect.resource,
                        },
                        anchor,
                    }),
            );
            let states = self
                .layout
                .regional
                .iter()
                .find(|r| r.instance == node.instance);
            if let Some(states) = states {
                let anchor = out.len() as u32;
                for (at, &init) in states.inits.iter().enumerate() {
                    let slot = states.base + at as u32;
                    match self.layout.env.iter().find(|env| env.slot == slot) {
                        Some(read) => env.push(EnvTemplate {
                            slot,
                            field: read.field,
                            anchor,
                        }),
                        None => locals.push(LocalTemplate { slot, init }),
                    }
                }
            }
        }
        let key = NodeKey(self.key);
        self.key += 1;
        let edges = self
            .edges
            .remove(&key)
            .unwrap_or_default()
            .into_iter()
            .map(|(symbol, bits)| {
                let cell = match self.regional.get(&symbol) {
                    Some(&slot) => CellRef::Local(slot),
                    None => CellRef::Shared(self.cell(symbol)),
                };
                (cell, bits)
            })
            .collect();
        let routes = self
            .routes
            .binary_search_by_key(&key, |(k, _)| *k)
            .map_or_else(|_| Vec::new(), |i| self.routes[i].1.clone());
        let control = self
            .controls
            .binary_search_by_key(&key, |(k, _)| *k)
            .ok()
            .map(|i| self.controls[i].1.clone());
        let at = out.len();
        out.push(ItemTemplate::Node {
            node: node_template(node),
            edges,
            routes,
            control,
        });
        if !authors_children(node) {
            self.skip(&node.children);
            return;
        }
        for child in &node.children {
            self.content(child, out, locals, env, effects, starters);
        }
        if let ItemTemplate::Node { node: template, .. } = &mut out[at] {
            template.child_count = node.children.len() as u32;
        }
    }

    /// The handler-table index of the entry registered at `at` in the view of
    /// `instance`, reported at `origin` when it is missing or does not run.
    fn entry(&mut self, instance: u32, at: Option<TextRange>, origin: TextRange) -> u32 {
        let Some(index) = at.and_then(|at| {
            self.layout.handler(Site {
                instance,
                at,
                part: 0,
            })
        }) else {
            self.errors.push(MountError::new(
                Some(at.unwrap_or(origin)),
                "internal: the region entry was not lowered",
            ));
            return 0;
        };
        let function = self
            .program
            .function(self.layout.handlers[index as usize].1);
        if let Err(unsupported) = &function.body {
            self.errors.push(MountError::new(
                Some(unsupported.at),
                format!("the region {}", unsupported.reason),
            ));
        }
        index
    }

    /// The cells `entries` read, ascending: every state their bodies, and the
    /// functions and closures those call, load.
    fn deps(&mut self, entries: &[u32]) -> Vec<CellRef> {
        let mut seen: HashSet<FuncId> = HashSet::new();
        let mut stack: Vec<FuncId> = entries
            .iter()
            .filter_map(|&index| self.layout.handlers.get(index as usize).map(|h| h.1))
            .collect();
        let mut slots: Vec<u32> = Vec::new();
        while let Some(func) = stack.pop() {
            if !seen.insert(func) {
                continue;
            }
            let Ok(body) = &self.program.function(func).body else {
                continue;
            };
            for inst in &body.insts {
                match inst {
                    Inst::LoadState { slot, .. } => slots.push(*slot),
                    Inst::Call { func, .. } | Inst::Closure { func, .. } => stack.push(*func),
                    _ => {}
                }
            }
        }
        let local: HashSet<u32> = self.regional.values().copied().collect();
        let mut deps: Vec<CellRef> = Vec::with_capacity(slots.len());
        for slot in slots {
            if local.contains(&slot) || self.layout.env.iter().any(|env| env.slot == slot) {
                deps.push(CellRef::Local(slot));
            } else if let Some(symbol) = self.slots.get(slot as usize).copied().flatten() {
                deps.push(CellRef::Shared(self.cell(symbol)));
            }
        }
        deps.sort_unstable_by_key(|cell| match *cell {
            CellRef::Shared(index) => (0, index),
            CellRef::Local(slot) => (1, slot),
        });
        deps.dedup();
        deps
    }

    /// The index of `symbol`'s cell in [`ViewRegions::states`].
    fn cell(&mut self, symbol: SymbolId) -> u32 {
        *self.cells.entry(symbol).or_insert_with(|| {
            self.out
                .states
                .push(StateKey::from_parts(symbol.hi, symbol.lo));
            self.out.states.len() as u32 - 1
        })
    }
}

/// The packaged form of a region node, its child count still to fill in.
fn node_template(node: &UiNode) -> viso_ui::aot::AotNode {
    viso_ui::aot::AotNode {
        kind: aot_kind(node.kind),
        style: aot_style(&node.style),
        child_count: 0,
    }
}

/// Whether a target authors `node`'s children: a container does, except a
/// `VirtualList`, which mounts its items itself.
fn authors_children(node: &UiNode) -> bool {
    node.kind.is_container() && node.kind != NodeKind::VirtualList
}

/// The item lists of a region's arms (a `for` has one, its body); none for a
/// node.
fn arms(item: &UiItem) -> Vec<&[UiItem]> {
    match item {
        UiItem::Node(_) => Vec::new(),
        UiItem::If(region) => region.arms.iter().map(|arm| arm.items.as_slice()).collect(),
        UiItem::For(region) => vec![region.body.as_slice()],
        UiItem::Match(region) => region.arms.iter().map(|arm| arm.items.as_slice()).collect(),
    }
}

/// Where the author wrote `item`.
fn origin(item: &UiItem) -> TextRange {
    match item {
        UiItem::Node(node) => node.origin,
        UiItem::If(region) => region.origin,
        UiItem::For(region) => region.origin,
        UiItem::Match(region) => region.origin,
    }
}
