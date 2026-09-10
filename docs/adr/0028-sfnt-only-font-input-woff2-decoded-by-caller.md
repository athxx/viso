# ADR 0028 — SFNT-only font input: the framework accepts decoded SFNT, WOFF2 is decoded by the caller

- Status: Accepted
- Date: 2026-09-10

## Context

The text/font runtime spec (`Viso_Text_Font_Runtime.md` §7) and the architecture
document (§37.7) previously made WOFF2 a first-class packaged/external font input
format, decoded inside the framework: the pipeline was `WOFF2 → decode/decompress
→ normalized SFNT face → FontFace → Shaper/Rasterizer`, with a matching worker
job, a temporary decode buffer released after face publish, and a
`woff2_decode_us` / `woff2_decode_time` counter.

WOFF2 decoding pulls a compression/decompression stage (Brotli + the WOFF2 table
transforms) into the framework purely to unwrap a transport container. That is a
§68 trigger — self-build vs external-dependency ownership for the font subsystem
— because the font subsystem is in the "Viso owns integration but prefers proven
algorithms" tier (§3.7), and taking on a container-decode dependency that adds no
shaping/rasterization capability is exactly the kind of ownership decision that
must be recorded rather than left implicit.

It also narrows ADR-022 ("字体系统采用 On-demand OS Fallback + byte-budgeted SLRU
+ page-aged Atlas"), whose decision and 代价 clauses named packaged WOFF2 as a
supported WASM/Canvas font source and listed WOFF2 decode as a standing cold
cost.

## Decision

The framework's packaged/external font input is **SFNT only** — TTF, OTF, TTC,
OTC. The framework does not decode or decompress WOFF2, and carries no WOFF2
dependency.

- A caller that has WOFF2 decompresses it to SFNT **before** supplying it —
  either in a build-time asset pipeline or at runtime — or injects
  already-decoded SFNT bytes through the External FontProvider seam (see ADR
  0026). Browsers and standard font tooling can perform this decompression; a
  project may also simply ship SFNT.
- `font_format` normalization handles SFNT container variants only (TTF/OTF and
  the TTC/OTC collection wrappers). There is no `woff2_decode` stage, no WOFF2
  worker job, no temporary WOFF2 decode buffer, and no `woff2_decode_us` /
  `woff2_decode_time` counter.
- The face pipeline is `SFNT (TTF/OTF/TTC/OTC) → FontFace → Shaper/Rasterizer`.
  Heavy font parse still stays off the UI frame hot path (worker/staging), as
  before.

This supersedes the packaged-WOFF2 clause of ADR-022; the rest of ADR-022 (OS
on-demand fallback, byte-budgeted SLRU face cache, page-aged atlas) is unchanged.

## Consequences / 代价

- No WOFF2 decode dependency (no Brotli/WOFF2-transform stage) is linked into the
  framework; the font subsystem's dependency surface stays limited to the parse /
  shape / raster algorithms it already owns integration for.
- WASM asset delivery keeps WOFF2's transport/size benefit only if the caller
  decompresses it: the browser or build tooling can decode WOFF2 to SFNT, or the
  project ships SFNT directly. The framework's WASM contract (no implicit system
  font; packaged fonts lazily fetched and manifested) is otherwise unchanged.
- Font licensing and source-resource semantics are never silently rewritten by
  the framework — it neither converts nor re-encodes user font resources. Any
  WOFF2 → SFNT conversion is an explicit caller/pipeline action.
- The External FontProvider path (ADR 0026) remains the escape hatch for any
  container the framework does not natively parse: a caller can decode WOFF2 (or
  any other wrapper) itself and inject the resulting SFNT bytes.
- ADR-022's packaged-WOFF2 clause is superseded by this ADR; `Viso_Architecture.md`
  §37.7 and `Viso_Text_Font_Runtime.md` §7 are updated to the SFNT-only input
  policy and made consistent with this record.
