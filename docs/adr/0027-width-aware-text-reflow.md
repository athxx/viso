# ADR 0027 — Width-aware text reflow via two-phase facade-driven constraint downflow

- Status: Accepted
- Date: 2026-09-09

## Context

A text leaf's height depends on the width it is allowed to occupy: a paragraph
that soft-wraps is taller in a narrow box than a wide one. But in Viso's layout
model width is a layout **output**, not a measure **input** — the measure pass
reads a node's cached intrinsic (`natural`) size bottom-up, and the width a
Flex/Grid box actually assigns is only known afterwards, top-down. So the
information text needs to compute its height arrives after the pass that would
consume it.

Two facts made a soft-wrap leaf never wrap, even though the engine could:

- The text engine **already** wraps. `viso_text::prepare(font, text, size,
  max_width: Option<f32>, ...)` breaks a run into rows and expands the per-row
  vertical metrics, so a wrapped paragraph's height is already correct
  (`crates/text/src/layout.rs`, benched at ≤8× the unwrapped alloc baseline in
  `crates/text/benches/wrap_line.rs`). The width simply never arrived: the
  facade shaped every run at a hardcoded `None`, so `natural` was always the
  unconstrained single-line width and the measure pass read that.
- The layer that decides width (`viso-ui`) **cannot** shape text. The
  architecture DAG forbids a `viso-ui → viso-text` edge (§3.5); the UI holds no
  font stack. Only the facade (`crates/viso`, an allowed `viso-text` dependent)
  owns the `TextShaper`. Whatever activates wrapping must not require the UI to
  shape.

This touches two §68 trigger areas — the layout sizing model and frame-phase
semantics — so it is recorded here. §20 / [[0025-text-subsystem-ownership-and-cache-boundaries]]
also governs it: "if text, font, shaping features, and width are unchanged, do
not reshape/reflow." Width now genuinely changes on resize, so the reflow path
must honor that boundary precisely rather than reshaping every frame.

## Decision

Reflow text in **two phases**, keeping the measure/layout engine ignorant of
text. Layout records a pure-data *reflow request* when it assigns a
wrap-eligible leaf a width its content was not shaped at; the facade — which
owns the font stack — drains the queue, reshapes at that width, writes back, and
re-runs layout, in a bounded loop.

### 1. Data model (`crates/ui/src/content.rs`)

- `Content::Text` gains `shaped_at_width: Option<f32>` — the `max_width` the run
  was actually shaped/wrapped at (`None` = shaped unconstrained, single-line).
  It participates in the existing `#[derive(PartialEq)]`, so a width-only
  reshape compares unequal and repaints, as intended.
- `Content::Text` and `TextRequest` gain `soft_wrap: bool` (default `false`).
  When `false`, a width-constrained leaf stays single-line and clips/overflows;
  when `true`, it wraps to its assigned box. `soft_wrap` is copied from the
  request onto the shaped payload at shape time so the layout-side recorder
  reads **only** the content column — it never reaches back into the request
  (which is drained at shape time; see §4).

### 2. Facade shaping (`crates/viso/src/text_content.rs`)

`TextShaper::shape` takes `max_width: Option<f32>` and passes it to `prepare` in
place of the former hardcoded `None`, but only when the run opted in
(`wrap_width = if request.soft_wrap { max_width } else { None }`). The produced
`Content::Text` records `shaped_at_width` so the layout pass can later tell
whether an assigned width still matches. The first shape of every run passes
`None` (single-line natural, which the measure pass reads for a `Fit` axis).

### 3. Layout recorder (`crates/ui/src/layout.rs`)

`LayoutTree` gains `fn request_text_reflow(&mut self, index, width)` with a
default no-op body — a pure data write; eligibility lives entirely in the impl.
The Flex child-placement loop, once a child's assigned box size is final and
before recursing, computes the child's assigned **width** for the axis
(`main_size` on a Row, `cross_size` on a Column) and calls the recorder
unconditionally. Grid, Scroll, and AbsoluteRows route through their own
`layout_*` functions and simply never call it — no special-casing.

### 4. Store queue + eligibility (`crates/ui/src/component.rs`)

The `request_text_reflow` impl holds all eligibility, reading only hot/content
columns already keyed by `index`:

