//! The release ahead-of-time UI package: format and loader (architecture section
//! 41; AGENTS 21.6, 60).
//!
//! A release build lowers the shared frontend IR into a compact, dependency-free
//! byte blob ([`package`]) that an app instantiates at startup ([`load`]) with **no
//! DSL compiler present** — the load-bearing exit criterion of Slice P. Everything
//! here lives in `viso-ui`, the only release-path resident, and imports no
//! `viso-dsl` type; the build-time emitter that *produces* a package lives in
//! `viso-dsl`, so the format has a single source of truth (the types are defined
//! here, the emitter merely constructs them and encodes).

pub mod load;
pub mod package;

pub use load::{instantiate, load_from_bytes};
pub use package::{AotAxis, AotEdge, AotLength, AotNode, AotNodeKind, AotPackage, AotStyle};
