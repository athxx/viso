//! Percent-component flow: which values may carry a `Percent` component, settled across a
//! module so a property or component input without a percent basis can reject one
//! (`E3104`).
//!
//! A value carries a `Percent` component when it spells one (a `%` literal, a
//! `Percent`-typed operand) or names a value that does: a `state` (its initializer or any
//! assignment), a `computed`, a `const`, a local (its initializer, the value its pattern
//! destructures, its assignments), or a record literal whose omitted fields default to
//! one. Composition keeps the component — arithmetic, field access, indexing, record,
//! list and tuple literals, `if`/`match`/block results — while a call or closure yields
//! only what its type says, its body being a unit of its own. An own `input` is never a
//! source here: whether it carries one depends on the caller, so a value reaching one is
//! reported where the caller passes the `Percent`.

use std::collections::{HashMap, HashSet};

use crate::resolve::Resolution;
use crate::syntax::TextRange;

/// What a value may carry a `Percent` component from.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Carry {
    /// Where the value spells a `Percent` itself, if it does.
    pub(crate) spelled: Option<TextRange>,
    /// The names whose values it carries.
    pub(crate) names: Vec<Resolution>,
}

impl Carry {
    /// Adds what `other` carries.
    pub(crate) fn join(&mut self, other: Carry) {
        if self.spelled.is_none() {
            self.spelled = other.spelled;
        }
        for name in other.names {
            if !self.names.contains(&name) {
                self.names.push(name);
            }
        }
    }
}

/// What every name a module's values carry from is defined as, gathered as it lowers.
#[derive(Debug, Default)]
pub(crate) struct PercentSources {
    defs: HashMap<Resolution, Carry>,
}

impl PercentSources {
    /// Records that `name` holds a value carrying `carry` (one of possibly several
    /// definitions: an initializer, each assignment).
    pub(crate) fn define(&mut self, name: Resolution, carry: Carry) {
        self.defs.entry(name).or_default().join(carry);
    }

    /// Records several definitions.
    pub(crate) fn extend(&mut self, defs: impl IntoIterator<Item = (Resolution, Carry)>) {
        for (name, carry) in defs {
            self.define(name, carry);
        }
    }

    /// Settles which names carry a `Percent` component, each with where one is spelled.
    pub(crate) fn solve(self) -> PercentFacts {
        let mut percent: HashMap<Resolution, TextRange> = HashMap::new();
        loop {
            let mut changed = false;
            for (name, carry) in &self.defs {
                if percent.contains_key(name) {
                    continue;
                }
                let origin = carry
                    .spelled
                    .or_else(|| carry.names.iter().find_map(|n| percent.get(n).copied()));
                if let Some(origin) = origin {
                    percent.insert(*name, origin);
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        PercentFacts {
            defs: self.defs,
            percent,
        }
    }
}

/// The settled flow of one module.
#[derive(Debug)]
pub(crate) struct PercentFacts {
    defs: HashMap<Resolution, Carry>,
    percent: HashMap<Resolution, TextRange>,
}

impl PercentFacts {
    /// Where a value carrying `carry` gets a `Percent` component from, if it does.
    pub(crate) fn percent(&self, carry: &Carry) -> Option<TextRange> {
        carry.spelled.or_else(|| {
            carry
                .names
                .iter()
                .find_map(|n| self.percent.get(n).copied())
        })
    }

    /// The names satisfying `wanted` that a value carrying `carry` holds, directly or
    /// through the definitions of the names it holds.
    pub(crate) fn reached(
        &self,
        carry: &Carry,
        wanted: impl Fn(Resolution) -> bool,
    ) -> Vec<Resolution> {
        let mut seen: HashSet<Resolution> = HashSet::new();
        let mut stack: Vec<Resolution> = carry.names.clone();
        let mut found = Vec::new();
        while let Some(name) = stack.pop() {
            if !seen.insert(name) {
                continue;
            }
            if wanted(name) {
                found.push(name);
            }
            if let Some(def) = self.defs.get(&name) {
                stack.extend(def.names.iter().copied());
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::SymbolId;
    use crate::syntax::TextSize;

    fn name(n: u32) -> Resolution {
        Resolution::Symbol(SymbolId::from_parts(n.into(), 0))
    }

    fn at(n: u32) -> TextRange {
        TextRange::new(TextSize::from(n), TextSize::from(n + 1))
    }

    #[test]
    fn a_percent_flows_through_definitions_to_a_fixed_point() {
        let mut sources = PercentSources::default();
        // 3 := 2; 2 := 1; 1 := 50%; 4 := 5 (an input, no definition).
        sources.define(
            name(3),
            Carry {
                spelled: None,
                names: vec![name(2)],
            },
        );
        sources.define(
            name(2),
            Carry {
                spelled: None,
                names: vec![name(1)],
            },
        );
        sources.define(
            name(1),
            Carry {
                spelled: Some(at(7)),
                names: vec![],
            },
        );
        sources.define(
            name(4),
            Carry {
                spelled: None,
                names: vec![name(5)],
            },
        );
        let facts = sources.solve();
        let via = |n| Carry {
            spelled: None,
            names: vec![name(n)],
        };
        assert_eq!(facts.percent(&via(3)), Some(at(7)));
        assert_eq!(facts.percent(&via(4)), None);
        assert_eq!(facts.reached(&via(4), |n| n == name(5)), [name(5)]);
        assert!(facts.reached(&via(3), |n| n == name(5)).is_empty());
    }
}
