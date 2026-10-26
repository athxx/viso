# ADR 0033 — Relative length units: `em`, Percent basis, and `MixedLength`

- Status: Accepted
- Date: 2026-09-26

## Context

`Viso_DSL_1.0.md` §19 already had `dp`, `px`, `sp` and `%`, but three gaps blocked
real UI authoring:

- `sp` was described only as a "font-scaled length"; its resolution rule was not
  written down.
- `Percent` had no basis. Nothing said what `50%` is relative to.
- Mixing units was forbidden outright (`50% + 1dp` was an error). That makes the
  everyday CSS `calc(100% - 16dp)` layout inexpressible.

There was also no unit that follows the node's text size (CSS `em`). Authors need it
for icon sizes, padding and gaps that scale with text.

The unit suffix set is closed by design (§19.2). Adding a suffix is a language change
and needs an ADR.

## Decision

### 1. Add the `em` suffix and the `Em` type

`1em` is the node's Resolved Font Size. The Typography Context passes one scalar down
the node ancestry. A node inherits its parent's size unless its schema declares a
bound `font_size`. The root node takes `theme.typography.base_size`, which defaults
to `14sp`. Inside `font_size` itself, `em` and `%` refer to the parent's size, so a
node cannot reference its own font size.

The lexer already starts a float exponent only when `e` is followed by
`[+-]? digit`, so `1em` lexes as a unit literal with no new ambiguity.
`unit_numeric_body` has no exponent, so `1e2dp` is `E1203`.

### 2. Pin the meaning of existing units

- Every length resolves to `Dp`.
- `1px = 1 / scale_factor`.
- `c sp = text_scale_curve(c)`; on a platform with linear scaling that is
  `c × env.text_scale`. `sp` is Viso's `rem`, so no `rem` suffix is added.
- `%` is a plain ratio. A length property accepts it only when the property's schema
  declares a `percent_basis`; otherwise the compiler reports `E3104`.
- §19.4 fixes the standard bases: parent content box per axis, the container's own
  size for `gap`, the node's shorter side for `corner_radius`, and font sizes for
  `font_size` and `line_height`.
- When the basis is indefinite, a pure percent value falls back to the property's
  default sizing, and a percent term inside a mixed value counts as `0`. Either case
  raises the `E3105` warning.

### 3. Add `MixedLength`, a linear combination and not an expression tree

Adding or subtracting two different length-family units produces a `MixedLength`.
Multiplying or dividing by a scalar is allowed. Comparison, length × length, and
`min`/`max`/`clamp` are rejected (`E2107`). Assigning a `MixedLength` to a concrete
unit is `E2106`. A concrete value can only come from explicit layout-context APIs.

It lowers to a fixed `LengthTerms { dp, px, sp, em, pct: F32 }`:

- constant values are folded at compile time;
- there is no heap allocation and no expression tree;
- layout resolves the value with one multiply-add (§8).

The terms that are non-zero decide the value's environment dependencies at compile
time:

| Non-zero term | Dependency           |
| ------------- | -------------------- |
| `px`          | `scale_factor`       |
| `sp`          | `text_scale`         |
| `em`          | Resolved Font Size   |
| `%`           | none; the parent layout supplies the basis |

This keeps the static-binding fast path (AGENTS §10.3).

### 4. `sp` follows the platform's text-scale curve

Linear scaling breaks at accessibility sizes. Android 14 scales large text less
than small text, and iOS Dynamic Type maps each content size category to its own
sizes. So an `sp` term resolves through the scope's `TextScaleCurve`. The curve is
a monotonic, piecewise-linear `sp → Dp` map that the platform supplies. Platforms
with no non-linear source use the linear curve. `env.text_scale` stays the nominal
factor, used for queries and semantics.

- The curve applies to the folded `sp` coefficient, so `8sp * 2` equals `16sp`. A
  value has one `sp` coefficient, so resolution stays O(1): one table lookup.
- `em` uses the already resolved font size. It is never scaled a second time.
- `AdaptiveScope` may clamp the curve with `text_scale_min` / `text_scale_max`. This
  is for height-bound chrome such as tab bars and system bars. Interactive nodes
  inside a clamped scope should offer the platform's large-content preview.

### 5. Value domain and non-finite values

- Size properties are non-negative and clamp at `0`. Offsets such as `margin`,
  `inset` and `offset` may be negative.
- When `min_*` exceeds `max_*`, `min_*` wins.
- Dividing a length by constant `0` is `E2109`.
- A non-finite runtime result falls back to the property's default. Debug reports
  `E3106` and counts `length_nonfinite`. It never reaches layout or render state.

### 6. Interpolation is term-wise

