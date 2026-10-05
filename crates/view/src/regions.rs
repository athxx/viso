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
//! Each mount of an arm or an item keeps the states of the component instances
//! it mounts ([`LocalTemplate`]), started from their initializers with the
//! mount's bindings: two items of a `for` mounting the same component keep two
//! sets of states, which follow their items' keys when the items reorder, stay
//! with a `preserve` arm switched away, and are dropped with the mount.
//!
//! A region entry that faults, a value a region cannot mount, and a repeated
//! key keep the region's current content and record the fault on the host.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use viso_behavior::{ComponentEffect, Fault, FaultKind, Value};
use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};
use viso_ui::adaptive::{AnchorId, EnvField};
use viso_ui::aot::{AotNode, build_aot_node};
use viso_ui::input::{ParkedFocus, park_focus, unpark_focus};
use viso_ui::state::StateKey;
use viso_ui::{BuildCx, DirtyClass, NodeId, NodeStore, StateId, StateValue, StructureCx};

use crate::attach::{Route, attach_node};
use crate::control::Control;
use crate::effects::mount_effect;
use crate::host::ViewHost;
use crate::route::EventRoute;
use crate::scope::{LocalEnv, Locals, Scope};
use crate::values::{Shown, control_cells};

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
    /// The cells the region's entries read: a change to one re-evaluates the
    /// region.
    pub deps: Vec<CellRef>,
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

/// A state cell a region reads or a region node binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CellRef {
    /// A cell of the view, by index into [`ViewRegions::states`].
    Shared(u32),
    /// The component state slot of an instance region content mounts, which
    /// the enclosing mount of that content keeps ([`ArmTemplate::locals`]).
    Local(u32),
}

/// A state of a component instance an arm mounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTemplate {
    /// The component state slot.
    pub slot: u32,
    /// Its initializer, by index into the view's handler table, called with
    /// the mount's bindings; `None` starts the state `Nil`.
    pub init: Option<u32>,
}

/// An `env` field a component instance an arm mounts reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvTemplate {
    /// The component state slot the field fills.
    pub slot: u32,
    /// The field.
    pub field: EnvField,
    /// The node its anchored fields resolve at, by item index in the arm.
    pub anchor: u32,
}

/// An effect of a component instance an arm mounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectTemplate {
    /// Its entries and run policy.
    pub effect: ComponentEffect,
    /// The node it is mounted on, the instance's root, by item index in the
    /// arm: freeing the node cancels it.
    pub anchor: u32,
}

