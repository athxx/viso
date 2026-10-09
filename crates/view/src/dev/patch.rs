//! The typed UI patch (`Viso_Hot_Reload.md` §9, §12, §13, §35): what the host
//! sends for each `.vs` file it reloads, and the runtime commits with no
//! compiler present.
//!
//! A view's patch is the candidate view in its release form, a
//! [`ViewPackage`], and the [`ReloadPlan`] that moves the file's running
//! mounts onto it: whether the candidate keeps every node of the last-good
//! template, the node state each kept node carries, and what happens to each
//! state cell, by durable [`StateKey`]. The host plans by compile-stable
//! identity; the plan names a static node by its pre-order index among the
//! view's static nodes, and a node a control-flow region mounts by the
//! region, arm and arm item of its template. No source text, name lookup or
//! template diff reaches the runtime.

use viso_behavior::native::MigratableState;
use viso_behavior::retype::Retyping;
use viso_ende::{Decode, Decoder, Encode, Encoder};
use viso_ui::StateValue;
use viso_ui::aot::AotNode;
use viso_ui::state::StateKey;

use super::wire::{
    FileId, MAX_CODE, MAX_FILES, MAX_LOG, MAX_NAME, WireError, malformed, read_file, read_list,
    read_option, read_string, read_u32, write_string,
};
use crate::ViewPackage;
use crate::package::{read_state_value, write_state_value};

/// The most node carries, state plans, structural ops or subtree nodes one
/// view's plan holds.
pub const MAX_PLAN_ENTRIES: usize = 1 << 16;

/// The `ui` section of a patch: each view it reloads.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct UiPatch {
    pub views: Vec<ViewPatch>,
}

/// One reloaded `.vs` file.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewPatch {
    pub file: FileId,
    /// The candidate view, in the form a release build embeds.
    pub package: ViewPackage,
    /// How each mount of the file moves from its last-good view to the
    /// candidate.
    pub plan: ReloadPlan,
}

/// How the mounts of a file move to its candidate view.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReloadPlan {
    /// Whether the candidate keeps every node of the last-good template in
    /// place: a mount without regions then restyles its nodes where they are
    /// instead of rebuilding.
    pub preserving: bool,
    /// A region-free mount's structural edit as node-level operations on the
    /// last-good tree, instead of freeing and rebuilding the whole view: a
    /// node no operation names keeps its live [`NodeId`](viso_ui::NodeId), so
    /// its state is never disturbed. Empty when `preserving`, or when a
    /// region makes the whole-view rebuild the only path (`commit.rs`'s
    /// `apply_structural` falls back to it whenever this is empty).
    pub structural: Vec<StructuralOp>,
    /// The kept nodes whose live state carries, and the position every other
    /// kept node's unchanged identity moves to, when the mount rebuilds.
    pub nodes: Vec<NodeCarry>,
    /// Every state cell of either view, and what happens to it.
    pub states: Vec<StatePlan>,
}

impl ReloadPlan {
    /// The plan rebuilding a mount into `package` with no last-good to plan
    /// against: every node rebuilt, every cell started from its initializer.
    /// A runtime rebuilds a mount built before its file was patched with it,
    /// and a host whose last-good of a file is unknown sends it.
    pub fn fresh(package: &ViewPackage) -> ReloadPlan {
        let mut states: Vec<StatePlan> = package
            .states
            .iter()
            .map(|state| StatePlan {
                key: state.key,
                action: StateAction::New,
                initial: (!state.tracked).then_some(state.initial),
                from_slot: None,
                slot: state.slot,
                retype: None,
            })
            .collect();
        for edge in &package.ui.edges {
            if !states.iter().any(|state| state.key == edge.state) {
                states.push(StatePlan {
                    key: edge.state,
                    action: StateAction::New,
                    initial: None,
                    from_slot: None,
                    slot: None,
                    retype: None,
                });
            }
        }
        ReloadPlan {
            preserving: false,
            structural: Vec::new(),
            nodes: Vec::new(),
            states,
        }
    }
}

