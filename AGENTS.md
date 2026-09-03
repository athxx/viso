# AGENTS.md — Viso Engineering Guide

> This file defines mandatory engineering rules for humans and coding agents working on Viso 1.0 Draft.
> It is intentionally opinionated. When a local implementation preference conflicts with this document, follow this document unless an accepted ADR explicitly changes the rule.

---

# 1. Mission

Viso is a **Rust-native, GPU-first, cross-platform application framework**.

Canonical naming rules:

- framework/repository/facade crate: `Viso` / `viso`;
- canonical external UI/DSL source extension: `.vs`;
- Rust-side DSL entry points are `ui! { ... }`, `component! { ... }`, and `view!("...vs")`; they MUST share the same schema/type/effect checking, Typed HIR, Reactive/UI/Shader IR, and runtime contracts;
- `.vs` MUST be used by generators, examples, docs, diagnostics, formatter, LSP, Studio, and migration output whenever an external DSL file is emitted;
- `Makepad` is used only for migration input, behavior/algorithm reference, source comparison, and benchmark baselines;
- current Makepad migration input MUST be modeled as Rust source containing `script_mod!`, `ScriptVm`, `App::from_script_mod`, widget registration, and script module initialization; do NOT model `.live` files as the current Makepad source format;
- use “hot reload” or “live editing” for the capability, not as a source-format name.

It must optimize for all three of the following at the same time:

1. **Excellent runtime performance**.
2. **Clear engineering boundaries and maintainable structure**.
3. **A very small public mental model**.

The canonical user entry point is:

```rust
use viso::prelude::*;

fn main() {
    viso::run::<App>();
}
```

Internal complexity is allowed. Public accidental complexity is not.

The core design summary is:

> External declarative, internal retained.  
> External object-oriented, internal data-oriented.  
> Dynamic in development, AOT in release.  
> Abstraction on cold paths, flat data on hot paths.

---

# 2. Sources of Truth

Before changing architecture-sensitive code, read:

1. `ARCHITECTURE.md` or `Viso_Architecture_and_Migration.md` if the repository still uses that filename.
2. Relevant crate-level docs.
3. Any ADR under `docs/adr/`.
4. This `AGENTS.md`.

Order of authority:

```text
Accepted ADR
    > Architecture document
    > AGENTS.md
    > crate docs
    > nearby implementation convention
```

If these disagree, do not silently choose one. Update the stale document as part of the change or call out the conflict.

---

# 3. Non-Negotiable Architecture Rules

## 3.1 One public facade

Normal applications depend on:

```toml
[dependencies]
viso = "1.0"
```

Do not require ordinary app code to import internal crates such as:

```text
viso_ui
viso_render
viso_runtime
```

unless the user is intentionally using advanced APIs.

## 3.2 Keep the prelude small

`viso::prelude::*` is curated.

Do not add a type to the prelude merely because it is public.

A prelude item must be:

- commonly used by normal apps;
- stable enough to expose broadly;
- low in naming ambiguity;
- part of the intended default mental model.

GPU/backend/internal compiler types never belong in the default prelude.

## 3.3 Crates are dependency boundaries, not folders

Do not create a new crate just to make the repository look layered.

Prefer a module inside an existing crate unless one of these is true:

- independent reuse is required;
- target-specific dependencies justify separation;
- an actual dependency cycle must be broken;
- compile-time measurements show a meaningful boundary benefit;
- the subsystem needs independent release/versioning;
- an unsafe/security boundary needs stronger isolation;
- a backend has materially different dependencies/build logic.

## 3.4 Avoid vague package/module names

Do not introduce new generic buckets named:

```text
core
common
utils
helpers
misc
shared
```

A module/package name should describe responsibility.

`viso-core`, `viso-ui-core`, and `viso-render-core` are not target architecture names.

## 3.5 Dependency direction must remain one-way

High-level conceptual direction:

```text
viso facade
      ↓
widgets / dsl / services
      ↓
ui
      ↓
render
      ↓
text / shader
      ↓
gpu
      ↓
runtime / platform
```

Exact low-level edges can differ where required by surface creation and runtime ownership, but the following are forbidden:

```text
platform -> ui
platform -> widgets
platform -> dsl
platform -> studio
ui       -> widgets
render   -> widgets
gpu      -> ui
runtime  -> studio
```

Studio and Inspector are clients of the framework, never dependencies of framework runtime crates.

## 3.6 Architecture boundaries are machine-enforced

Do not rely on this file or code review alone to preserve layering.

Architecture-sensitive changes MUST keep the machine-readable policy and CI checker passing:

```text
architecture.toml
cargo xtask arch-check
```

The checker should enforce crate dependency allow/deny edges and selected module-level forbidden imports. `cargo-modules` or similar tools may be used for visualization, but the repository-owned checker is the source of truth.

## 3.7 Self-build vs dependency policy

Use the Ownership Ladder:

**Viso must own:** Node identity/lifecycle, reactive invalidation, layout contract, render batching/upload semantics, `.vs` + `ui!`/`component!`/`view!` HIR+AOT+hot reload semantics, Shader ABI, frame scheduler, semantics tree.

**Viso owns integration but should prefer proven algorithms:** Unicode/BiDi/shaping primitives, font rasterization, media codecs, accessibility OS bridge, platform bindings.

**Adapters/external by default:** async executors, HTTP/TLS, databases, telemetry, wgpu reference/experimental backend.

Do not reimplement a standards-heavy subsystem merely for repository purity. Fork/replace only when capability, profiling, security, or maintenance evidence justifies it.

A target architecture contract may be Viso-owned while its first implementation is dependency-backed. Prefer a narrow provisional adapter over blocking a measurable vertical slice. When replacing an adapter with a custom implementation, remove the old path instead of keeping permanent dual implementations.

---

# 4. Repository Layout

Target top-level layout:

```text
architecture.toml

crates/
    viso/
    macros/
    runtime/
    platform/
    gpu/
    shader/
    text/
    render/
    ui/
    widgets/
    dsl/
    services/

integrations/
extras/
tools/
examples/
benches/
tests/
docs/
xtask/
vendor/
```

