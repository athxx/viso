//! State-type compatibility — whether a kept state's live value carries into
//! its recompiled type, and the conversion that carries it. Pure, like the
//! stages before the commit: it compares the two candidates' declarations and
//! returns plain data the commit applies to the live values.
//!
//! The conversion is [`viso_behavior::retype`]'s closed matrix over the two
//! candidates' [`ValueSchema`]s; a value it does not convert is reset to its
//! new initializer, unless a `@migrate` function converts from its old type.

use viso_behavior::retype::ValueSchema;
pub use viso_behavior::retype::{Conversion, FieldSource, IntType, Retyping, Shape, VariantMap};

use crate::hir::Ty;
use crate::hotreload::migrate::{MigrationPlan, Retype, StateAction};
use crate::hotreload::plan::CandidatePlan;
use crate::resolve::SymbolId;

/// Refines each kept state of `migration` against the two candidates' types:
/// a state whose type and cell form are unchanged stays
/// [`Keep`](StateAction::Keep); one whose value converts becomes
/// [`Convert`](StateAction::Convert); any other becomes
/// [`Reset`](StateAction::Reset). Each refined state gets a [`Retype`].
pub fn retype(last_good: &CandidatePlan, candidate: &CandidatePlan, migration: &mut MigrationPlan) {
    for state in &mut migration.states {
        if state.action != StateAction::Keep {
            continue;
        }
        let symbol = state.symbol;
        let (Some((old, _)), Some((new, at))) =
            (last_good.declaration(symbol), candidate.declaration(symbol))
        else {
            continue;
        };
        let held = last_good.initial(symbol).is_some();
        let same_form = held == candidate.initial(symbol).is_some();
        if matches!(old, Ty::Unknown) || matches!(new, Ty::Unknown) {
            continue;
        }
        let (old_schema, new_schema) = (described(last_good, old), described(candidate, new));
        let exact = old_schema.same(&new_schema);
        if exact && same_form {
            continue;
        }
        let from = last_good.schemas.describe(old);
        let (conversion, migrator) = if exact {
            (Some(Retyping::keep()), None)
        } else {
            (
                Retyping::between(&old_schema, &new_schema),
                migrate_fn(candidate, &from, &old_schema, &new_schema),
            )
        };
        state.action = if conversion.is_some() || migrator.is_some() {
            StateAction::Convert
        } else {
            StateAction::Reset
        };
        let name = candidate
            .sources
            .iter()
            .position(|s| *s == symbol)
            .map_or_else(String::new, |i| candidate.source_names[i].clone());
        migration.retypes.push(Retype {
            symbol,
            name,
            conversion,
            migrator,
            from,
            to: candidate.schemas.describe(new),
            at,
            from_slot: slot_of(last_good, symbol),
            held,
        });
    }
}

/// The `@migrate` function of `candidate` that carries a value of `old` (a
/// type the last good build spells `from`) into `new`, and how the value
/// converts into its parameter; the function is its behavior chunk.
fn migrate_fn(
    candidate: &CandidatePlan,
    from: &str,
    old: &ValueSchema,
    new: &ValueSchema,
) -> Option<(Retyping, u32)> {
    candidate.migrators.iter().find_map(|m| {
        if m.from != from || !described(candidate, &m.ret).same(new) {
            return None;
        }
        let conversion = Retyping::between(old, &described(candidate, &m.param))?;
        Some((conversion, m.func.0))
    })
}

/// `ty` of `plan` as a value schema, its record field defaults `plan`'s.
fn described(plan: &CandidatePlan, ty: &Ty) -> ValueSchema {
    plan.schemas.value_schema(ty, &|record, index| {
        plan.field_default(record, index).map(|f| f.0)
    })
}

/// The behavior state slot of `symbol` in `plan`.
fn slot_of(plan: &CandidatePlan, symbol: SymbolId) -> Option<u32> {
    let view = plan.view.as_ref()?;
    view.slots
        .iter()
        .find(|&&(s, _)| s == symbol)
        .map(|&(_, slot)| slot)
}
