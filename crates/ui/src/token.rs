//! Style tokens: a semantic name (`color.bg`, `radius.sm`) resolved to a value
//! through a theme, treated as a *state-like source* for invalidation.
//!
//! A token is authored by name but never looked up by string on a frame path.
//! [`TokenInterner`] folds each `(namespace, name)` to a compact [`TokenId`] at
//! build time (a cold, string-keyed step). A [`Theme`] then maps each `TokenId`
//! to a backing [`StateId`] in the ordinary [`StateStore`] — so the token's
//! *value* lives in a normal state cell, and a theme swap is just a
//! [`StateStore::set`] on that cell. That write rides the same pending / flush /
//! bind machinery a counter uses: a bound node is marked dirty, nothing else
//! recomputes. Resolution ([`Theme::resolve`]) is a `Vec` index plus a state
//! read — never a string map, never a full cascade.
//!
//! [`StateStore`]: crate::state::StateStore
//! [`StateStore::set`]: crate::state::StateStore::set

use crate::state::{StateId, StateStore, StateValue};

/// The semantic namespaces a style token can live in. A token's identity is its
/// namespace plus its interned index, so the same name in two namespaces
/// (`color.bg` vs `spacing.bg`) is two distinct tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenNamespace {
    /// Fill / stroke / text colors (`color.*`).
    Color,
    /// Gaps, paddings, margins (`spacing.*`).
    Spacing,
    /// Corner radii (`radius.*`).
    Radius,
    /// Font sizes, line heights, weights (`typography.*`).
    Typography,
    /// Shadow / layer depth (`elevation.*`).
    Elevation,
    /// Durations, easings (`motion.*`).
    Motion,
}

impl TokenNamespace {
    /// The authoring prefix for this namespace (the part before the dot).
    pub fn prefix(self) -> &'static str {
        match self {
            TokenNamespace::Color => "color",
            TokenNamespace::Spacing => "spacing",
            TokenNamespace::Radius => "radius",
            TokenNamespace::Typography => "typography",
            TokenNamespace::Elevation => "elevation",
            TokenNamespace::Motion => "motion",
        }
    }

    /// This namespace's slot in the per-namespace tables.
    #[inline]
    fn slot(self) -> usize {
        match self {
            TokenNamespace::Color => 0,
            TokenNamespace::Spacing => 1,
            TokenNamespace::Radius => 2,
            TokenNamespace::Typography => 3,
            TokenNamespace::Elevation => 4,
            TokenNamespace::Motion => 5,
        }
    }
}

/// A compact identity for a style token: its namespace plus a dense index
/// assigned by interning. Cheap to copy and compare; carries no string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenId {
    namespace: TokenNamespace,
    index: u32,
}

impl TokenId {
    /// The namespace this token belongs to.
    #[inline]
    pub fn namespace(self) -> TokenNamespace {
        self.namespace
    }
    /// The dense index within the namespace — the key into a [`Theme`]'s
    /// per-namespace table.
    #[inline]
    pub fn index(self) -> u32 {
        self.index
    }
}

/// Interns token names into [`TokenId`]s. Build-time / authoring machinery: the
/// string→id fold uses a per-namespace map that is never touched on a frame
/// path, so a `HashMap` here is within the cold-path allowance.
#[derive(Default)]
pub struct TokenInterner {
    /// Per-namespace name tables; a token's index is its position here.
    names: [Vec<String>; 6],
    /// Per-namespace string→index fold (cold).
    lookup: [std::collections::HashMap<String, u32>; 6],
}

impl TokenInterner {
    /// A fresh interner with no tokens.
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `name` in `namespace`, returning its [`TokenId`]. A repeated name
    /// in the same namespace folds to the same id; the same string in another
    /// namespace is a distinct token.
    pub fn intern(&mut self, namespace: TokenNamespace, name: &str) -> TokenId {
        let slot = namespace.slot();
        if let Some(&index) = self.lookup[slot].get(name) {
            return TokenId { namespace, index };
        }
        let index = self.names[slot].len() as u32;
        self.names[slot].push(name.to_string());
        self.lookup[slot].insert(name.to_string(), index);
        TokenId { namespace, index }
    }

    /// The id a name interns to, if it has been interned. Does not intern.
    pub fn get(&self, namespace: TokenNamespace, name: &str) -> Option<TokenId> {
        self.lookup[namespace.slot()]
            .get(name)
            .map(|&index| TokenId { namespace, index })
    }

