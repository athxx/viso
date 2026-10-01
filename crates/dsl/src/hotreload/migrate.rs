//! State / focus / scroll migration plan — the third, purely functional stage of
//! the hot reload transaction (architecture section 42.1; AGENTS 21.7).
//!
//! Given the reactive-source identities the last-good tree was built from, the
//! identities the candidate compiled, and the structural patch that aligns the two
//! templates, this stage decides — as plain data, mutating nothing — what happens
//! to each piece of live runtime state across the reload:
//!
//! - **State** is matched by [`SymbolId`], the name-derived compile-stable identity
//!   both templates agree on. A symbol present in both is *kept* (its live cell and
//!   value carry over; the commit's value-level keep/widen/reset is decided against
//!   the live [`viso_ui::StateValue`] variant it finds, using the widen policy the
//!   commit supplies). A symbol only in the candidate is *new* (allocated from its
//!   initializer). A symbol only in the last-good set is *dropped* (its cell is
//!   freed). Matching by identity — not position — is what makes editing one line
//!   leave every other state untouched.
//! - **Focus** survives iff the focused node's template slot survives with the same
//!   identity (it is in the patch's `keep` set). Otherwise focus is lost and the
//!   report records it.
//! - **Scroll** survives per surviving scroll container: a kept slot keeps its
//!   absolute offset (the commit restores it via `set_scroll`); a replaced or
//!   removed slot drops it.
//!
//! This is a pure function of the two identity sets and the [`StructuralPatch`]. It
//! allocates only the returned [`MigrationPlan`] and never touches the live tree —
//! a failure earlier in the pipeline short-circuits before commit, so the live tree
//! stays at last-good with no snapshot (the keep-last-good invariant; see the
//! module docs).

use std::collections::BTreeSet;

use crate::hotreload::compat::Retyping;
use crate::hotreload::diff::StructuralPatch;
use crate::ir::binding_ir::NodeKey;
use crate::resolve::SymbolId;
use crate::syntax::TextRange;

/// What happens to one reactive-source state cell across the reload, keyed by its
/// compile-stable [`SymbolId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateAction {
    /// The symbol is present in both templates with the same type and cell
    /// form: keep the live cell and value as they are.
    Keep,
    /// The symbol is present in both templates and its live value converts
    /// into its new type by its [`Retype`]; a value with no counterpart in the
    /// new type (an active variant that was removed) is reset instead.
    Convert,
    /// The symbol is present in both templates but its old type does not
    /// convert into the new one: reset it to its new initializer and notify.
    Reset,
    /// The symbol is only in the candidate: allocate a fresh cell from its
    /// initializer.
    New,
    /// The symbol is only in the last-good template: free the cell.
    Dropped,
}

/// One state cell's migration decision: its identity and what the commit does with
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StateMigration {
    /// The reactive source's compile-stable identity (the migration key). The
    /// commit bridges this to the runtime `StateKey` by its `(hi, lo)` parts.
    pub symbol: SymbolId,
    /// What happens to the cell.
    pub action: StateAction,
}

/// One surviving scroll container whose absolute offset the commit restores. Only
/// slots that survive with unchanged identity (a `keep`) can restore scroll; a
/// replaced/removed container's offset is dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollMigration {
    /// The surviving template slot holding the scroll offset to restore.
    pub node: NodeKey,
}

/// One behavior state slot whose value the reloaded host carries: the state
/// kept its identity, so its VM value moves from the last-good component's
/// slot to the candidate's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotMigration {
    /// The state's identity.
    pub symbol: SymbolId,
    /// The state's slot in the last-good component.
    pub from: u32,
    /// The state's slot in the candidate component.
    pub to: u32,
}

/// The migration plan: the per-state decisions plus whether focus and each live
/// scroll offset survive. Pure data — the commit stage applies it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationPlan {
    /// Every reactive source across both templates, with its migration action, in
    /// deterministic identity order.
    pub states: Vec<StateMigration>,
    /// The behavior state slots the reloaded host carries, ascending by
    /// candidate slot. The VM values follow the same identity match as the UI
    /// cells, never the state names.
    pub slots: Vec<SlotMigration>,
    /// Scroll containers whose absolute offset the commit restores, in slot order.
    pub scroll: Vec<ScrollMigration>,
    /// Whether the currently focused node survives the reload. `false` means focus
    /// was lost (the focused slot was replaced or removed); the report records it.
    /// `None` means nothing was focused, so there is nothing to migrate.
    pub focus_survives: Option<bool>,
    /// Each converted or reset state's types and conversion, by the order the
    /// states were refined.
    pub retypes: Vec<Retype>,
}