Rules:

- Third-party forks belong in `vendor/`.
- Optional large functionality belongs in `extras/` or `integrations/`, not default widgets.
- Apps/examples must not be imported by framework crates.
- `tools/studio` must not gain privileged hidden runtime dependencies.

---

# 5. File Organization

Use this rule:

> A simple concept is a file. A complex subsystem is a directory.

Good:

```text
widgets/src/controls/button.rs
```

Good when complexity grows:

```text
widgets/src/controls/text_input/
    mod.rs
    edit.rs
    selection.rs
    ime.rs
    layout.rs
    paint.rs
    semantics.rs
```

Do not mechanically create one file per struct.

Do not let `lib.rs` become an implementation dump. `lib.rs` should mostly contain:

- crate docs;
- module declarations;
- public re-exports;
- minimal crate initialization.

---

# 6. Public API Rules

## 6.1 Optimize for the common path

Normal users should be able to write apps without understanding:

- render graph internals;
- GPU batches;
- shader ABI;
- node arena internals;
- platform backend details;
- dynamic DSL VM internals;
- frame scheduler internals.

Advanced escape hatches may exist but must be opt-in and clearly named.

## 6.2 Avoid exposing implementation accidents

Do not expose properties equivalent to:

```text
new_batch: true
```

when they only exist to satisfy renderer implementation details.

Correctness must not depend on users understanding hidden batching or draw ordering mechanics.

## 6.3 Prefer typed handles

Public component handles should be typed and cheap to copy.

Preferred shape:

```rust
Handle<Button>
```

Internally this may contain a generational `NodeId`.

Do not reintroduce `Rc<RefCell<Box<dyn Widget>>>` as the standard public ownership model.

## 6.4 Keep APIs phase-aware

Do not add more capabilities to a universal context just because it is convenient.

Prefer purpose-specific contexts:

```text
AppCx
EventCx
UpdateCx
LayoutCx
PaintCx
RenderCx
TaskCx
```

A layout function must not be able to accidentally launch a network request or submit arbitrary GPU work.

---

# 7. Performance Philosophy

Performance is an architectural contract, not a late optimization pass.

## 7.1 Hot path contract

For warmed-up steady-state frame paths, design toward:

```text
0 unnecessary heap allocations
0 string property lookups
0 global HashMap lookup per node
0 Rc/RefCell node traversal
0 UI-main-thread mutex locks
0 per-node backend virtual dispatch
0 full-tree rebuild for a local state update
```

This is a design target for framework code. User callbacks may allocate if they choose.

## 7.2 Cold-path abstractions are fine

Using `dyn Trait`, `Arc`, or hash maps can be appropriate for:

- clipboard;
- file dialog;
- permissions;
- push notifications;
- plugin discovery;
- developer tooling;
- service registries;
- compiler symbol tables.

Do not waste complexity optimizing rare calls while leaving frame traversal pointer-heavy.

## 7.3 Never claim a performance improvement without measurement

Any change described as a performance optimization must include at least one of:

- benchmark result;
- profiler trace;
- allocation count comparison;
- GPU timing/draw-call comparison;
- memory comparison.

If measurement infrastructure is missing, add it first or label the change as a hypothesis.

---

# 8. UI Runtime Data Model

## 8.1 Retained tree

Viso uses a retained UI tree.

Do not introduce a default Virtual DOM rebuild/diff architecture.

Structural reconciliation is allowed when structure genuinely changes, but ordinary property/state updates must target retained nodes.

## 8.2 Generational node identity

Runtime node identity is based on a compact generational ID, conceptually:

```rust
struct NodeId {
    index: u32,
    generation: u32,
}
```

Stale handles must be detectable.

## 8.3 UI tree is not a generic ECS

Keep explicit UI ancestry:

```text
parent
first_child
next_sibling
```

Layout, focus, clipping, semantics, and event propagation depend strongly on ancestry.

Use data-oriented side storage for hot data, but do not turn the entire UI into a generic ECS merely for stylistic purity.

## 8.4 Hot/warm/cold separation

Keep frequently traversed data compact.

Hot examples:

```text
links
bounds
transform
dirty flags
visibility
clip/hit-test flags
```

Cold examples:

```text
debug names
source spans
full reflection metadata
inspector-only strings
```

Do not put cold Strings in structures traversed for every node every frame.

## 8.5 Node hot storage is fixed-index hybrid SoA

The target storage model is not open-ended:

- compact `NodeMeta[index]` AoS for hierarchy/type/flags;
- layout/transform/paint/interaction hot stores indexed directly by the same `NodeIndex`;
- cold/optional data in sparse side tables;
- no per-node HashMap lookup in steady-state traversal;
- no archetype migration as the default UI-node lifecycle.

Changing this model requires an ADR plus benchmark evidence.

## 8.6 `Handle<T>` is identity/capability, not a state borrow

Public `Handle<T>` MUST NOT expose long-lived `&T`/`&mut T` access to component internals. Reads/writes go through context-scoped query, generated property access, typed actions, or update APIs that preserve lifetime and invalidation accounting. Internal guards may exist but must not survive the frame/borrow scope.

## 8.7 Transform invalidation is separate from layout invalidation

Scroll/translate/scale/transform-only animation MUST be able to update `TRANSFORM/HIT_TEST/PAINT` without marking `MEASURE/LAYOUT` dirty by default. Layout and transform share `NodeId` but use separate hot stores and propagation rules.

---

# 9. Component Model

Component, Node, and Render Primitive are different concepts.

## Component

Owns logical state, behavior, lifecycle, effects.

## Node

Owns retained runtime identity, layout/style/input/semantics references.

## Render primitive

Contains renderer-facing data.

Do not make one trait/object simultaneously represent all three.

A component may map to multiple nodes. A node may emit multiple primitives. Many primitives may batch into one draw call.

---

# 10. State and Reactive Updates

## 10.1 No manual redraw as default state protocol

New application code should not need to call:

```rust
render();
redraw_all();
```

after ordinary state changes.

State mutation must schedule precise invalidation.

Manual invalidation may exist as an advanced/debug escape hatch.

