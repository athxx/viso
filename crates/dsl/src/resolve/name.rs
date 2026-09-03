//! Compiler-local name interning.
//!
//! Resolution and every layer above it carry a [`NameId`] — a dense `u32` handle
//! into a per-compilation [`NameInterner`] — rather than repeated `String`s. This
//! keeps symbol tables, scopes, and module paths pointer-free and cheap to compare,
//! while the interner keeps the original text for diagnostics and reverse lookup.
//!
//! This is a cold-path structure: interning happens once during resolution, not on
//! any runtime frame path, so a `HashMap` + owned `String`s are appropriate here
//! (AGENTS section 7.2). The identity that leaves the compiler is [`super::SymbolId`],
//! not `NameId`; a `NameId` is only meaningful within the interner that minted it.

use std::collections::HashMap;

/// A dense handle to an interned name string, unique within one [`NameInterner`].
///
/// Equality and ordering are by handle, not by text — two `NameId`s are equal iff
/// they name the same interned string in the same interner. A `NameId` is *not*
/// stable across interners or across compilations; durable cross-artifact identity
/// is [`super::SymbolId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NameId(u32);

impl NameId {
    /// The raw index, for dense side-table storage keyed by name.
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A string→[`NameId`] interner with reverse lookup.
///
/// Interning is idempotent: the same text always returns the same `NameId` within
/// one interner. The reverse direction ([`NameInterner::text`]) recovers the
/// original string for diagnostics.
#[derive(Debug, Default)]
pub struct NameInterner {
    /// Interned strings, indexed by `NameId`.
    names: Vec<String>,
    /// Reverse map from text to its assigned id.
    lookup: HashMap<String, NameId>,
}

impl NameInterner {
    /// A fresh, empty interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns `text`, returning its stable-within-this-interner [`NameId`].
    ///
    /// Repeated calls with equal text return the same id without allocating again.
    pub fn intern(&mut self, text: &str) -> NameId {
        if let Some(&id) = self.lookup.get(text) {
            return id;
        }
        let id = NameId(self.names.len() as u32);
        self.names.push(text.to_owned());
        self.lookup.insert(text.to_owned(), id);
        id
    }

    /// The text an id was interned from.
    ///
    /// Returns `None` only for an id minted by a different interner (a misuse the
    /// type system does not prevent, since `NameId` carries no interner tag).
    pub fn text(&self, id: NameId) -> Option<&str> {
        self.names.get(id.index()).map(String::as_str)
    }

    /// The number of distinct interned names.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// Whether no name has been interned yet.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_idempotent_and_reversible() {
        let mut interner = NameInterner::new();
        let a = interner.intern("count");
        let b = interner.intern("count");
        let c = interner.intern("label");
        assert_eq!(a, b, "equal text interns to the same id");
        assert_ne!(a, c, "distinct text interns to distinct ids");
        assert_eq!(interner.text(a), Some("count"));
        assert_eq!(interner.text(c), Some("label"));
        assert_eq!(interner.len(), 2, "only two distinct names");
    }
}
