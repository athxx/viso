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
use viso_schema::StateKey;
use viso_schema::view::{
    ArmTemplate, CellRef, Control, EffectTemplate, EnvTemplate, GroupTemplate, ItemTemplate,
    ListLanes, LocalTemplate, MOUNT, RegionKind, RegionTemplate, Route, SlotTemplate,
    StarterTemplate, ViewRegions,
};

use crate::aot::{aot_node, terms_of};
use crate::behavior::ir::{ComponentLayout, FuncId, Inst, Program, Site};
use crate::frontend::{Compiled, SourceKind};
use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{NodeKind, UiItem, UiNode, UiTree};
use crate::resolve::SymbolId;
use crate::syntax::TextRange;
use crate::view_behavior::MountError;

/// Each node's static pre-order index, by [`NodeKey`]: `None` for a node a
/// region mounts, or one under a node that authors no children (a leaf).
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

    /// Each node a region mounts, by region, arm and arm item, ascending.
    pub fn iter(&self) -> impl Iterator<Item = ((u32, u32, u32), NodeKey)> + '_ {
        self.keys.iter().copied()
    }

    /// The region, arm and arm item that mounts the node `key`, `None` for a
    /// node no region mounts. Linear: a reload plans it once per kept node.
    pub fn item(&self, key: NodeKey) -> Option<(u32, u32, u32)> {
        self.keys
            .iter()
            .find(|&&(_, k)| k == key)
            .map(|&(at, _)| at)
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
/// a `VirtualList` whose body is not its one keyed `for`, and an `Island`
/// whose content is not one component, or that is in another.
pub fn structure_errors(tree: &UiTree) -> Vec<MountError> {
    fn walk(tree: &UiTree, item: &UiItem, island: bool, errors: &mut Vec<MountError>) {
        match item {
            UiItem::Node(node) => {
                let is_island = node.type_name == ISLAND;
                if is_island && island {
                    errors.push(MountError::new(
                        Some(tree.own_span(node.instance, node.origin)),
                        "an `Island` is not in another: its content is all the renderer draws",
                    ));
                    return;
                }
                if is_island && island_component(tree, node).is_none() {
                    errors.push(MountError::new(
                        Some(tree.own_span(node.instance, node.origin)),
                        "an `Island`'s content is exactly one component of the view's file, whose inputs and events are all that cross into it",
                    ));
                    return;
                }
                if node.kind == NodeKind::VirtualList
                    && !matches!(node.children.as_slice(), [UiItem::For(f)] if f.key.is_some())
                {
                    errors.push(MountError::new(
                        Some(tree.own_span(node.instance, node.origin)),
                        "a `VirtualList`'s body is exactly one keyed `for`, its item template",
                    ));
                    return;
                }
                for child in &node.children {
                    walk(tree, child, island || is_island, errors);
                }
            }
            _ => {
                for items in arms(item) {
                    for item in items {
                        walk(tree, item, island, errors);
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
        walk(tree, item, false, &mut errors);
    }
    errors
}

/// The widget that marks a component the renderer draws.
pub const ISLAND: &str = "Island";

/// The instance `island`, an `Island` node, mounts as its content: its one
/// child, the root of a component instance its own view places; `None` for
/// any other content.
pub fn island_component(tree: &UiTree, island: &UiNode) -> Option<u32> {
    let [UiItem::Node(child)] = island.children.as_slice() else {
        return None;
    };
    let placed = child.instance.checked_sub(1)?;
    let instance = tree.instances.get(placed as usize)?;
    (instance.parent == island.instance).then_some(child.instance)
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
        tree: &compiled.tree,
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
        builder.item(item, None, None);
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
    /// The tree walked, whose spans of other files it reports where its own
    /// file places them.
    tree: &'a UiTree,
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
    /// static node when it has a region among them, and `list` is that node
    /// when it is a `VirtualList`.
    fn item(
        &mut self,
        item: &UiItem,
        slots: Option<&mut Vec<SlotTemplate>>,
        list: Option<&UiNode>,
    ) {
        let UiItem::Node(node) = item else {
            let region = self.region(item, list);
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
                self.item(child, None, None);
            }
            return;
        }
        let list = (node.kind == NodeKind::VirtualList).then_some(&**node);
        let mut children = Vec::with_capacity(node.children.len());
        for child in &node.children {
            self.item(child, Some(&mut children), list);
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
    /// index; the `for` of the `VirtualList` `list` lowers to its item
    /// template.
    fn region(&mut self, item: &UiItem, list: Option<&UiNode>) -> u32 {
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
                // A list row's root `sticky` says, of each item, whether its
                // row heads a section, and its `list.full_line` whether it
                // spans a grid's line.
                let mut read = |property: &str| {
                    list?;
                    let at = region.body.iter().find_map(|item| match item {
                        UiItem::Node(root) => root
                            .control_reads
                            .iter()
                            .find(|(name, _, instance)| {
                                name == property && *instance == region.instance
                            })
                            .map(|&(_, at, _)| at),
                        _ => None,
                    })?;
                    Some(self.entry(region.instance, Some(at), region.origin))
                };
                let sticky = read("sticky");
                let full = read("list.full_line");
                let kind = match (list, key) {
                    (Some(list), Some(key)) => {
                        let shape = list.style.list.unwrap_or_default();
                        RegionKind::Virtual {
                            items,
                            key,
                            estimate: terms_of(shape.estimate),
                            overscan: shape.overscan,
                            sticky,
                            pages: shape.pages,
                            lanes: shape.lanes.map(|lanes| ListLanes {
                                columns: lanes.columns,
                                min: terms_of(lanes.min),
                                gap: terms_of(lanes.gap),
                                cross_gap: terms_of(lanes.cross_gap),
                                masonry: lanes.masonry,
                            }),
                            full,
                        }
                    }
                    _ => RegionKind::For { items, key },
                };
                let entries = [items]
                    .into_iter()
                    .chain(key)
                    .chain(sticky)
                    .chain(full)
                    .collect();
                (kind, entries, vec![false])
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
                    None,
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
    #[allow(clippy::too_many_arguments)]
    fn content(
        &mut self,
        item: &UiItem,
        list: Option<&UiNode>,
        out: &mut Vec<ItemTemplate>,
        locals: &mut Vec<LocalTemplate>,
        env: &mut Vec<EnvTemplate>,
        effects: &mut Vec<EffectTemplate>,
        starters: &mut Vec<StarterTemplate>,
    ) {
        let UiItem::Node(node) = item else {
            // An instance whose view root is this region has no node of its
            // own here: the mount keeps it, as its key may read its states.
            let instance = match item {
                UiItem::If(region) => region.instance,
                UiItem::For(region) => region.instance,
                UiItem::Match(region) => region.instance,
                UiItem::Node(_) => unreachable!(),
            };
            self.claim(instance, MOUNT, locals, env, effects, starters);
            let region = self.region(item, list);
            out.push(ItemTemplate::Region(region));
            return;
        };
        self.claim(
            node.instance,
            out.len() as u32,
            locals,
            env,
            effects,
            starters,
        );
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
            .map(|i| Box::new(self.controls[i].1.clone()));
        let at = out.len();
        out.push(ItemTemplate::Node {
            node: Box::new(aot_node(node)),
            edges,
            routes,
            control,
            node_ref: node.node_ref,
        });
        if !authors_children(node) {
            self.skip(&node.children);
            return;
        }
        let list = (node.kind == NodeKind::VirtualList).then_some(&**node);
        for child in &node.children {
            self.content(child, list, out, locals, env, effects, starters);
        }
        if let ItemTemplate::Node { node: template, .. } = &mut out[at] {
            template.child_count = node.children.len() as u32;
        }
    }

    /// Collects the states, `env` reads, effects and task starters of
    /// `instance` when region content mounts it and no earlier item claimed
    /// them, anchored at the item `anchor` (or [`MOUNT`]); an instance whose
    /// own view has the region is the root's.
    fn claim(
        &mut self,
        instance: u32,
        anchor: u32,
        locals: &mut Vec<LocalTemplate>,
        env: &mut Vec<EnvTemplate>,
        effects: &mut Vec<EffectTemplate>,
        starters: &mut Vec<StarterTemplate>,
    ) {
        let Some(states) = self.layout.regional.iter().find(|r| r.instance == instance) else {
            return;
        };
        if !self.claimed.insert(instance) {
            return;
        }
        if self.layout.starters.contains(&instance) {
            starters.push(StarterTemplate { instance, anchor });
        }
        effects.extend(
            self.layout
                .effects
                .iter()
                .filter(|effect| effect.instance == instance)
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
                Some(self.tree.own_span(instance, at.unwrap_or(origin))),
                "internal: the region entry was not lowered",
            ));
            return 0;
        };
        let function = self
            .program
            .function(self.layout.handlers[index as usize].1);
        if let Err(unsupported) = &function.body {
            self.errors.push(MountError::new(
                Some(self.tree.own_span(instance, unsupported.at)),
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
            if local.contains(&slot) || self.layout.host_filled(slot) {
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

/// Whether a target authors `node`'s children: a container does, a
/// `VirtualList` through its item template.
fn authors_children(node: &UiNode) -> bool {
    node.kind.is_container()
}

/// The item lists of a region's arms (a `for` has one, its body); none for a
/// node.
pub(crate) fn arms(item: &UiItem) -> Vec<&[UiItem]> {
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