## 10.2 Prefer compiled dependency metadata

For normal Rust components and typed Viso DSL UI, prefer precomputed/compiled binding relationships.

Conceptually:

```text
StateId(count)
    -> Label.text
    -> Progress.value
```

Do not build the entire normal reactive model around per-signal `Rc<Vec<Box<dyn Subscriber>>>`.

## 10.3 Dynamic fallback is explicit and observable

Dynamic scripts may register dependencies at runtime, but a compiler-known typed binding MUST NOT silently fall back to runtime dynamic dependency tracking.

Dynamic binding requires an explicit dynamic construct/API or a compiler diagnostic. The dynamic path must not define the cost of the static fast path.

Profiler/CI counters must distinguish at least:

```text
static_binding_eval
dynamic_binding_eval
dynamic_subscribe
dynamic_fallback_nodes
```

Maintain benchmarks for static, mixed, and fully dynamic dependency paths. New dynamic fallback in strict typed examples should fail CI unless explicitly approved.

## 10.4 Transaction batching

Multiple state writes during one input/action transaction should flush once.

Avoid repeated immediate layout/paint work after each setter.

## 10.5 Computed must be pure

`Computed` values are cached derivations.

No I/O, timers, subscriptions, or native side effects inside computed evaluation.

## 10.6 Effects have lifecycle

Effects must support:

- cancellation;
- cleanup;
- dependency restarts;
- unmount cleanup;
- hot-reload semantics.

Do not launch effects from view rendering.

---

# 11. Dirty Invalidation Rules

Use explicit dirty classes, at minimum conceptually:

```text
STRUCTURE
STYLE
MEASURE
LAYOUT
TRANSFORM
PAINT
HIT_TEST
SEMANTICS
```

Every property/state binding must define what it invalidates.

Examples:

```text
text content       -> MEASURE + LAYOUT + PAINT + SEMANTICS
text color         -> PAINT
background         -> PAINT
width              -> MEASURE + LAYOUT
transform          -> TRANSFORM + HIT_TEST + PAINT bounds
accessibility text -> SEMANTICS
```

Do not replace this with a single generic `dirty = true` unless the subsystem is intentionally coarse-grained and benchmarked.

Propagation must stop at valid boundaries. A paint-only change must not make ancestors layout dirty.

---

# 12. Layout Rules

## 12.1 Public simplicity, specialized internals

Public concepts:

```text
Row
Column
Flex
Grid
Stack
Absolute
Scroll
```

Internal algorithms may be specialized single-pass implementations.

Do not expose internal cursor/Turtle mechanics as required knowledge for ordinary app layout.

## 12.2 Do not default to a generic constraint solver

Use direct algorithms for Flex/Grid/Stack/Flow.

A general constraint layout may exist as an optional specialized container.

## 12.3 Cache measurement

Reuse measure results when constraints/content/style versions have not changed.

Do not repeatedly measure stable text/subtrees.

## 12.4 Virtualize large collections

A `List` with 100k logical items must not create 100k mounted nodes.

Virtualized list implementation must support:

- stable item keys;
- recycling;
- visible range + overscan;
- variable height cache;
- scroll anchor preservation.

---

# 13. Input, Focus, and Gestures

Normalize platform events before UI dispatch.

Expected route:

```text
Platform event
    -> normalized input
    -> hit-test/focus target
    -> capture
    -> target
    -> bubble
```

Do not traverse the entire widget tree for every pointer event when a target route is sufficient.

Focus, IME, pointer capture, keyboard navigation, and gesture arbitration are framework subsystems, not ad hoc widget behavior.

---

# 14. Style and Theme

Do not use runtime strings as the normal property lookup mechanism.

Source names compile to IDs such as:

```text
PropertyId
TokenId
StyleId
```

Theme should be semantic-token based:

```text
color.*
typography.*
spacing.*
radius.*
elevation.*
motion.*
```

Normal frames must not recalculate the entire style cascade when nothing relevant changed.

---

# 15. Accessibility

Accessibility is mandatory architecture, not optional polish.

Every interactive official widget must have appropriate semantics.

Semantics updates are incremental.

Do not add visual-only interactions that have no keyboard/accessibility equivalent unless the component is explicitly non-interactive decoration.

Tests for complex controls should include semantics snapshots.

---

# 16. Paint and Renderer Rules

## 16.1 UI never directly owns backend command submission

UI produces paint/primitive data.

Renderer decides:

- batching;
- atlas usage;
- clip/layer strategy;
- GPU uploads;
- pass ordering;
- render graph details.

## 16.2 Automatic batching

Users must never manually select a batch boundary for normal correctness.

Batch keys should use compact IDs, not strings.

Respect visual order and clipping. Do not reduce draw calls by breaking z-order correctness.

## 16.3 Retain and reuse paint data

A local paint change should not rebuild all primitives for the entire UI.

Prefer retained ranges/caches and targeted buffer updates where practical.

---

# 17. GPU Rules

## 17.1 Keep the RHI small

GPU API concepts should stay close to:

```text
Device
Queue
Buffer
Texture
Sampler
Pipeline
BindGroup
CommandEncoder
Surface
Fence
```

Do not leak widgets/layout/state into `viso-gpu`.

## 17.2 Static backend specialization

Target builds should normally select one backend at compile/build time.

Do not add per-primitive runtime `dyn GpuBackend` dispatch.

## 17.3 Avoid lowest-common-denominator design

Backend-specific fast paths are allowed behind capability checks/internal specialization.

The abstraction must not prevent Metal/Vulkan/D3D12/WebGPU-specific performance opportunities where they materially matter.

## 17.4 Persistent resources

Do not create GPU buffers/textures/pipelines every frame when they can be reused.

Use persistent buffers, ring uploads, atlases, and caches.

---

# 18. GPU Instance ABI

Host structs and GPU instance data are separate concerns.

Preferred:

```rust
struct Painter {
    cache: HostCache,
    pipeline: PipelineId,
}

#[repr(C)]
#[derive(GpuInstance)]
struct Instance {
    rect: Vec4,
    color: Vec4,
}
```

Do not rely on implicit assumptions like:

