//! The typed HIR node shapes — the doc's node contract (spec node-contract section).
//!
//! Every HIR node carries the same eight facts, held here in [`HirMeta`]: the symbol it
//! resolved to, the type inference gave it, the effect class it carries, the capability
//! set it needs, its ownership mode, the reactive sources it reads, where in source it
//! came from, and its constant value when one is statically known. A member-level node
//! (a [`HirState`], [`HirComputed`], [`HirInput`], [`HirCallable`], …) is that metadata
//! plus the member-specific fields lowering fills in.
//!
//! This slice lowers the *core* surface — component/input/state/computed/event/callable/
//! view — into these nodes. Advanced members (trait/impl/generics/native/shader) are not
//! lowered to typed nodes yet; they keep a source origin only, to be filled in by their
//! consumer slice. The nodes deliberately hold owned `String`/`Vec`/`Rc` data: this is
//! cold-path frontend state (AGENTS section 7.2), and the zero-allocation contract binds
//! what these lower *to* (the UI/Binding IR of the next slice), not the nodes themselves.

use std::collections::BTreeSet;

use crate::resolve::SymbolId;
use crate::syntax::TextRange;

use super::capability::CapabilitySet;
use super::effect::EffectClass;
use super::ty::Ty;

/// How a binding owns the value it holds — the doc's ownership-mode axis of the node
/// contract. This slice distinguishes only the coarse modes the core surface needs;
/// borrow/lifetime refinement lands with its consumer slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipMode {
    /// The binding owns its value outright (a `state`, a `let`, a record field).
    Owned,
    /// The binding is a derived view that owns nothing (a `computed`).
    Derived,
    /// The binding is supplied by the parent and read-only here (an `input`).
    Borrowed,
    /// No value binding (an `event` declaration, a `view`).
    None,
}

/// A statically known constant value, when inference could fold one. This slice folds
/// only the shapes the core numeric/scalar surface needs; richer constant folding is a
/// later concern. `None` on a node's [`HirMeta`] means "not a known constant", which is
/// the common case.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    /// A constant boolean.
    Bool(bool),
    /// A constant integer, with the type it was typed at.
    Int(i128, Ty),
    /// A constant float, with the type it was typed at.
    Float(f64, Ty),
    /// A constant string.
    Str(String),
}

/// The eight-field node contract every HIR node carries (spec node-contract section).
///
/// After lowering, none of these is allowed to be left undetermined for a core node:
/// `resolved_symbol` is set for every declaration, `inferred_type` is never an
/// undetermined numeric placeholder, `effect_class` is decided, and `capability_set` is
/// concrete (possibly empty). [`constant_value`] is the one field that is legitimately
/// absent — most nodes are not constants.
#[derive(Debug, Clone, PartialEq)]
pub struct HirMeta {
    /// The durable identity of the declaration this node lowers, when it is a
    /// declaration. `None` for an anonymous sub-expression node.
    pub resolved_symbol: Option<SymbolId>,
    /// The type inference assigned. Never an `InferInt`/`InferFloat` placeholder after
    /// lowering completes (that is the point of the HIR-complete assertion).
    pub inferred_type: Ty,
    /// The effect class this node carries.
    pub effect_class: EffectClass,
    /// The capabilities this node (transitively) requires.
    pub capability_set: CapabilitySet,
    /// How this node owns its value.
    pub ownership_mode: OwnershipMode,
    /// The reactive sources (state/input/computed symbols) this node reads.
    pub reactive_reads: BTreeSet<SymbolId>,
    /// Where in source this node came from.
    pub source_origin: TextRange,
    /// The node's constant value, when one is statically known.
    pub constant_value: Option<ConstValue>,
}

impl HirMeta {
    /// A metadata block for a declaration node: its symbol, type, effect, ownership, and
    /// source span, with an empty capability set and read set and no constant. Callers
    /// fill `capability_set`/`reactive_reads`/`constant_value` in as later passes learn
    /// them.
    pub fn decl(
        symbol: SymbolId,
        inferred_type: Ty,
        effect_class: EffectClass,
        ownership_mode: OwnershipMode,
        source_origin: TextRange,
    ) -> Self {
        HirMeta {
            resolved_symbol: Some(symbol),
            inferred_type,
            effect_class,
            capability_set: CapabilitySet::new(),
            ownership_mode,
            reactive_reads: BTreeSet::new(),
            source_origin,
            constant_value: None,
        }
    }

