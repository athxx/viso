# ADR 0012 — `viso-math` and `viso-ende`: two owned Tier-A leaf foundations

- Status: Accepted
- Date: 2026-09-03

## Context

The architecture doc gained two Tier-A ownership entries that had been missed:
**`viso-math`** (an allocation-free numeric/geometry foundation — "math 是画图必须的")
and **`viso-ende`** (Encode/Decode infrastructure — "ende 这些都是数据交互必须的").
Both are things Viso must own outright (AGENTS section 3.7 Ownership Ladder: math is
the substrate of the layout contract and render/GPU ABI; ende owns the wire/cache/snapshot
and diagnostic-JSON format), so both are self-built rather than dependency-backed.

Self-building two major foundational subsystems is an ADR trigger (AGENTS section 68:
"self-build vs external-dependency ownership for a major subsystem"). This ADR records the
placement, the precision policy, the deliberate divergences from the reference framework's
math library, and the ende bounded-decoder / no-serde / no-media contract.

Both crates are **DAG leaves**: they depend on no other `viso-*` crate and take no
third-party dependency, matching the `crates/handle` leaf template. Neither has a consumer
wired yet — this work lands the foundations themselves. Today's geometry is scattered and
duplicated (`viso_render::{Rect,Point}` f32, `viso_ui::layout::Vec2` f32); `viso-math`
becomes the shared source of these types, but **migrating the existing crates onto it is a
separate later task** (recorded in todo), not in scope here.

The reference math library (`makepad/libs/math`, LGPL-3.0) was read for **semantics only** —
no code was copied. It uses f64 for UI geometry (`Rect`/`DVec2`) and f32 for GPU/3D; `Rect`
is pos+size with an *inclusive* `contains` and a strict `intersects`; `Mat4` is a
column-major `[f32;16]` with `M*v` and translation in elements 12–14; SIMD is
`core::arch` cfg-gated (SSE2/NEON/wasm128), not `std::simd` and not a feature flag; it has
**no 2D affine type** and exposes `cross`/quaternion-multiply as *associated* functions.
It also depends on `makepad-micro-serde` for `SerBin`/`DeBin` — a dependency Viso does not
take, because wire encoding is `viso-ende`'s job (spec section 10.1 forbidden edge).

## Decision

### 1. Precision policy: f32-primary, f64 `D`-variants scoped to the UI accuracy path

This is the "best for display quality *and* performance" lever, chosen deliberately over a
wholesale f64 copy of the reference library:

- **f32 is the primary precision** for all vector/matrix/geometry types (`Vec2/3/4`,
  `Mat2/3/4`, `Quat`, `Point/Size/Rect/Insets`, `Affine2`, `Transform3`, `Ray/Plane/Aabb`).
  This matches the existing f32 `viso_render`/`viso_ui` types (zero-friction future
  migration), is cache-friendly, and is GPU-native (the upload boundary is f32 anyway).
- **f64 `D`-prefixed variants only where large-coordinate accumulation causes visible
  sub-pixel shimmer**: `DVec2`, `DPoint`, `DRect`. This is the accuracy lever the reference
  reaches for (scroll/DPI accumulation over a large canvas). The library is **not** made f64
  wholesale — f64 is scoped to the UI-layout accuracy path with cheap explicit
  `From`/`into_*` conversions to/from the f32 types at the GPU boundary.

### 2. Divergences from the reference math library (warts fixed)

- **Methods, not associated functions.** `dot`/`cross`/`normalize`/`length`/`length_squared`
  are methods on the vector: `a.cross(b)`, not `Vec3::cross(a, b)`. `normalize` returns the
  zero vector for a zero-length input (the reference's guard, kept).
- **Uniform flat `[f32; N]` matrix storage.** `Mat2`/`Mat3`/`Mat4` all store a flat
  column-major array, fixing the reference's inconsistency (Mat3 as three column vectors vs
  Mat4 flat). One storage story across all three.