> everything after field X in this Rust struct is GPU instance memory.

The `GpuInstance` derive/compiler must validate offsets, alignment, types, and shader declarations.

Any unsafe memory reinterpretation requires explicit safety documentation and tests.

---

# 19. Shader Rules

Shader flow should be:

```text
source
 -> parsed syntax
 -> typed IR
 -> validation
 -> backend codegen
```

Shader and Viso DSL compilers may share diagnostic/source infrastructure, but do not force them to share a runtime VM.

Shader compile errors during hot reload must preserve the last-good pipeline when possible.

---

# 20. Text Rules

Text is a dedicated performance subsystem.

Do not implement text-heavy controls by treating every glyph/character as a normal UI node.

Cache boundaries should include:

- font/face;
- shaping;
- paragraph/line layout;
- glyph atlas.

If text, font, shaping features, and width are unchanged, do not reshape/reflow.

Text editing must be grapheme-aware and IME-aware.

---

# 21. Viso DSL / Hot Reload Rules

[DSL DESIGN](./Viso_DSL_1.0.md)

## 21.1 Dependency direction

`viso-dsl` depends on schemas/UI interfaces.

The UI runtime does not require the Viso DSL compiler or an optional dynamic VM to exist.

Pure Rust UI must remain a first-class path.

## 21.2 Language pipeline

New Viso syntax must pass through stable compiler layers:

```text
Tokenizer
 -> Lossless CST
 -> AST
 -> Module Resolution
 -> Typed HIR
 -> UI IR / Binding IR / Shader IR / optional bytecode
```

Do not add new language features directly in runtime opcode handling without updating the syntax/HIR/tooling model.

## 21.3 Typed by default

Normal component properties, event payloads, and state are typed.

`dynamic` is an explicit escape hatch.

## 21.4 No manual registration-order API for normal apps

Module dependencies must be resolved by the compiler/module graph.

Do not require application authors to know that module A must be manually registered before module B.

## 21.5 DSL source forms

Canonical Rust-side entry points are:

```rust
ui! { Button { text: "Save"; } }       // ViewFragment
view!("view.vs")                        // external .vs
component! { Tiny { view { Text {} } } } // ComponentDecl
```

Rules:

- `ui!` MUST parse only the ViewFragment entry grammar;
- `component!` MUST parse only the ComponentDecl entry grammar;
- `view!` MUST compile an external `.vs` source through the normal module/file frontend;
- all three forms MUST share component/native schema, name resolution, type/effect/capability checking, Typed HIR, Reactive/UI/Shader IR, and diagnostics semantics;
- do not create a mini inline DSL or macro-only runtime semantics;
- `.vs` is the canonical external file format;
- external `.vs` files are preferred for page/theme/large-component hot reload;
- `ui!` is preferred for small local view fragments/tests/examples;
- `component!` is for small Rust-adjacent complete components, not a replacement for normal `.vs` component modules;
- changes to inline macros normally require Rust incremental compilation; the runtime is not required to parse Rust source to hot-reload inline macros.

### 21.5.1 Surface complexity budget

Viso DSL is NOT a second Rust language by default.

Core/normal authoring MUST remain usable without understanding user-defined traits, impl blocks, const generics, trait objects, hand-written native declarations, or compiler plugins.

Treat language features as:

```text
Core: component/input/state/computed/action/view, record/enum,
      node/property/event/control-flow/keyed-list,
      system hooks, basic functions, shader surface

Standard: effect/task/resource/slot/style/theme

Advanced/preview: user trait/impl/general generics/const generics,
                  template/part metaprogramming,
                  hand-written native declarations,
                  fine-grained capability annotations
```

Advanced features MUST NOT block the first production vertical slice and MUST NOT appear in Quick Start examples unless the task specifically needs them.

### 21.5.2 Declarative property syntax

In View/Style authoring, property binding uses `:` and a terminating semicolon:

```viso
Text {
    text: label;
    color: theme.colors.foreground;
}
```

Imperative behavior assignment continues to use `=` / `+=` / etc.

Do not reintroduce Makepad's `:=`, `+:`, `<:`, `>:` or other assignment-family syntax. Named node identity is explicit with `node name: Type { ... }`; merge/override behavior uses explicit typed constructs.

### 21.5.3 Source headers and type ceremony

Normal app `.vs` files derive language version from `Viso.toml`/lock metadata and module path from package + source path. Do not require every normal file to begin with `language viso ...; module ...;`.

Private `state` MAY infer its type from a stable initializer. Public/exported boundaries, inputs/events/slots/native/shader interfaces remain explicitly typed.

### 21.5.4 Native and capability authoring

Normal native APIs come from generated Rust/native schema. Do not make application authors duplicate every native signature in `.vs`.

Capability requirements should be inferred from the typed call graph and checked against package/profile grants. Explicit `requires` clauses are contracts/assertions for public APIs, not mandatory boilerplate on every callable.

### 21.5.5 Game support is a core vertical slice

Game/system support MUST NOT wait for user-defined trait/impl/general-generic support. The compiler may consume scheduler/system traits provided by Native Schema (for example `FixedUpdate`) before the language supports author-defined traits.

Keep the parser domain-neutral: do not add `game`, `physics`, `ecs`, `audio`, or engine-specific keywords. Provide these through typed schema/profile APIs.

## 21.6 Release AOT

Release builds should not require parsing `.vs` source at startup.

Build output should contain compact typed IR/assets.

## 21.7 Hot reload is transactional

Hot reload should:

```text
compile
validate
prepare migration
diff
commit atomically
```

On failure, keep last-good UI/runtime state when possible.

## 21.8 Stable keys

Dynamic repeated UI must use stable keys when identity matters.

Strict mode should warn about stateful repeated content without stable keys.

---

# 22. Dynamic Script Security

If a general script VM exists, use capabilities, not only execution budgets.

Potential capabilities:

```text
ui
timer
network(hosts)
asset_read(pattern)
clipboard
filesystem
native_service(name)
```

AI-generated or third-party code should not automatically receive all native capabilities.

---

# 23. Platform Rules

`viso-platform` should remain narrow.

