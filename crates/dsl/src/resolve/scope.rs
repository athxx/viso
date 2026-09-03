//! Lexical scopes and the per-module symbol table.
//!
//! Resolution splits a module's names across three **namespaces** (doc section 40):
//! a value namespace (state, computed, inputs, consts, functions, actions, tasks),
//! a type namespace (records, enums, type aliases, and the component/system types
//! themselves), and an event namespace (declared events). Two declarations may share
//! a name across namespaces (`state color` and `record Color` coexist) but a repeat
//! within one namespace is a collision.
//!
//! Local scopes use a dense slot idiom: a `view` block, a `for` body, and a
//! `fn`/`action`/handler body each open a scope whose bindings are dense **slots**
//! resolved by walking the scope stack innermost-first. This is a cold-path structure
//! (built once per module during resolution), so `HashMap`/`Vec` are appropriate
//! (AGENTS section 7.2); the durable identity a name resolves *to* is a
//! [`SymbolId`](super::SymbolId), never a slot — slots are module-local.

use std::collections::HashMap;

use super::name::NameId;
use super::symbol::SymbolId;

/// Which of a module's three name namespaces a symbol lives in (doc section 40).
///
/// A name may be defined once per namespace; the same name in two namespaces is not
/// a collision. The resolver picks the namespace from the *use* position: a type
/// path looks in [`Namespace::Type`], an event handler's event name in
/// [`Namespace::Event`], everything else in [`Namespace::Value`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Namespace {
    /// Runtime values: state, computed, input, const, and the callables.
    Value,
    /// Types: record, enum, type alias, and component/system type names.
    Type,
    /// Declared events, addressed by `on <event> { .. }`.
    Event,
}

/// A module-level definition: the durable [`SymbolId`] it minted plus whether it is
/// exported from the module.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleSymbol {
    /// The declaration's durable identity.
    pub id: SymbolId,
    /// Whether an `export` prefix made this visible to importers.
    pub exported: bool,
}

/// A per-module symbol table: name + namespace to a [`ModuleSymbol`].
///
/// [`SymbolTable::define`] reports a within-namespace repeat by returning the
/// existing symbol (the caller emits the collision diagnostic with both spans);
/// cross-namespace names never collide because the key includes the namespace.
#[derive(Debug, Default)]
pub struct SymbolTable {
    entries: HashMap<(NameId, Namespace), ModuleSymbol>,
}

impl SymbolTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Defines `name` in `namespace`. Returns `Err(existing)` if the name is already
    /// defined in that namespace (a collision), leaving the first definition in place.
    pub fn define(
        &mut self,
        name: NameId,
        namespace: Namespace,
        symbol: ModuleSymbol,
    ) -> Result<(), ModuleSymbol> {
        match self.entries.get(&(name, namespace)) {
            Some(&existing) => Err(existing),
            None => {
                self.entries.insert((name, namespace), symbol);
                Ok(())
            }
        }
    }

    /// Looks a name up in one namespace.
    pub fn get(&self, name: NameId, namespace: Namespace) -> Option<ModuleSymbol> {
        self.entries.get(&(name, namespace)).copied()
    }

    /// The number of defined symbols across all namespaces.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the table has no definitions.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A dense, scope-local slot for a locally bound name (a `let`, a parameter, a `for`
/// pattern binding, or a `node`/`part` local name).
///
/// Slots are numbered per resolution pass in binding order; they are meaningful only
/// within the module being resolved. A name use that resolves to a slot is a local
/// reference, not a durable [`SymbolId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LocalSlot(u32);

impl LocalSlot {
    /// The raw slot index.
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A stack of lexical scopes, innermost last.
///
/// Each scope maps a [`NameId`] to the [`LocalSlot`] it binds; [`ScopeStack::lookup`]
/// walks the stack innermost-first so an inner binding shadows an outer one. Slots
/// are minted from a single monotonic counter so every local in a module has a
/// distinct slot regardless of nesting.
#[derive(Debug, Default)]
pub struct ScopeStack {
    scopes: Vec<HashMap<NameId, LocalSlot>>,
    next_slot: u32,
}

impl ScopeStack {
    /// A fresh, empty scope stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a new innermost scope.
    pub fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Closes the innermost scope. Slots minted in it stay spent (the counter does
    /// not rewind), so no two locals ever share a slot within one pass.
    pub fn pop(&mut self) {
        self.scopes.pop();
    }

    /// Binds `name` in the innermost scope to a freshly minted slot, returning it.
    /// A rebinding within the same scope shadows the earlier one (the later slot
    /// wins), matching block-scoped `let` shadowing.
    pub fn bind(&mut self, name: NameId) -> LocalSlot {
        let slot = LocalSlot(self.next_slot);
        self.next_slot += 1;
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, slot);
        }
        slot
    }

    /// Resolves `name` against the scope stack, innermost-first. Returns the slot of
    /// the nearest enclosing binding, or `None` if the name is not locally bound.
    pub fn lookup(&self, name: NameId) -> Option<LocalSlot> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(&name).copied())
    }

    /// The current scope depth (number of open scopes).
    pub fn depth(&self) -> usize {
        self.scopes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::NameInterner;

    fn sym(hi: u64) -> ModuleSymbol {
        ModuleSymbol {
            id: SymbolId::from_parts(hi, 0),
            exported: false,
        }
    }

    #[test]
    fn cross_namespace_names_coexist_but_within_namespace_collides() {
        let mut interner = NameInterner::new();
        let mut table = SymbolTable::new();
        let n = interner.intern("color");
        assert!(
            table.define(n, Namespace::Value, sym(1)).is_ok(),
            "first value definition"
        );
        assert!(
            table.define(n, Namespace::Type, sym(2)).is_ok(),
            "same name in the type namespace is fine"
        );
        let collision = table.define(n, Namespace::Value, sym(3));
        assert_eq!(
            collision,
            Err(sym(1)),
            "a repeat in the same namespace collides and keeps the first"
        );
        assert_eq!(table.get(n, Namespace::Value), Some(sym(1)));
        assert_eq!(table.get(n, Namespace::Type), Some(sym(2)));
    }

    #[test]
    fn inner_scope_shadows_outer_and_slots_are_distinct() {
        let mut interner = NameInterner::new();
        let mut scopes = ScopeStack::new();
        let x = interner.intern("x");
        scopes.push();
        let outer = scopes.bind(x);
        assert_eq!(scopes.lookup(x), Some(outer));
        scopes.push();
        let inner = scopes.bind(x);
        assert_ne!(outer, inner, "each binding mints a distinct slot");
        assert_eq!(scopes.lookup(x), Some(inner), "inner binding shadows outer");
        scopes.pop();
        assert_eq!(
            scopes.lookup(x),
            Some(outer),
            "closing the inner scope reveals the outer binding again"
        );
        scopes.pop();
        assert_eq!(scopes.lookup(x), None, "no binding once all scopes close");
    }
}
