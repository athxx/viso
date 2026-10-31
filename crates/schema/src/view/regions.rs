//! A view's control-flow regions: the `if`/`match`/`for` templates the
//! runtime mounts and re-shapes, with their state edges and nested regions,
//! and the item template a `VirtualList` mounts a window of.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};

use crate::aot::{AotNode, decode_terms, encode_terms};
use crate::effect::ComponentEffect;
use crate::env::EnvField;
use crate::length::LengthTerms;
use crate::state::StateKey;
use crate::view::control::Control;
use crate::view::route::Route;

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
#[derive(Debug, Clone, Copy, PartialEq)]
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
    /// The keyed `for` a `VirtualList` (the region's parent) virtualizes: it
    /// mounts only the items in the viewport and its overscan, each on a row
    /// of its own, from the same entries a `for` has.
    Virtual {
        /// The iterable.
        items: u32,
        /// The item key.
        key: u32,
        /// The extent a row not measured yet is estimated at, folded at the
        /// list.
        estimate: LengthTerms,
        /// The entry of an estimate the list reads from a value, in place of
        /// `estimate`, read when the list attaches.
        estimated: Option<u32>,
        /// The rows mounted past each end of the viewport.
        overscan: u32,
        /// Whether an item's row heads a section (called with the item too):
        /// its root's `sticky`.
        sticky: Option<u32>,
        /// Whether each row is as long as the viewport: a pager's pages.
        pages: bool,
        /// How it lays its items in lanes across its axis, if it does.
        lanes: Option<ListLanes>,
        /// Whether an item spans a grid's line (called with the item too):
        /// its root's `list.full_line`.
        full: Option<u32>,
    },
}

/// How a `VirtualList` lays its items in lanes across its axis: a grid or a
/// masonry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ListLanes {
    /// Exactly this many lanes, or `0` for as many at least `min` wide as
    /// fit.
    pub columns: u16,
    /// The narrowest an adaptive lane is.
    pub min: LengthTerms,
    /// The room between two items along the axis.
    pub gap: LengthTerms,
    /// The room between two lanes.
    pub cross_gap: LengthTerms,
    /// Whether each item packs under the lane that ends soonest.
    pub masonry: bool,
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
    /// The node its anchored fields resolve at, by item index in the arm;
    /// [`MOUNT`] for the node the mount is placed under.
    pub anchor: u32,
}

/// The anchor of an instance whose view root is a region, so it has no node
/// of its own in the arm: the mount itself. Its effects and tasks belong to
/// a node outside the tree the mount frees with its content, and its anchored
/// `env` fields resolve at the node the mount is placed under.
pub const MOUNT: u32 = u32::MAX;

/// An effect of a component instance an arm mounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectTemplate {
    /// Its entries and run policy.
    pub effect: ComponentEffect,
    /// The node it is mounted on, the instance's root, by item index in the
    /// arm, or [`MOUNT`]: freeing the node cancels it.
    pub anchor: u32,
}

/// A component instance region content mounts that starts tasks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StarterTemplate {
    /// The inlined instance.
    pub instance: u32,
    /// The instance's root, by item index in the arm, or [`MOUNT`]: it owns
    /// the tasks, so freeing it cancels them.
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
        node: Box<AotNode>,
        /// Its state edges: a cell and the [`DirtyClass`](crate::DirtyClass) bits a change of the
        /// cell marks.
        edges: Vec<(CellRef, u8)>,
        /// Its handler routes.
        routes: Vec<Route>,
        /// Its built-in response, for a native control node.
        control: Option<Box<Control>>,
        /// The key its component's actions and handlers reach it by as a
        /// `NodeRef`, `None` when they reach it by none.
        node_ref: Option<u32>,
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