It owns OS-facing primitives such as:

- window/surface;
- raw input;
- lifecycle;
- native handles;
- cursor;
- clipboard hook;
- display information.

It must not become a bucket for:

- optional Viso DSL scripting;
- Studio protocol;
- widgets;
- app navigation;
- arbitrary networking/business services.

Platform-specific modules may remain inside one crate until measurement/dependency complexity justifies separate crates.

---

# 24. Services Rules

Use service protocols for low-frequency app/platform capabilities:

```text
files
share
permissions
camera
location
notifications
secure storage
haptics
network
```

Trait objects are acceptable here when they simplify portability/testing.

Every service should be mockable for headless tests.

Business pages should not scatter `#[cfg(target_os = ...)]` branches for standard platform behavior.

---

# 25. Async Rules

Viso owns the UI frame loop.

External runtimes such as Tokio are adapters, not the owner of the application frame scheduler.

Viso owns a UI task protocol (task identity, wakeup, cancellation, timers/lifecycle integration, scoped ownership), not a general-purpose async executor, I/O reactor, HTTP stack, or TLS stack. Prefer adapters to mature executors and I/O libraries.

Default application API should be simple:

```rust
cx.spawn(async move {
    ...
});
```

Scoped tasks must define cancellation behavior when a component/page unmounts.

Do not block the UI thread waiting for async work.

Do not hold UI arena references across `.await`.

---

# 26. Threading Rules

The UI tree is primarily main-thread owned unless a future ADR explicitly introduces another model.

Background threads may perform:

- image decode;
- network I/O;
- expensive parsing;
- AI/model work;
- asset loading;
- selected text shaping/cache work if data ownership permits.

Communicate results back through typed queues/handles.

Do not wrap the entire UI state in a mutex just to make it `Send + Sync`.

---

# 27. Unsafe Rules

Unsafe is allowed where it has a clear performance/FFI purpose.

Typical valid zones:

- OS FFI;
- GPU FFI;
- packed buffers/SIMD;
- proven arena fast paths;
- mapped memory;
- generated ABI bridges.

Every unsafe block must include a `SAFETY:` comment describing the invariant.

Do not use unsafe to bypass architecture ownership rules.

If safe code is within a small measured margin, prefer safe code.

---

# 28. Allocation Rules

When changing hot code, inspect allocations.

Do not introduce hidden allocation in:

- node traversal;
- layout loops;
- paint loops;
- hit testing;
- animation ticks;
- renderer batch building;
- per-glyph iteration.

Watch for hidden allocation from:

```text
format!
collect::<Vec<_>>()
Box::new
String cloning
HashMap entry growth
closure boxing
trait-object boxing
```

Preallocate/reuse scratch buffers when stable workloads justify it.

---

# 29. Hashing and Strings

Strings are welcome in authoring/compiler/debug tooling.

Strings are not the normal identity mechanism for runtime hot paths.

Compile names into IDs:

```text
NodeId
PropertyId
TokenId
ShaderId
TextureId
PipelineId
ComponentTypeId
```

A runtime HashMap may be appropriate for infrequent lookup/registry operations, but not for every node every frame.

---

# 30. Error Handling and Diagnostics

Compiler/tooling diagnostics should carry:

- severity;
- stable diagnostic code;
- primary source span;
- related spans;
- notes;
- fix suggestions when safe.

Do not replace actionable user errors with generic panics.

Internal invariant failures may panic in debug builds.

Memory corruption or silent fallback is never an acceptable response to an invalid GPU/shader layout.

---

# 31. Project Structure for Examples and Apps

Prefer feature-first organization and progressive splitting.

Start small:

```text
features/home/
    mod.rs
    view.vs
```

Split only when needed:

```text
features/home/
    mod.rs
    view.vs
    state.rs
    model.rs
    api.rs
```

Do not force every page to have state/effects/models/controller files from day one.

Avoid top-level `utils/` and `common/` junk drawers.

---

# 32. File Types

Default first-class source formats are:

```text
.rs
.vs
```

Do not invent a new `.theme`, `.route`, `.asset`, or other language/file type without a concrete reason and an accepted design change.

Each new language implies long-term costs for:

- parser;
- formatter;
- LSP;
- diagnostics;
- migration;
- documentation;
- AI tooling.

Theme can live in `theme.vs`. Routes can normally be typed Rust.

---

# 33. Widget Rules

Official widgets must:

- have typed public properties/events;
- declare invalidation behavior;
- expose semantics/accessibility;
- avoid per-frame allocation in normal use;
- avoid string child lookup in event/paint hot paths;
- work in headless tests where applicable;
- have benchmark coverage if performance-sensitive.

Large/complex features such as PDF/browser/map/chart should not be added to the default widget crate dependency graph.

---

# 34. Tooling Rules

Studio, Inspector, and AI tools must use stable introspection/debug interfaces.

Do not add framework behavior that only works because Studio knows private memory layout.

Debug APIs should expose:

- Node tree;
- layout bounds;
- dirty reasons;
- state dependency edges;
- semantics;
- paint primitives;
- batches;
- frame timings;
- GPU resource counters.

---

# 35. Testing Requirements

Every non-trivial subsystem change needs appropriate tests.

Use the smallest sufficient set:

## Correctness

```text
cargo test --workspace
```

or project-specific narrowed tests while iterating.

## Formatting

```text
cargo fmt --all -- --check
```

## Lints

Use repository-defined Clippy command. If none is defined, prefer:

```text
cargo clippy --workspace --all-targets -- -D warnings
```

Do not blindly add `--all-features` when platform-exclusive features cannot coexist; follow the repository CI matrix.

## Visual/UI changes

A successful `cargo check` is not visual verification.

Use the repository's headless snapshot or Studio runner once available.

For UI changes, verify at least one of:

- screenshot/golden;
- node/layout dump;
- input replay;
- semantics snapshot;
- direct interactive Studio verification.

## Parser/compiler changes

Add:

- positive parse/type tests;
- diagnostic tests;
- malformed input recovery tests;
- incremental edit tests where applicable.

## Unsafe/GPU changes

