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
}
