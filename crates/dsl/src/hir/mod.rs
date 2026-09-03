//! Typed HIR — the type / effect / capability layer over the resolved AST.
//!
//! Name resolution ([`crate::resolve`]) answers *what each name refers to*. This
//! layer answers *what type it has, what effects it carries, and what capabilities
//! it needs*: it re-walks the AST behind each resolved module, projects every core
//! construct (component/input/state/computed/action/fn/event/view) into a typed HIR
//! node, and runs the three static checks the compilation pipeline requires between
//! name resolution and domain IR (doc pipeline: name-resolved AST → Typed HIR →
//! effect/capability check → domain IR).
//!
//! Every HIR node carries the doc's node contract: a resolved symbol, an inferred
//! type, an effect class, a capability set, an ownership mode, the reactive sources
//! it reads, its source origin, and a constant value where one is statically known.
//! After lowering there must be no unresolved identifier, no untyped numeric literal,
//! no undetermined effect class, and no implicit dynamic — the whole point of the
//! layer is to discharge those into concrete facts or diagnostics.
//!
//! Like the rest of the frontend this is cold-path (AGENTS section 7.2): lowering
//! runs once per build, never on a frame path, so `String`/`HashMap`/`Rc` are fine.
//! The zero-allocation contract binds what this layer *lowers to* (the UI/Binding IR
//! of the next slice), not the layer itself.
//!
//! The reference framework's script layer is a single-file dynamic VM with no static
//! type/effect/capability checker, so there is nothing to port here: this layer is
//! Viso-owned, built on the resolver's slot-based local scopes and durable symbol
//! identities.

mod capability;
mod component;
mod effect;
mod infer;
mod nodes;
mod reads;
mod ty;

pub use capability::CapabilitySet;
pub use component::{MemberEnv, lower_component};
pub use effect::{BodyContext, EffectClass, EffectCx, EffectEnv};
pub use infer::{InferCx, TypeEnv};
pub use nodes::{
    CallableKind, ComponentSchema, ConstValue, HirCallable, HirComponent, HirComputed, HirEvent,
    HirInput, HirMeta, HirState, OwnershipMode,
};
pub use reads::{ReadEnv, collect_reads};
pub use ty::{Ty, TypeError, WidenError};