/// A node of a view's template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRef {
    /// A static node, by its pre-order index among the view's static nodes.
    Static(u32),
    /// A node item `item` of arm `arm` of region `region` mounts.
    Region { region: u32, arm: u32, item: u32 },
}

/// A kept node whose live state carries from its last-good node to its
/// candidate node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeCarry {
    pub from: NodeRef,
    pub to: NodeRef,
    /// The state its widget schema marks migratable.
    pub carries: MigratableState,
}

/// A node-level mutation of the last-good tree (`Viso_Hot_Reload.md` §12,
/// §66): every [`NodeRef`] here names a node of the *last-good* tree, the one
/// the runtime already holds live when it applies the plan, so each
/// operation resolves independently of the order the runtime runs them in —
/// a kept node's live id never moves until something names it.
#[derive(Debug, Clone, PartialEq)]
pub enum StructuralOp {
    /// The candidate drops `node`: free its subtree.
    Remove { node: NodeRef },
    /// The candidate's node at `node`'s position changed type: free its
    /// subtree and build `subtree` in its place, at the candidate's static
    /// ordinals `start..start + subtree.len()`.
    Replace {
        node: NodeRef,
        start: u32,
        subtree: Vec<AotNode>,
    },
    /// The candidate adds a node `last_good` never had: build `subtree`, at
    /// the candidate's static ordinals `start..start + subtree.len()`, and
    /// attach it under `parent` immediately before `before` (or after every
    /// other child, when `None`).
    Insert {
        parent: NodeRef,
        before: Option<NodeRef>,
        start: u32,
        subtree: Vec<AotNode>,
    },
}

/// What happens to a state cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateAction {
    /// The state keeps its type and cell form: its value stays.
    Keep,
    /// Its value converts into its new type by its [`RetypePlan`].
    Convert,
    /// Its value does not convert: it restarts from its initializer.
    Reset,
    /// It is new: its cell starts from its initializer.
    New,
}

/// One state cell of the candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct StatePlan {
    pub key: StateKey,
    pub action: StateAction,
    /// The value its candidate cell starts from, `None` for a cell holding a
    /// revision of a state no cell holds (a string, a list), or a source the
    /// reload does not initialize.
    pub initial: Option<StateValue>,
    /// Its behavior slot in the last-good component.
    pub from_slot: Option<u32>,
    /// Its behavior slot in the candidate component.
    pub slot: Option<u32>,
    /// How a converted or reset state's value retypes.
    pub retype: Option<Box<RetypePlan>>,
}

impl Eq for StatePlan {}

/// A kept state whose type, or the form its cell holds, changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetypePlan {
    /// How its live value converts, `None` when its old type does not.
    pub conversion: Option<Retyping>,
    /// The `@migrate` function a value `conversion` does not carry goes
    /// through: how the value converts into its parameter, and its chunk.
    pub migrator: Option<(Retyping, u32)>,
    /// Whether its last-good cell held its value rather than a revision.
    pub held: bool,
    /// Its name in the candidate, its old and new types as source spells
    /// them, and the byte range of its declaration, for the notice its reset
    /// raises.
    pub name: String,
    pub from: String,
    pub to: String,
    pub at: (u32, u32),
}

pub(super) fn write_ui(enc: &mut Encoder, ui: &UiPatch) {
    enc.write_varint(ui.views.len() as u64);
    for view in &ui.views {
        enc.write_varint(u64::from(view.file.0));
        view.package.encode(enc);
        write_plan(enc, &view.plan);
    }
}

pub(super) fn read_ui(dec: &mut Decoder<'_>) -> Result<UiPatch, WireError> {
    let views = read_list(dec, MAX_FILES, "patched views", |dec| {
        Ok(ViewPatch {
            file: read_file(dec)?,
            package: ViewPackage::decode(dec)?,
            plan: read_plan(dec)?,
        })
    })?;
    Ok(UiPatch { views })
}

