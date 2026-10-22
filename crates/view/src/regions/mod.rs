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
//! - the `for` of a `VirtualList` re-evaluates its items and keys the same
//!   way but mounts only the rows its list shows ([`rows`]);
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

mod rows;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::{Rc, Weak};

use viso_behavior::{Fault, FaultKind, Text, Value};
use viso_ende::{Decode, DecodeError, Decoder, Encoder};
use viso_ui::adaptive::AnchorId;
use viso_ui::aot::build_aot_node;
use viso_ui::input::{ParkedFocus, park_focus, unpark_focus};
use viso_ui::state::StateKey;
use viso_ui::{BuildCx, DirtyClass, NodeId, NodeStore, StateId, StateValue, StructureCx, Template};

use crate::attach::attach_node;
use crate::control::Control;
use crate::effects::mount_effect;
use crate::host::ViewHost;
use crate::nodes::NamedNodes;
use crate::scope::{LocalEnv, Locals, Scope};
use crate::values::{Shown, control_cells};

use rows::Rows;
pub(crate) use rows::key_hash;

pub use viso_schema::view::regions::{
    ArmTemplate, CLOSED, CellRef, EffectTemplate, EnvTemplate, GroupTemplate, HALF_OPEN,
    ItemTemplate, LocalTemplate, RegionKind, RegionTemplate, SlotTemplate, StarterTemplate,
    ViewRegions,
};

/// The most items a range region mounts; a longer range faults instead of
/// allocating an unbounded tree.
pub const MAX_RANGE_ITEMS: i64 = 1 << 20;

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
                ItemTemplate::Node { control, .. } => control.as_deref(),
                _ => None,
            });
        let mut styled = false;
        for control in controls {
            control_cells(control, &Scope::EMPTY, &view, &mut deps);
            styled |= control.reads_state();
        }
        if styled {
            deps.push(cx.store.interaction_cell(cx.states));
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
    let preserves = arms().any(|arm| arm.preserve);
    let trim = preserves.then(|| {
        let trim = cx.store.memory_trim_cell(cx.states);
        deps.push(trim);
        trim
    });
    let templates = regions
        .regions
        .iter()
        .map(|region| vec![Template::new(); region.arms.len()])
        .collect();
    let names = (0..regions.regions.len() as u32)
        .map(|region| names_nodes(&regions, region, 0))
        .collect();
    let mounted = Rc::new_cyclic(|me| {
        RefCell::new(Mounted {
            me: me.clone(),
            templates,
            regions,
            cells: resolved,
            env,
            pulse,
            trim,
            host: Rc::clone(host),
            groups,
            scratch: Vec::new(),
            names,
        })
    });
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

impl ItemKey {
    /// The deepest an aggregate key nests when it is read back.
    const MAX_DEPTH: u32 = 32;

    /// Writes the key to `enc`.
    pub fn write(&self, enc: &mut Encoder) {
        fn write(key: &Key, enc: &mut Encoder) {
            match key {
                Key::Nil => enc.write_u8(0),
                Key::Int(n) => {
                    enc.write_u8(1);
                    enc.write_varint_signed(*n);
                }
                Key::Str(s) => {
                    enc.write_u8(2);
                    enc.write_str(s);
                }
                Key::Agg(tag, fields) => {
                    enc.write_u8(3);
                    enc.write_varint(u64::from(*tag));
                    enc.write_varint(fields.len() as u64);
                    for field in fields {
                        write(field, enc);
                    }
                }
            }
        }
        write(&self.0, enc);
    }

    /// Reads a key [`write`](Self::write) wrote.
    ///
    /// # Errors
    ///
    /// Bytes that are not a key, or one nested deeper than 32.
    pub fn read(dec: &mut Decoder<'_>) -> Result<ItemKey, DecodeError> {
        fn read(dec: &mut Decoder<'_>, depth: u32) -> Result<Key, DecodeError> {
            let malformed = |dec: &Decoder<'_>| DecodeError::Malformed {
                offset: dec.position(),
            };
            Ok(match dec.read_u8()? {
                0 => Key::Nil,
                1 => Key::Int(dec.read_varint_signed()?),
                2 => Key::Str(Rc::new(dec.read_str()?.into())),
                3 if depth < ItemKey::MAX_DEPTH => {
                    let tag = u32::try_from(dec.read_varint()?).map_err(|_| malformed(dec))?;
                    let count = dec.read_varint()?;
                    if count > dec.remaining() as u64 {
                        return Err(malformed(dec));
                    }
                    let fields = (0..count)
                        .map(|_| read(dec, depth + 1))
                        .collect::<Result<_, _>>()?;
                    Key::Agg(tag, fields)
                }
                _ => return Err(malformed(dec)),
            })
        }
        read(dec, 0).map(ItemKey)
    }
}

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
    Str(Rc<Text>),
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
    /// and its nodes, and the kept nodes of each `preserve` arm switched away.
    Arms {
        subject: Value,
        active: Option<usize>,
        live: Option<Box<Frag>>,
        kept: Vec<Option<Kept>>,
    },
    /// A `for`: its items in order.
    List(Vec<Item>),
    /// The `for` of a `VirtualList`: the rows its list mounts.
    Rows(Box<Rows>),
}

