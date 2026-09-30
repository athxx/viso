//! Control-flow regions: the parts of a view an `if`, a `match` or a keyed
//! `for` mounts, switches and reorders while the view runs.
//!
//! The compiler lowers every region to a [`RegionTemplate`]: the handler-table
//! entries that decide what it mounts (an arm choice, a scrutinee, an iterable,
//! an item key), the state cells those entries read, and the flattened node
//! templates of each arm. The macros, the hot reload commit and the release
//! package all mount one [`ViewRegions`] through [`mount_regions`], so a region
//! behaves the same under every target.
//!
//! A mounted view registers one structure hook with the union of its regions'
//! cells. When one changes, the hook walks the view's regions once, in reverse
//! child order so each region knows the node its content must precede:
//!
//! - an `if` or `match` whose cells changed re-evaluates its choice; switching
//!   arms frees the old arm's nodes, or detaches and keeps them when the arm is
//!   marked `preserve`, and mounts the new arm from its kept nodes or afresh;
//! - a keyed `for` re-evaluates its items and their keys, keeps the nodes of
//!   every key still present, moves them into the new order, mounts the new
//!   keys and frees the removed ones;
//! - a region whose cells did not change keeps its content and only visits the
//!   regions nested in it.
//!
//! A region node carries the same packaged node data as a static node of the
//! release package, and binds its state edges to the retained nodes it mounts.
//! Handlers inside a region receive the enclosing `for` items and `match`
//! scrutinees; when an item or a scrutinee changes in place, its nodes'
//! handlers are re-attached with the new bindings.
//!
//! A region entry that faults, a value a region cannot mount, and a repeated
//! key keep the region's current content and record the fault on the host.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use viso_behavior::{Fault, FaultKind, Value};
use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};
use viso_ui::aot::{AotNode, build_aot_node};
use viso_ui::state::StateKey;
use viso_ui::{BuildCx, DirtyClass, NodeId, StateId, StructureCx};

use crate::attach::{Route, attach_node};
use crate::control::Control;
use crate::host::ViewHost;
use crate::route::EventRoute;

/// The most items a range region mounts; a longer range faults instead of
/// allocating an unbounded tree.
pub const MAX_RANGE_ITEMS: i64 = 1 << 20;

/// A view's control-flow regions.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewRegions {
    /// The state cells the regions read and their nodes bind, by durable key;
    /// templates name a cell by its index here.
    pub states: Vec<StateKey>,
    /// Every region, nested ones included; a template names a region by its
    /// index here.
    pub regions: Vec<RegionTemplate>,
    /// Each static node with a region among its children.
    pub groups: Vec<GroupTemplate>,
}

impl ViewRegions {
    /// Whether the view has no region.
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

/// One region.
#[derive(Debug, Clone, PartialEq)]
pub struct RegionTemplate {
    /// What decides the region's content.
    pub kind: RegionKind,
    /// The cells the region's entries read, by index into
    /// [`ViewRegions::states`]: a change to one re-evaluates the region.
    pub deps: Vec<u32>,
    /// The arms, in source order; a `for` has one, its body.
    pub arms: Vec<ArmTemplate>,
}

/// What decides a region's content. Each entry is an index into the view's
/// handler table, called with the enclosing regions' bindings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// An `if` chain: `select` returns the index of the arm to mount, or `-1`.
    If {
        /// The arm choice.
        select: u32,
    },
    /// A `match`: `scrutinee` returns the value, `select` (called with it too)
    /// returns the index of the arm to mount, or `-1`.
    Match {
        /// The scrutinee.
        scrutinee: u32,
        /// The arm choice.
        select: u32,
    },
    /// A `for`: `items` returns the iterable — a list, or an integer range as
    /// a two-field aggregate tagged [`HALF_OPEN`] or [`CLOSED`] — and `key`
    /// (called with an item too) returns the item's identity; without one an
    /// item is keyed by its position.
    For {
        /// The iterable.
        items: u32,
        /// The item key.
        key: Option<u32>,
    },
}

/// The aggregate tag of a half-open integer range a `for` region iterates.
pub const HALF_OPEN: u32 = 0;

/// The aggregate tag of a closed integer range a `for` region iterates.
pub const CLOSED: u32 = 1;

/// One arm of a region.
#[derive(Debug, Clone, PartialEq)]
pub struct ArmTemplate {
    /// Whether switching away keeps the arm's nodes for when it is chosen
    /// again.
    pub preserve: bool,
    /// The arm's content, flattened in pre-order.
    pub items: Vec<ItemTemplate>,
}

/// One item of an arm's pre-order content.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemTemplate {
    /// A node, followed by the items of its `child_count` children.
    Node {
        /// The packaged node.
        node: AotNode,
        /// Its state edges: a cell index into [`ViewRegions::states`] and the
        /// [`DirtyClass`] bits a change of the cell marks.
        edges: Vec<(u32, u8)>,
        /// Its handler routes.
        routes: Vec<Route>,
        /// Its built-in response, for a native control node.
        control: Option<Control>,
    },
    /// A nested region, by index into [`ViewRegions::regions`]; it counts as one
    /// child of the node it sits in.
    Region(u32),
}