/// A component instance region content mounts that starts tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StarterTemplate {
    /// The inlined instance.
    pub instance: u32,
    /// The instance's root, by item index in the arm: it owns the tasks, so
    /// freeing it cancels them.
    pub anchor: u32,
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
    /// The states of the component instances the arm mounts outside its
    /// nested regions, ascending by slot: each mount of the arm keeps its own.
    pub locals: Vec<LocalTemplate>,
    /// The `env` fields those instances read, ascending by slot: each mount of
    /// the arm resolves its own anchored fields.
    pub env: Vec<EnvTemplate>,
    /// The effects of those instances, in source order: each mount of the arm
    /// runs its own.
    pub effects: Vec<EffectTemplate>,
    /// Those instances that start tasks: each mount of the arm owns their
    /// tasks by the instance's root.
    pub starters: Vec<StarterTemplate>,
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
        /// Its state edges: a cell and the [`DirtyClass`] bits a change of the
        /// cell marks.
        edges: Vec<(CellRef, u8)>,
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
///
/// A node the regions mount shows its values as a static node does: they are
/// delivered when it mounts or its mount's bindings change, and again when a
/// cell they read changes, so the hook also depends on every shared cell a
/// content node's values read.
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
    let env = host.borrow().env_cells();
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
                    SlotTemplate::Region(region) => Some(Slot::Region(Box::new(Mount::new(
                        region,
                        &regions.regions[region as usize],
                    )))),
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Group {
                parent: node(group.parent)?,
                slots,
            })
        })
        .collect();
    let mut deps: Vec<StateId> = regions
        .regions
        .iter()
        .flat_map(|region| &region.deps)
        .filter_map(|cell| match *cell {
            CellRef::Shared(cell) => resolved.get(cell as usize).copied().flatten(),
            CellRef::Local(slot) => env_cell(&env, slot),
        })
        .collect();
    {
        let view = host.borrow();
        let controls = regions
            .regions
            .iter()
            .flat_map(|region| &region.arms)
            .flat_map(|arm| &arm.items)
            .filter_map(|item| match item {
                ItemTemplate::Node { control, .. } => control.as_ref(),
                _ => None,
            });
        for control in controls {
            control_cells(control, &Scope::EMPTY, &view, &mut deps);
        }
    }
    let arms = || regions.regions.iter().flat_map(|region| &region.arms);
    for read in arms().flat_map(|arm| &arm.env) {
        if let Some(cell) = cx.states.env_cell(read.field)
            && !deps.contains(&cell)
        {
            deps.push(cell);
        }
    }
    let keeps =
        arms().any(|arm| !arm.locals.is_empty() || !arm.env.is_empty() || !arm.starters.is_empty());
    let pulse = keeps.then(|| {
        let pulse = cx.states.alloc(StateValue::Int(0));
        host.borrow_mut().adopt_pulse(pulse);
        deps.push(pulse);
        pulse
    });
    let mounted = Mounted {
        regions,
        cells: resolved,
        env,
        pulse,
        host: Rc::clone(host),
        groups,
        scratch: Vec::new(),
    };
    let mounted = Rc::new(RefCell::new(mounted));
    mounted.borrow_mut().patch(cx, &[]);
    let kept = Rc::clone(&mounted);
    let hook = cx.store.add_structure_hook(deps, move |cx, changed| {
        kept.borrow_mut().patch(cx, changed);
    });
    host.borrow_mut().mark_regions(hook, &mounted);
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

/// The identity of an item of a keyed `for`, as its key evaluated.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ItemKey(Key);

/// A node a view's regions mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionNode {
    /// Its region, by index into [`ViewRegions::regions`].
    pub region: u32,
    /// The arm of the region that mounted it.
    pub arm: u32,
    /// The item it was built from, by index into the arm's
    /// [`items`](ArmTemplate::items).
    pub item: u32,
    /// The keys of the `for` items it sits in, outermost first.
    pub path: Vec<ItemKey>,
    /// The node.
    pub node: NodeId,
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