Add boundary/invariant tests and run relevant renderer benchmarks.

---

# 36. Benchmark Requirements

Performance-sensitive changes must use release/profile builds.

Never compare debug build timing and call it a performance result.

Canonical benchmark categories should include:

```text
layout
large list
text
state invalidation
animation
hit testing
paint
batching
startup
memory
hot reload
```

When a benchmark regresses materially, do not hide it behind unrelated cleanup.

Explain the tradeoff or fix the regression.

---

# 37. Profiling Rules

Before optimizing, identify the cost center.

Collect at least one of:

- CPU profile;
- GPU capture/timing;
- allocation profile;
- frame phase timing;
- cache/memory measurement.

Avoid speculative complexity such as inline caches, custom allocators, or unsafe shortcuts until the relevant path is demonstrated hot, unless the architecture document explicitly requires the structure to avoid known systemic cost.

---

# 38. Migration Rules

Viso is a clean-slate framework. Migration support is source-level tooling, not runtime compatibility.

## 38.1 Runtime compatibility is forbidden

Production crates MUST NOT introduce or depend on:

```text
Makepad WidgetRef wrappers
Makepad Widget-in-Node hosts
Makepad UI-runtime compatibility feature flags
dual Makepad/Viso widget runtimes
```

Do not keep an Makepad runtime abstraction alive merely to make migration easier.

## 38.2 Migration belongs to tooling

Makepad-aware code may exist under:

```text
tools/migrate/
tests/migration/
fixtures/makepad/
docs/migration/
```

It MUST NOT become a dependency of:

```text
viso-runtime
viso-platform
viso-gpu
viso-render
viso-ui
viso-widgets
viso-dsl
```

The migration tool may parse and understand Makepad. The Viso runtime must not.

## 38.3 Migration autofix must be conservative

Treat migration output as:

```text
Auto      proven local semantic rewrite
Assisted  generated Viso skeleton + precise diagnostics/TODOs
Manual    architecture/lifecycle rewrite required
```

`--apply` may only perform idempotent, high-confidence Auto fixes. Do not create runtime wrappers to inflate migration automation coverage. The primary value of `viso migrate` is structural analysis and a trustworthy migration plan.

The Rust-side DSL entry points are `ui!`, `component!`, and `view!`. Do not add an alternative general-purpose DSL entry-point macro; keep their parser-entry contracts explicit and route all three through the same schema/HIR/IR pipeline.

At least one real Makepad crate must be kept as a migration canary. Track `script_mod!` recognition, ScriptVm/module-graph recovery, mapping coverage, Auto/Assisted/Manual distribution, and characterization-test parity. Do not promise an auto-migration percentage before canary data exists.

## 38.4 Migrate semantics, not architecture

When porting an existing subsystem, preserve useful:

- behavior;
- algorithms;
- tests;
- performance characteristics;
- platform edge cases;
- shader/text knowledge.

Do not preserve obsolete ownership, lifecycle, or API structure.

A Makepad widget should be used as a behavior/performance reference and then reimplemented directly on Viso Node/State/Layout/Input/Paint contracts.

## 38.5 Prefer vertical native slices

Prefer making one small Viso slice fully native:

```text
input -> state -> layout -> paint -> GPU
```

rather than creating compatibility wrappers across many subsystems.

## 38.6 Keep the Makepad baseline measurable

Do not delete Makepad characterization fixtures or benchmark data until the corresponding Viso implementation can be compared.

The baseline is a measurement reference, not a runtime dependency.

# 39. Legacy-to-Next Mapping

Treat these as migration intent:

```text
app_main! / AppMain
    -> viso::run::<App>() / Application

&mut Cx everywhere
    -> phase-specific contexts

WidgetRef
    -> NodeId / Handle<T>

Widget object owns everything
    -> Component + Node + Painter

Walk / public Turtle knowledge
    -> declarative layout + internal specialized algorithm

manual render/redraw after state mutation
    -> reactive invalidation

new_batch authoring property
    -> automatic renderer batching

script_mod! registration ordering
    -> module dependency graph

ScriptVm runtime evaluation / App::from_script_mod
    -> Viso `.vs` compiler + dev hot-reload evaluator + schema/module instantiation

implicit GPU instance struct tail
    -> explicit GpuInstance descriptor
```

Do not make a migration superficially compile while keeping the old semantics hidden behind new names.

For Makepad-to-Viso migration work, treat the current Makepad source model as:

```text
Rust source
  -> script_mod! macro bodies
  -> ScriptVm initialization/evaluation
  -> App::from_script_mod
  -> Struct::register_widget(vm)
  -> mod.widgets.* / prelude registration order
```

The migration tool MUST parse or structurally inspect these sources and reconstruct their dependency graph. It MUST NOT assume a `.live` file exists. Historical `live_design!` code is outside the default current-source migration path unless a task explicitly targets archived Makepad code.

---

# 40. Change Scope Rules

Keep changes focused.

Do not combine all of these in one patch unless unavoidable:

- crate move;
- API redesign;
- formatting entire repository;
- performance optimization;
- behavior change;
- migration cleanup.

Focused commits make A/B validation and regression diagnosis possible.

When a broad mechanical move is needed, prefer:

1. mechanical move with no behavior change;
2. verify;
3. semantic redesign in follow-up.

---

# 41. Refactoring Rules

Before creating a new abstraction, answer:

1. What dependency or invariant does it clarify?
2. Is it on a hot path?
3. Does it allocate?
4. Does it introduce dynamic dispatch?
5. Does it require a new crate?
6. Can the same goal be achieved with a module/private type?
7. How will it be measured/tested?

Avoid abstraction for its own sake.

---

# 42. Dynamic Dispatch Checklist

Before adding `dyn Trait` on a path called per frame/per node, stop and justify it.

Ask:

- Can enum dispatch work?
- Can static generics work?
- Can data be grouped by kind and processed in batches?
- Can dispatch happen once per batch rather than once per node?

Dynamic dispatch is acceptable when frequency is low or extensibility is worth the measured cost.

---

# 43. Data Layout Checklist

For hot structures, inspect:

- total size;
- alignment/padding;
- cache-line locality;
- fields touched together;
- cold fields mixed into hot structs;
- pointer indirection;
- allocation ownership.

Do not blindly convert everything to SoA. Use access patterns and benchmarks.

---

# 44. Rust Ownership Guidance

Prefer clear single ownership in runtime arenas.

Use:

```text
NodeId / typed handles
owned Vec/arena storage
short-lived borrows
message queues for cross-thread transfer
```

over pervasive shared interior mutability.

`Rc<RefCell<T>>` is not globally banned, but it requires strong justification in new framework hot-path code.

It is acceptable for cold tooling/compiler/editor structures when appropriate.

---

# 45. HashMap Guidance

HashMap is not banned.

Good uses:

- compiler symbol tables;
- resource registry lookup during creation;
- tooling indices;
- service registry;
- infrequent cache misses.

Bad uses:

- lookup every property of every node every frame;
- hit-test identity resolution per event when direct IDs exist;
- renderer batch property lookup when a packed key can be computed.

---

# 46. Logging Guidance

Do not log inside per-node hot loops by default.

Debug tracing should be feature/profile gated and preferably recorded into compact event buffers.

Profiler instrumentation must have known overhead and be disableable/strip-able in release.

---

# 47. Documentation Requirements

When adding a public concept, document:

- what it is;
- when normal users need it;
- lifecycle/ownership;
- performance implications if relevant;
- relation to adjacent concepts;
- minimal example.

Architecture-changing PRs must update the architecture document or add an ADR.

Do not let implementation become the only source of truth.

---

# 48. Examples Policy

Examples are part of the API contract.

Maintain a progression such as:

```text
00-minimal
01-counter
02-layout
03-state
04-navigation
05-async
06-list
07-text-input
08-adaptive
09-accessibility
10-custom-shader
99-full-app
```

New examples should demonstrate the Viso 1.0 architecture, not Makepad compatibility patterns.

Do not copy internal APIs into examples because public APIs are missing; fix the public API or clearly label the example advanced/internal.

---

# 49. Review Checklist — Architecture

Before finalizing a framework change, verify:

- [ ] Dependency direction is preserved.
- [ ] No unnecessary new crate was created.
- [ ] No vague `core/common/utils` bucket was introduced.
- [ ] Public API complexity did not increase without reason.
- [ ] New state/property behavior has a DirtyMask contract.
- [ ] Studio/tooling dependency did not leak into runtime.
- [ ] Pure Rust path still works without Viso DSL runtime assumptions.
- [ ] Release path does not require developer-only metadata unnecessarily.

---

# 50. Review Checklist — Performance

For hot-path changes:

- [ ] Allocation behavior checked.
- [ ] No accidental String/HashMap lookup added in inner loops.
- [ ] No new `Rc<RefCell>` node traversal.
- [ ] No per-node backend virtual dispatch added.
- [ ] Dirty propagation remains targeted.
- [ ] Layout cache invalidation is correct.
- [ ] Paint cache reuse remains possible.
- [ ] GPU resource reuse remains possible.
- [ ] Relevant release benchmark run.

---

# 51. Review Checklist — UI Correctness

- [ ] Pointer behavior verified.
- [ ] Keyboard/focus behavior verified when relevant.
- [ ] IME behavior considered for text controls.
- [ ] Accessibility semantics updated.
- [ ] Layout tested under resize/scale.
- [ ] Mobile safe area/keyboard considered when relevant.
- [ ] Dynamic/repeated children have stable identity where needed.

---

# 52. Review Checklist — Viso DSL / Compiler / Hot Reload

- [ ] CST/AST/HIR layers updated coherently.
- [ ] Formatter/source preservation considered.
- [ ] Diagnostics have source spans.
- [ ] Incremental edit behavior tested.
- [ ] Module graph remains deterministic.
- [ ] Release AOT path updated.
- [ ] Hot reload failure keeps last-good state when feasible.
- [ ] State migration semantics defined for structural changes.

---

# 53. Review Checklist — GPU/Unsafe

- [ ] ABI layout explicitly validated.
- [ ] Unsafe has a SAFETY comment.
- [ ] No field-order convention is relied on implicitly.
- [ ] Resource lifetime is explicit.
- [ ] Device/surface loss path considered.
- [ ] Backend-specific code is behind a clean capability/boundary.
- [ ] GPU timings/draw calls checked for performance-sensitive changes.

---

# 54. Agent Workflow

When assigned a task:

1. Identify the owning subsystem/crate.
2. Read its crate docs and relevant architecture section.
3. Search for the Viso 1.0 recommended pattern before copying Makepad-specific patterns.
4. Determine whether the change touches a hot path.
5. If hot, identify the benchmark/profile needed before implementation.
6. Make the smallest architecture-correct change.
7. Add/update tests.
8. Run formatting/lints/tests appropriate to the changed area.
9. For UI behavior, perform actual runtime/headless/Studio validation; compilation alone is insufficient.
10. For performance claims, run release benchmarks.
11. Update docs/ADR when architecture or public behavior changes.

---

# 55. Agent Rules During Ambiguous Tasks

If a task is underspecified, prefer the architecture's simplest stable path.

Do not invent a new subsystem, crate, DSL feature, or generic abstraction just because requirements are ambiguous.

When two approaches are reasonable:

1. prefer the one with a smaller public API;
2. prefer the one preserving static fast paths;
3. prefer the one with clearer ownership;
4. prefer the one measurable by existing tests/benchmarks;
5. document the tradeoff if it is architectural.

---

# 56. Do Not Preserve Legacy Accidents

Source migration does not mean preserving every Makepad internal concept.

Do not carry forward an old pattern merely because many files use it.

Examples of patterns intentionally targeted for replacement:

```text
universal Cx capabilities
Rc<RefCell<dyn Widget>> as primary UI identity
manual render() after ordinary state changes
manual batch boundaries for normal UI correctness
script module registration order requirements
implicit shader instance field-order ABI
string-heavy hot-path lookup
```

Do not implement these as runtime shims. Use source migration tooling and native Viso rewrites instead.

---

# 57. Do Not Over-Engineer Small Components