/// A static node with a region among its children.
#[derive(Debug, Clone, PartialEq)]
pub struct GroupTemplate {
    /// The node, by static pre-order index.
    pub parent: u32,
    /// Its children, in order.
    pub slots: Vec<SlotTemplate>,
}

/// One child of a static group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotTemplate {
    /// A static node, by static pre-order index.
    Node(u32),
    /// A region, by index into [`ViewRegions::regions`].
    Region(u32),
}

/// Mounts `regions` under the static nodes `nodes` (indexed by static
/// pre-order), resolving each state key through `cells`, and registers the
/// structure hook that re-shapes them. The regions' entries run on `host`.
pub fn mount_regions(
    cx: &mut StructureCx<'_>,
    regions: Rc<ViewRegions>,
    host: &Rc<RefCell<ViewHost>>,
    nodes: &[Option<NodeId>],
    cells: &[(StateKey, StateId)],
) {
    if regions.is_empty() {
        return;
    }
    let resolved: Vec<Option<StateId>> = regions
        .states
        .iter()
        .map(|key| cells.iter().find(|(k, _)| k == key).map(|(_, id)| *id))
        .collect();
    let node = |index: u32| nodes.get(index as usize).copied().flatten();
    let groups = regions
        .groups
        .iter()
        .filter_map(|group| {
            let slots = group
                .slots
                .iter()
                .map(|slot| match *slot {
                    SlotTemplate::Node(index) => node(index).map(Slot::Node),
                    SlotTemplate::Region(region) => Some(Slot::Region(Mount::new(
                        region,
                        &regions.regions[region as usize],
                    ))),
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Group {
                parent: node(group.parent)?,
                slots,
            })
        })
        .collect();
    let deps: Vec<StateId> = regions
        .regions
        .iter()
        .flat_map(|region| &region.deps)
        .filter_map(|&cell| resolved.get(cell as usize).copied().flatten())
        .collect();
    let mut mounted = Mounted {
        regions,
        cells: resolved,
        host: Rc::clone(host),
        groups,
        scratch: Vec::new(),
    };
    mounted.patch(cx, &[]);
    cx.store
        .add_structure_hook(deps, move |cx, changed| mounted.patch(cx, changed));
}

/// [`mount_regions`] for the encoded regions a macro expansion embeds, run over
/// the stores of the build that just authored the static nodes.
///
/// # Panics
///
/// If the bytes do not decode. The macro that embeds them encodes them at
/// build time, so this does not happen for bytes it produced.
#[doc(hidden)]
pub fn __mount_embedded(
    cx: &mut BuildCx<'_>,
    bytes: &'static [u8],
    host: &Rc<RefCell<ViewHost>>,
    nodes: &[Option<NodeId>],
    cells: &[(StateKey, StateId)],
) {
    let regions = ViewRegions::decode_from_slice(bytes)
        .unwrap_or_else(|error| panic!("embedded view regions do not decode: {error}"));
    cx.structure(|cx| mount_regions(cx, Rc::new(regions), host, nodes, cells));
}

/// An item's identity in a keyed `for`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Nil,
    Int(i64),
    Str(Rc<String>),
    Agg(u32, Box<[Key]>),
}

impl Key {
    /// The key a value names, `None` for a value without a stable identity (a
    /// float, a list, a closure, a handle).
    fn of(value: &Value) -> Option<Key> {
        Some(match value {
            Value::Nil => Key::Nil,
            Value::Int(n) => Key::Int(*n),
            Value::Str(s) => Key::Str(Rc::clone(s)),
            Value::Agg(agg) => Key::Agg(
                agg.tag,
                agg.fields.iter().map(Key::of).collect::<Option<_>>()?,
            ),
            Value::Float(_) | Value::List(_) | Value::Closure(_) | Value::Handle(_) => {
                return None;
            }
        })
    }
}

/// A child position under a mounted parent.
enum Slot {
    Node(NodeId),
    Region(Mount),
}

/// A mounted node with a region among its children.
struct Group {
    parent: NodeId,
    slots: Vec<Slot>,
}

/// A mounted region.
struct Mount {
    region: u32,
    /// Whether the region has not been evaluated since it was created or its
    /// content was re-attached.
    fresh: bool,
    content: Content,
}

enum Content {
    /// An `if` or a `match`: the scrutinee (`Nil` for an `if`), the chosen arm
    /// and its nodes, and the kept nodes of each `preserve` arm.
    Arms {
        subject: Value,
        active: Option<usize>,
        live: Option<Frag>,
        kept: Vec<Option<Frag>>,
    },
    /// A `for`: its items in order.
    List(Vec<Item>),
}

struct Item {
    key: Key,
    value: Value,
    frag: Frag,
}