/// A pre-allocation hint that never trusts a count past a sane ceiling.
fn bounded(count: usize) -> usize {
    count.min(4096)
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
                RegionKind::Virtual {
                    items,
                    key,
                    estimate,
                    estimated,
                    overscan,
                    sticky,
                    pages,
                    lanes,
                    full,
                } => {
                    enc.write_u8(3);
                    enc.write_varint(u64::from(items));
                    enc.write_varint(u64::from(key));
                    encode_terms(&estimate, enc);
                    enc.write_varint(estimated.map_or(0, |at| u64::from(at) + 1));
                    enc.write_varint(u64::from(overscan));
                    enc.write_varint(sticky.map_or(0, |sticky| u64::from(sticky) + 1));
                    enc.write_bool(pages);
                    enc.write_bool(lanes.is_some());
                    if let Some(lanes) = lanes {
                        enc.write_varint(u64::from(lanes.columns));
                        encode_terms(&lanes.min, enc);
                        encode_terms(&lanes.gap, enc);
                        encode_terms(&lanes.cross_gap, enc);
                        enc.write_bool(lanes.masonry);
                    }
                    enc.write_varint(full.map_or(0, |full| u64::from(full) + 1));
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
                            node_ref,
                        } => {
                            enc.write_u8(0);
                            node.encode(enc);
                            enc.write_varint(edges.len() as u64);
                            for &(cell, class) in edges {
                                write_cell(enc, cell);
                                enc.write_u8(class);
                            }
                            enc.write_varint(routes.len() as u64);
                            for route in routes {
                                route.encode(enc);
                            }
                            enc.write_bool(control.is_some());
                            if let Some(control) = control {
                                control.encode(enc);
                            }
                            enc.write_varint(node_ref.map_or(0, |key| u64::from(key) + 1));
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
        let count = dec.read_count(usize::MAX)?;
        let mut states = Vec::with_capacity(bounded(count));
        for _ in 0..count {
            let hi = dec.read_u64()?;
            let lo = dec.read_u64()?;
            states.push(StateKey::from_parts(hi, lo));
        }
        let count = dec.read_count(usize::MAX)?;
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
                3 => RegionKind::Virtual {
                    items: read_u32(dec)?,
                    key: read_u32(dec)?,
                    estimate: decode_terms(dec)?,
                    estimated: read_u32(dec)?.checked_sub(1),
                    overscan: read_u32(dec)?,
                    sticky: read_u32(dec)?.checked_sub(1),
                    pages: dec.read_bool()?,
                    lanes: match dec.read_bool()? {
                        false => None,
                        true => Some(ListLanes {
                            columns: u16::try_from(dec.read_varint()?)
                                .map_err(|_| DecodeError::Malformed { offset })?,
                            min: decode_terms(dec)?,
                            gap: decode_terms(dec)?,
                            cross_gap: decode_terms(dec)?,
                            masonry: dec.read_bool()?,
                        }),
                    },
                    full: read_u32(dec)?.checked_sub(1),
                },
                _ => return Err(DecodeError::Malformed { offset }),
            };
            let count = dec.read_count(usize::MAX)?;
            let mut deps = Vec::with_capacity(bounded(count));
            for _ in 0..count {
                deps.push(read_cell(dec)?);
            }
            let count = dec.read_count(usize::MAX)?;
            let mut arms = Vec::with_capacity(bounded(count));
            for _ in 0..count {
                let preserve = dec.read_bool()?;
                let count = dec.read_count(usize::MAX)?;
                let mut locals = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    locals.push(LocalTemplate {
                        slot: read_u32(dec)?,
                        init: read_u32(dec)?.checked_sub(1),
                    });
                }
                let count = dec.read_count(usize::MAX)?;
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
                let count = dec.read_count(usize::MAX)?;
                let mut effects = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    effects.push(EffectTemplate {
                        effect: ComponentEffect::decode(dec)?,
                        anchor: read_u32(dec)?,
                    });
                }
                let count = dec.read_count(usize::MAX)?;
                let mut starters = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    starters.push(StarterTemplate {
                        instance: read_u32(dec)?,
                        anchor: read_u32(dec)?,
                    });
                }
                let count = dec.read_count(usize::MAX)?;
                let mut items = Vec::with_capacity(bounded(count));
                for _ in 0..count {
                    let offset = dec.position();
                    items.push(match dec.read_u8()? {
                        0 => {
                            let node = Box::new(AotNode::decode(dec)?);
                            let count = dec.read_count(usize::MAX)?;
                            let mut edges = Vec::with_capacity(bounded(count));
                            for _ in 0..count {
                                edges.push((read_cell(dec)?, dec.read_u8()?));
                            }
                            let count = dec.read_count(usize::MAX)?;
                            let mut routes = Vec::with_capacity(bounded(count));
                            for _ in 0..count {
                                routes.push(Route::decode(dec)?);
                            }
                            let control = if dec.read_bool()? {
                                Some(Box::new(Control::decode(dec)?))
                            } else {
                                None
                            };
                            let node_ref = match dec.read_varint()? {
                                0 => None,
                                key => Some(
                                    u32::try_from(key - 1)
                                        .map_err(|_| DecodeError::Malformed { offset })?,
                                ),
                            };
                            ItemTemplate::Node {
                                node,
                                edges,
                                routes,
                                control,
                                node_ref,
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
        let count = dec.read_count(usize::MAX)?;
        let mut groups = Vec::with_capacity(bounded(count));
        for _ in 0..count {
            let parent = read_u32(dec)?;
            let count = dec.read_count(usize::MAX)?;
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
                RegionKind::Virtual { estimate, .. } => {
                    region.arms.len() == 1
                        && [
                            estimate.dp,
                            estimate.px,
                            estimate.sp,
                            estimate.em,
                            estimate.pct,
                        ]
                        .iter()
                        .all(|term| term.is_finite())
                }
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
                        && (env.anchor == MOUNT
                            || matches!(
                                arm.items.get(env.anchor as usize),
                                Some(ItemTemplate::Node { .. })
                            ))
                }))?;
                let anchors = arm.effects.iter().map(|effect| effect.anchor);
                check(
                    anchors
                        .chain(arm.starters.iter().map(|starter| starter.anchor))
                        .all(|anchor| {
                            anchor == MOUNT
                                || matches!(
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
    use super::*;
    use crate::aot::{AotNodeKind, AotStyle};
    use crate::view::route::EventRoute;

    fn node(kind: AotNodeKind, child_count: u32) -> AotNode {
        AotNode {
            kind,
            style: AotStyle::default(),
            lengths: None,
            rules: None,
            grid: None,
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
                                run: crate::effect::EffectRun::Change,
                                resource: Some(crate::effect::ResourceLoad {
                                    state: 10,
                                    write: 11,
                                    debounce: Some(core::time::Duration::from_millis(250)),
                                    cache_for: None,
                                    cache_errors: false,
                                    keep_latest: true,
                                    shared: Some(2),
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
                                node: Box::new(node(AotNodeKind::Flex, 1)),
                                edges: vec![(CellRef::Shared(0), 4), (CellRef::Local(6), 1)],
                                routes: vec![Route {
                                    event: EventRoute::Click,
                                    handler: 1,
                                    capture: true,
                                }],
                                control: None,
                                node_ref: Some(1 << 16 | 2),
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
                            node: Box::new(node(AotNodeKind::Leaf, 0)),
                            edges: vec![],
                            routes: vec![],
                            control: Some(Box::new(Control::new(crate::view::ControlKind::Check))),
                            node_ref: None,
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

        let mut rows = regions.clone();
        rows.regions[1].kind = RegionKind::Virtual {
            items: 2,
            key: 3,
            estimate: LengthTerms::dp(30.0) + LengthTerms::em(1.5),
            overscan: 4,
            estimated: Some(7),
            sticky: Some(5),
            pages: true,
            lanes: Some(ListLanes {
                columns: 0,
                min: LengthTerms::dp(96.0),
                gap: LengthTerms::dp(8.0),
                cross_gap: LengthTerms::sp(4.0),
                masonry: true,
            }),
            full: Some(6),
        };
        assert_eq!(
            ViewRegions::decode_from_slice(&rows.encode_to_vec()),
            Ok(rows.clone())
        );
        rows.regions[1].kind = RegionKind::Virtual {
            items: 2,
            key: 3,
            estimate: LengthTerms::dp(f32::NAN),
            estimated: None,
            overscan: 4,
            sticky: None,
            pages: false,
            lanes: None,
            full: None,
        };
        assert!(ViewRegions::decode_from_slice(&rows.encode_to_vec()).is_err());

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
}