- content is `Content::Text` with `soft_wrap` set (a non-wrapping run returns);
- the width axis `Length` is `Fill`/`Fixed`, never `Fit` (a `Fit` leaf sizes to
  content, is never width-constrained, and is the one genuinely hazardous case —
  see convergence);
- the assigned width, **quantized to integer physical px** (`width.round()`),
  differs from `shaped_at_width` beyond `EPS = 0.5` physical px. Trigger:
  `None => qwidth < natural.x - EPS` (unconstrained run that would wrap now),
  `Some(w) => (w - qwidth).abs() > EPS` (already-wrapped run whose box changed).

Eligible entries push `(arena.live_id(index), qwidth)` into a `text_reflows`
handoff Vec, drained by `take_text_reflows` (mirrors `take_text_requests`).

Two invalidation write paths:
- `set_content_payload` (text-change path) marks `MEASURE|LAYOUT|PAINT|SEMANTICS`.
- `set_reflowed_content` (width-only reshape) marks `MEASURE|LAYOUT|PAINT` but
  **omits SEMANTICS**: a width-only reshape does not change the accessible name,
  so dirtying the semantics tree on every resize would be spurious (§11/§15).

### 5. Facade drain loop (`crates/viso/src/lib.rs`)

In `FramePhase::Layout`, after `relayout_and_paint()` and before
`virtual_list::absorb_measurements` (which must see final wrapped heights), a
bounded loop: `take_text_reflows`; stop if empty; else reshape each entry at
`Some(width)` and write via `set_reflowed_content`; then re-run layout. Capped
at 3 passes.

Because the store **drains** the `TextRequest` column at shape time (an edit
re-declares it; it cannot be read back), Phase B has no source text to reshape
from. The facade therefore retains `wrap_sources: HashMap<NodeId, TextRequest>`
populated at shape time with **only** `soft_wrap` runs (a rare subset) and
liveness-pruned (`retain(is_live)`) on the first reflow pass, never a steady
frame. This preserves the store's write-once drain contract and leaves the hot
columns untouched.

### 6. Widget (`crates/widgets/src/text.rs`)

`LabelStyle` gains `soft_wrap` (default `false`) and a `.wrap()` setter, threaded
into the emitted `TextRequest`. Default labels are `Fit`/`Fit`, so they never
wrap regardless — `soft_wrap` only bites once the author sets width `Fill`/`Fixed`.

## Why this converges

A reshape only ever changes a leaf's **height** and can only shrink or hold its
natural **width** (wrapping never widens a run). Assigned width is a function of
the parent box for `Fill`/`Fixed` — the eligible axes — so it is stable across a
reshape: after the leaf wraps, its parent hands down the same width, the
mismatch clears, and the queue empties. The one width→height→width hazard (a
`Fit` run as a Row main axis, whose natural width would feed its own assigned
width) is excluded by eligibility. Monotonicity + eligibility is the real proof;
the 3-pass cap is a safety net, not the argument.

## Consequences

- **Resize is subsumed.** `on_geometry` already marks the root
  `MEASURE|LAYOUT|PAINT`, so the next Layout frame re-derives assigned widths and
  the width-mismatch trigger re-fires — no separate width-keyed cache.
- **The §20 unchanged-width contract holds.** The quantize-to-integer-px +
  `EPS` guard means a continuous drag-resize does not reshape on every sub-pixel
  frame; only a whole-pixel width change past the threshold reflows. This is the
  reflow-side reading of ADR 0025's "unchanged width must not reshape."
- **Known, non-steady first-frame cost.** A wrapped `Fill` paragraph is shaped
  once at `None` (Phase A) then once at its width (Phase B) on first appearance —
  inherent to the two-phase premise (width is a layout output), not a
  steady-state regression. A §61 counter, gated behind `VISO_FRAME_TRACE`,
  prints the reflow pass count so the double-shape is visible.
- **Scope is Flex `Fill`/`Fixed` only.** Grid (Auto/Fr tracks size from child
  naturals → genuine width→width feedback), Scroll (wrapping to the viewport
  defeats horizontal scroll — a policy decision), and AbsoluteRows (virtual-list
  canvas) are deferred; each would open its own ADR when a need arises. They
  never call the recorder, so they pay nothing.
- Nested `Fill` text propagates its taller wrapped height up through the
  existing `MEASURE` redo-from-parent path — no new propagation machinery.
