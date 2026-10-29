# ADR 0043 — Frame phases, dirty worklists and the transform plane

- Status: Accepted (2026-10-29)
- Date: 2026-10-17

## Context

ADR 0001 fixes twelve frame phases. That list has no slot for:

- animation ticks;
- `spawn_then` continuations and due timers;
- dev patch commit;
- effects;
- transform propagation, which §0.2 declares an independent plane.

Semantics are also built before paint, which puts them on the input-to-photon
path.

A transform or scroll change marks `TRANSFORM | HIT_TEST | PAINT` (ADR 0007),
so scrolling rebuilds and re-uploads the primitives of the visible subtree.

Dirty classes are bits on each node. Nothing specifies how a phase finds the
dirty nodes without visiting the whole tree.

## Decision

- Frame phases, in order: `CollectInput`, `DispatchInput`,
  `RunContinuations`, `ApplyDevPatches` (Dev only), `AdvanceAnimations`,
  `FlushStateTransactions`, `ResolveStyle`, `Measure`, `Layout`,
  `UpdateTransforms`, `BuildPaintChanges`, `BuildRenderBatches`,
  `UploadGpuChanges`, `Submit`, `RunEffects`, `UpdateSemantics`,
  `PostFrameCleanup`.
  - Writes from dispatch, continuations and animations form one transaction,
    flushed once.
  - Effects and semantics run after present.
- Every dirty class has a worklist. A node is appended when its bit goes from
  0 to 1. Measure and layout reduce their worklist to dirty roots, then
  process them by depth. Buffers are reused across frames. A phase with an
  empty worklist costs O(1).
- Transform roots get transform-table entries: a scroll container's content
  and a node that has been translated (it stays a root while it lives). Paint
  brackets each root's primitives with `Primitive::Transform(slot)` /
  `TransformEnd` and records the roots in pre-order, so an enclosing root
  precedes the roots inside it.
  - Primitives are written in world space as painted; the table records each
    root's displacement at that moment. A later frame that only moves roots
    recomputes the table (one add per root) and hands it to
    `Renderer::set_transforms`, which shifts each root's retained draws by
    `current - painted`. The shift travels in the `shift` member of the
    16-byte inline uniform every built-in already reads, and a clip opened
    inside a root moves with it. No instance carries a transform id, no
    instance is rewritten and nothing is uploaded: the cost is per draw, not
    per primitive, and no backend needs a storage buffer (GLES 3.0 and
    `CpuRaster` get it for free).
  - Draws in different roots never batch together. Content a shift cannot
    move — an offscreen layer (its region of interest was cut as painted), a
    backdrop or material, a user program — taints its root; moving a tainted
    root, or moving anything while the frame captures a backdrop, answers
    `NeedsUpload`, and the frame repaints at the new displacements.
  - Scrolling marks `TRANSFORM` only. A node's first translate also marks
    `PAINT` (the paint that records the new root); later ones mark `TRANSFORM`
    only.
  - On-screen rects are derived, not stored: `bounds` plus the displacement
    of the roots above. Hit testing carries the displacement down its walk —
    the pointer mapped through each root's inverse — so it needs no
    `HIT_TEST`.
  - Paint emits every node of a root, so a node scrolled into view always has
    retained paint; a virtual list mounts rows through structure, which paints.
  - Displacements snap to device pixels, so A8 glyph rasters stay valid.
  - A root's entry is a *place*: the affine from where its content was
    painted to where it draws, carried in the view uniforms (`shift` and the
    images of the axes) every built-in reads. Paint bakes only translations;
    a scaled or rotated root draws through its place even right after a
    repaint. A scissor follows only a translation, so a clip opened inside a
    turned root composites through an offscreen layer, and a root whose
    scissored clips would turn answers `NeedsUpload`. Content in an offscreen
    pass draws where its root sits relative to the pass's root.
- Each frame's damage is the union of old and new bounds of changed
  primitive ranges, and old and new world bounds of changed transform-root
  subtrees. An empty damage skips `Submit`.

## Consequences

- `viso_runtime::FramePhase::ORDER`, the scroll path (ADR 0007) and the
  VirtualList remount path (ADR 0008) are updated to match.
- Scroll cost no longer depends on the number of visible children.
- This ADR supersedes the phase list of ADR 0001 and the dirty set of
  ADR 0007 once accepted.
