# ADR 0025 — Text subsystem ownership: external-backed algorithms behind Viso-owned cache boundaries

- Status: Accepted
- Date: 2026-09-08

## Context

Rendering centred multilingual text (Latin, CJK, Thai, colour emoji) in one run
required the text subsystem (`crates/text/`) to grow from a Phase-2 single-face,
hard-coded-LTR, `split('\n')` shaper into a full pipeline: a multi-face fallback
chain, BiDi + script itemization, complex shaping, itemized layout, an SDF glyph
atlas, and a second RGBA atlas for colour-bitmap emoji. The governing criterion
is the standing one — best performance, least resource use, best effect, most
reasonable design, easiest to use, not least code.

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
- **Rasterization:** `ttf-parser` outlines → `ab_glyph_rasterizer` coverage →
  `sdfer` SDF for text glyphs; colour-bitmap glyphs decode via `zune-png` (pure
  Rust) into premultiplied RGBA.

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
- **glyph atlas** — two persistent atlases (R8 SDF, RGBA colour) packed once and
  grown incrementally by re-uploading only the region a shape reports dirty.
  Steady-state text (a fixed label set) uploads each atlas once and never again;
  a pure-text run never touches the colour atlas.

`GlyphKey` carries `kind` (SDF vs colour) and quantizes colour glyphs to a
size-bucket so the same emoji at nearby pixel sizes reuses one bitmap rather than
storing one per exact size.

### 3. Colour emoji reuses the Image pipeline (no new RHI)

Colour-bitmap glyphs lower to `Primitive::Image` against the RGBA atlas with a
white opaque tint (`[1,1,1,1]`): the existing `IMAGE_FRAGMENT_BODY`
(`texel * float4(tint.rgb*tint.a, tint.a)`) passes a premultiplied RGBA texel
through unchanged. No new `GpuInstance`, shader, pipeline, or ABI validation
(§18/§53) is introduced, and golden coverage stays fully headless — the real-Metal
gate ([[viso-msl-reserved-half]]) is not triggered on this path. A dedicated
colour-glyph pipeline variant (for emoji-specific tint/outline/MSDF effects) is
explicitly deferred and would open its own ADR when a capability need arises.

## Consequences

- The subsystem is measurable against the §36 `text` benchmark category and the
  §35 headless golden path with no new RHI surface.
- Steady-state redraw of unchanged text allocates zero and re-uploads no atlas
  region (verified by the alloc test); changing text reshapes and re-uploads a
  bounded region.
- COLR/SVG vector emoji, RTL beyond BiDi reordering, and an advanced OpenType
  feature panel are explicitly deferred (bitmap emoji covers the scope); the
  external-backed choice leaves room to add them without changing the cache
  boundaries.
- The platform-facing seams (system-font resolution and colour-glyph
  rasterization) are the subject of [[0026-system-font-provider-and-color-glyph-seam]].