    /// Whether this metadata still carries an undetermined type — an `InferInt`,
    /// `InferFloat`, or `Unknown`. The HIR-complete assertion (spec node-contract
    /// section) forbids any of these surviving into a finished core node.
    pub fn type_is_undetermined(&self) -> bool {
        matches!(
            self.inferred_type,
            Ty::InferInt | Ty::InferFloat | Ty::Unknown
        )
    }
}

/// A lowered `input` member: an externally-supplied, always-explicitly-typed property.
#[derive(Debug, Clone, PartialEq)]
pub struct HirInput {
    /// The input's name, for schema and diagnostics.
    pub name: String,
    /// The node contract.
    pub meta: HirMeta,
}

/// A lowered `state` member: owned reactive state, its type either annotated or inferred
/// uniquely from its initializer.
#[derive(Debug, Clone, PartialEq)]
pub struct HirState {
    /// The state's name.
    pub name: String,
    /// Whether the type was written explicitly (vs inferred from the initializer).
    pub type_was_annotated: bool,
    /// The node contract.
    pub meta: HirMeta,
}

/// A lowered `computed` member: a pure cached derivation with its dependency set.
#[derive(Debug, Clone, PartialEq)]
pub struct HirComputed {
    /// The computed's name.
    pub name: String,
    /// The other members this computed depends on (its `reactive_reads`, kept here too
    /// for the dependency-graph pass). Held as symbols in declaration order via the meta.
    pub meta: HirMeta,
}

/// A lowered `event` declaration: a payload signature the component can emit. Events bind
/// no value, so they carry `OwnershipMode::None` and `EffectClass::Pure`.
#[derive(Debug, Clone, PartialEq)]
pub struct HirEvent {
    /// The event's name.
    pub name: String,
    /// The node contract.
    pub meta: HirMeta,
}

/// The kind of callable a [`HirCallable`] lowers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallableKind {
    /// A pure/read `fn`.
    Fn,
    /// A synchronous mutating `action`.
    Action,
    /// An asynchronous `task`.
    Task,
    /// An `event` handler body (`on <event> { ... }`).
    EventHandler,
}

/// A lowered callable (`fn`/`action`/`task`) or event handler: its kind, its declared
/// signature, and the node contract (whose `effect_class` is the callable's effect and
/// whose `capability_set` is what its body transitively needs).
#[derive(Debug, Clone, PartialEq)]
pub struct HirCallable {
    /// The callable's name (empty for an anonymous event-handler body).
    pub name: String,
    /// Which callable form this is.
    pub kind: CallableKind,
    /// The node contract.
    pub meta: HirMeta,
}

/// The typed schema of one component: its members grouped by kind, in declaration order.
///
/// This is the spec member-classification result — every core member of a `component`
/// (or `system`) lowered and bucketed. The `view` is held as its source span this slice
/// (view IR is Slice N); the members carry the full node contract.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentSchema {
    /// The component's name.
    pub name: String,
    /// The component's declaration symbol.
    pub symbol: SymbolId,
    /// The `input` members, in declaration order.
    pub inputs: Vec<HirInput>,
    /// The `state` members, in declaration order.
    pub states: Vec<HirState>,
    /// The `computed` members, in dependency-topological order (see
    /// [`super::component`]).
    pub computeds: Vec<HirComputed>,
    /// The `event` declarations.
    pub events: Vec<HirEvent>,
    /// The `fn`/`action`/`task` members and event handlers.
    pub callables: Vec<HirCallable>,
    /// The `view` declaration's source span, when the component has one.
    pub view: Option<TextRange>,
}

/// A fully lowered component: its schema plus its own declaration span.
#[derive(Debug, Clone, PartialEq)]
pub struct HirComponent {
    /// The typed member schema.
    pub schema: ComponentSchema,
    /// The component declaration's source span.
    pub source_origin: TextRange,
}