/// A `preserve` arm switched away: its detached nodes, the focus they took
/// with them, and when it was left, for the view's preserve budget.
struct Kept {
    frag: Box<Frag>,
    focus: ParkedFocus,
    stamp: u64,
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
    /// The nodes this `for` item names, when its content names any.
    named: Option<Rc<NamedNodes>>,
    /// Each node with handlers, and its item index in the arm template.
    routed: Vec<(NodeId, u32)>,
    /// Each node showing a value of the view.
    shown: Vec<Shown>,
}

impl Mount {
    fn new(region: u32, template: &RegionTemplate) -> Mount {
        let content = match template.kind {
            RegionKind::For { .. } => Content::List(Vec::new()),
            RegionKind::Virtual { .. } => Content::Rows(Box::new(Rows::new(region))),
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
            // The rows sit on the list's canvas, not among the viewport's
            // children the slots order.
            Content::Rows(_) => None,
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
    /// Itself, for the lists its `VirtualList` regions attach to build their
    /// rows through.
    me: Weak<RefCell<Mounted>>,
    regions: Rc<ViewRegions>,
    /// Each arm's template, by region: every mount of an arm (each item of a
    /// `for`) that binds the same way shares its edges (§18.1).
    templates: Vec<Vec<Template>>,
    /// The live cell of each [`ViewRegions::states`] key.
    cells: Vec<Option<StateId>>,
    /// The revision cell of each `env` slot of the view's static instances,
    /// by ascending slot.
    env: Vec<(u32, StateId)>,
    /// The cell raised with every write of a state a mount of region content
    /// keeps, `None` for regions that mount no component instance.
    pulse: Option<StateId>,
    /// The store's memory-trim cell, which frees every kept branch when it
    /// changes; `None` for regions without a `preserve` arm.
    trim: Option<StateId>,
    /// Shared with the view's node handlers: the hook runs after the flush and
    /// a handler from a dispatched event, both on the cold path and neither
    /// inside the other, so the borrow is never re-entered.
    host: Rc<RefCell<ViewHost>>,
    groups: Vec<Group>,
    scratch: Vec<NodeId>,
    /// Whether each region's content, nested regions included, holds a node
    /// its component reaches as a `NodeRef`: each item of a `for` that does
    /// keeps a table of the nodes it names.
    names: Box<[bool]>,
}

impl Mounted {
    /// Appends every node the regions show to `out`: the content of each
    /// chosen arm and of each `for` item, nested regions included; an arm
    /// switched away and kept is not shown.
    /// How many preserved branches the regions keep switched away, nested
    /// ones included.
    pub(crate) fn kept_count(&self) -> usize {
        let mut stamps = Vec::new();
        for group in &self.groups {
            kept_stamps(&group.slots, &mut stamps);
        }
        stamps.len()
    }

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
            me,
            regions,
            templates,
            cells,
            env,
            pulse,
            trim,
            host,
            groups,
            scratch,
            names,
        } = self;
        let mut patch = Patch {
            cx,
            me,
            regions,
            templates,
            cells,
            env,
            pulse: *pulse,
            host,
            changed,
            scratch,
            names,
            freed: false,
        };
        for group in groups.iter_mut() {
            patch.slots(&mut group.slots, &Scope::EMPTY, group.parent, None, false);
        }
        let limit = match trim {
            Some(trim) if changed.contains(trim) => Some(0),
            _ => host.borrow().preserve_budget(),
        };
        if let Some(limit) = limit {
            patch.evict(groups, limit);
        }
        if patch.freed {
            patch.cx.bindings.collect(patch.cx.store, patch.cx.states);
        }
    }
}

