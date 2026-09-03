# ADR 0013 — `.vs` Typed HIR and type / effect / capability checking

- Status: Accepted
- Date: 2026-09-03

## Context

Slice L (ADR 0011) delivered the `.vs` typed AST, cross-module name resolution, and
a deterministic module graph: `resolve::resolve(...)` yields a `ResolvedModule` per
unit carrying a `SymbolTable` (top-level declaration `NameId × Namespace → SymbolId`),
a `refs: Vec<ResolvedRef>` (each name use's `TextRange → Resolution{Symbol|Local}`),
and a `Vec<Diagnostic>`. That layer answers *what each name refers to* and deliberately
leaves *what type it has, what effect it carries, and what capability it needs* to
Slice M.

Slice M builds the **Typed HIR** on top of the resolved AST and runs the compilation
pipeline segment `Name-resolved AST → Typed HIR → Effect/Capability Check` (AGENTS
section 21.2). It is full Core coverage: `component`/`input`/`state`/`computed`/
`action`/`fn`/`event`/`view` get typed HIR nodes, a static scalar type system, static
effect checking, a `computed` dependency graph, and capability inference. Advanced
productions (`trait`/`impl`, general/const generics, native schema, shader, resource,
task-async) stay placeholders this slice per the section 21.5.1 surface-complexity
budget.

Governing spec is `Viso_DSL_1.0.md`: the Typed HIR node contract, the unique scalar
list (no `Int`/`UInt`/`Float`/platform-width integers), the inference boundary (what
may infer vs what must be explicit, never falling back to `dynamic`), numeric-literal
typing (host default `1`→I64, `1.0`→F64; instantiate at the expected type under
context), the implicit-widening rules (only `I8→…→I64`, `U8→…→U64`, `F32→F64`; every
cross-family conversion needs explicit `as`/`checked_cast`), the effect call matrix,
state initialization order, `computed` purity + dependency graph, and the capability
set model. AGENTS section 68 lists "Viso DSL language/module semantics" as an ADR
trigger, so this slice lands an ADR.

The reference framework's script layer (`makepad/platform/script`) is a single-file
dynamic VM with no static type/effect/capability checker — there is nothing to port.
Per the standing "take semantics, not coarse architecture" rule, this whole layer is
Viso-owned, built on the resolver's slot-based local scopes and durable `SymbolId`
identities. It is a cold-path compiler frontend, so `Rc`/`HashMap`/`String` are allowed
(AGENTS section 7.2); the hot-path zero-allocation contract applies to what this layer
*lowers to* (Slice N), not to the layer itself.

## Decision

### 1. A self-built static Typed HIR layer (`hir/`)

Slice M adds one subsystem directory, `crates/dsl/src/hir/`, that re-walks each
resolved module's AST behind the same module-path→`CompilationUnit` matching the
resolver uses, consumes the module's `refs` (indexed `TextRange → Resolution` for O(1)
"what does this name refer to") and cross-module `table`s, and emits typed HIR while
accumulating new diagnostics into the shared `Diagnostic` type. It does not change any
Slice-L behavior; it only adds `mod hir;` plus re-exports. `viso-dsl` gains **zero** new
dependencies and never depends on `viso-widgets` — it programs against schema interfaces
(the native/widget schema registry is a later slice's boundary; this slice uses the
component's own declarations as the schema).

Modules: `ty` (the type lattice + widening/conversion tables + `TypePath → Ty`),
`infer` (expression type inference and literal typing), `effect` (effect classes + the
call matrix), `capability` (capability sets + call-graph propagation), `nodes` (the HIR
node types carrying the node contract), `component` (member classification → schema +
state-order + `computed` dependency graph), `reads` (reactive-read collection), and
`lower` (the `lower(graph, units, resolved, interner, package) → LoweredPackage` entry
point that ties it together).

### 2. The node contract: eight fields, no undetermined residue

Every HIR node carries the spec's node contract — a resolved symbol, an inferred type,
an effect class, a capability set, an ownership mode, the reactive sources it reads, its
source origin, and a constant value where one is statically known. After lowering there
must be no unresolved identifier, no untyped numeric literal, no undetermined effect
class, and no implicit `dynamic`. A debug `debug_assert!` at the end of `lower` walks the
lowered package and rejects any node whose inferred type is still an inference placeholder
(`InferInt`/`InferFloat`/`Unknown`) where the source did not annotate it — the layer's
whole purpose is to discharge those into concrete facts or diagnostics.

### 3. Three static checks, one diagnostic-code set

- **Type** (`infer`): a fixed scalar list (`Bool`, `I8..I64`, `U8..U64`, `F32`, `F64`,
  `Char`, `String`, `Bytes`, `Unit`, `Never`, `Color`, plus the UI dimensions) with no
  `Int`/`UInt`/`Float`/platform-width integer. Untyped numeric literals type at the
  expected type under context (including F32 instantiation, not an F64→F32 conversion),
  or at the host default with none. Implicit widening is only same-family upward
  (`I8→…→I64`, `U8→…→U64`, `F32→F64`); every other conversion is explicit. A removed
  `Float` annotation is **E2101**, an illegal implicit numeric conversion is **E2102**, a
  type mismatch or a literal that cannot be uniquely inferred / is out of range is
  **E2103**.
- **Effect** (`effect`): each callable carries a `Pure`/`Read`/`Action`/`Task` class,
  and each body is walked in a `BodyContext` (`View`/`Computed`/`Fn`/`Action`/`Event`/
  `Task`). The call matrix: View/Computed/`fn` may call only pure/read callables;
  Action/Event may also call `action`; Task may also call other `task`s directly. A
  matrix violation is **E2501**; a side effect specifically inside a reactive
  View/Computed body is **E2502**.
- **State / computed** (`component`): `state` may initialize only from source-preceding
  state — a forward read is **E2104** (no "dependency is clear" exception; forward
  derivation must use `computed`). Omitted private-`state`/`computed` types must infer to
  a unique concrete type, and failure to do so is a compile error. All `computed` form a
  dependency graph that is topologically sorted; a cycle is **E2105** with the full cycle
  path in related spans.
- **Capability** (`capability`): a `CapabilitySet` is a deterministic ordered set
  (`BTreeSet`). A callable's inferred set is the union of its own direct conferrals and
  the inferred sets of everything it (transitively) calls, computed as a fixed point over
  an index-based call graph so it is transitive and terminates on cycles. An explicit
  `requires {}` clause is a public upper-bound contract, not mandatory boilerplate: the
  inferred set must be a subset of the declared set, and a callable that transitively
  needs a capability it did not declare earns **E2601** in deterministic
  missing-capability order. Until a native schema declares which native calls confer
  which capability, direct conferrals are empty and inferred sets are empty; the
  machinery unions direct conferrals in already, so that source drops in without a
  propagation change.

The three checks are decoupled from the HIR node types through small `&self` environment
traits (`TypeEnv`/`ReadEnv`/`EffectEnv`/`MemberEnv`), so each is unit-testable against a
stub before component lowering exists. `lower` supplies one `ModuleEnv` per module, built
in a `&mut interner` pre-pass into pure lookup tables (module members, per-member facts,
and a decl-span→component-symbol map focused per component via a `Cell` before each
component lowers, so a module declaring several components gives each the right
`component_symbol`).

### 4. Advanced placeholders and the consumer boundary

`system` declarations share the component member surface but are not `ComponentDecl`s;
their dedicated lowering (system hooks / scheduler schema) lands with their consumer
slice. Module-level `fn`/`action`/`task` likewise lower fully with their consumer.
`trait`/`impl`, generic arity, native schema, shader, resource, and task-async bodies
lower to placeholders (source origin + symbol recorded, no deep inference /
monomorphization / effect refinement) and are completed when their consumer slice lands.
This keeps the Core vertical slice verifiable end to end while honoring the section
21.5.1 budget.

## Consequences

- Slice N (UI IR + Binding IR) consumes `LoweredPackage` directly: the typed HIR is its
  input, and `reads` already supplies the reactive-read sets its Binding IR needs.
- The three diagnostic-code families minted here (E2101–E2105, E2501/E2502, E2601) join
  the Slice-L set; each has a dedicated test.
- `viso-dsl` gains **zero** new dependencies and still does not depend on `viso-widgets`;
  the workspace stays **15 crates** and `cargo xtask check-deps` stays green.
- Advanced productions and module-level/`system` callables lower as placeholders this
  slice and are completed by their consumer slices (recorded in todo). The
  native/widget schema registry — the eventual source of capability conferrals and
  property/event schema validation — is a later slice's boundary; this slice infers
  capabilities from the call graph and uses each component's own declarations as its
  schema.
- Verification for this slice: `cargo build/clippy -D warnings/fmt/test -p viso-dsl`
  clean — 160 tests (90 unit across `ty`/`infer`/`effect`/`capability`/`component`/
  `reads`/`lower`, including positive type-check, one case per E-code, effect/capability
  violations, `computed` topological order + cycle path, private-state inference,
  reactive-read collection, and a full component lowering end to end with empty
  diagnostics; plus 70 lexer/parser/grammar/AST integration tests carried from Slice L).
  `cargo xtask check-deps` green (15 crates, 0 new deps). No shaders/UI/GPU touched, so
  no Metal/headless run was required; this is a cold-path frontend, so no benchmark is
  required (the static/mixed/dynamic reactive benchmark lands with Slice N).