/// A kept state whose type, or the form its cell holds, changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retype {
    pub symbol: SymbolId,
    /// The state's name in the candidate.
    pub name: String,
    /// How its live value converts, `None` when its old type does not convert.
    pub conversion: Option<Retyping>,
    /// Its old and new types, as source spells them.
    pub from: String,
    pub to: String,
    /// Its declaration in the candidate.
    pub at: TextRange,
    /// Its behavior slot in the last-good component.
    pub from_slot: Option<u32>,
    /// Whether its last-good cell held its value rather than a revision.
    pub held: bool,
}

impl MigrationPlan {
    /// The symbols whose cells the commit keeps (present in both templates).
    pub fn kept(&self) -> impl Iterator<Item = SymbolId> + '_ {
        self.states
            .iter()
            .filter(|m| {
                matches!(
                    m.action,
                    StateAction::Keep | StateAction::Convert | StateAction::Reset
                )
            })
            .map(|m| m.symbol)
    }

    /// The retype of the kept state `symbol`, if its type or cell form changed.
    pub fn retype(&self, symbol: SymbolId) -> Option<&Retype> {
        self.retypes.iter().find(|r| r.symbol == symbol)
    }

    /// The behavior slot migration of state `symbol`.
    pub fn slot(&self, symbol: SymbolId) -> Option<SlotMigration> {
        self.slots.iter().copied().find(|s| s.symbol == symbol)
    }

    /// The symbols the commit allocates fresh (candidate-only).
    pub fn added(&self) -> impl Iterator<Item = SymbolId> + '_ {
        self.states
            .iter()
            .filter(|m| m.action == StateAction::New)
            .map(|m| m.symbol)
    }

    /// The symbols the commit frees (last-good-only).
    pub fn dropped(&self) -> impl Iterator<Item = SymbolId> + '_ {
        self.states
            .iter()
            .filter(|m| m.action == StateAction::Dropped)
            .map(|m| m.symbol)
    }
}

/// The live focus/scroll facts the migration must preserve where the structure
/// allows. Supplied by the caller from the runtime it is about to commit into;
/// keeping them as plain inputs keeps this stage a pure function with no dependency
/// on the UI runtime.
#[derive(Debug, Clone, Default)]
pub struct LiveAnchors {
    /// The template slot currently focused, if any. Focus survives iff this slot is
    /// kept.
    pub focused: Option<NodeKey>,
    /// The template slots that currently hold a non-zero scroll offset worth
    /// restoring. A slot's offset is restored iff the slot is kept.
    pub scrolled: Vec<NodeKey>,
}