/// One walk over a view's regions.
struct Patch<'p, 'c> {
    cx: &'p mut StructureCx<'c>,
    me: &'p Weak<RefCell<Mounted>>,
    regions: &'p ViewRegions,
    templates: &'p mut [Vec<Template>],
    cells: &'p [Option<StateId>],
    env: &'p [(u32, StateId)],
    pulse: Option<StateId>,
    host: &'p Rc<RefCell<ViewHost>>,
    changed: &'p [StateId],
    scratch: &'p mut Vec<NodeId>,
    names: &'p [bool],
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
            (
                RegionKind::Virtual {
                    items,
                    key,
                    estimate,
                    overscan,
                    sticky,
                    pages,
                    lanes,
                    full,
                },
                Content::Rows(rows),
            ) => {
                let list = rows::ListEntries {
                    items,
                    key,
                    estimate,
                    overscan,
                    sticky,
                    pages,
                    lanes,
                    full,
                };
                self.rows(rows, list, scope, parent, touched, force);
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
                    let focus = self.detach(&old, parent);
                    let stamp = self.host.borrow_mut().park_stamp();
                    kept[was] = Some(Kept {
                        frag: old,
                        focus,
                        stamp,
                    });
                } else {
                    self.free(*old);
                }
            }
            *active = next;
            if let Some(arm) = next {
                let frag = match kept[arm].take() {
                    Some(Kept { frag, focus, .. }) => {
                        parked = focus;
                        frag
                    }
                    None => Box::new(self.build(region, arm as u32, scope, subject, parent)),
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
            frag.scope = frag_scope(scope, extra, frag.own.as_ref(), frag.named.as_ref());
            let items = &self.regions.regions[frag.region as usize].arms[frag.arm as usize].items;
            for &(id, index) in &frag.routed {
                if let ItemTemplate::Node {
                    routes, control, ..
                } = &items[index as usize]
                {
                    attach_node(
                        self.cx.store,
                        self.host,
                        id,
                        routes,
                        control.as_deref().cloned(),
                        &frag.scope,
                    );
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
        self.cx.bindings.begin_instance(
            &self.templates[region as usize][arm as usize],
            self.cx.store,
            self.cx.states,
        );
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
        let region_names = matches!(
            regions.regions[region as usize].kind,
            RegionKind::For { .. } | RegionKind::Virtual { .. }
        ) && self.names[region as usize];
        let named = region_names.then(|| Rc::new(NamedNodes::default()));
        let frag_scope = frag_scope(scope, extra, own.as_ref(), named.as_ref());
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
                node_ref,
                ..
            } = &items[index]
            else {
                continue;
            };
            if let Some(key) = *node_ref {
                self.host.borrow().name_node(key, id, &frag_scope);
            }
            for &(cell, class) in edges {
                if let Some(state) = self.cell(cell, &frag_scope) {
                    self.cx
                        .bindings
                        .bind(state, id, DirtyClass::from_bits(class));
                }
            }
            if !routes.is_empty() || control.is_some() {
                attach_node(
                    self.cx.store,
                    self.host,
                    id,
                    routes,
                    control.as_deref().cloned(),
                    &frag_scope,
                );
                routed.push((id, index as u32));
            }
            if let Some(control) = control
                && let Ok(mut host) = self.host.try_borrow_mut()
            {
                let control = Control::clone(control);
                let mut node = Shown::new(id, control, frag_scope.clone(), &host, self.cx);
                node.deliver(self.cx, &mut host);
                shown.push(node);
            }
        }
        self.cx.bindings.end_instance(
            &mut self.templates[region as usize][arm as usize],
            self.cx.store,
            self.cx.states,
        );
        Frag {
            region,
            arm,
            roots,
            groups,
            scope: frag_scope,
            own,
            named,
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
        let init = frag_scope(scope, extra, None, None);
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
                    cells.push(self.cx.states.alloc_local(StateValue::Int(0)));
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
                    Content::Rows(_) => {}
                },
            }
        }
    }

    /// Frees the least recently left of the preserved branches under
    /// `groups` until at most `limit` stay kept.
    fn evict(&mut self, groups: &mut [Group], limit: usize) {
        let mut stamps = Vec::new();
        for group in groups.iter() {
            kept_stamps(&group.slots, &mut stamps);
        }
        if stamps.len() <= limit {
            return;
        }
        stamps.sort_unstable();
        // The oldest kept stamp that survives; every older one goes.
        let survives = stamps
            .get(stamps.len() - limit)
            .copied()
            .unwrap_or(u64::MAX);
        for group in groups.iter_mut() {
            self.evict_slots(&mut group.slots, survives);
        }
    }

    /// Frees every kept branch under `slots` left before `survives`.
    fn evict_slots(&mut self, slots: &mut [Slot], survives: u64) {
        for slot in slots {
            let Slot::Region(mount) = slot else {
                continue;
            };
            match &mut mount.content {
                Content::Arms { live, kept, .. } => {
                    if let Some(frag) = live {
                        self.evict_frag(frag, survives);
                    }
                    for entry in kept.iter_mut() {
                        match entry.take() {
                            Some(old) if old.stamp < survives => self.free(*old.frag),
                            Some(mut young) => {
                                self.evict_frag(&mut young.frag, survives);
                                *entry = Some(young);
                            }
                            None => {}
                        }
                    }
                }
                Content::List(items) => {
                    for item in items {
                        self.evict_frag(&mut item.frag, survives);
                    }
                }
                Content::Rows(rows) => {
                    let set = rows.shared();
                    for row in set.borrow_mut().rows.iter_mut() {
                        self.evict_frag(&mut row.frag, survives);
                    }
                }
            }
        }
    }

    fn evict_frag(&mut self, frag: &mut Frag, survives: u64) {
        self.evict_slots(&mut frag.roots, survives);
        for group in &mut frag.groups {
            self.evict_slots(&mut group.slots, survives);
        }
    }

    /// Frees `frag`'s nodes and instance states, and those its nested regions
    /// keep.
    fn free(&mut self, frag: Frag) {
        self.drop_frag(frag, true);
    }

    /// Frees the instance states `frag` and its nested regions keep, its nodes
    /// already freed with the list row that held them.
    fn release(&mut self, frag: Frag) {
        self.drop_frag(frag, false);
    }

    fn drop_frag(&mut self, frag: Frag, nodes: bool) {
        self.freed = true;
        if let Some(own) = &frag.own {
            own.release(self.cx.states);
        }
        for slot in frag.roots {
            self.drop_slot(slot, nodes);
        }
        for group in frag.groups {
            for slot in group.slots {
                self.drop_slot(slot, nodes);
            }
        }
    }

    fn drop_slot(&mut self, slot: Slot, nodes: bool) {
        match slot {
            Slot::Node(id) => {
                if nodes {
                    self.cx
                        .store
                        .free_tree(id, &mut *self.cx.effects, &mut *self.scratch);
                }
            }
            Slot::Region(mount) => match mount.content {
                Content::Arms { live, kept, .. } => {
                    let kept = kept.into_iter().flatten().map(|kept| kept.frag);
                    for frag in live.into_iter().chain(kept) {
                        self.drop_frag(*frag, nodes);
                    }
                }
                Content::List(items) => {
                    for item in items {
                        self.drop_frag(item.frag, nodes);
                    }
                }
                Content::Rows(rows) => self.drop_rows(*rows),
            },
        }
    }
}