fn write_plan(enc: &mut Encoder, plan: &ReloadPlan) {
    enc.write_bool(plan.preserving);
    enc.write_varint(plan.structural.len() as u64);
    for op in &plan.structural {
        write_structural_op(enc, op);
    }
    enc.write_varint(plan.nodes.len() as u64);
    for carry in &plan.nodes {
        write_node(enc, carry.from);
        write_node(enc, carry.to);
        enc.write_u8(carry.carries.bits());
    }
    enc.write_varint(plan.states.len() as u64);
    for state in &plan.states {
        enc.write_u64(state.key.hi);
        enc.write_u64(state.key.lo);
        enc.write_u8(match state.action {
            StateAction::Keep => 0,
            StateAction::Convert => 1,
            StateAction::Reset => 2,
            StateAction::New => 3,
        });
        match state.initial {
            Some(initial) => {
                enc.write_u8(1);
                write_state_value(enc, initial);
            }
            None => enc.write_u8(0),
        }
        for slot in [state.from_slot, state.slot] {
            enc.write_varint(slot.map_or(0, |slot| u64::from(slot) + 1));
        }
        match &state.retype {
            Some(retype) => {
                enc.write_u8(1);
                write_retype(enc, retype);
            }
            None => enc.write_u8(0),
        }
    }
}

fn read_plan(dec: &mut Decoder<'_>) -> Result<ReloadPlan, WireError> {
    let preserving = dec.read_bool()?;
    let structural = read_list(dec, MAX_PLAN_ENTRIES, "structural ops", read_structural_op)?;
    let nodes = read_list(dec, MAX_PLAN_ENTRIES, "node carries", |dec| {
        Ok(NodeCarry {
            from: read_node(dec)?,
            to: read_node(dec)?,
            carries: MigratableState::from_bits(dec.read_u8()?).ok_or_else(|| malformed(dec))?,
        })
    })?;
    let states = read_list(dec, MAX_PLAN_ENTRIES, "state plans", |dec| {
        let key = StateKey::from_parts(dec.read_u64()?, dec.read_u64()?);
        let action = match dec.read_u8()? {
            0 => StateAction::Keep,
            1 => StateAction::Convert,
            2 => StateAction::Reset,
            3 => StateAction::New,
            _ => return Err(malformed(dec)),
        };
        let initial = read_option(dec, |dec| Ok(read_state_value(dec)?))?;
        let slot = |dec: &mut Decoder<'_>| read_u32(dec).map(|slot| slot.checked_sub(1));
        let from_slot = slot(dec)?;
        let candidate_slot = slot(dec)?;
        let retype = read_option(dec, |dec| read_retype(dec).map(Box::new))?;
        Ok(StatePlan {
            key,
            action,
            initial,
            from_slot,
            slot: candidate_slot,
            retype,
        })
    })?;
    Ok(ReloadPlan {
        preserving,
        structural,
        nodes,
        states,
    })
}

fn write_node(enc: &mut Encoder, node: NodeRef) {
    match node {
        NodeRef::Static(index) => {
            enc.write_u8(0);
            enc.write_varint(u64::from(index));
        }
        NodeRef::Region { region, arm, item } => {
            enc.write_u8(1);
            for part in [region, arm, item] {
                enc.write_varint(u64::from(part));
            }
        }
    }
}

fn read_node(dec: &mut Decoder<'_>) -> Result<NodeRef, WireError> {
    Ok(match dec.read_u8()? {
        0 => NodeRef::Static(read_u32(dec)?),
        1 => NodeRef::Region {
            region: read_u32(dec)?,
            arm: read_u32(dec)?,
            item: read_u32(dec)?,
        },
        _ => return Err(malformed(dec)),
    })
}