/// Compute the migration plan from the old/new reactive-source identity sets,
/// the old/new behavior state slots by identity, the structural patch aligning
/// the templates, and the live focus/scroll anchors.
///
/// Pure: reads only its inputs, allocates only the returned plan. State is matched
/// by identity (a `BTreeSet` union so the output is deterministic and each symbol
/// appears once); focus and scroll survive iff their slot is in the patch's `keep`
/// set. The value-level keep/widen/reset choice for a kept symbol is intentionally
/// *not* made here — it needs the live `StateValue`, which only the commit has.
pub fn migrate(
    old_sources: &[SymbolId],
    new_sources: &[SymbolId],
    old_slots: &[(SymbolId, u32)],
    new_slots: &[(SymbolId, u32)],
    patch: &StructuralPatch,
    anchors: &LiveAnchors,
) -> MigrationPlan {
    let old_set: BTreeSet<SymbolId> = old_sources.iter().copied().collect();
    let new_set: BTreeSet<SymbolId> = new_sources.iter().copied().collect();

    // Every symbol across both templates, once, in deterministic identity order.
    let mut states = Vec::new();
    for &symbol in old_set.union(&new_set) {
        let action = match (old_set.contains(&symbol), new_set.contains(&symbol)) {
            (true, true) => StateAction::Keep,
            (false, true) => StateAction::New,
            (true, false) => StateAction::Dropped,
            (false, false) => unreachable!("a symbol in the union is in at least one set"),
        };
        states.push(StateMigration { symbol, action });
    }

    // A behavior slot is carried iff its state is kept: present in both sets.
    let mut slots: Vec<SlotMigration> = new_slots
        .iter()
        .filter(|(symbol, _)| old_set.contains(symbol) && new_set.contains(symbol))
        .filter_map(|&(symbol, to)| {
            let &(_, from) = old_slots.iter().find(|(old, _)| *old == symbol)?;
            Some(SlotMigration { symbol, from, to })
        })
        .collect();
    slots.sort_unstable_by_key(|slot| slot.to);

    // The kept slots — the only slots whose live focus/scroll can be preserved,
    // because only they reuse the same live instance.
    let kept: BTreeSet<NodeKey> = patch.keep.iter().map(|k| k.key).collect();

    let scroll = anchors
        .scrolled
        .iter()
        .filter(|slot| kept.contains(slot))
        .map(|&node| ScrollMigration { node })
        .collect();

    let focus_survives = anchors.focused.map(|slot| kept.contains(&slot));

    MigrationPlan {
        states,
        slots,
        scroll,
        focus_survives,
        retypes: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotreload::diff::KeptNode;

    /// A distinct identity per test name, deterministic and collision-free for the
    /// small sets these tests use.
    fn sym(n: u64) -> SymbolId {
        SymbolId::from_parts(n, 0)
    }

    fn kept_patch(keys: &[u32]) -> StructuralPatch {
        StructuralPatch {
            keep: keys.iter().map(|&k| KeptNode { key: NodeKey(k) }).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_symbol_in_both_is_kept() {
        let plan = migrate(
            &[sym(1)],
            &[sym(1)],
            &[],
            &[],
            &kept_patch(&[]),
            &LiveAnchors::default(),
        );
        assert_eq!(plan.states.len(), 1);
        assert_eq!(plan.states[0].symbol, sym(1));
        assert_eq!(plan.states[0].action, StateAction::Keep);
    }

    #[test]
    fn a_candidate_only_symbol_is_new_and_an_old_only_symbol_is_dropped() {
        // old reads {1, 2}; new reads {2, 3}: 1 dropped, 2 kept, 3 new.
        let plan = migrate(
            &[sym(1), sym(2)],
            &[sym(2), sym(3)],
            &[],
            &[],
            &kept_patch(&[]),
            &LiveAnchors::default(),
        );
        let kept: Vec<_> = plan.kept().collect();
        let added: Vec<_> = plan.added().collect();
        let dropped: Vec<_> = plan.dropped().collect();
        assert_eq!(kept, vec![sym(2)]);
        assert_eq!(added, vec![sym(3)]);
        assert_eq!(dropped, vec![sym(1)]);
    }

    #[test]
    fn a_kept_state_carries_its_behavior_slot_by_identity() {
        // old slots: 1@0, 2@1; new slots: 3@0, 2@1, 1@2. Slot 3 is new.
        let plan = migrate(
            &[sym(1), sym(2)],
            &[sym(1), sym(2), sym(3)],
            &[(sym(1), 0), (sym(2), 1)],
            &[(sym(3), 0), (sym(2), 1), (sym(1), 2)],
            &kept_patch(&[]),
            &LiveAnchors::default(),
        );
        assert_eq!(
            plan.slots,
            vec![
                SlotMigration {
                    symbol: sym(2),
                    from: 1,
                    to: 1,
                },
                SlotMigration {
                    symbol: sym(1),
                    from: 0,
                    to: 2,
                },
            ]
        );
    }

    #[test]
    fn focus_survives_when_its_slot_is_kept() {
        let anchors = LiveAnchors {
            focused: Some(NodeKey(2)),
            scrolled: vec![],
        };
        let plan = migrate(&[], &[], &[], &[], &kept_patch(&[0, 1, 2]), &anchors);
        assert_eq!(plan.focus_survives, Some(true));
    }

    #[test]
    fn focus_is_lost_when_its_slot_is_not_kept() {
        let anchors = LiveAnchors {
            focused: Some(NodeKey(2)),
            scrolled: vec![],
        };
        // Slot 2 replaced/removed → not in keep set → focus lost.
        let plan = migrate(&[], &[], &[], &[], &kept_patch(&[0, 1]), &anchors);
        assert_eq!(plan.focus_survives, Some(false));
    }

    #[test]
    fn nothing_focused_yields_no_focus_migration() {
        let plan = migrate(
            &[],
            &[],
            &[],
            &[],
            &kept_patch(&[0]),
            &LiveAnchors::default(),
        );
        assert_eq!(plan.focus_survives, None);
    }

    #[test]
    fn scroll_is_restored_only_for_surviving_containers() {
        let anchors = LiveAnchors {
            focused: None,
            // Slot 1 survives (kept); slot 3 does not.
            scrolled: vec![NodeKey(1), NodeKey(3)],
        };
        let plan = migrate(&[], &[], &[], &[], &kept_patch(&[0, 1, 2]), &anchors);
        assert_eq!(
            plan.scroll.len(),
            1,
            "only the kept container restores scroll"
        );
        assert_eq!(plan.scroll[0].node, NodeKey(1));
    }
}
