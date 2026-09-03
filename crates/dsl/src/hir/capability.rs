//! Capability sets — the compile-time capability model (spec capability section).
//!
//! A capability is a named permission a callable needs (network, timer, filesystem, …).
//! The doc's model is a *set*, not a string bag checked at runtime: a call site's
//! required capabilities are the union of what the callables it invokes declare, a
//! caller missing one of a callee's capabilities is a compile error (`E2601`), and a
//! private callable's capability set is inferred from the typed call graph rather than
//! hand-annotated (an explicit `requires {}` is a public contract / upper-bound
//! assertion, not mandatory boilerplate).
//!
//! This section lands the set type — an ordered set so a component's capability facts are
//! deterministic (spec determinism requirement) — and the union operation the call-graph
//! propagation needs. The graph propagation, the `requires {}` assertion check, and the
//! missing-capability `E2601` diagnostic land with the capability-inference pass (the
//! `lower` end-to-end section). Until a native schema exists to declare which native
//! calls confer which capability, an inferred set is empty; the machinery is ready for
//! that source when it lands.

use std::collections::BTreeSet;

use crate::diag::Diagnostic;
use crate::syntax::TextRange;

/// A deterministic set of capability names.
///
/// Ordered (a `BTreeSet`) so a component's capability set renders and compares
/// identically across runs — the compilation pipeline must be deterministic. Capability
/// names are the doc's dotted paths (`network`, `timer`, `filesystem`, …), interned as
/// owned strings on this cold path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    names: BTreeSet<String>,
}

impl CapabilitySet {
    /// An empty capability set.
    pub fn new() -> Self {
        CapabilitySet {
            names: BTreeSet::new(),
        }
    }

    /// Whether the set is empty (the common case for a core callable with no native
    /// calls in this slice).
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// The number of capabilities in the set.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether the set contains `name`.
    pub fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Inserts a capability, returning whether it was newly added.
    pub fn insert(&mut self, name: impl Into<String>) -> bool {
        self.names.insert(name.into())
    }

    /// Folds every capability of `other` into this set (the union used when a call site
    /// absorbs a callee's requirements).
    pub fn union_with(&mut self, other: &CapabilitySet) {
        for name in &other.names {
            self.names.insert(name.clone());
        }
    }

    /// The capabilities in `other` that this set does not contain — the missing
    /// capabilities a caller would need for `E2601`, in deterministic order.
    pub fn missing_from(&self, required: &CapabilitySet) -> Vec<String> {
        required
            .names
            .iter()
            .filter(|n| !self.names.contains(*n))
            .cloned()
            .collect()
    }

    /// The capability names in order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }

    /// Whether every capability of `self` is also in `bound` — i.e. `self` is within the
    /// upper bound `bound`. A `requires {}` clause is an upper bound: an inferred set that
    /// is a subset of the declared set honours the contract.
    pub fn is_subset_of(&self, bound: &CapabilitySet) -> bool {
        self.names.is_subset(&bound.names)
    }
}

/// One callable in the capability call graph: what it *directly* confers (its own native
/// capabilities — empty until a native schema exists), whether it declared a `requires {}`
/// upper bound, and the callables it calls.
///
/// Indices into the graph's callable slice identify nodes; a call edge is such an index.
/// The graph is deliberately index-based rather than symbol-keyed so the propagation is a
/// dense fixed point over a `Vec`, and so a caller that has no symbol (an anonymous event
/// handler) still participates.
pub struct CapabilityNode {
    /// The capabilities this callable confers on its own (native calls in its body). Empty
    /// in this slice — no native schema declares conferrals yet — but the machinery unions
    /// it in so the source drops in without a propagation change.
    pub direct: CapabilitySet,
    /// The declared `requires {}` upper bound, when the callable wrote one, with the span
    /// to anchor an `E2601` on. `None` for a callable with no clause (its inferred set is
    /// its contract).
    pub declared: Option<(CapabilitySet, TextRange)>,
    /// The indices of the callables this one calls, within the same graph.
    pub calls: Vec<usize>,
}

/// Propagates capabilities up a call graph to a fixed point and checks each declared
/// `requires {}` upper bound.
///
/// The inferred set of a callable is the union of its own direct conferrals and the
/// inferred sets of everything it (transitively) calls — the doc's "a call site's required
/// capabilities are the union of its callees' sets". Iterating the union to a fixed point
/// makes it transitive and terminates on cycles (a recursive or mutually-recursive call
/// group converges once no set grows). Then, for every callable that declared a
/// `requires {}` clause, the inferred set must be a subset of the declared set: a callable
/// that transitively needs a capability it did not publicly declare violates its contract
/// and earns an `E2601`, in deterministic missing-capability order.
///
/// Returns the inferred set per callable (index-parallel to `nodes`) so the caller can
/// write each into its `HirCallable` metadata, plus the diagnostics.
pub fn propagate(nodes: &[CapabilityNode]) -> (Vec<CapabilitySet>, Vec<Diagnostic>) {
    // Seed each callable with its own direct conferrals, then union callee sets in until a
    // full pass adds nothing. Each callable's set only grows, so the fixed point is reached
    // in at most `nodes.len()` passes even through cycles.
    let mut inferred: Vec<CapabilitySet> = nodes.iter().map(|n| n.direct.clone()).collect();
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..nodes.len() {
            for &callee in &nodes[i].calls {
                // Union the callee's current set into the caller's; note growth.
                let before = inferred[i].len();
                let callee_set = inferred[callee].clone();
                inferred[i].union_with(&callee_set);
                if inferred[i].len() != before {
                    changed = true;
                }
            }
        }
    }

    let mut diagnostics = Vec::new();
    for (i, node) in nodes.iter().enumerate() {
        if let Some((declared, span)) = &node.declared {
            let missing = declared.missing_from(&inferred[i]);
            if !missing.is_empty() {
                diagnostics.push(Diagnostic::error(
                    "E2601",
                    *span,
                    format!(
                        "this callable requires the capability {} but its `requires` clause does not declare {}",
                        quote_list(&missing),
                        if missing.len() == 1 { "it" } else { "them" }
                    ),
                ));
            }
        }
    }
    (inferred, diagnostics)
}