Transitions and animations lerp the five terms separately. A `50%` to `200dp`
animation needs no resolution when it starts, and it stays correct when the basis
changes mid-animation. Interpolation keeps the property's dirty classes: animating
`width` is `MEASURE|LAYOUT` every frame, so size-like motion should use a `scale`
transform instead.

### 7. Pixel alignment

Layout stays in `F32` Dp and never rounds. Rounding happens in device space on the
render side (Rendering §5.7).

- A container derives neighbouring children's shared edge from one running edge
  value, so the two edges are the same `F32`. `PixelSnap::Bounds` then rounds both
  to the same device pixel, so adjacent fills neither gap nor overlap at scales such
  as 1.25, 1.5 or 1.75.
- A stroke or border width with only an integer `px` term resolves to whole device
  pixels, and standard widgets snap it with `PixelSnap::Stroke`. A scale-independent
  one-pixel rule uses `Hairline`.

### 8. Hot storage holds a pre-folded pair

`LengthTerms` and its dependency mask live in the binding side table, not in the
layout hot store. When a dependency changes, the environment terms fold into one
scalar:

```text
fixed = dp + px / scale_factor + text_scale_curve(sp) + em × resolved_font_size
```

The layout hot store keeps the folded `Length` (`viso_schema::Length`): a value with a
percent term is `Length::Relative { fixed: F32, pct: F32 }`, an 8-byte payload. Layout
resolves it with one `fixed + pct × basis`, clamped at zero. A pure `Dp` constant is
`Length::Fixed`, so it costs the same as a fixed length. `font_size` and
`line_height` fold their percent term into `fixed` during typography resolution. An
environment change re-folds only the bindings whose mask matches it.

### 9. Explicitly not added

| Unit                        | Why not / use instead                                    |
| --------------------------- | -------------------------------------------------------- |
| `rem`                       | `sp` already covers it                                   |
| `vw` / `vh` / `vmin` / `vmax` | Wrong inside nested Adaptive Scopes; use `%` or `env.constraints` |
| `ex` / `ch` / `lh`          | Depend on font metrics                                   |
| `fr`                        | A Grid track value, not a length                         |
| Physical units              | No stable mapping to logical UI coordinates              |

## Consequences

- The DSL spec now covers these rules in §19.1–§19.10, plus cross-references in §73,
  §76, §122, §141, §152, §153 and Appendices B and C.
- `viso-dsl` needs:
  - `Ty::Em` and `Ty::MixedLength`;
  - `em` in the suffix table;
  - the §19.3 operator typing;
  - `E2106`, `E2107`, `E2109`, `E3104`, `E3105` and `E3106`.
- Today's code (`fold_length` in `crates/dsl/src/ir/mod.rs`) disagrees with this ADR
  in three places, and all must change when it lands:
  - `LengthIr` folds `%` to `Fill`. This ADR says `100%` is a basis ratio and not
    "fill the rest".
  - `px` folds to `Fixed` as if it were `dp`. It must divide by `scale_factor`.
  - Any other suffix, such as `pt`, and bare numbers also fold to `Fixed`. Unknown
    suffixes must be `E1204`, and a bare number must not silently become a length.
- The pure Rust path (`viso_ui::Length`) gains the same model: `dp`, `px`, `sp`, `em`
  and `pct` constructors, plus `+`, `-`, unary `-` and scalar `*` / `/`, all as
  `const fn` where possible. `ui!`, `component!` and plain Rust must produce the
  same values.
- Release AOT encodes a length as a one-byte term mask followed by only its non-zero
  terms.
- Hot reload patches a changed length constant in place and dirties only that
  binding's classes. It never forces a structural change.
- Lowering needs a `LengthTerms` arm. This extends `LengthIr::Fixed(f32)` from ADR
  0014.
- Layout needs:
  - the Typography Context scalar;
  - a per-property `percent_basis` in the property schema;
  - `LengthTerms` resolution.
- Re-resolving `em` must stop at nodes that set their own absolute font size, so a
  font-size change does not dirty the whole subtree.
- Platform backends supply `TextScaleCurve`: Android's font-scale converter table,
  the iOS Dynamic Type category sizes, Windows text scale, and the web root font size.
- Benchmarks must land with the layout change, in release builds:
  - layout of a 10k-node tree mixing `dp`, `%` and `em` against a pure-`dp` baseline;
  - a `text_scale` change, measuring the bindings re-folded and the relayout cost;
  - a font-size change on a subtree root, checking that re-folding stops at absolute
    `font_size` boundaries.
- Until these land, the claim that the hot path matches today's fixed-length cost is
  a hypothesis.