fn write_structural_op(enc: &mut Encoder, op: &StructuralOp) {
    match op {
        StructuralOp::Remove { node } => {
            enc.write_u8(0);
            write_node(enc, *node);
        }
        StructuralOp::Replace {
            node,
            start,
            subtree,
        } => {
            enc.write_u8(1);
            write_node(enc, *node);
            write_subtree(enc, *start, subtree);
        }
        StructuralOp::Insert {
            parent,
            before,
            start,
            subtree,
        } => {
            enc.write_u8(2);
            write_node(enc, *parent);
            match before {
                Some(before) => {
                    enc.write_u8(1);
                    write_node(enc, *before);
                }
                None => enc.write_u8(0),
            }
            write_subtree(enc, *start, subtree);
        }
    }
}

fn read_structural_op(dec: &mut Decoder<'_>) -> Result<StructuralOp, WireError> {
    Ok(match dec.read_u8()? {
        0 => StructuralOp::Remove {
            node: read_node(dec)?,
        },
        1 => {
            let node = read_node(dec)?;
            let (start, subtree) = read_subtree(dec)?;
            StructuralOp::Replace {
                node,
                start,
                subtree,
            }
        }
        2 => {
            let parent = read_node(dec)?;
            let before = match dec.read_u8()? {
                0 => None,
                1 => Some(read_node(dec)?),
                _ => return Err(malformed(dec)),
            };
            let (start, subtree) = read_subtree(dec)?;
            StructuralOp::Insert {
                parent,
                before,
                start,
                subtree,
            }
        }
        _ => return Err(malformed(dec)),
    })
}

fn write_subtree(enc: &mut Encoder, start: u32, subtree: &[AotNode]) {
    enc.write_varint(u64::from(start));
    enc.write_varint(subtree.len() as u64);
    for node in subtree {
        node.encode(enc);
    }
}

fn read_subtree(dec: &mut Decoder<'_>) -> Result<(u32, Vec<AotNode>), WireError> {
    let start = read_u32(dec)?;
    let subtree = read_list(dec, MAX_PLAN_ENTRIES, "subtree nodes", |dec| {
        Ok(AotNode::decode(dec)?)
    })?;
    Ok((start, subtree))
}

fn write_retype(enc: &mut Encoder, retype: &RetypePlan) {
    match &retype.conversion {
        Some(conversion) => {
            enc.write_u8(1);
            conversion.encode(enc);
        }
        None => enc.write_u8(0),
    }
    match &retype.migrator {
        Some((conversion, chunk)) => {
            enc.write_u8(1);
            conversion.encode(enc);
            enc.write_varint(u64::from(*chunk));
        }
        None => enc.write_u8(0),
    }
    enc.write_bool(retype.held);
    write_string(enc, &retype.name);
    write_string(enc, &retype.from);
    write_string(enc, &retype.to);
    enc.write_varint(u64::from(retype.at.0));
    enc.write_varint(u64::from(retype.at.1));
}

fn read_retype(dec: &mut Decoder<'_>) -> Result<RetypePlan, WireError> {
    let conversion = read_option(dec, |dec| Ok(Retyping::decode(dec)?))?;
    let migrator = read_option(dec, |dec| Ok((Retyping::decode(dec)?, read_u32(dec)?)))?;
    let held = dec.read_bool()?;
    let name = read_string(dec, MAX_NAME, "state name")?;
    let from = read_string(dec, MAX_LOG, "state type")?;
    let to = read_string(dec, MAX_LOG, "state type")?;
    let at = (read_u32(dec)?, read_u32(dec)?);
    if at.0 > at.1 {
        return Err(malformed(dec));
    }
    Ok(RetypePlan {
        conversion,
        migrator,
        held,
        name,
        from,
        to,
        at,
    })
}

/// The code of the notice a state's reset raises.
pub const RESET_NOTICE: &str = "E5101";

const _: () = assert!(RESET_NOTICE.len() <= MAX_CODE);