- **New `Affine2`** — the 2D UI transform the reference lacks: a 2×2 linear part plus a
  translation, with `identity`/`from_translation`/`from_scale`/`from_rotation`/`then`
  (compose)/`transform_point`/`transform_vector`/`inverse`. This is the type scroll/zoom/UI
  transforms need.
- **New `Insets`** (top/right/bottom/left f32) — listed in the spec, absent from the
  reference.
- **`Transform3`** = `Quat` + `Vec3` (the reference's `Pose`): `transform_point` =
  rotate-then-translate, `then`, `inverse`, `to_mat4`.

### 3. `contains` semantics: half-open for 2D `Rect`, inclusive for 3D `Aabb`

This is a **deliberate divergence** from the reference's inclusive `Rect::contains`, and the
two geometry families intentionally differ from each other:

- **`Rect::contains` is half-open** on both axes: a point is inside when
  `x ∈ [rect.x, rect.x + rect.w)` and `y ∈ [rect.y, rect.y + rect.h)`. Half-open is the
  correct hit-test/tiling semantics for pixel-space UI: adjacent rects that share an edge
  partition the plane without double-counting the boundary, so a point on the shared edge
  belongs to exactly one rect. `Rect::intersects` is **strict** (a shared edge alone is not
  an overlap), consistent with the half-open interior.
- **`Aabb::contains` / `Aabb::intersects` are inclusive** (closed intervals). A 3D bounding
  volume is a containment/culling primitive, not a tiling of space; touching bounds should
  count as contained/overlapping so a surface exactly on a bound is not culled. The contrast
  with `Rect` is intentional and is the reason both are spelled out here.

### 4. Internal SIMD, scalar-ABI public surface, bit-exact fallback

SIMD is an **internal** optimization; the public ABI is always the scalar `#[repr(C)]`
layout and never leaks a backend SIMD type (spec forbidden edge). `Mat4` multiply has four
`#[cfg]`-gated kernels — x86_64→SSE2, aarch64→NEON, wasm32+simd128→wasm128, else scalar —
selected at compile time, never by a runtime `dyn` or a feature flag.

The hardware kernels are **bit-exact against the scalar fallback**: separate multiply +
add (no FMA, whose single rounding would diverge), left-associative accumulation, matching
the scalar element order. A test asserts SIMD output equals scalar output bit-for-bit; it
was verified on this aarch64 host (NEON active). Every `unsafe` SIMD block carries a
`SAFETY:` comment (AGENTS section 27).

### 5. math hot-path ABI contract

Every public `viso-math` type is `#[repr(C)]` + `Copy`; no public struct has a `usize`/`isize`
field (so layout is target-pointer-width-independent); no `String`/`HashMap`/`Rc`/`Arc`/`dyn`;
no heap allocation on any path; no serde/`SerBin` derive (wire repr is `viso-ende`'s job). The
0-alloc / no-usize / no-dyn contract is verified by construction and exercised by the six spec
benches (`benches/math.rs`, release-only per AGENTS section 36): vec2 ops, mat4 mul, affine2
point transform, rect hit-test, aabb intersection, transform chain.

### 6. `viso-ende`: bounded decoder, mirrored codec, no serde, no media

- **Wire format.** Scalars are fixed-width little-endian (`to_le_bytes`/`from_le_bytes`).
  Byte-string/text lengths are unsigned **LEB128 varints**, and signed integers use
  **zig-zag** varints — chosen over the reference's raw-`usize` length so a short slice costs
  one length byte instead of eight and so `usize` never appears on the wire (width is fixed
  regardless of target). `Encoder` is an append-only `Vec<u8>` writer that cannot fail (no
  `Result` on the write path); `Decoder` mirrors it exactly, covered by round-trip tests.
- **The bounded decoder is the point.** Every read funnels through a single `read_raw(len)`
  gate that checks `len > remaining()` before touching a byte, so decoding arbitrary
  (including adversarial) input **never panics and never reads past the slice**. A varint is
  rejected when it runs overlong or exceeds the u64 range; a hostile length narrows through
  a helper that fails as EOF rather than allocating. A fuzz-smoke test (AGENTS section 67)
  drives every reader over 20 000 deterministic pseudo-random inputs and asserts the cursor
  never moves backward or past the end.
- **`DecodeError` is heap-free.** A `#[non_exhaustive]` `Copy` enum carrying only numeric
  context (offset/needed/available/remaining) — no `String`, diverging from the reference's
  string-carrying error — with `Display` + `std::error::Error` + `From<DecodeError> for
  io::Error`.
- **`wire.rs` defines the *format* of typed IDs, not the identity type.** A `WireId` is a
  one-byte `IdKind` tag (`Name`/`Symbol`/`Custom(u8)`) plus a varint `u64` value; a
  `ProtocolTag` is a `b"VE"` magic + `WIRE_VERSION`. Defining the format here rather than the
  ID type keeps `ende` a leaf: it never needs an edge back into the identity-owning crates
  (e.g. `viso-dsl`'s `NameId`/`SymbolId`), and those crates encode *through* this format.
- **`json.rs` is a hand-rolled write-only emitter** for diagnostics/tool interchange (the
  section 138 Diagnostic schema is the target shape): compact, correct string escaping,
  heap-free integer formatting, non-finite floats emitted as `null`. No serde.
- **Explicitly out of scope for `ende`:** RON, image/audio/video media codecs, and the
  frame's internal data model. Serde compatibility, when needed, lives in
  `integrations/serde`, not here (spec). `ende` is not on the frame data-flow main chain.

### 7. Forbidden edges are machine-checked

Both crates are registered in `xtask` `allowed_edges()` with an **empty** allowed-dependency
list (`("viso-math", &[])`, `("viso-ende", &[])`), so `cargo xtask check-deps` fails if either
ever gains a `viso-*` dependency. The workspace goes from **13 to 15 crates**, all edges still
within the section 10 DAG.

## Consequences

- `viso-math` is the future single source of `Vec2`/`Rect`/`Point`/transform types; the
  existing scattered f32 geometry in `viso_render`/`viso_ui` will migrate onto it in a
  **separate later task** (not in scope here). No consumer is wired yet.
- The half-open `Rect::contains` divergence must be honored by every future hit-test/tiling
  consumer; the inclusive `Aabb` contrast is intentional. Both are pinned by tests.
- SIMD kernels must stay bit-exact with scalar on every supported target; the bit-exact test
  is the guard, and any new kernel must pass it (verified today only on aarch64/NEON — the
  SSE2 and wasm128 kernels are asserted by the same test but not yet run on those targets).
- `viso-ende`'s bounded-decoder contract (never panic / never over-read) is load-bearing for
  its eventual untrusted-input consumers (cache/snapshot load, Studio/Inspector transport,
  Phase 9); the fuzz-smoke test is the standing guard. Advanced transport / schema-registry /
  Studio-Inspector-Profiler protocol and cache/snapshot wiring are deferred to their Phase 9
  consumers (recorded in todo).
- `integrations/serde` compatibility is deferred (spec places serde there, not in `ende`).
- Verification: `cargo build/clippy -D warnings/fmt/test` clean for both crates — **61 tests**
  for `viso-math` (vector/matrix/quat/transform/rect/geom identities + the SIMD bit-exact
  assertion) and **16 tests** for `viso-ende` (scalar/str/bytes/varint round-trips, EOF /
  overlong-varint / bad-UTF-8 / trailing-bytes rejection, the 20 000-iteration fuzz smoke,
  wire/protocol-tag round-trips, and the JSON emitter). All six `viso-math` benches build and
  run in release. `cargo xtask check-deps` green at **15 crates**. No shaders/UI/GPU touched,
  so no Metal/headless run was required.