/// The mounted content of one arm or one item.
struct Frag {
    region: u32,
    arm: u32,
    /// Its top-level children, in order.
    roots: Vec<Slot>,
    /// Its nodes with a region among their children.
    groups: Vec<Group>,
    /// The bindings its handlers and nested regions receive: the enclosing
    /// regions' and this item's or scrutinee's.
    scope: Vec<Value>,
    /// Each node with handlers, and its item index in the arm template.
    routed: Vec<(NodeId, u32)>,
}

impl Mount {
    fn new(region: u32, template: &RegionTemplate) -> Mount {
        let content = match template.kind {
            RegionKind::For { .. } => Content::List(Vec::new()),
            RegionKind::If { .. } | RegionKind::Match { .. } => Content::Arms {
                subject: Value::Nil,
                active: None,
                live: None,
                kept: template.arms.iter().map(|_| None).collect(),
            },
        };
        Mount {
            region,
            fresh: true,
            content,
        }
    }

    /// The first node the region mounts, in child order.
    fn first(&self) -> Option<NodeId> {
        match &self.content {
            Content::Arms { live, .. } => live.as_ref().and_then(|frag| first_of(&frag.roots)),
            Content::List(items) => items.iter().find_map(|item| first_of(&item.frag.roots)),
        }
    }
}

fn first_of(slots: &[Slot]) -> Option<NodeId> {
    slots.iter().find_map(|slot| match slot {
        Slot::Node(id) => Some(*id),
        Slot::Region(mount) => mount.first(),
    })
}

/// A view's mounted regions, owned by its structure hook.
struct Mounted {
    regions: Rc<ViewRegions>,
    /// The live cell of each [`ViewRegions::states`] key.
    cells: Vec<Option<StateId>>,
    /// Shared with the view's node handlers: the hook runs after the flush and
    /// a handler from a dispatched event, both on the cold path and neither
    /// inside the other, so the borrow is never re-entered.
    host: Rc<RefCell<ViewHost>>,
    groups: Vec<Group>,
    scratch: Vec<NodeId>,
}

impl Mounted {
    fn patch(&mut self, cx: &mut StructureCx<'_>, changed: &[StateId]) {
        let Mounted {
            regions,
            cells,
            host,
            groups,
            scratch,
        } = self;
        let mut patch = Patch {
            cx,
            regions,
            cells,
            host,
            changed,
            scratch,
            args: Vec::new(),
            freed: false,
        };
        for group in groups.iter_mut() {
            patch.slots(&mut group.slots, &[], group.parent, None, false);
        }
        if patch.freed {
            let arena = patch.cx.store.arena();
            patch.cx.bindings.retain_static(|node| arena.is_live(node));
        }
    }
}

/// One walk over a view's regions.
struct Patch<'p, 'c> {
    cx: &'p mut StructureCx<'c>,
    regions: &'p ViewRegions,
    cells: &'p [Option<StateId>],
    host: &'p Rc<RefCell<ViewHost>>,
    changed: &'p [StateId],
    scratch: &'p mut Vec<NodeId>,
    args: Vec<Value>,
    freed: bool,
}

impl Patch<'_, '_> {
    /// Brings `slots`, the children of `parent` that precede `anchor`, up to
    /// date and into place. `force` re-evaluates every region among them.
    fn slots(
        &mut self,
        slots: &mut [Slot],
        scope: &[Value],
        parent: NodeId,
        mut anchor: Option<NodeId>,
        force: bool,
    ) {
        for slot in slots.iter_mut().rev() {
            match slot {
                Slot::Node(id) => {
                    self.place(*id, parent, anchor);
                    anchor = Some(*id);
                }
                Slot::Region(mount) => {
                    self.mount(mount, scope, parent, anchor, force);
                    if let Some(first) = mount.first() {
                        anchor = Some(first);
                    }
                }
            }
        }
    }

    /// Links `id` under `parent` right before `anchor` (last for `None`),
    /// unless it already is.
    fn place(&mut self, id: NodeId, parent: NodeId, anchor: Option<NodeId>) {
        let Some(links) = self.cx.store.arena().links(id) else {
            return;
        };
        if links.parent == Some(parent) && links.next_sibling == anchor {
            return;
        }
        if let Some(prior) = links.parent {
            self.cx.store.arena_detach(id);
            self.cx.store.mark_dirty(prior, relayout());
        }
        self.cx.store.arena_insert_before(parent, id, anchor);
    }

