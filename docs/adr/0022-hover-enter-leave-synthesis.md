# ADR 0022 — Hover: Per-Node Enter/Leave Synthesis

- Status: Accepted
- Date: 2026-09-07

## Context

ADR 0006 opened the pointer-phase dispatch surface (Down/Move/Up + a
window-level Leave) and ADR 0007 added wheel routing; together they gave the tree
a hit-tested, capture-aware pointer route (§13: normalized input → hit-test →
capture → target → bubble). What that route never had was **hover**: no notion of
"the pointer is currently over this node", no per-node enter/leave notification,
and so no substrate for the hover feedback every conventional control needs
(a button lightening on mouse-over, a cursor hint, a tooltip trigger). The only
`Leave` that existed was window-level — the pointer left the whole window — with
no per-node meaning and no consumer.

This slice adds per-node hover. It touches the input event semantics (a new
pointer phase, a redefined `Leave`) and the frame/dispatch surface (a synthesized
single-node dispatch distinct from the chain route), both §68 ADR triggers, so
the contract is recorded here.

Makepad is the behavior reference (§38.4). Its model: a mouse-move is delivered to
the tree, then `cycle_hover_area()` **commits** the new hover after dispatch
(propose-during-dispatch, commit-after); `Hit::FingerHoverIn`/`FingerHoverOut`
are per-node enter/leave, distinct from a window-level leave; `Event::ClearHover`
broadcasts "drop all hover" when the pointer leaves the window or a capture
begins. Hover is **single-leaf** — a `handled` flag lets the front-most area claim
it, so exactly one node is hovered — and drives a paint-only animated mix, never
layout. Viso keeps these semantics and drops the `Turtle`/area architecture:
a single retained hover slot, a move-time diff, and a single-node dispatch.

## Decision

### 1. `PointerPhase::Enter` + `Leave` redefined as per-node

Rather than introduce a separate hover event type (which would duplicate the
whole `EventCx`/take-handler/restore dispatch machinery — against §55's
smaller-public-API rule), hover rides the existing `PointerPhase` enum. A new
`Enter` variant means "the pointer entered this node"; `Leave` is redefined from
"the pointer left the window" to the per-node dual meaning "the pointer left this
node". A control already matches on `phase` inside its one pointer handler, so
`Enter`/`Leave` branches cost it no new API surface.

The router never delivers a bare window-level leave to the tree. A window leave is
handled by *synthesizing* a per-node `Leave` to whatever was hovered and clearing
the slot — equivalent to Makepad's `ClearHover` broadcast, but targeted at the one
hovered node instead of walked across the tree.

### 2. A single retained hover slot on `NodeStore`

`NodeStore` gains `hovered: Option<NodeId>`, with `hovered()` / `set_hovered()`
mirroring the existing `capture`/`focused` single-slot, live-guarded accessors: a
write pointing at a freed handle is a no-op, a clear is always honored, and a
structural rebuild resets it. This is a single pointer's hover this slice — a cold,
one-slot projection of the current hit target, not a per-node hot column (§8.4).
Zero steady-state cost: when the pointer moves within one node the slot is
untouched.

### 3. Move-time diff, single-node dispatch, commit-after

Hover is synthesized only on the **non-capture Move** branch of the router (and on
window Leave). Given the move's hit target `new` and the stored `old`:

- `new == old` → nothing dispatched, slot untouched (the steady-state move within
  one node does zero hover work and allocates nothing);
- otherwise → `Leave` is dispatched to `old` (if any), then `Enter` to `new` (if
  any), then `set_hovered(new)` commits the new target **after** dispatch, matching
  Makepad's commit-after cycle.

Dispatch is **single-node**, not the capture→target→bubble chain: hover is
single-leaf (only the entered/left node is notified), so there is nothing to
bubble and no `stop` to honor. A dedicated `hover_dispatch` mirrors the chain
dispatch's take-handler → `EventCx` → restore borrow dance and the *same* deferred
side-effect apply (capture/focus/visibility/queued requests), so both routes
settle handler effects through one shared path with no special cases. A hover
handler almost always only writes a reactive PAINT cell, but the full apply keeps
one code path.

### 4. Hover is PAINT-only and never bubbles

A hover state change feeds visual feedback only — a background/foreground mix — so
its reactive binding invalidates PAINT and nothing more (§11). It never marks
MEASURE/LAYOUT and never bubbles to ancestors, the same non-bubbling contract as
the scroll transform of ADR 0007. Hover is also **non-semantic**: it does not
enter the accessibility tree (§15) — an accessibility snapshot is identical hovered
or not.

### 5. Capture suppresses hover synthesis

While a pointer is captured (a drag, a slider grab) the router routes straight to
the capturing node and synthesizes no hover: a drag must not light up whatever
passes under it. Hover follows the free-moving pointer, exactly as the platform's
own hover cycle is move-driven. A window Leave still clears hover regardless of
capture — the pointer is gone.

## Consequences

- The tree gains per-node hover with one enum variant, one store slot, and one
  dispatch helper — no new event type, context, or public trait.
- The first consumer is `Button`, which adds a `hovered` reactive cell bound PAINT
  and a hover box in its style, with paint priority pressed > hover > resting.
- The steady-state move within a hovered node is a hit-test plus one `new == old`
  comparison: no dispatch, no allocation. Verified by a microbench (move-within vs
  cross-node) and an allocation profile (steady move = zero alloc).
- `Leave` no longer means "left the window" to any handler; it always means "left
  me". The facade's window-leave lowering feeds the router, which does the
  per-node synthesis — no facade change was needed.
- Hover is single-leaf by construction (one slot, one entered node). Overlapping
  interactive nodes resolve through the existing front-to-back hit test, which
  already returns the top-most node.