    /// How many distinct tokens have been interned in `namespace`.
    pub fn count(&self, namespace: TokenNamespace) -> u32 {
        self.names[namespace.slot()].len() as u32
    }
}

/// A theme: the mapping from each [`TokenId`] to the [`StateStore`] cell that
/// holds its current value. The theme owns *bindings*, not values — the value
/// lives in the state cell, so re-theming a token is [`StateStore::set`] on its
/// cell, flowing through the ordinary flush. Resolution is a `Vec` index and a
/// state read.
///
/// [`StateStore::set`]: crate::state::StateStore::set
#[derive(Default)]
pub struct Theme {
    /// Per-namespace cell tables, indexed by [`TokenId::index`]. A token with no
    /// binding yet is `None`.
    cells: [Vec<Option<StateId>>; 6],
}

impl Theme {
    /// An empty theme; every token resolves to `None` until defined.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `token` to the state cell holding its value. Overwrites any prior
    /// binding for the same token.
    pub fn define(&mut self, token: TokenId, cell: StateId) {
        let table = &mut self.cells[token.namespace.slot()];
        let i = token.index as usize;
        if i >= table.len() {
            table.resize(i + 1, None);
        }
        table[i] = Some(cell);
    }

    /// The state cell backing `token`, if defined — the [`StateId`] to bind a
    /// node against so a swap of this token invalidates that node.
    pub fn cell(&self, token: TokenId) -> Option<StateId> {
        self.cells[token.namespace.slot()]
            .get(token.index as usize)
            .copied()
            .flatten()
    }

    /// Resolve `token` to its current value: index its cell, read the live
    /// state. `None` if the token is undefined or its cell is stale.
    pub fn resolve(&self, token: TokenId, states: &StateStore) -> Option<StateValue> {
        self.cell(token).and_then(|cell| states.get(cell))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{StateStore, StateValue};

    #[test]
    fn intern_folds_repeats_and_separates_namespaces() {
        let mut interner = TokenInterner::new();
        let a = interner.intern(TokenNamespace::Color, "bg");
        let b = interner.intern(TokenNamespace::Color, "bg");
        let c = interner.intern(TokenNamespace::Color, "fg");
        assert_eq!(a, b, "same name in same namespace folds to one id");
        assert_ne!(a, c, "different names get distinct ids");
        assert_eq!(a.namespace(), TokenNamespace::Color);
        // Same string in a different namespace is a distinct token.
        let s = interner.intern(TokenNamespace::Spacing, "bg");
        assert_ne!(a, s);
        assert_eq!(interner.count(TokenNamespace::Color), 2);
        assert_eq!(interner.count(TokenNamespace::Spacing), 1);
        assert_eq!(interner.get(TokenNamespace::Color, "bg"), Some(a));
        assert_eq!(interner.get(TokenNamespace::Color, "missing"), None);
    }

    #[test]
    fn theme_resolves_a_token_through_its_state_cell() {
        let mut states = StateStore::new();
        let mut interner = TokenInterner::new();
        let mut theme = Theme::new();

        let bg = interner.intern(TokenNamespace::Color, "bg");
        let cell = states.alloc(StateValue::Color(0.1, 0.2, 0.3, 1.0));
        theme.define(bg, cell);

        assert_eq!(theme.cell(bg), Some(cell));
        assert_eq!(
            theme.resolve(bg, &states),
            Some(StateValue::Color(0.1, 0.2, 0.3, 1.0))
        );
    }

    #[test]
    fn theme_swap_is_a_state_write() {
        let mut states = StateStore::new();
        let mut interner = TokenInterner::new();
        let mut theme = Theme::new();
        let bg = interner.intern(TokenNamespace::Color, "bg");
        let cell = states.alloc(StateValue::Color(0.0, 0.0, 0.0, 1.0));
        theme.define(bg, cell);

        // A "theme swap" for this token is just writing its backing cell.
        states.set(cell, StateValue::Color(1.0, 1.0, 1.0, 1.0));
        assert_eq!(
            theme.resolve(bg, &states),
            Some(StateValue::Color(1.0, 1.0, 1.0, 1.0))
        );
        assert!(
            states.has_pending(),
            "the swap scheduled a flush like any state write"
        );
    }

    #[test]
    fn undefined_token_resolves_to_none() {
        let states = StateStore::new();
        let mut interner = TokenInterner::new();
        let theme = Theme::new();
        let radius = interner.intern(TokenNamespace::Radius, "sm");
        assert_eq!(theme.resolve(radius, &states), None);
    }
}