    fn mount(
        &mut self,
        mount: &mut Mount,
        scope: &[Value],
        parent: NodeId,
        anchor: Option<NodeId>,
        force: bool,
    ) {
        let regions = self.regions;
        let template = &regions.regions[mount.region as usize];
        let touched = force
            || mount.fresh
            || template.deps.iter().any(|&cell| {
                self.cells
                    .get(cell as usize)
                    .copied()
                    .flatten()
                    .is_some_and(|id| self.changed.contains(&id))
            });
        mount.fresh = false;
        let region = mount.region;
        match (template.kind, &mut mount.content) {
            (
                RegionKind::If { select },
                Content::Arms {
                    active, live, kept, ..
                },
            ) => {
                let mut next = *active;
                if touched && let Some(choice) = self.evaluate(select, scope, None) {
                    next = arm_of(&choice, template.arms.len());
                }
                let arms = Arms {
                    region,
                    template,
                    active,
                    live,
                    kept,
                };
                self.switch(arms, next, scope, None, parent, anchor, force);
            }
            (
                RegionKind::Match { scrutinee, select },
                Content::Arms {
                    subject,
                    active,
                    live,
                    kept,
                },
            ) => {
                let mut next = *active;
                if touched
                    && let Some(value) = self.evaluate(scrutinee, scope, None)
                    && let Some(choice) = self.evaluate(select, scope, Some(&value))
                {
                    next = arm_of(&choice, template.arms.len());
                    *subject = value;
                }
                let arms = Arms {
                    region,
                    template,
                    active,
                    live,
                    kept,
                };
                self.switch(arms, next, scope, Some(subject), parent, anchor, force);
            }
            (RegionKind::For { items: entry, key }, Content::List(items)) => {
                if touched
                    && let Some(values) = self.iterate(entry, scope)
                    && let Some(keys) = self.keys(key, &values, scope)
                {
                    self.reconcile(region, items, values, keys, scope, parent);
                }
                let mut anchor = anchor;
                for item in items.iter_mut().rev() {
                    self.frag(
                        &mut item.frag,
                        scope,
                        Some(&item.value),
                        parent,
                        anchor,
                        force,
                    );
                    if let Some(first) = first_of(&item.frag.roots) {
                        anchor = Some(first);
                    }
                }
            }
            _ => {}
        }
    }

    /// Mounts arm `next` of an `if` or a `match` in place of the active one,
    /// then brings the mounted arm up to date.
    #[allow(clippy::too_many_arguments)]
    fn switch(
        &mut self,
        arms: Arms<'_>,
        next: Option<usize>,
        scope: &[Value],
        subject: Option<&Value>,
        parent: NodeId,
        anchor: Option<NodeId>,
        mut force: bool,
    ) {
        let Arms {
            region,
            template,
            active,
            live,
            kept,
        } = arms;
        if next != *active {
            if let (Some(old), Some(was)) = (live.take(), *active) {
                if template.arms[was].preserve {
                    self.detach(&old, parent);
                    kept[was] = Some(old);
                } else {
                    self.free(old);
                }
            }
            *active = next;
            if let Some(arm) = next {
                let frag = match kept[arm].take() {
                    Some(frag) => frag,
                    None => self.build(region, arm as u32, scope, subject, parent),
                };
                *live = Some(frag);
                force = true;
            }
        }
        if let Some(frag) = live {
            self.frag(frag, scope, subject, parent, anchor, force);
        }
    }

    /// Brings `frag` up to date under the bindings `scope` and `extra`, and into
    /// place before `anchor`.
    fn frag(
        &mut self,
        frag: &mut Frag,
        scope: &[Value],
        extra: Option<&Value>,
        parent: NodeId,
        anchor: Option<NodeId>,
        mut force: bool,
    ) {
        let current = frag.scope.len() == scope.len() + usize::from(extra.is_some())
            && frag.scope[..scope.len()] == *scope
            && extra.is_none_or(|extra| frag.scope[scope.len()] == *extra);
        if !current {
            frag.scope.clear();
            frag.scope.extend_from_slice(scope);
            frag.scope.extend(extra.cloned());
            let items = &self.regions.regions[frag.region as usize].arms[frag.arm as usize].items;
            for &(id, index) in &frag.routed {
                if let ItemTemplate::Node {
                    routes, control, ..
                } = &items[index as usize]
                {
                    attach_node(self.cx.store, self.host, id, routes, *control, &frag.scope);
                }
            }
            force = true;
        }
        let Frag {
            roots,
            groups,
            scope,
            ..
        } = frag;
        self.slots(roots, scope, parent, anchor, force);
        for group in groups.iter_mut() {
            self.slots(&mut group.slots, scope, group.parent, None, force);
        }
    }

    /// Reorders `items` into the order of `keys`, keeping the nodes of every
    /// key still present, mounting the new keys and freeing the removed ones.
    fn reconcile(
        &mut self,
        region: u32,
        items: &mut Vec<Item>,
        values: Vec<Value>,
        keys: Vec<Key>,
        scope: &[Value],
        parent: NodeId,
    ) {
        let mut index: HashMap<&Key, usize> = HashMap::with_capacity(items.len());
        for (at, item) in items.iter().enumerate() {
            index.insert(&item.key, at);
        }
        let reused: Vec<Option<usize>> = keys.iter().map(|key| index.remove(key)).collect();
        let mut old: Vec<Option<Item>> = items.drain(..).map(Some).collect();
        for (at, item) in old.iter_mut().enumerate() {
            if !reused.contains(&Some(at))
                && let Some(item) = item.take()
            {
                self.free(item.frag);
            }
        }
        for ((key, value), reused) in keys.into_iter().zip(values).zip(reused) {
            let item = match reused.and_then(|at| old[at].take()) {
                Some(mut item) => {
                    item.value = value;
                    item
                }
                None => {
                    let frag = self.build(region, 0, scope, Some(&value), parent);
                    Item { key, value, frag }
                }
            };
            items.push(item);
        }
    }