/// Renders a list of capability names as a comma-separated, back-tick-quoted list for a
/// diagnostic message.
fn quote_list(names: &[String]) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_set_is_empty() {
        let set = CapabilitySet::new();
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn insert_reports_novelty_and_dedups() {
        let mut set = CapabilitySet::new();
        assert!(set.insert("network"));
        assert!(!set.insert("network"));
        assert_eq!(set.len(), 1);
        assert!(set.contains("network"));
    }

    #[test]
    fn union_is_the_set_union() {
        let mut a = CapabilitySet::new();
        a.insert("timer");
        let mut b = CapabilitySet::new();
        b.insert("network");
        b.insert("timer");
        a.union_with(&b);
        assert_eq!(a.len(), 2);
        assert!(a.contains("network") && a.contains("timer"));
    }

    #[test]
    fn missing_is_required_minus_held_in_order() {
        let mut held = CapabilitySet::new();
        held.insert("timer");
        let mut required = CapabilitySet::new();
        required.insert("network");
        required.insert("filesystem");
        required.insert("timer");
        // Deterministic order (BTreeSet): filesystem before network.
        assert_eq!(held.missing_from(&required), vec!["filesystem", "network"]);
    }

    #[test]
    fn iter_yields_names_in_order() {
        let mut set = CapabilitySet::new();
        set.insert("timer");
        set.insert("network");
        let names: Vec<&str> = set.iter().collect();
        assert_eq!(names, vec!["network", "timer"]);
    }

    #[test]
    fn subset_is_the_upper_bound_check() {
        let mut inferred = CapabilitySet::new();
        inferred.insert("network");
        let mut declared = CapabilitySet::new();
        declared.insert("network");
        declared.insert("timer");
        assert!(inferred.is_subset_of(&declared));
        // The other direction fails: inferred needs more than declared.
        assert!(!declared.is_subset_of(&inferred));
    }

    /// A capability set with a single named capability.
    fn cap(name: &str) -> CapabilitySet {
        let mut set = CapabilitySet::new();
        set.insert(name);
        set
    }

    #[test]
    fn propagation_unions_callee_sets_transitively() {
        // 0 -> 1 -> 2; only 2 confers `network` directly. Both 0 and 1 inherit it.
        let span = TextRange::new(0.into(), 1.into());
        let nodes = vec![
            CapabilityNode {
                direct: CapabilitySet::new(),
                declared: None,
                calls: vec![1],
            },
            CapabilityNode {
                direct: CapabilitySet::new(),
                declared: None,
                calls: vec![2],
            },
            CapabilityNode {
                direct: cap("network"),
                declared: None,
                calls: vec![],
            },
        ];
        let (inferred, diags) = propagate(&nodes);
        assert!(diags.is_empty());
        assert!(inferred[0].contains("network"));
        assert!(inferred[1].contains("network"));
        assert!(inferred[2].contains("network"));
        let _ = span;
    }

    #[test]
    fn propagation_terminates_on_a_cycle() {
        // 0 <-> 1 mutually recursive; 0 confers `timer`, 1 confers `network`. Both end up
        // with the union, and the fixed point terminates.
        let nodes = vec![
            CapabilityNode {
                direct: cap("timer"),
                declared: None,
                calls: vec![1],
            },
            CapabilityNode {
                direct: cap("network"),
                declared: None,
                calls: vec![0],
            },
        ];
        let (inferred, diags) = propagate(&nodes);
        assert!(diags.is_empty());
        assert!(inferred[0].contains("timer") && inferred[0].contains("network"));
        assert!(inferred[1].contains("timer") && inferred[1].contains("network"));
    }

    #[test]
    fn a_requires_clause_under_declaring_is_e2601() {
        // Callable 0 transitively needs `network` (via callee 1) but declared only `timer`.
        let span = TextRange::new(0.into(), 4.into());
        let nodes = vec![
            CapabilityNode {
                direct: CapabilitySet::new(),
                declared: Some((cap("timer"), span)),
                calls: vec![1],
            },
            CapabilityNode {
                direct: cap("network"),
                declared: None,
                calls: vec![],
            },
        ];
        let (_, diags) = propagate(&nodes);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, "E2601");
        assert_eq!(diags[0].primary, span);
    }

    #[test]
    fn a_requires_clause_covering_the_inferred_set_is_ok() {
        let span = TextRange::new(0.into(), 4.into());
        let nodes = vec![
            CapabilityNode {
                direct: CapabilitySet::new(),
                declared: Some((cap("network"), span)),
                calls: vec![1],
            },
            CapabilityNode {
                direct: cap("network"),
                declared: None,
                calls: vec![],
            },
        ];
        let (_, diags) = propagate(&nodes);
        assert!(diags.is_empty());
    }
}