A Button does not need a directory of ten files unless behavior actually requires it.

A new application feature does not need separate state/model/effects/controller files by default.

Progressive structure is preferred over mandatory ceremony.

Engineering clarity means responsibilities are obvious, not that every responsibility has its own crate/file.

---

# 58. Do Not Optimize the Wrong Layer

Do not reject a clean service trait because a virtual call exists once when opening a file picker.

Do reject a design that allocates and dynamically dispatches for every node during layout.

Always ask:

> How often is this executed, and how much data does it touch?

Performance priorities follow execution frequency and data volume.

---

# 59. Do Not Confuse Declarative Syntax with Rebuild Semantics

A declarative Rust macro or `.vs` syntax does not imply Virtual DOM.

Compiler/macros should lower declaration into static templates, binding metadata, and retained runtime structures whenever possible.

Do not implement a declarative API by reconstructing a heap tree every frame unless an ADR and benchmark demonstrate that it is the right tradeoff.

---

# 60. Do Not Let Dev Features Tax Release

Hot reload, source maps, inspector strings, reflection, dynamic scripting, and verbose diagnostics are valuable.

They must not automatically impose permanent steady-state release costs.

When adding development metadata, specify whether it can be stripped.

When adding a dynamic path, keep a static/AOT path where architecture requires one.

---

# 61. Expected Performance Counters

New runtime/renderer code should make it possible to expose counters such as:

```text
frame_cpu_ms
frame_gpu_ms
node_count
visible_node_count
dirty_style
dirty_layout
dirty_paint
draw_calls
quad_instances
glyph_instances
gpu_upload_bytes
allocations_per_frame
```

If a new subsystem can dominate frame cost but has no timing/counter visibility, add instrumentation as part of the subsystem.

---

# 62. Expected Debug Introspection

Important runtime structures should be inspectable without unsafe memory poking:

```text
NodeId -> parent/children
NodeId -> layout box
NodeId -> dirty flags/reasons
NodeId -> component/schema
NodeId -> semantics
NodeId -> paint primitive ranges
StateId -> bindings
BatchId -> pipeline/resources
```

This supports Studio, tests, and AI automation using the same underlying model.

---

# 63. Naming Conventions

Use names that reveal responsibility.

Prefer:

```text
NodeArena
FrameScheduler
PaintCache
BatchBuilder
ShaderIr
StateSlot
BindingTable
SemanticsTree
```

Avoid names that merely indicate importance:

```text
CoreManager
GlobalContext
CommonState
BaseSystem
Utils
```

Use `Manager` sparingly; often the managed resource itself has a clearer name.

---

# 64. Feature Flags

Feature flags should represent meaningful optional capability, not arbitrary internal file boundaries.

Good candidates:

```text
hot-reload
mobile
accessibility
inspector
```

Potentially poor candidates:

```text
layout
state
button
quad
```

if they are fundamental to normal framework operation.

Avoid combinatorial feature matrices that CI cannot realistically validate.

---

# 65. Backends

Do not create a separate crate per backend merely because backends exist.

Start with modules where practical.

Split only when:

- target dependencies materially isolate;
- build scripts/toolchains differ significantly;
- compile times benefit;
- ownership/release needs justify it.

This rule applies to platform and GPU backend organization.

## 65.1 Backend support tiers

Tier-1 target paths are:

```text
macOS/iOS -> Metal
Windows   -> D3D12
Linux     -> Vulkan
Android   -> Vulkan
Web       -> WebGPU
```

This is a performance/support baseline, not a lowest-common-denominator contract. Compatibility backends (for example GL/GLES or a reduced Web path) may be added as Tier-2 when adoption data justifies them. They must not weaken Tier-1 RHI/render semantics; capability differences must be explicit and testable.

---

# 66. Headless Backend

Headless is a first-class testing backend, not an afterthought.

It should support deterministic:

- time;
- input tapes;
- window/surface size;
- node/layout dumps;
- semantics dumps;
- screenshots or render output where possible;
- frame counters.

Tests should not require physical pointer interaction when a deterministic input tape can represent the case.

---

# 67. CI Architecture Gates

The repository MUST enforce architecture policy in CI. The baseline command is:

```text
cargo xtask arch-check
```

The repository should also enforce:

- forbidden crate dependency edges;
- formatting/lints;
- unit/integration tests;
- headless UI snapshots;
- parser/shader fuzz smoke tests;
- benchmark trend reporting;
- public API change reports;
- unsafe inventory where feasible.

Agents adding architecture-critical code should contribute to these gates rather than relying on review memory.

---

# 68. ADR Trigger Conditions

Create/update an ADR when changing any of these:

- crate dependency direction;
- node ownership/identity;
- reactive semantics;
- layout sizing model;
- frame phase semantics;
- renderer primitive/batching model;
- GPU RHI contract;
- Viso DSL language/module semantics;
- async runtime ownership;
- public application/component lifecycle;
- migration-boundary policy;
- self-build vs external-dependency ownership for a major subsystem;
- `Handle<T>` state-access semantics;
- transform/layout invalidation relationship;
- canonical DSL source-form semantics.

Minor implementation details do not need ADRs.

---

# 69. Completion Criteria for Agent Tasks

A task is not complete merely because code compiles.

Completion means the relevant combination of:

- implementation complete;
- tests added/updated;
- format/lint clean;
- `cargo xtask arch-check` clean for architecture-sensitive changes;
- runtime behavior verified;
- performance measured if claimed or hot-path-affecting;
- docs/ADR updated when architecture changes;
- migration behavior documented if migration-related.

State explicitly what was and was not verified.

---

# 70. Final Engineering Standard

When evaluating any design, ask four questions:

### Public API

Can a normal application developer avoid learning this internal concept?

### Engineering

Does ownership and dependency direction remain obvious?

### CPU

Does the hot path operate on compact IDs/data rather than scattered shared objects?

### GPU

Can work be batched, cached, reused, and partially updated?

A good Viso change should make at least one of these better without accidentally making the others substantially worse.

The framework should feel simple at the top because complexity is intentionally contained below—not because the implementation ignores hard problems.