    /// The items a `for` iterates, or `None` after recording why not.
    fn iterate(&mut self, entry: u32, scope: &[Value]) -> Option<Vec<Value>> {
        let value = self.evaluate(entry, scope, None)?;
        match value {
            Value::List(list) => Some(Rc::unwrap_or_clone(list)),
            Value::Agg(range)
                if range.fields.len() == 2 && matches!(range.tag, HALF_OPEN | CLOSED) =>
            {
                let (Some(lo), Some(hi)) = (range.fields[0].as_int(), range.fields[1].as_int())
                else {
                    return self.fail(FaultKind::Internal, "a range bound is not an integer");
                };
                let hi = if range.tag == CLOSED {
                    hi.saturating_add(1)
                } else {
                    hi
                };
                if hi.saturating_sub(lo) > MAX_RANGE_ITEMS {
                    return self.fail(
                        FaultKind::Unsupported,
                        format!("a `for` region mounts at most {MAX_RANGE_ITEMS} items"),
                    );
                }
                Some((lo..hi).map(Value::Int).collect())
            }
            _ => self.fail(
                FaultKind::Internal,
                "a `for` region's iterable has the wrong kind",
            ),
        }
    }

    /// Each item's key, or `None` after recording why a key is unusable.
    fn keys(&mut self, entry: Option<u32>, values: &[Value], scope: &[Value]) -> Option<Vec<Key>> {
        let mut keys = Vec::with_capacity(values.len());
        let mut seen = HashSet::with_capacity(values.len());
        for (at, value) in values.iter().enumerate() {
            let key = match entry {
                Some(entry) => {
                    let key = self.evaluate(entry, scope, Some(value))?;
                    match Key::of(&key) {
                        Some(key) => key,
                        None => {
                            return self.fail(
                                FaultKind::Unsupported,
                                "a `for` key is a float, a list, a closure or a handle, \
                                 which has no stable identity",
                            );
                        }
                    }
                }
                None => Key::Int(at as i64),
            };
            if !seen.insert(key.clone()) {
                return self.fail(
                    FaultKind::Unsupported,
                    format!("two items of a `for` region share the key {key:?}"),
                );
            }
            keys.push(key);
        }
        Some(keys)
    }

    /// Calls entry `entry` with `scope` and `extra`, or records its fault.
    fn evaluate(&mut self, entry: u32, scope: &[Value], extra: Option<&Value>) -> Option<Value> {
        self.args.clear();
        self.args.extend_from_slice(scope);
        self.args.extend(extra.cloned());
        let Ok(mut host) = self.host.try_borrow_mut() else {
            return None;
        };
        let result = host.evaluate(entry, &self.args, &*self.cx.states);
        self.args.clear();
        match result {
            Ok(value) => Some(value),
            Err(fault) => {
                host.record_fault(fault);
                None
            }
        }
    }

    fn fail<T>(&mut self, kind: FaultKind, message: impl Into<String>) -> Option<T> {
        if let Ok(mut host) = self.host.try_borrow_mut() {
            host.record_fault(Fault {
                kind,
                at: None,
                message: message.into(),
            });
        }
        None
    }

    /// Mounts arm `arm` of region `region` as the last children of `parent`.
    /// Its nested regions start empty and mount on their first patch.
    fn build(
        &mut self,
        region: u32,
        arm: u32,
        scope: &[Value],
        extra: Option<&Value>,
        parent: NodeId,
    ) -> Frag {
        let regions = self.regions;
        let items = &regions.regions[region as usize].arms[arm as usize].items;
        let mut roots = Vec::new();
        let mut groups = Vec::new();
        let mut built = Vec::new();
        {
            let mut cx = BuildCx::with_parent(
                &mut *self.cx.store,
                &mut *self.cx.states,
                &mut *self.cx.bindings,
                parent,
            );
            let mut cursor = 0;
            while cursor < items.len() {
                build_item(
                    &mut cx,
                    regions,
                    items,
                    &mut cursor,
                    &mut roots,
                    &mut groups,
                    &mut built,
                );
            }
        }
        self.cx.store.mark_dirty(parent, relayout());
        let mut frag_scope = scope.to_vec();
        frag_scope.extend(extra.cloned());
        let mut routed = Vec::new();
        for (id, index) in built {
            let ItemTemplate::Node {
                edges,
                routes,
                control,
                ..
            } = &items[index]
            else {
                continue;
            };
            for &(cell, class) in edges {
                if let Some(Some(state)) = self.cells.get(cell as usize) {
                    self.cx
                        .bindings
                        .bind(*state, id, DirtyClass::from_bits(class));
                }
            }
            if !routes.is_empty() || control.is_some() {
                attach_node(self.cx.store, self.host, id, routes, *control, &frag_scope);
                routed.push((id, index as u32));
            }
        }
        Frag {
            region,
            arm,
            roots,
            groups,
            scope: frag_scope,
            routed,
        }
    }

