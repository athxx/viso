# ADR 0025 — Text subsystem ownership: external-backed algorithms behind Viso-owned cache boundaries

- Status: Accepted
- Date: 2026-09-08
- Revised: 2026-09-10 — glyph representation revised in place from a single pure-SDF
  text lane to a quality/performance-first adaptive model (exact A8 Coverage
  default, Temporal Promotion to MTSDF under sustained transform, retained
  OutlineVector at extreme scale, source-aware colour glyph), with per-representation
  residency pools replacing the two-atlas split. Decision 2 below carries the revised
  model. The ADR number is retained; the superseded pure-SDF rationale is not preserved.

## Context

Rendering centred multilingual text (Latin, CJK, Thai, colour emoji) in one run
required the text subsystem (`crates/text/`) to grow from a Phase-2 single-face,
hard-coded-LTR, `split('\n')` shaper into a full pipeline: a multi-face fallback
chain, BiDi + script itemization, complex shaping, itemized layout, an adaptive
glyph representation, and per-representation residency pools. The governing
criterion is the standing one — best performance, least resource use, best
effect, most reasonable design, easiest to use, not least code.

Two questions touch §68 trigger areas and §3.7 (self-build vs dependency), so
they are recorded here rather than left implicit:

**1. Who owns the standards-heavy algorithms.** Unicode segmentation, BiDi,
OpenType shaping, and font rasterization are exactly the "prefer proven
algorithms" category of §3.7: standards-heavy, adversarially tested by their
ecosystems, and a poor use of repository purity to reimplement. Reimplementing
BiDi or a rustybuzz-equivalent shaper for repository purity would be a net loss.

**2. Where the cache boundaries sit.** §20 mandates the cache boundaries a text
subsystem must expose (font/face → shaping → paragraph/line layout → glyph
atlas) and that unchanged text must not reshape/reflow. These boundaries are the
part Viso must own, because steady-state frame cost and the retained-tree
invalidation contract depend on them — they are not delegable to a dependency.

## Decision

### 1. External-backed algorithms, Viso-owned integration (§3.7)

The subsystem integrates proven external crates for the standards-heavy work and
owns the pipeline, data model, and caches around them:

- **Segmentation / BiDi / script:** `unicode-bidi` (LTR fast path — a pure-LTR
  paragraph skips BiDi entirely) and `unicode-script` (script itemization),
  driven from the facade's shaping seam.
- **Shaping:** `rustybuzz` per itemized run, over the fallback chain.
- **Rasterization:** `ttf-parser` supplies outlines and colour tables;
  `ab_glyph_rasterizer` produces the exact coverage that the default A8
  representation stores; `sdfer` produces the multi-channel signed-distance
  (MTSDF) field used only for glyphs the runtime has promoted under sustained
  transform; colour-bitmap glyphs decode via `zune-png` (pure Rust) into
  premultiplied RGBA. The choice among these representations is a runtime
  Temporal Promotion decision (Decision 2), not a fixed per-crate lane.

viso-text stays a pure algorithm layer: it never links a platform font API. The
one platform-facing capability it needs — resolving and rasterizing *system*
faces — is expressed as trait seams the facade implements (ADR 0026), preserving
the forbidden `text -> platform` direction.

### 2. Viso-owned cache boundaries (§20)

The subsystem owns four cache boundaries, and unchanged inputs at each boundary
skip the work above it:

- **font/face** — `FontStore` registers owned sfnt bytes once per face and
  reconstructs cheap `ttf-parser`/`rustybuzz` faces on demand; a face's coarse
  coverage metadata (`glyph_count`, `has_char`) orders the fallback chain.
- **shaping (run)** — a run reshapes only when text, font, features, or the
  chain change.
- **paragraph/line layout** — itemized runs lay out across faces on a shared
  baseline; positions are logical-pixel and dpi-invariant.
- **glyph residency** — a glyph resolves to one of five representations
  (`GlyphImageKind`): exact `MaskA8` coverage (the default), `ScalableMtsdf`,
  retained `OutlineVector`, `ColorRgba8`, or `ColorVector`. Each representation
  has its own persistent residency pool (A8 coverage / MTSDF / RGBA colour /
  vector), so a pure-text run touches only the A8 coverage pool and never
  allocates in the colour or MTSDF pools. Pools are packed once and grown
  incrementally by re-uploading only the region a shape reports dirty; a fixed
  label set in the steady state uploads once and never again. Eviction is
  page-age + CLOCK over pool pages, not per-glyph LRU.

Which representation a glyph resolves to is a runtime **Temporal Promotion**
decision, not a fixed lane baked into the key. A glyph starts as exact `MaskA8`
coverage. Sustained scale or rotation promotes it to `ScalableMtsdf` (multi-channel
signed distance — sharp corners plus a true distance channel — not a single-channel
SDF); extreme sustained zoom promotes it to a retained `OutlineVector`; a colour
source resolves to `ColorRgba8` or `ColorVector` by what the face provides.
Promotion is hysteretic (a quality/transform window with a multi-resolution bucket),
and any representation can fall back to exact `MaskA8` coverage when its pool is
under pressure — coverage is always a correct answer, so residency pressure never
produces a wrong glyph. `GlyphKey` therefore carries the resolved `GlyphImageKind`
and its resolution bucket; colour glyphs quantize to a size bucket so one bitmap
serves nearby pixel sizes.

### 3. Colour emoji reuses the Image pipeline (no new RHI)

`ColorRgba8` glyphs (bitmap strikes) lower to `Primitive::Image` against the RGBA
colour pool with a white opaque tint (`[1,1,1,1]`): the existing `IMAGE_FRAGMENT_BODY`
(`texel * float4(tint.rgb*tint.a, tint.a)`) passes a premultiplied RGBA texel
through unchanged. No new `GpuInstance`, shader, pipeline, or ABI validation
(§18/§53) is introduced on this path, and golden coverage stays fully headless —
the real-Metal gate ([[viso-msl-reserved-half]]) is not triggered by it. The
`ColorVector` representation (COLR/SVG glyphs into the vector pool) is part of the
target model rather than deferred; its dedicated lowering — and any emoji-specific
tint/outline effect — is scheduled by the implementation order in
`Viso_Text_Font_Runtime.md`, and introducing a colour-glyph pipeline variant with
its own `GpuInstance`/shader would re-trigger the ABI validation and real-Metal
gate above at that point.

## Consequences

- The subsystem is measurable against the §36 `text` benchmark category and the
  §35 headless golden path with no new RHI surface.
- Steady-state redraw of unchanged text allocates zero and re-uploads no residency
  pool region (verified by the alloc test); changing text reshapes and re-uploads a
  bounded region. Coverage as the always-correct fallback means residency pressure
  degrades quality gracefully rather than dropping glyphs.
- The full world-ready paragraph contract (UAX#9 BiDi, UAX#14 line break, UAX#29
  segmentation, complex-script shaping, and CJK line-break tailoring) is owned by
  the subsystem per `Viso_Text_Font_Runtime.md`; this ADR fixes only the ownership
  and cache-boundary shape, and the representation model above must hold across
  every script that contract admits.
- An advanced OpenType feature panel remains explicitly deferred; the adaptive
  representation model already admits COLR/SVG colour glyphs (`ColorVector`) and
  vector-retained outlines, so adding them changes no cache boundary here.
- The platform-facing seams (system-font resolution and colour-glyph
  rasterization) are the subject of [[0026-system-font-provider-and-color-glyph-seam]].