/// The arm state of an `if` or `match` mount, borrowed for a switch.
struct Arms<'m> {
    region: u32,
    template: &'m RegionTemplate,
    active: &'m mut Option<usize>,
    live: &'m mut Option<Box<Frag>>,
    kept: &'m mut Vec<Option<Kept>>,
}

/// Appends the stamp of every kept branch under `slots`, nested ones
/// included.
fn kept_stamps(slots: &[Slot], out: &mut Vec<u64>) {
    fn frag_stamps(frag: &Frag, out: &mut Vec<u64>) {
        kept_stamps(&frag.roots, out);
        for group in &frag.groups {
            kept_stamps(&group.slots, out);
        }
    }
    for slot in slots {
        let Slot::Region(mount) = slot else {
            continue;
        };
        match &mount.content {
            Content::Arms { live, kept, .. } => {
                if let Some(frag) = live {
                    frag_stamps(frag, out);
                }
                for kept in kept.iter().flatten() {
                    out.push(kept.stamp);
                    frag_stamps(&kept.frag, out);
                }
            }
            Content::List(items) => {
                for item in items {
                    frag_stamps(&item.frag, out);
                }
            }
            Content::Rows(rows) => {
                for row in &rows.shared().borrow().rows {
                    frag_stamps(&row.frag, out);
                }
            }
        }
    }
}