    /// Unlinks `frag`'s nodes from `parent`, keeping them alive.
    fn detach(&mut self, frag: &Frag, parent: NodeId) {
        self.detach_slots(&frag.roots);
        self.cx.store.mark_dirty(parent, relayout());
    }

    fn detach_slots(&mut self, slots: &[Slot]) {
        for slot in slots {
            match slot {
                Slot::Node(id) => {
                    self.cx.store.arena_detach(*id);
                }
                Slot::Region(mount) => match &mount.content {
                    Content::Arms { live, .. } => {
                        if let Some(frag) = live {
                            self.detach_slots(&frag.roots);
                        }
                    }
                    Content::List(items) => {
                        for item in items {
                            self.detach_slots(&item.frag.roots);
                        }
                    }
                },
            }
        }
    }

    /// Frees `frag`'s nodes, and the nodes its nested regions keep.
    fn free(&mut self, frag: Frag) {
        self.freed = true;
        for slot in frag.roots {
            self.free_slot(slot);
        }
        for group in frag.groups {
            for slot in group.slots {
                self.free_slot(slot);
            }
        }
    }

    fn free_slot(&mut self, slot: Slot) {
        match slot {
            Slot::Node(id) => {
                self.cx
                    .store
                    .free_tree(id, &mut *self.cx.effects, &mut *self.scratch);
            }
            Slot::Region(mount) => match mount.content {
                Content::Arms { live, kept, .. } => {
                    for frag in live.into_iter().chain(kept.into_iter().flatten()) {
                        self.free(frag);
                    }
                }
                Content::List(items) => {
                    for item in items {
                        self.free(item.frag);
                    }
                }
            },
        }
    }
}

/// The arm state of an `if` or `match` mount, borrowed for a switch.
struct Arms<'m> {
    region: u32,
    template: &'m RegionTemplate,
    active: &'m mut Option<usize>,
    live: &'m mut Option<Frag>,
    kept: &'m mut Vec<Option<Frag>>,
}

/// The arm an arm choice names: `None` for `-1` or a value out of range.
fn arm_of(choice: &Value, arms: usize) -> Option<usize> {
    let index = usize::try_from(choice.as_int()?).ok()?;
    (index < arms).then_some(index)
}

/// The dirty classes a change of a node's children marks on it.
fn relayout() -> DirtyClass {
    DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT
}

/// Authors the item at `cursor` and its subtree, pushing its slot to `out`.
fn build_item(
    cx: &mut BuildCx<'_>,
    regions: &ViewRegions,
    items: &[ItemTemplate],
    cursor: &mut usize,
    out: &mut Vec<Slot>,
    groups: &mut Vec<Group>,
    built: &mut Vec<(NodeId, usize)>,
) {
    let index = *cursor;
    *cursor += 1;
    match &items[index] {
        ItemTemplate::Region(region) => out.push(Slot::Region(Mount::new(
            *region,
            &regions.regions[*region as usize],
        ))),
        ItemTemplate::Node { node, .. } => {
            let mut children = Vec::new();
            let count = node.child_count;
            let handle = build_aot_node(cx, node, |cx| {
                for _ in 0..count {
                    build_item(cx, regions, items, cursor, &mut children, groups, built);
                }
            });
            let id = handle.id();
            built.push((id, index));
            if children.iter().any(|slot| matches!(slot, Slot::Region(_))) {
                groups.push(Group {
                    parent: id,
                    slots: children,
                });
            }
            out.push(Slot::Node(id));
        }
    }
}

/// A pre-allocation hint that never trusts a length prefix past a sane ceiling.
fn bounded(count: u64) -> usize {
    count.min(4096) as usize
}

fn read_u32(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    let offset = dec.position();
    u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })
}

fn write_u32s(enc: &mut Encoder, values: &[u32]) {
    enc.write_varint(values.len() as u64);
    for &value in values {
        enc.write_varint(u64::from(value));
    }
}

fn read_u32s(dec: &mut Decoder<'_>) -> Result<Vec<u32>, DecodeError> {
    let count = dec.read_varint()?;
    let mut values = Vec::with_capacity(bounded(count));
    for _ in 0..count {
        values.push(read_u32(dec)?);
    }
    Ok(values)
}

