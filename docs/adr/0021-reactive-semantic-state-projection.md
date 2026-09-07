# ADR 0021 — Reactive semantic-state projection (live control state in the derived semantics tree)

- Status: Accepted
- Date: 2026-09-07

## Context

The four stateful controls — `CheckBox`, `Toggle`, `Radio`, `Slider` — hold their live
state (checked, on/off, selected option, slider value) in a reactive cell. Their visual
paint already reacts to that cell through a `bind(cell, node, PAINT)` binding. The derived
accessibility tree did not: `NodeStore::derive_semantics` only read the *authored*, static
`Semantics` column (role + label), so an assistive-technology client — and the headless
a11y snapshot in §35/§66 — could never learn that a box is *checked*, a slider sits at
*value 42*, or which radio option is *selected*. All four roles even carried the same stale
comment ("the derive pass has no state store today"), and `Toggle`/`Slider`/`Radio` had to
borrow `Role::CheckBox` for lack of a dedicated role.

§15 makes accessibility mandatory architecture, and reactive semantics is an §68 ADR
trigger, so this records how live state reaches the derived tree.

The hard constraint is the §3.5 dependency direction. The derive pass holds only `&self`
(a `&NodeStore`); it must **not** read the state store. There was already a precedent for
"a live value the derive pass reads from a node column, not from the state store": `focused`
is a single node slot, and `derive_into` reads it off `&self`, never off a `StateStore`. The
governing criterion (best performance, least resource use, most reasonable design, correct
dependency direction) selects an extension of exactly that precedent.

## Decision

### 1. A compact `SemanticState` on a node side column (`crates/ui/src/semantics.rs`, `component.rs`)

`SemanticState` (`semantics.rs:138`) is a `Copy` struct carrying the a11y state scalars:
`checked: Option<bool>` (CheckBox / Toggle / Radio option), `value: Option<f32>` (a slider's
value, already resolved to the real range — not a 0..1 fraction), `range: Option<(f32, f32)>`
(a slider's inclusive `(min, max)` bounds, control constants that never enter a cell), and
`expanded: Option<bool>` (reserved for disclosure controls; structure only this slice, not
yet wired to a control). It is `Copy` with no heap data — a cold column entry that costs
nothing to store or read.

`NodeStore` gained a `semantic_state: Vec<Option<SemanticState>>` side column, aligned to the
arena alongside the `semantics` column, with the standard "live-guard + assign + mark_dirty"
setter shape (`component.rs`): `set_semantic_state(id, state)` (`:823`) writes the column and
marks the node `SEMANTICS`-dirty. SEMANTICS bubbles to the root, so an ancestor learns its
subtree changed without a separate mark. `derive_into` (`:1689`) reads the column into a new
`SemanticsNode.state` field off `&self` (`:1711`) — never a cross-layer state-store read.

Radio option selection folds into `checked` rather than a separate `selected` field: a
selected option is `checked = Some(true)`. Authored `Semantics` stays cold and static (role +
label only); live state never enters it.

### 2. Two dedicated roles (`crates/ui/src/semantics.rs`)

`Role` gained `Slider` and `Radio` (`semantics.rs:20`). The controls' authored roles are now
`Slider` for the slider, `Radio` for each radio option (`CheckBox` retained for checkbox and
toggle — a switch is a two-state boolean toggle). The stale "borrow CheckBox / no state store
today" comments are removed.

### 3. A projection binding parallel to `bind`, flushed where both stores are live (`component.rs`, `reactive.rs`)

The mechanism is a new binding parallel to the existing `bind`, **not** a change to
`Computed` semantics. A control registers it at build time with
`cx.bind_semantic_state(node, |cx| SemanticState { ... })` (`component.rs:2377`): the closure
reads one or more cells and returns the node's new `SemanticState`. `SemanticProjector`
(`reactive.rs:763`) owns these projections with a wake-driven reverse index
(`dep_index[state] -> Vec<ProjectId>`), mirroring the effect/computed reactor shape.

Projections run in the `FlushStateTransactions` frame phase (`crates/viso/src/lib.rs:784`),
the one steady point where both the node store and the state store are live, immediately
after `wake_computed` and before effects: `ws.projectors.wake(&changed, &states, &mut store)`.
`wake` re-runs only the projections whose cells changed, each computing the new
`SemanticState` and calling `set_semantic_state` — so the flush-phase projection reads the
state store, and the `&self` derive pass that follows reads only the node column. This is the
same read/write split as `resolve_styles` (state values → node style column) and the same
dependency discipline as `focused`.

The initial value is written at build time (the control seeds `set_semantic_state` with the
cell's initial value), so the first derived tree already carries state — no wait for the first
toggle. A single cell change drives both bindings: the `PAINT` bind (redraw the mark) and the
projection (update the semantic-state column, marking `SEMANTICS`).

Steady-state cost: `SemanticProjector::wake` allocates nothing on a warmed-up path. Three
buffers reach capacity after the first wakes and are then reused — the `wake_scratch` target
list, each projection's reverse-index dependency `Vec`, and the projector's reused dependency
`cursor` (`project()` takes/reuses/restores it via `core::mem::take` rather than allocating a
fresh `DepCursor` per eval). This is proven, not asserted (§7.3): the alloc-profile test
`crates/ui/tests/semantic_projection_alloc.rs` arms a counting global allocator over a 32-way
radio fan-out and requires `[0, 0]` allocations across two steady wakes. The microbench
`crates/ui/benches/semantic_projection.rs` measures `project_wake` (~90.7µs, FANOUT=256) and
`derive_with_state` (~4.2µs).

### 4. Text content marks SEMANTICS (backlog item 4) (`component.rs`)

A text node's visible run is its accessible name, so §11's contract is
`text content -> MEASURE + LAYOUT + PAINT + SEMANTICS`. `set_content_payload` (`:842`) now
adds `SEMANTICS` **only** when the content is `Content::Text`; `Image`/`Path` carry no
intrinsic accessible name and stay `MEASURE | LAYOUT | PAINT` (an image's alt text lives in
authored `Semantics`, not its pixels). This is a deliberately honest, narrow fix: `Content::Text`
holds already-shaped glyphs, not a source string, and the `TextRequest` string is cleared
from its column once the shaping tier drains it — so there is no name-to-recover-from-glyphs
path, and none is invented. The SEMANTICS mark drives a re-derive; the accessible name still
comes from the authored `Semantics.label` a text control writes alongside its content.

## Consequences

- The derived accessibility tree now carries live control state (checked / value / range /
  selected option) with dedicated `Slider`/`Radio` roles, so headless a11y snapshots and
  real assistive technology can announce state. Each control's snapshot test drives the
  change through an input tape and asserts the new state field before and after.
- The §3.5 red line holds: the derive pass never reads the state store. Live state is
  projected into a node column in the flush phase (both stores live), then read by `&self`.
  This mirrors the `focused` precedent exactly.
- A warmed-up projection wake allocates nothing (proven by the alloc profile), and a frame
  with no cell change runs no projection — the reverse index wakes only affected projections.
- `expanded` is structure-only this slice: the field and builder exist, but no control wires
  it yet. Disclosure controls (Navigation / Dialog expand-collapse) are a follow-up.
- The derive pass still rebuilds the whole tree from the root on any SEMANTICS mark
  (SEMANTICS bubbles, so any change reaches root). Per-subtree incremental derivation is a
  later refinement (Phase 8.2), deferred until a live consumer needs it.