/// A child position under a mounted parent: a region is boxed, so a group of
/// plain nodes packs one id per position.
enum Slot {
    Node(NodeId),
    Region(Box<Mount>),
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
    /// and its nodes, and the kept nodes of each `preserve` arm with the focus
    /// they took with them.
    Arms {
        subject: Value,
        active: Option<usize>,
        live: Option<Frag>,
        kept: Vec<Option<(Frag, ParkedFocus)>>,
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
    /// What its handlers and nested regions run with: the enclosing regions'
    /// bindings and this item's or scrutinee's, and the enclosing mounts'
    /// instance states and [`own`](Self::own).
    scope: Scope,
    /// The instance states this mount keeps.
    own: Option<Rc<Locals>>,
    /// Each node with handlers, and its item index in the arm template.
    routed: Vec<(NodeId, u32)>,
    /// Each node showing a value of the view.
    shown: Vec<Shown>,
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
pub(crate) struct Mounted {
    regions: Rc<ViewRegions>,
    /// The live cell of each [`ViewRegions::states`] key.
    cells: Vec<Option<StateId>>,
    /// The revision cell of each `env` slot of the view's static instances,
    /// by ascending slot.
    env: Vec<(u32, StateId)>,
    /// The cell raised with every write of a state a mount of region content
    /// keeps, `None` for regions that mount no component instance.
    pulse: Option<StateId>,
    /// Shared with the view's node handlers: the hook runs after the flush and
    /// a handler from a dispatched event, both on the cold path and neither
    /// inside the other, so the borrow is never re-entered.
    host: Rc<RefCell<ViewHost>>,
    groups: Vec<Group>,
    scratch: Vec<NodeId>,
}

impl Mounted {
    /// Appends every node the regions show to `out`: the content of each
    /// chosen arm and of each `for` item, nested regions included; an arm
    /// switched away and kept is not shown.
    pub(crate) fn census(&self, store: &NodeStore, out: &mut Vec<RegionNode>) {
        let mut path = Vec::new();
        for slot in self.groups.iter().flat_map(|group| &group.slots) {
            if let Slot::Region(mount) = slot {
                census_mount(&self.regions, mount, store, &mut path, out);
            }
        }
    }

    fn patch(&mut self, cx: &mut StructureCx<'_>, changed: &[StateId]) {
        let Mounted {
            regions,
            cells,
            env,
            pulse,
            host,
            groups,
            scratch,
        } = self;
        let mut patch = Patch {
            cx,
            regions,
            cells,
            env,
            pulse: *pulse,
            host,
            changed,
            scratch,
            freed: false,
        };
        for group in groups.iter_mut() {
            patch.slots(&mut group.slots, &Scope::EMPTY, group.parent, None, false);
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
    env: &'p [(u32, StateId)],
    pulse: Option<StateId>,
    host: &'p Rc<RefCell<ViewHost>>,
    changed: &'p [StateId],
    scratch: &'p mut Vec<NodeId>,
    freed: bool,
}

impl Patch<'_, '_> {
    /// The live cell `cell` names in `scope`: a view cell, a state the
    /// enclosing mounts keep, or else the `env` field a static instance's
    /// slot holds.
    fn cell(&self, cell: CellRef, scope: &Scope) -> Option<StateId> {
        match cell {
            CellRef::Shared(cell) => self.cells.get(cell as usize).copied().flatten(),
            CellRef::Local(slot) => scope.cell(slot).or_else(|| env_cell(self.env, slot)),
        }
    }

    /// Brings `slots`, the children of `parent` that precede `anchor`, up to
    /// date and into place. `force` re-evaluates every region among them.
    fn slots(
        &mut self,
        slots: &mut [Slot],
        scope: &Scope,
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
        scope: &Scope,
        parent: NodeId,
        anchor: Option<NodeId>,
        force: bool,
    ) {
        let regions = self.regions;
        let template = &regions.regions[mount.region as usize];
        let touched = force
            || mount.fresh
            || template.deps.iter().any(|&cell| {
                self.cell(cell, scope)
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
        scope: &Scope,
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
        let mut parked = ParkedFocus::default();
        if next != *active {
            if let (Some(old), Some(was)) = (live.take(), *active) {
                if template.arms[was].preserve {
                    let taken = self.detach(&old, parent);
                    kept[was] = Some((old, taken));
                } else {
                    self.free(old);
                }
            }
            *active = next;
            if let Some(arm) = next {
                let frag = match kept[arm].take() {
                    Some((frag, taken)) => {
                        parked = taken;
                        frag
                    }
                    None => self.build(region, arm as u32, scope, subject, parent),
                };
                *live = Some(frag);
                force = true;
            }
        }
        if let Some(frag) = live {
            self.frag(frag, scope, subject, parent, anchor, force);
        }
        unpark_focus(self.cx.store, parked);
    }

    /// Brings `frag` up to date under the bindings `scope` and `extra`, and into
    /// place before `anchor`.
    fn frag(
        &mut self,
        frag: &mut Frag,
        scope: &Scope,
        extra: Option<&Value>,
        parent: NodeId,
        anchor: Option<NodeId>,
        mut force: bool,
    ) {
        let values = &frag.scope.values;
        let locals = &frag.scope.locals;
        let current = values.len() == scope.values.len() + usize::from(extra.is_some())
            && values[..scope.values.len()] == *scope.values
            && extra.is_none_or(|extra| values[scope.values.len()] == *extra)
            && locals.len() == scope.locals.len() + usize::from(frag.own.is_some())
            && locals
                .iter()
                .zip(&scope.locals)
                .all(|(a, b)| Rc::ptr_eq(a, b));
        if !current {
            frag.scope = frag_scope(scope, extra, frag.own.as_ref());
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
        if !frag.shown.is_empty()
            && let Ok(mut host) = self.host.try_borrow_mut()
        {
            for shown in &mut frag.shown {
                if !current {
                    shown.rescope(frag.scope.clone(), &host);
                }
                if force || shown.reads_any(self.changed) {
                    shown.deliver(self.cx, &mut host);
                }
            }
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
        scope: &Scope,
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
    fn iterate(&mut self, entry: u32, scope: &Scope) -> Option<Vec<Value>> {
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
    fn keys(&mut self, entry: Option<u32>, values: &[Value], scope: &Scope) -> Option<Vec<Key>> {
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

    /// Calls entry `entry` in `scope` with `extra`, or records its fault.
    fn evaluate(&mut self, entry: u32, scope: &Scope, extra: Option<&Value>) -> Option<Value> {
        let Ok(mut host) = self.host.try_borrow_mut() else {
            return None;
        };
        let result = host.evaluate(entry, scope, extra, &*self.cx.states);
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

    /// Mounts arm `arm` of region `region` as the last children of `parent`,
    /// starting the instance states it keeps. Its nested regions start empty
    /// and mount on their first patch.
    fn build(
        &mut self,
        region: u32,
        arm: u32,
        scope: &Scope,
        extra: Option<&Value>,
        parent: NodeId,
    ) -> Frag {
        let regions = self.regions;
        let template = &regions.regions[region as usize].arms[arm as usize];
        let items = &template.items;
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
        let own = self.keep(template, scope, extra, &built);
        let frag_scope = frag_scope(scope, extra, own.as_ref());
        for effect in &template.effects {
            let anchor = effect.anchor as usize;
            if let Some(&(id, _)) = built.iter().find(|&&(_, index)| index == anchor) {
                mount_effect(
                    self.cx.store,
                    self.host,
                    effect.effect,
                    frag_scope.clone(),
                    id,
                );
            }
        }
        let mut routed = Vec::new();
        let mut shown = Vec::new();
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
                if let Some(state) = self.cell(cell, &frag_scope) {
                    self.cx
                        .bindings
                        .bind(state, id, DirtyClass::from_bits(class));
                }
            }
            if !routes.is_empty() || control.is_some() {
                attach_node(self.cx.store, self.host, id, routes, *control, &frag_scope);
                routed.push((id, index as u32));
            }
            if let Some(control) = *control
                && let Ok(mut host) = self.host.try_borrow_mut()
            {
                let mut node = Shown::new(id, control, frag_scope.clone(), &host);
                node.deliver(self.cx, &mut host);
                shown.push(node);
            }
        }
        Frag {
            region,
            arm,
            roots,
            groups,
            scope: frag_scope,
            own,
            routed,
            shown,
        }
    }

    /// The instance states and `env` slots a new mount of `arm` in `scope`,
    /// with the binding `extra`, keeps: the states' initial values and a
    /// revision cell each, and for each `env` slot the field's cell, anchored
    /// at the mounted node `built` holds for its anchor item.
    fn keep(
        &mut self,
        arm: &ArmTemplate,
        scope: &Scope,
        extra: Option<&Value>,
        built: &[(NodeId, usize)],
    ) -> Option<Rc<Locals>> {
        let pulse = self.pulse.filter(|_| {
            !arm.locals.is_empty() || !arm.env.is_empty() || !arm.starters.is_empty()
        })?;
        let Ok(mut host) = self.host.try_borrow_mut() else {
            return None;
        };
        let init = frag_scope(scope, extra, None);
        let initial = host.initialize(&arm.locals, &init, &*self.cx.states);
        let count = arm.locals.len() + arm.env.len();
        let mut slots = Vec::with_capacity(count);
        let mut values = Vec::with_capacity(count);
        let mut cells = Vec::with_capacity(count);
        let mut env = Vec::with_capacity(arm.env.len());
        let mut anchors: Vec<(u32, AnchorId)> = Vec::new();
        let mut states = arm.locals.iter().zip(initial).peekable();
        let mut reads = arm.env.iter().peekable();
        loop {
            let read = match (states.peek(), reads.peek()) {
                (None, None) => break,
                (Some((local, _)), Some(read)) => read.slot < local.slot,
                (None, Some(_)) => true,
                (Some(_), None) => false,
            };
            if !read {
                if let Some((local, value)) = states.next() {
                    slots.push(local.slot);
                    values.push(value);
                    cells.push(self.cx.states.alloc(StateValue::Int(0)));
                }
                continue;
            }
            let Some(read) = reads.next() else {
                break;
            };
            let (cell, anchor) = if read.field.anchored() {
                let anchor = match anchors.iter().find(|(item, _)| *item == read.anchor) {
                    Some(&(_, anchor)) => Some(anchor),
                    None => built
                        .iter()
                        .find(|&&(_, item)| item == read.anchor as usize)
                        .map(|&(node, _)| {
                            let anchor = self.cx.states.anchor_env(node, Some(pulse));
                            anchors.push((read.anchor, anchor));
                            anchor
                        }),
                };
                let cell = anchor.and_then(|anchor| self.cx.states.anchor_cell(anchor, read.field));
                (cell, anchor)
            } else {
                (self.cx.states.env_cell(read.field), None)
            };
            let Some(cell) = cell else {
                continue;
            };
            env.push(LocalEnv {
                at: slots.len() as u32,
                field: read.field,
                anchor,
                seen: Cell::new(None),
            });
            slots.push(read.slot);
            values.push(Value::Nil);
            cells.push(cell);
        }
        let owners = arm
            .starters
            .iter()
            .filter_map(|starter| {
                let &(node, _) = built
                    .iter()
                    .find(|&&(_, item)| item == starter.anchor as usize)?;
                Some((starter.instance, node))
            })
            .collect();
        let kept = Rc::new(Locals {
            slots: slots.into(),
            values: RefCell::new(values.into()),
            cells: cells.into(),
            env: env.into(),
            pulse,
            owners,
        });
        host.adopt(&kept);
        Some(kept)
    }

    /// Unlinks `frag`'s nodes from `parent`, keeping them alive, and takes
    /// the focus they hold with them.
    fn detach(&mut self, frag: &Frag, parent: NodeId) -> ParkedFocus {
        self.detach_slots(&frag.roots);
        self.cx.store.mark_dirty(parent, relayout());
        park_focus(self.cx.store, parent)
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

    /// Frees `frag`'s nodes and instance states, and those its nested regions
    /// keep.
    fn free(&mut self, frag: Frag) {
        self.freed = true;
        if let Some(own) = &frag.own {
            own.release(self.cx.states);
        }
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
                    let kept = kept.into_iter().flatten().map(|(frag, _)| frag);
                    for frag in live.into_iter().chain(kept) {
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
    kept: &'m mut Vec<Option<(Frag, ParkedFocus)>>,
}

/// The revision cell of `env` slot `slot` in `env`, sorted by slot.
fn env_cell(env: &[(u32, StateId)], slot: u32) -> Option<StateId> {
    env.binary_search_by_key(&slot, |&(s, _)| s)
        .ok()
        .map(|index| env[index].1)
}

/// The scope of a mount in `scope` with the binding `extra`, keeping `own`.
fn frag_scope(scope: &Scope, extra: Option<&Value>, own: Option<&Rc<Locals>>) -> Scope {
    let mut values = Vec::with_capacity(scope.values.len() + 1);
    values.extend_from_slice(&scope.values);
    values.extend(extra.cloned());
    let mut locals = Vec::with_capacity(scope.locals.len() + usize::from(own.is_some()));
    locals.extend(scope.locals.iter().cloned());
    locals.extend(own.cloned());
    Scope { values, locals }
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
fn census_mount(
    regions: &ViewRegions,
    mount: &Mount,
    store: &NodeStore,
    path: &mut Vec<ItemKey>,
    out: &mut Vec<RegionNode>,
) {
    match &mount.content {
        Content::Arms { live, .. } => {
            if let Some(frag) = live {
                census_frag(regions, frag, store, path, out);
            }
        }
        Content::List(items) => {
            for item in items {
                path.push(ItemKey(item.key.clone()));
                census_frag(regions, &item.frag, store, path, out);
                path.pop();
            }
        }
    }
}

fn census_frag(
    regions: &ViewRegions,
    frag: &Frag,
    store: &NodeStore,
    path: &mut Vec<ItemKey>,
    out: &mut Vec<RegionNode>,
) {
    let mut census = Census {
        regions,
        frag,
        items: &regions.regions[frag.region as usize].arms[frag.arm as usize].items,
        store,
        path,
        out,
        cursor: 0,
    };
    for slot in &frag.roots {
        census.slot(slot);
    }
}

/// One fragment's walk: its mounted nodes against the arm's pre-order items.
struct Census<'a> {
    regions: &'a ViewRegions,
    frag: &'a Frag,
    items: &'a [ItemTemplate],
    store: &'a NodeStore,
    path: &'a mut Vec<ItemKey>,
    out: &'a mut Vec<RegionNode>,
    /// The next item.
    cursor: usize,
}

impl Census<'_> {
    fn slot(&mut self, slot: &Slot) {
        match slot {
            Slot::Node(id) => self.node(*id),
            Slot::Region(mount) => {
                let at = self.cursor;
                self.cursor += 1;
                if matches!(self.items.get(at), Some(ItemTemplate::Region(_))) {
                    census_mount(self.regions, mount, self.store, self.path, self.out);
                }
            }
        }
    }

    /// The node `id`, built from the next item, and its children: the slots
    /// of its group when it has a region among them, else its children in
    /// the tree, one per child item.
    fn node(&mut self, id: NodeId) {
        let at = self.cursor;
        let Some(ItemTemplate::Node { node, .. }) = self.items.get(at) else {
            self.skip();
            return;
        };
        self.cursor += 1;
        self.out.push(RegionNode {
            region: self.frag.region,
            arm: self.frag.arm,
            item: at as u32,
            path: self.path.clone(),
            node: id,
        });
        if let Some(group) = self.frag.groups.iter().find(|group| group.parent == id) {
            for slot in &group.slots {
                self.slot(slot);
            }
            return;
        }
        let arena = self.store.arena();
        let mut child = arena.links(id).and_then(|l| l.first_child);
        for _ in 0..node.child_count {
            match child {
                Some(next) => {
                    child = arena.links(next).and_then(|l| l.next_sibling);
                    self.node(next);
                }
                None => self.skip(),
            }
        }
    }

    /// Steps past the next item and its children.
    fn skip(&mut self) {
        let at = self.cursor;
        self.cursor += 1;
        if let Some(ItemTemplate::Node { node, .. }) = self.items.get(at) {
            for _ in 0..node.child_count {
                self.skip();
            }
        }
    }
}

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
        ItemTemplate::Region(region) => out.push(Slot::Region(Box::new(Mount::new(
            *region,
            &regions.regions[*region as usize],
        )))),
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

/// A cell reference as one varint: the index or slot, then whether it is
/// [`CellRef::Local`] in the low bit.
fn write_cell(enc: &mut Encoder, cell: CellRef) {
    let (index, local) = match cell {
        CellRef::Shared(index) => (index, 0),
        CellRef::Local(slot) => (slot, 1),
    };
    enc.write_varint((u64::from(index) << 1) | local);
}

fn read_cell(dec: &mut Decoder<'_>) -> Result<CellRef, DecodeError> {
    let offset = dec.position();
    let raw = dec.read_varint()?;
    let index = u32::try_from(raw >> 1).map_err(|_| DecodeError::Malformed { offset })?;
    Ok(if raw & 1 == 0 {
        CellRef::Shared(index)
    } else {
        CellRef::Local(index)
    })
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
            enc.write_varint(region.deps.len() as u64);
            for &cell in &region.deps {
                write_cell(enc, cell);
            }
            enc.write_varint(region.arms.len() as u64);
            for arm in &region.arms {
                enc.write_bool(arm.preserve);
                enc.write_varint(arm.locals.len() as u64);
                for local in &arm.locals {
                    enc.write_varint(u64::from(local.slot));
                    enc.write_varint(local.init.map_or(0, |init| u64::from(init) + 1));
                }
                enc.write_varint(arm.env.len() as u64);
                for env in &arm.env {
                    enc.write_varint(u64::from(env.slot));
                    enc.write_u8(env.field.tag());
                    enc.write_varint(u64::from(env.anchor));
                }
                enc.write_varint(arm.effects.len() as u64);
                for effect in &arm.effects {
                    effect.effect.encode(enc);
                    enc.write_varint(u64::from(effect.anchor));
                }
                enc.write_varint(arm.starters.len() as u64);
                for starter in &arm.starters {
                    enc.write_varint(u64::from(starter.instance));
                    enc.write_varint(u64::from(starter.anchor));
                }
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
                                write_cell(enc, cell);
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
            let count = dec.read_varint()?;
            let mut deps = Vec::with_capacity(bounded(count));
            for _ in 0..count {
                deps.push(read_cell(dec)?);
            }
            let count = dec.read_varint()?;
            let mut arms = Vec::with_capacity(bounded(count));
            for _ in 0..count {
                let preserve = dec.read_bool()?;
                let count = dec.read_varint()?;
                let mut locals = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    locals.push(LocalTemplate {
                        slot: read_u32(dec)?,
                        init: read_u32(dec)?.checked_sub(1),
                    });
                }
                let count = dec.read_varint()?;
                let mut env = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    let slot = read_u32(dec)?;
                    let offset = dec.position();
                    let field = EnvField::from_tag(dec.read_u8()?)
                        .ok_or(DecodeError::Malformed { offset })?;
                    env.push(EnvTemplate {
                        slot,
                        field,
                        anchor: read_u32(dec)?,
                    });
                }
                let count = dec.read_varint()?;
                let mut effects = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    effects.push(EffectTemplate {
                        effect: ComponentEffect::decode(dec)?,
                        anchor: read_u32(dec)?,
                    });
                }
                let count = dec.read_varint()?;
                let mut starters = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    starters.push(StarterTemplate {
                        instance: read_u32(dec)?,
                        anchor: read_u32(dec)?,
                    });
                }
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
                                edges.push((read_cell(dec)?, dec.read_u8()?));
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
                arms.push(ArmTemplate {
                    preserve,
                    locals,
                    env,
                    effects,
                    starters,
                    items,
                });
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
        let shared = |cell: CellRef| match cell {
            CellRef::Shared(index) => (index as usize) < states,
            CellRef::Local(_) => true,
        };
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
            check(region.deps.iter().all(|&cell| shared(cell)))?;
            let arms = match region.kind {
                RegionKind::For { .. } => region.arms.len() == 1,
                RegionKind::If { .. } | RegionKind::Match { .. } => true,
            };
            check(arms)?;
            for arm in &region.arms {
                check(arm.locals.windows(2).all(|w| w[0].slot < w[1].slot))?;
                check(arm.env.windows(2).all(|w| w[0].slot < w[1].slot))?;
                check(arm.env.iter().all(|env| {
                    arm.locals
                        .binary_search_by_key(&env.slot, |l| l.slot)
                        .is_err()
                        && matches!(
                            arm.items.get(env.anchor as usize),
                            Some(ItemTemplate::Node { .. })
                        )
                }))?;
                let anchors = arm.effects.iter().map(|effect| effect.anchor);
                check(
                    anchors
                        .chain(arm.starters.iter().map(|starter| starter.anchor))
                        .all(|anchor| {
                            matches!(
                                arm.items.get(anchor as usize),
                                Some(ItemTemplate::Node { .. })
                            )
                        }),
                )?;
                // The children still owed to the open nodes; an item owed to
                // none is a new top-level item.
                let mut owed = 0usize;
                for item in &arm.items {
                    owed = owed.saturating_sub(1);
                    match item {
                        ItemTemplate::Node { node, edges, .. } => {
                            check(edges.iter().all(|&(cell, _)| shared(cell)))?;
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
                    deps: vec![CellRef::Shared(0), CellRef::Local(5)],
                    arms: vec![ArmTemplate {
                        preserve: true,
                        locals: vec![
                            LocalTemplate {
                                slot: 5,
                                init: Some(4),
                            },
                            LocalTemplate {
                                slot: 6,
                                init: None,
                            },
                        ],
                        env: vec![EnvTemplate {
                            slot: 7,
                            field: EnvField::SizeClass,
                            anchor: 0,
                        }],
                        effects: vec![EffectTemplate {
                            effect: ComponentEffect {
                                deps: Some(8),
                                body: 9,
                                run: viso_behavior::EffectRun::Change,
                                resource: Some(viso_behavior::ResourceLoad {
                                    state: 10,
                                    write: 11,
                                    debounce: Some(std::time::Duration::from_millis(250)),
                                    cache_for: None,
                                    cache_errors: false,
                                    keep_latest: true,
                                }),
                            },
                            anchor: 0,
                        }],
                        starters: vec![StarterTemplate {
                            instance: 3,
                            anchor: 0,
                        }],
                        items: vec![
                            ItemTemplate::Node {
                                node: node(AotNodeKind::Flex, 1),
                                edges: vec![(CellRef::Shared(0), 4), (CellRef::Local(6), 1)],
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
                        locals: vec![],
                        env: vec![],
                        effects: vec![],
                        starters: vec![],
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

        let mut dangling = regions.clone();
        dangling.groups[0].slots[1] = SlotTemplate::Region(1);
        let bytes = dangling.encode_to_vec();
        assert!(ViewRegions::decode_from_slice(&bytes).is_err());

        let mut unknown = regions.clone();
        unknown.regions[0].deps[0] = CellRef::Shared(1);
        assert!(ViewRegions::decode_from_slice(&unknown.encode_to_vec()).is_err());

        let mut unordered = regions.clone();
        unordered.regions[0].arms[0].locals.reverse();
        assert!(ViewRegions::decode_from_slice(&unordered.encode_to_vec()).is_err());

        let mut misanchored = regions.clone();
        misanchored.regions[0].arms[0].env[0].anchor = 1;
        assert!(ViewRegions::decode_from_slice(&misanchored.encode_to_vec()).is_err());

        let mut misanchored = regions.clone();
        misanchored.regions[0].arms[0].effects[0].anchor = 1;
        assert!(ViewRegions::decode_from_slice(&misanchored.encode_to_vec()).is_err());

        let mut overlapping = regions;
        overlapping.regions[0].arms[0].env[0].slot = 6;
        assert!(ViewRegions::decode_from_slice(&overlapping.encode_to_vec()).is_err());
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