impl Encode for ViewRegions {
    fn encode(&self, enc: &mut Encoder) {
        ProtocolTag::current().encode(enc);
        enc.write_varint(self.states.len() as u64);
        for key in &self.states {
            enc.write_u64(key.hi);
            enc.write_u64(key.lo);
        }
        enc.write_varint(self.regions.len() as u64);
        for region in &self.regions {
            match region.kind {
                RegionKind::If { select } => {
                    enc.write_u8(0);
                    enc.write_varint(u64::from(select));
                }
                RegionKind::Match { scrutinee, select } => {
                    enc.write_u8(1);
                    enc.write_varint(u64::from(scrutinee));
                    enc.write_varint(u64::from(select));
                }
                RegionKind::For { items, key } => {
                    enc.write_u8(2);
                    enc.write_varint(u64::from(items));
                    enc.write_varint(key.map_or(0, |key| u64::from(key) + 1));
                }
            }
            write_u32s(enc, &region.deps);
            enc.write_varint(region.arms.len() as u64);
            for arm in &region.arms {
                enc.write_bool(arm.preserve);
                enc.write_varint(arm.items.len() as u64);
                for item in &arm.items {
                    match item {
                        ItemTemplate::Node {
                            node,
                            edges,
                            routes,
                            control,
                        } => {
                            enc.write_u8(0);
                            node.encode(enc);
                            enc.write_varint(edges.len() as u64);
                            for &(cell, class) in edges {
                                enc.write_varint(u64::from(cell));
                                enc.write_u8(class);
                            }
                            enc.write_varint(routes.len() as u64);
                            for &(route, handler) in routes {
                                enc.write_u8(route as u8);
                                enc.write_varint(u64::from(handler));
                            }
                            enc.write_bool(control.is_some());
                            if let Some(control) = control {
                                control.encode(enc);
                            }
                        }
                        ItemTemplate::Region(region) => {
                            enc.write_u8(1);
                            enc.write_varint(u64::from(*region));
                        }
                    }
                }
            }
        }
        enc.write_varint(self.groups.len() as u64);
        for group in &self.groups {
            enc.write_varint(u64::from(group.parent));
            enc.write_varint(group.slots.len() as u64);
            for slot in &group.slots {
                let (tag, index) = match *slot {
                    SlotTemplate::Node(index) => (0, index),
                    SlotTemplate::Region(index) => (1, index),
                };
                enc.write_u8(tag);
                enc.write_varint(u64::from(index));
            }
        }
    }
}

impl Decode for ViewRegions {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let offset = dec.position();
        if !ProtocolTag::decode(dec)?.is_compatible() {
            return Err(DecodeError::Malformed { offset });
        }
        let count = dec.read_varint()?;
        let mut states = Vec::with_capacity(bounded(count));
        for _ in 0..count {
            let hi = dec.read_u64()?;
            let lo = dec.read_u64()?;
            states.push(StateKey::from_parts(hi, lo));
        }
        let count = dec.read_varint()?;
        let mut regions = Vec::with_capacity(bounded(count));
        for _ in 0..count {
            let offset = dec.position();
            let kind = match dec.read_u8()? {
                0 => RegionKind::If {
                    select: read_u32(dec)?,
                },
                1 => RegionKind::Match {
                    scrutinee: read_u32(dec)?,
                    select: read_u32(dec)?,
                },
                2 => RegionKind::For {
                    items: read_u32(dec)?,
                    key: read_u32(dec)?.checked_sub(1),
                },
                _ => return Err(DecodeError::Malformed { offset }),
            };
            let deps = read_u32s(dec)?;
            let count = dec.read_varint()?;
            let mut arms = Vec::with_capacity(bounded(count));
            for _ in 0..count {
                let preserve = dec.read_bool()?;
                let count = dec.read_varint()?;
                let mut items = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    let offset = dec.position();
                    items.push(match dec.read_u8()? {
                        0 => {
                            let node = AotNode::decode(dec)?;
                            let count = dec.read_varint()?;
                            let mut edges = Vec::with_capacity(bounded(count));
                            for _ in 0..count {
                                edges.push((read_u32(dec)?, dec.read_u8()?));
                            }
                            let count = dec.read_varint()?;
                            let mut routes = Vec::with_capacity(bounded(count));
                            for _ in 0..count {
                                let offset = dec.position();
                                let route = EventRoute::from_u8(dec.read_u8()?)
                                    .ok_or(DecodeError::Malformed { offset })?;
                                routes.push((route, read_u32(dec)?));
                            }
                            let control = if dec.read_bool()? {
                                Some(Control::decode(dec)?)
                            } else {
                                None
                            };
                            ItemTemplate::Node {
                                node,
                                edges,
                                routes,
                                control,
                            }
                        }
                        1 => ItemTemplate::Region(read_u32(dec)?),
                        _ => return Err(DecodeError::Malformed { offset }),
                    });
                }
                arms.push(ArmTemplate { preserve, items });
            }
            regions.push(RegionTemplate { kind, deps, arms });
        }
        let count = dec.read_varint()?;
        let mut groups = Vec::with_capacity(bounded(count));
        for _ in 0..count {
            let parent = read_u32(dec)?;
            let count = dec.read_varint()?;
            let mut slots = Vec::with_capacity(bounded(count));
            for _ in 0..count {
                let offset = dec.position();
                slots.push(match dec.read_u8()? {
                    0 => SlotTemplate::Node(read_u32(dec)?),
                    1 => SlotTemplate::Region(read_u32(dec)?),
                    _ => return Err(DecodeError::Malformed { offset }),
                });
            }
            groups.push(GroupTemplate { parent, slots });
        }
        let parsed = ViewRegions {
            states,
            regions,
            groups,
        };
        parsed
            .validate()
            .map_err(|()| DecodeError::Malformed { offset })?;
        Ok(parsed)
    }
}

