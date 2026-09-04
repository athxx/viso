//! Name resolution and the module graph.
//!
//! This layer turns the typed AST (see [`crate::ast`]) into resolved identities: a
//! compiler-local [`NameInterner`] mints dense [`NameId`]s for text, a versioned
//! fingerprint mints durable [`SymbolId`]s for declarations, and a deterministic
//! [`ModuleGraph`] wires source units together by their `import` edges. It answers
//! *what each name refers to*, not *what type it has* — type, effect, and capability
//! checking belong to the next slice.
//!
//! Everything here is cold-path: resolution runs once per build, never on a frame
//! path, so interning, hashing, and graph structures may allocate freely (AGENTS
//! section 7.2). The identities they produce — `SymbolId` above all — are what the
//! hot-path layers downstream are lowered against.

mod module;
mod name;
mod resolver;
mod scope;
mod symbol;

pub use module::{GraphModule, ModuleGraph, ModuleIndex, ModulePath, ResolveErrorKind, SourceUnit};
pub use name::{NameId, NameInterner};
pub use resolver::{
    Resolution, ResolvedFragment, ResolvedModule, ResolvedRef, SymbolDecl, resolve,
    resolve_fragment,
};
pub use scope::{LocalSlot, ModuleSymbol, Namespace, ScopeStack, SymbolTable};
pub use symbol::{FINGERPRINT_VERSION, SymbolId, SymbolIdentity, SymbolKind, fingerprint};
