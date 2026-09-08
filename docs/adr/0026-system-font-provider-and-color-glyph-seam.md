# ADR 0026 — System-font provider and color-glyph rasterizer seams: trait in viso-text, CoreText binding in the facade

- Status: Accepted
- Date: 2026-09-08

## Context

Viso bundles no default font. The default UI face, per-script fallbacks, and the
color-emoji face are all resolved from the OS at runtime, and the color-emoji
face must be rasterized through the platform text engine. This is the source
strategy the user set: no embedded default English face, no embedded
NotoColorEmoji; the default reads system fonts; the user may load their own faces
at init; WASM loads no font by default and shapes to nothing rather than
panicking.

Two forbidden dependency edges (§3.5) constrain how this lands. viso-text is a
pure algorithm layer and must never link a platform font API — `text -> platform`
is forbidden. Yet the resolution *policy* (which script is missing, the negative
cache, the fallback-chain ordering) is platform-independent and belongs in
viso-text where it is testable with a mock. So the platform binding and the
policy must be split across the trait boundary. This is a §68 trigger
(self-build vs external-dependency ownership for a subsystem, plus a new
cross-crate seam), recorded here.

## Decision

### 1. Two cold-path trait seams in viso-text, implemented by the facade

viso-text defines two `dyn`-object trait seams (both cold path — queried only
when the shaper reports a genuine gap, so virtual dispatch is fine per §42) and
the facade (`crates/viso/src/system_fonts.rs`) supplies the macOS CoreText
implementation:

- **`SystemFontProvider`** — `load(&SystemFontQuery) -> Option<SystemFontResult>`.
  A query carries a `FontRole` (`Ui`/`Cjk`/`Emoji`), a sample string, and a
  BCP-47-ish lang hint. Resolution is **by sample string, not family name**
  (makepad semantics): CoreText's `new_ui_font_for_language` gives the base UI
  face and `for_string` cascades to a face covering the sample. The facade
  reassembles the live `CTFont`'s sfnt tables into a byte blob ttf-parser /
  rustybuzz can parse.

- **`ColorGlyphRasterizer`** — `rasterize(ps_name, expected_glyph_count,
  glyph_id, dpx_per_em) -> Option<ColorGlyph>`. A system emoji face arrives with
  its color strikes stripped, so ttf-parser cannot draw it; the facade re-opens
  the face by PostScript name (probing candidates against `expected_glyph_count`
  to bind the *same* face, not a lookalike) and rasterizes with `CTFontDrawGlyphs`
  into premultiplied RGBA. `TextSystem::prepare` takes this rasterizer as an
  `Option<&dyn ColorGlyphRasterizer>` 5th parameter so the pure layer can reach
  the facade's binding without depending on it.

`FontStore` grows two pieces of metadata to support the emoji path: a
`is_color_emoji` flag the provider sets when it resolves a `FontRole::Emoji` face
(the stripped strikes make it undetectable from the table set — same cause as the
reference), and a `postscript_name()` reader that decodes name id 6 from a
Macintosh/Roman record as ASCII (ttf-parser's own decoder rejects it, so the raw
bytes are read directly).

### 2. Negative cache and fallback ordering live in viso-text

`SystemFallback` (viso-text) owns the negative cache: which scripts / emoji have
already been queried, resolved or not, so an uncoverable run does not re-ask the
OS every frame. The fallback chain orders user-loaded faces ahead of
system-resolved faces — a user face loaded at init is the primary; the system UI
face seeds the chain on the first shape only if the chain is still empty. This
policy is platform-independent and unit-tested with a mock provider; the CoreText
binding carries none of it (it is stateless).

### 3. Facade holds the platform crates directly (no dlopen)

The facade legally depends on `objc2-core-text` / `-core-graphics` /
`-core-foundation`, so it links CoreText directly rather than `dlopen`-ing it (the
reference `dlopen`s because its draw crate has no apple-sys dependency; Viso's
facade does). sfnt reassembly skips color-bitmap tables (AppleColorEmoji's `sbix`
is ~179 MB and undecodable by ttf-parser) and variable-font tables (`gvar`/`fvar`,
consuming only the default instance), and leaves checksums zero (ttf-parser does
not verify them).

**Divergence from the reference** ([[viso-diverge-from-makepad]]): the reference
un-premultiplies the CoreText bitmap into straight alpha (its atlas stores
straight coverage); Viso keeps the bitmap **premultiplied**, because the
color-glyph GPU path lowers to an image draw with a white tint (`texel * tint`,
`tint = [1,1,1,1]`) that passes a premultiplied texel through unchanged (ADR
0025 §3). So the facade only swizzles BGRA→RGBA and never un-premultiplies.

### 4. WASM / non-macOS is a no-op stub, not a panic

On non-macOS targets both seams compile to stubs that return `None`: the provider
resolves no system face and the rasterizer declines every color glyph. A chain
that stays empty (no user face loaded, no system provider) shapes to no glyphs —
a run lays out normally but emits nothing, matching the reference's
settled-but-incomplete behavior. Users on those targets load their own faces
through the facade's init-time font API.

## Consequences

- viso-text stays platform-free and mockable; the forbidden `text -> platform`
  edge is never introduced, verified by `cargo xtask arch-check`.
- The `LastResort` tofu face is rejected at the provider (glyph count ≤ 16) so
  the shaper's missing-script scan keeps re-firing rather than settling on boxes.
- Adding a Windows/Linux provider is implementing the same two traits in the
  facade behind a `cfg`; the policy, negative cache, and tests are already shared.
- The real-CoreText paths carry `#[cfg(target_os = "macos")]` smoke coverage;
  the resolution policy and negative cache are covered headless with a mock
  provider (ADR 0025 §2, §35).
- Colour-glyph rasterization has no RHI/ABI surface of its own — it feeds the
  RGBA atlas and the reused Image pipeline ([[0025-text-subsystem-ownership-and-cache-boundaries]]).