impl ViewRegions {
    /// Checks every cross-reference, so a well-formed but inconsistent blob is
    /// rejected at load instead of panicking at mount. A region is named once
    /// and only after the region naming it, so nesting is acyclic.
    fn validate(&self) -> Result<(), ()> {
        let states = self.states.len();
        let regions = self.regions.len();
        let check = |ok: bool| if ok { Ok(()) } else { Err(()) };
        let mut named = vec![false; regions];
        let mut name = |region: u32, by: Option<usize>| -> Result<(), ()> {
            let index = region as usize;
            check(index < regions && !named[index] && by.is_none_or(|by| index > by))?;
            named[index] = true;
            Ok(())
        };
        for group in &self.groups {
            for slot in &group.slots {
                if let SlotTemplate::Region(region) = *slot {
                    name(region, None)?;
                }
            }
        }
        for (at, region) in self.regions.iter().enumerate() {
            check(region.deps.iter().all(|&cell| (cell as usize) < states))?;
            let arms = match region.kind {
                RegionKind::For { .. } => region.arms.len() == 1,
                RegionKind::If { .. } | RegionKind::Match { .. } => true,
            };
            check(arms)?;
            for arm in &region.arms {
                // The children still owed to the open nodes; an item owed to
                // none is a new top-level item.
                let mut owed = 0usize;
                for item in &arm.items {
                    owed = owed.saturating_sub(1);
                    match item {
                        ItemTemplate::Node { node, edges, .. } => {
                            check(edges.iter().all(|&(cell, _)| (cell as usize) < states))?;
                            check(node.kind.is_container() || node.child_count == 0)?;
                            owed += node.child_count as usize;
                        }
                        ItemTemplate::Region(nested) => name(*nested, Some(at))?,
                    }
                }
                check(owed == 0)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use viso_ui::aot::{AotNodeKind, AotStyle};

    use super::*;

    fn node(kind: AotNodeKind, child_count: u32) -> AotNode {
        AotNode {
            kind,
            style: AotStyle::default(),
            child_count,
        }
    }

    #[test]
    fn regions_round_trip_and_reject_a_dangling_region() {
        let regions = ViewRegions {
            states: vec![StateKey::from_parts(1, 2)],
            regions: vec![
                RegionTemplate {
                    kind: RegionKind::If { select: 0 },
                    deps: vec![0],
                    arms: vec![ArmTemplate {
                        preserve: true,
                        items: vec![
                            ItemTemplate::Node {
                                node: node(AotNodeKind::Flex, 1),
                                edges: vec![(0, 4)],
                                routes: vec![(EventRoute::Click, 1)],
                                control: None,
                            },
                            ItemTemplate::Region(1),
                        ],
                    }],
                },
                RegionTemplate {
                    kind: RegionKind::For {
                        items: 2,
                        key: Some(3),
                    },
                    deps: vec![],
                    arms: vec![ArmTemplate {
                        preserve: false,
                        items: vec![ItemTemplate::Node {
                            node: node(AotNodeKind::Leaf, 0),
                            edges: vec![],
                            routes: vec![],
                            control: Some(Control::new(crate::ControlKind::Toggle)),
                        }],
                    }],
                },
            ],
            groups: vec![GroupTemplate {
                parent: 0,
                slots: vec![SlotTemplate::Node(1), SlotTemplate::Region(0)],
            }],
        };
        let bytes = regions.encode_to_vec();
        assert_eq!(ViewRegions::decode_from_slice(&bytes), Ok(regions.clone()));

        let mut dangling = regions;
        dangling.groups[0].slots[1] = SlotTemplate::Region(1);
        let bytes = dangling.encode_to_vec();
        assert!(ViewRegions::decode_from_slice(&bytes).is_err());
    }

    #[test]
    fn a_key_names_only_stable_values() {
        assert_eq!(Key::of(&Value::Int(3)), Some(Key::Int(3)));
        assert_eq!(Key::of(&Value::Float(1.0)), None);
        assert_eq!(
            Key::of(&Value::Str(Rc::new("a".into()))),
            Some(Key::Str(Rc::new("a".into())))
        );
    }
}
