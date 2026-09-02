# ADR 0006 — Application Scene Seam, Reactive-Reach BuildCx, and Pointer-Phase Access

- Status: Accepted
- Date: 2026-09-02

## Context

ADR 0003 landed the retained tree; ADR 0004 seeded the first frame; ADR 0005
made an ordinary state write turn into targeted, incremental invalidation. Slice
G derived a `SemanticsTree` from the node model. Across those slices the facade
ran a *hardcoded demo `Scene`* — the tree was assembled by internal code, not by
the application, so there was no seam through which a normal `viso::run::<A>()`
app could author its own retained UI. The Phase 4 wrap-up closes that: `App`
becomes the thing that builds the scene, and `01-counter` becomes the first
end-to-end interactive Viso app (click → `set` → bound node re-derives/repaints),
exercised headlessly by an input-tape test.

Standing up that path surfaced one framework gap. A pointer handler is invoked
on every phase of a gesture (Down, Move, Up, Leave), but `EventCx` exposed no way
to read *which* phase — so a handler could not act on release only, and a single
down/up tap double-counted. Closing that gap is a public-API decision (the shape
of the handler's context), so it is recorded here alongside the scene seam.

This touches two §68 ADR-trigger areas — public application/component lifecycle
(the `build` seam) and input dispatch surface (`EventCx::pointer`) — so the
decisions are recorded together. Makepad's `AppMain`/`Cx` grants a universal
context and an event-walk that visits the tree per event; per the standing
divergence rule (§38.3) Viso keeps the *semantics* (an app authors its scene,
handlers read the sample under dispatch) but not that architecture — a
phase-specific `BuildCx`, a target-route dispatch, and one narrow accessor per
input kind.

## Decision

### 1. `Application::build` — the scene seam, default empty

`Application` gains a defaulted `fn build(&mut self, cx: &mut BuildCx<'_>)` whose
default body builds nothing (an empty window). `run::<A>()` calls it once on
launch, and its output *replaces* the framework's former hardcoded demo scene —
the facade default scene is now empty. An app authors its retained tree here and
nowhere else; there is no manual `render()`/`redraw_all()` (§10.1). Authoring is
a cold, once-per-launch pass, not a per-frame rebuild (§8.1, §59).

`build` is separate from `new(cx: &mut AppCx) -> Self` deliberately: `new`
constructs the app value under the application-scope marker context, which cannot
borrow the live reactive stores; `build` runs later with those stores reachable
(see decision 2), which is why state allocation and binding belong in `build`,
not `new`. `01-counter` shows the idiom — `count: Option<StateId>` is `None`
after `new` and filled in `build`.

### 2. `BuildCx::with_reactive` — build-time reach into the reactive stores

`BuildCx` gains a second constructor `with_reactive(store, states, bindings)`
alongside the plain `new(store)`. The reactive one borrows the `StateStore` and
`BindingTable` as sibling fields of the driver, so a single `BuildCx` can, in one
authoring pass, allocate state cells (`cx.state`), declare nodes (`cx.flex`/
`cx.leaf`), attach handlers (`cx.on_pointer`) and semantics (`cx.semantics`), and
wire bindings (`cx.bind`) — the whole reactive scene assembled inline. `new`
(reactive-None) stays for node-only tests and callers that author no state.

This is why scene authoring works in `build` where new-time allocation could not:
the session-long `AppCx` marker cannot retain a live store borrow, but the driver
holds `store`/`states`/`bindings` as sibling fields and can borrow all three
together into one build context for the duration of the call.

### 3. `EventCx::pointer()` — the pointer sample under dispatch

`EventCx` carries a borrow of the input event under dispatch and exposes one
accessor per kind: `pointer()`, `key()`, `ime()` — exactly one is `Some` per
dispatch. `pointer()` returns the `&PointerEvent` (phase, position, buttons,
modifiers) so a handler gates on phase: acting on `PointerPhase::Up` makes a
down/up pair count as one click, where before every phase fired the body. The
router builds this context via `EventCx::__new_pointer`, threading the sample
through `pointer_dispatch`; the state-only constructor `__new` leaves all three
`None`.

This keeps the input surface symmetric and narrow (§6.4): the three input kinds
have three parallel accessors, no universal event object, no per-event tree walk
— dispatch is still the target route capture→target→bubble of §13.

### 4. `01-counter` + headless input-tape test — the end-to-end proof

`examples/01-counter` is the first interactive example (§48): a click increments
a state cell bound to two nodes — a `Label`-role button (bound `SEMANTICS`, so
the count re-derives for assistive tech) and a paint proxy bar (bound `PAINT`).
It uses only the public facade, no internal crates (§3.1). Because there is no
text control yet, the count is accessible semantics plus a paint proxy, not
rendered glyphs — nothing fakes text (§20); the label becomes real text when a
text control lands.

`crates/viso/tests/counter.rs` drives the same scene headlessly through an input
tape (§66): it replays a synthetic down/up click through `PointerRouter::route`,
folds the pending writes through the state flush, and asserts the click
increments the cell and dirties *exactly* the two bound edges (the button's
SEMANTICS, the bar's PAINT — neither crossing into the other's class), that a
miss changes nothing, and that the derived semantics react. This is the §69
"runtime behavior verified" for the seam — headless is sufficient, no shader
change, no real-machine pass needed.

## Consequences

- The facade default scene is now empty; the demo `Scene` is deleted. A
  `viso::run::<A>()` with a no-`build` app opens an empty window, which is the
  correct floor — the framework builds nothing the app did not author.
- No new crate (§3.3): `Application::build` and `BuildCx::with_reactive` are
  items in existing crates, `EventCx::pointer` is one accessor; check-deps stays
  at 13 crates, no new edge.
- Tradeoff vs Makepad's universal `Cx` + event-walk: Viso trades one universal
  context for a phase-specific `BuildCx` (authoring) and `EventCx` (dispatch),
  and a per-event tree walk for a target route — smaller public mental model and
  a dispatch cost that scales with the route, not the tree. The standing
  divergence: port semantics, not the coarse model (§38.3).
- Known follow-ups, out of scope this pass:
  - **Structural teardown / effect cancellation on unmount.** `build` runs once
    and frees nothing, so there is no live call site for `cancel_for_node` yet;
    a targeted rebuild that frees nodes must run per-node effect cleanup before
    slot reuse. This is where that hooks.
  - **Text control.** The counter's label is semantics-only until a text control
    adds MEASURE+LAYOUT+PAINT on top; `StateValue::Text` and glyph rendering land
    with it.
  - **`build` re-invocation.** The seam runs once on launch; a hot-reload or a
    structural rebuild that re-authors part of the tree is a later slice, layered
    on the transactional-reload contract (§21.6) without changing this seam.