/// The revision cell of `env` slot `slot` in `env`, sorted by slot.
fn env_cell(env: &[(u32, StateId)], slot: u32) -> Option<StateId> {
    env.binary_search_by_key(&slot, |&(s, _)| s)
        .ok()
        .map(|index| env[index].1)
}

/// The scope of a mount in `scope` with the binding `extra`, keeping `own`
/// and, for a `for` item naming nodes, `named`.
fn frag_scope(
    scope: &Scope,
    extra: Option<&Value>,
    own: Option<&Rc<Locals>>,
    named: Option<&Rc<NamedNodes>>,
) -> Scope {
    let mut values = Vec::with_capacity(scope.values.len() + 1);
    values.extend_from_slice(&scope.values);
    values.extend(extra.cloned());
    let mut locals = Vec::with_capacity(scope.locals.len() + usize::from(own.is_some()));
    locals.extend(scope.locals.iter().cloned());
    locals.extend(own.cloned());
    let mut nodes = Vec::with_capacity(scope.named.len() + usize::from(named.is_some()));
    nodes.extend(scope.named.iter().cloned());
    nodes.extend(named.cloned());
    Scope {
        values,
        locals,
        named: nodes,
    }
}

/// Whether region `region`'s content, nested regions included, holds a node
/// its component reaches as a `NodeRef`.
fn names_nodes(regions: &ViewRegions, region: u32, depth: u32) -> bool {
    // A nested region has a higher index than its parent, so a malformed
    // cycle ends here.
    if depth as usize > regions.regions.len() {
        return false;
    }
    regions.regions[region as usize]
        .arms
        .iter()
        .flat_map(|arm| &arm.items)
        .any(|item| match item {
            ItemTemplate::Node { node_ref, .. } => node_ref.is_some(),
            ItemTemplate::Region(nested) => names_nodes(regions, *nested, depth + 1),
        })
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
        Content::Rows(rows) => {
            for row in &rows.shared().borrow().rows {
                path.push(ItemKey(row.key.clone()));
                census_frag(regions, &row.frag, store, path, out);
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
        let mut child = arena.first_child(id);
        for _ in 0..node.child_count {
            match child {
                Some(next) => {
                    child = arena.next_sibling(next);
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
            let handle = build_aot_node(cx, &node.record(), |cx| {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_names_only_stable_values() {
        assert_eq!(Key::of(&Value::Int(3)), Some(Key::Int(3)));
        assert_eq!(Key::of(&Value::Float(1.0)), None);
        assert_eq!(
            Key::of(&Value::Str(Rc::new("a".into()))),
            Some(Key::Str(Rc::new(Text::from("a"))))
        );
    }
}
