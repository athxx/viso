//! Color effects and their fusion into render-target ops (§17.3, §16.2).
//!
//! Every effect in this module is a *per-pixel recolor*: it reads one source
//! pixel and writes one destination pixel, with no neighbourhood and no
//! geometry. Nine of the ten are **affine in straight RGBA** — brightness,
//! contrast, saturation, hue rotation, grayscale, sepia, inversion, an explicit
//! matrix, and a tint are all `out = M * (r, g, b, a, 1)` for some 4×5 `M`
//! ([`ColorMatrix`]). Affine maps compose by matrix multiplication, so a run of
//! them is *one* matrix, evaluated in one fragment — never one pass each.
//!
//! [`fuse`] is that compiler. It walks an effect chain and emits the minimum
//! number of [`ColorOp`]s: a run of affine stages collapses into a single op,
//! and only a stage that is *not* expressible in the fused form
//! ([`ColorEffect::Gamma`], a per-channel power) forces the run to close and a
//! new op to open. The renderer then rides the **last** op on the layer's
//! composite draw, so an all-affine chain of any length costs zero extra render
//! target passes; each earlier op is one extra pass on a pooled transient
//! target, exactly like a blur-ladder rung.
//!
//! All math is on **straight** (non-premultiplied) linear RGBA — the same
//! convention the shaders unpremultiply into — so a matrix's alpha row is
//! independent of its color rows and a layer opacity can be folded in by
//! scaling that one row ([`ColorOp::with_opacity`]).
//!
//! This is a cold-path module: chains are fused once when a layer is ingested,
//! never per frame (§7.2), and the result is plain `Copy` data with no
//! allocation of its own.

/// Rec. 709 luminance weights (`feColorMatrix` / CSS filter convention): the
/// row every saturation-family matrix desaturates toward.
const LUMA_R: f32 = 0.213;
const LUMA_G: f32 = 0.715;
const LUMA_B: f32 = 0.072;

/// A 4×5 color matrix in row-major order: `out[i] = Σ_j rows[i][j] * src[j]`
/// over `src = (r, g, b, a, 1)`, on **straight** linear RGBA.
///
/// The fifth column is a constant offset. It is not decoration: brightness-as-
/// add, contrast, inversion, sepia, and tint all need it, and without it the
/// nine affine effects would not close under composition. This is exactly the
/// SVG `feColorMatrix` / CSS `filter` matrix shape, so authoring translates
/// one-to-one.
///
/// 80 bytes of `Copy` POD; composition ([`then`](Self::then)) is 20 dot
/// products on the cold path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorMatrix {
    /// One row per output channel (r, g, b, a); five coefficients per row —
    /// four for the input channels, then the constant offset.
    pub rows: [[f32; 5]; 4],
}

impl Default for ColorMatrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl ColorMatrix {
    /// The matrix that changes nothing: `out = src`.
    pub const IDENTITY: ColorMatrix = ColorMatrix {
        rows: [
            [1.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0, 0.0],
        ],
    };

    /// A matrix that scales the three color channels by `scale` and adds
    /// `offset`, leaving alpha untouched. The shape brightness, contrast,
    /// inversion, and tint all reduce to.
    fn rgb_scale_offset(scale: [f32; 3], offset: [f32; 3]) -> ColorMatrix {
        let mut m = ColorMatrix::IDENTITY;
        for c in 0..3 {
            m.rows[c][c] = scale[c];
            m.rows[c][4] = offset[c];
        }
        m
    }

    /// Whether this matrix is the exact identity, i.e. applying it is a no-op
    /// and the op carrying it can be dropped entirely.
    ///
    /// Deliberately an exact comparison: the constructors below return the
    /// literal identity for a neutral parameter (`brightness(1.0)`,
    /// `grayscale(0.0)`, …), which is the case worth eliding. A matrix that is
    /// merely *nearly* identity still gets its pass — approximating there would
    /// silently change pixels.
    pub fn is_identity(&self) -> bool {
        *self == ColorMatrix::IDENTITY
    }

    /// The matrix equivalent to applying `self` first, then `next`.
    ///
    /// This is what makes fusion possible: `a.then(b).then(c)` is one matrix
    /// for a three-effect chain, so `Brightness → Contrast → Saturation` is one
    /// fragment evaluation instead of three render-target passes.
    pub fn then(&self, next: &ColorMatrix) -> ColorMatrix {
        let mut out = ColorMatrix {
            rows: [[0.0; 5]; 4],
        };
        for i in 0..4 {
            for j in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += next.rows[i][k] * self.rows[k][j];
                }
                out.rows[i][j] = sum;
            }
            // The offset column picks up `next`'s own offset on top of the
            // offsets it inherits through `self`'s columns.
            let mut sum = next.rows[i][4];
            for k in 0..4 {
                sum += next.rows[i][k] * self.rows[k][4];
            }
            out.rows[i][4] = sum;
        }
        out
    }

    /// Apply this matrix to one straight linear RGBA pixel — the pure linear
    /// map, *unclamped*.
    ///
    /// Clamping to the representable `[0, 1]` range happens at a pass boundary,
    /// not per effect: see [`ColorOp::apply`], which mirrors the shader. Keeping
    /// this pure is what makes `a.then(b).apply(p) == b.apply(a.apply(p))` hold
    /// exactly, and it is also why fusing is *more* faithful than a chain of
    /// unorm passes — the intermediate values never get crushed.
    pub fn apply(&self, src: [f32; 4]) -> [f32; 4] {
        let mut out = [0.0f32; 4];
        for (c, row) in self.rows.iter().enumerate() {
            out[c] = row[0] * src[0] + row[1] * src[1] + row[2] * src[2] + row[3] * src[3] + row[4];
        }
        out
    }

    /// Scale the color channels by `amount`: `0` black, `1` unchanged, `> 1`
    /// brighter. Matches CSS `filter: brightness()`.
    pub fn brightness(amount: f32) -> ColorMatrix {
        Self::rgb_scale_offset([amount; 3], [0.0; 3])
    }

    /// Push the color channels away from (`> 1`) or toward (`< 1`) mid-grey:
    /// `out = (src - 0.5) * amount + 0.5`. Matches CSS `filter: contrast()`.
    pub fn contrast(amount: f32) -> ColorMatrix {
        let offset = (1.0 - amount) * 0.5;
        Self::rgb_scale_offset([amount; 3], [offset; 3])
    }

    /// Interpolate between luminance (`0`) and the source (`1`); `> 1`
    /// oversaturates. Matches SVG `feColorMatrix type="saturate"`.
    pub fn saturation(amount: f32) -> ColorMatrix {
        let s = amount;
        let mut m = ColorMatrix::IDENTITY;
        m.rows[0][0] = LUMA_R + (1.0 - LUMA_R) * s;
        m.rows[0][1] = LUMA_G - LUMA_G * s;
        m.rows[0][2] = LUMA_B - LUMA_B * s;
        m.rows[1][0] = LUMA_R - LUMA_R * s;
        m.rows[1][1] = LUMA_G + (1.0 - LUMA_G) * s;
        m.rows[1][2] = LUMA_B - LUMA_B * s;
        m.rows[2][0] = LUMA_R - LUMA_R * s;
        m.rows[2][1] = LUMA_G - LUMA_G * s;
        m.rows[2][2] = LUMA_B + (1.0 - LUMA_B) * s;
        m
    }

    /// Rotate hue by `radians` around the luminance axis. Matches SVG
    /// `feColorMatrix type="hueRotate"` (the standard's fixed coefficients,
    /// which are a luminance-preserving approximation, not an exact HSL spin).
    pub fn hue_rotate(radians: f32) -> ColorMatrix {
        let (sin, cos) = radians.sin_cos();
        let mut m = ColorMatrix::IDENTITY;
        m.rows[0][0] = LUMA_R + cos * (1.0 - LUMA_R) - sin * LUMA_R;
        m.rows[0][1] = LUMA_G - cos * LUMA_G - sin * LUMA_G;
        m.rows[0][2] = LUMA_B - cos * LUMA_B + sin * (1.0 - LUMA_B);
        m.rows[1][0] = LUMA_R - cos * LUMA_R + sin * 0.143;
        m.rows[1][1] = LUMA_G + cos * (1.0 - LUMA_G) + sin * 0.140;
        m.rows[1][2] = LUMA_B - cos * LUMA_B - sin * 0.283;
        m.rows[2][0] = LUMA_R - cos * LUMA_R - sin * (1.0 - LUMA_R);
        m.rows[2][1] = LUMA_G - cos * LUMA_G + sin * LUMA_G;
        m.rows[2][2] = LUMA_B + cos * (1.0 - LUMA_B) + sin * LUMA_B;
        m
    }

    /// Desaturate by `amount`: `0` unchanged, `1` fully grey. The complement of
    /// [`saturation`](Self::saturation); matches CSS `filter: grayscale()`.
    pub fn grayscale(amount: f32) -> ColorMatrix {
        Self::saturation(1.0 - amount)
    }

    /// Blend toward the classic sepia matrix by `amount` (`0` unchanged, `1`
    /// full sepia). Matches CSS `filter: sepia()`.
    pub fn sepia(amount: f32) -> ColorMatrix {
        const FULL: [[f32; 3]; 3] = [
            [0.393, 0.769, 0.189],
            [0.349, 0.686, 0.168],
            [0.272, 0.534, 0.131],
        ];
        let a = amount;
        let mut m = ColorMatrix::IDENTITY;
        for (r, full_row) in FULL.iter().enumerate() {
            for (c, full) in full_row.iter().enumerate() {
                let identity = if r == c { 1.0 } else { 0.0 };
                m.rows[r][c] = identity + (full - identity) * a;
            }
        }
        m
    }

    /// Blend toward the photographic negative by `amount`:
    /// `out = src * (1 - 2a) + a`, so `0` is unchanged and `1` is a full
    /// inversion. Matches CSS `filter: invert()`.
    pub fn invert(amount: f32) -> ColorMatrix {
        Self::rgb_scale_offset([1.0 - 2.0 * amount; 3], [amount; 3])
    }

    /// Blend the color channels toward the straight linear `color` by `amount`
    /// (`0` unchanged, `1` flat `color`), leaving alpha — and therefore the
    /// silhouette — untouched. The recolor a themed icon or a pressed-state
    /// overlay wants.
    pub fn tint(color: [f32; 3], amount: f32) -> ColorMatrix {
        let w = amount;
        Self::rgb_scale_offset([1.0 - w; 3], [color[0] * w, color[1] * w, color[2] * w])
    }
}

/// One authored color effect on a layer (§17.3).
///
/// The first nine variants are affine and therefore *mergeable*: any run of
/// them fuses into a single [`ColorOp`] regardless of length or order.
/// [`Gamma`](Self::Gamma) is the deliberate exception — a per-channel power is
/// not affine, so it is the one stage that can force an extra render-target
/// pass, which is exactly what makes "only a non-expressible filter costs a
/// pass" a testable property rather than a claim.
///
/// Neutral parameters (`Brightness(1.0)`, `Grayscale(0.0)`, `Gamma(1.0)`, …)
/// fuse away completely: they contribute the identity, and an all-identity
/// chain emits no op and forces no offscreen pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColorEffect {
    /// Scale the color channels (`1.0` unchanged).
    Brightness(f32),
    /// Expand or compress around mid-grey (`1.0` unchanged).
    Contrast(f32),
    /// Interpolate between luminance and source (`1.0` unchanged).
    Saturation(f32),
    /// Rotate hue around the luminance axis, in radians.
    HueRotate(f32),
    /// Desaturate by the given amount (`0.0` unchanged, `1.0` grey).
    Grayscale(f32),
    /// Blend toward sepia (`0.0` unchanged, `1.0` full sepia).
    Sepia(f32),
    /// Blend toward the negative (`0.0` unchanged, `1.0` inverted).
    Invert(f32),
    /// An explicit 4×5 matrix, for effects the named variants do not cover.
    ColorMatrix(ColorMatrix),
    /// Blend the color channels toward a straight linear RGB color.
    Tint {
        /// The straight linear RGB color to blend toward.
        color: [f32; 3],
        /// How far to blend (`0.0` unchanged, `1.0` flat `color`).
        amount: f32,
    },
    /// Raise each color channel to the given power — **not** affine, so this is
    /// the only stage that can split a fused run (`1.0`, or any non-positive /
    /// non-finite value, is a no-op).
    Gamma(f32),
}

/// What an effect contributes to the fused form: a matrix to multiply in, or a
/// gamma exponent to accumulate.
enum Stage {
    /// An affine stage — merges into the current op's matrix.
    Affine(ColorMatrix),
    /// A per-channel power — merges into the current op's gamma, but only if
    /// no affine stage has been applied *after* the existing gamma.
    Gamma(f32),
}

impl ColorEffect {
    /// Lower this effect into its fused-form contribution.
    fn stage(self) -> Stage {
        match self {
            ColorEffect::Brightness(k) => Stage::Affine(ColorMatrix::brightness(k)),
            ColorEffect::Contrast(k) => Stage::Affine(ColorMatrix::contrast(k)),
            ColorEffect::Saturation(s) => Stage::Affine(ColorMatrix::saturation(s)),
            ColorEffect::HueRotate(r) => Stage::Affine(ColorMatrix::hue_rotate(r)),
            ColorEffect::Grayscale(a) => Stage::Affine(ColorMatrix::grayscale(a)),
            ColorEffect::Sepia(a) => Stage::Affine(ColorMatrix::sepia(a)),
            ColorEffect::Invert(a) => Stage::Affine(ColorMatrix::invert(a)),
            ColorEffect::ColorMatrix(m) => Stage::Affine(m),
            ColorEffect::Tint { color, amount } => Stage::Affine(ColorMatrix::tint(color, amount)),
            // A non-positive or non-finite exponent has no sensible meaning on
            // a color channel; treat it as "no gamma" rather than emitting NaNs.
            ColorEffect::Gamma(g) => Stage::Gamma(if g.is_finite() && g > 0.0 { g } else { 1.0 }),
        }
    }
}

/// One fused color operation: an affine matrix followed by an optional
/// per-channel gamma, evaluated in a single fragment.
///
/// This is the renderer's unit of work, not the author's. A chain of *N*
/// effects compiles to as few of these as the math allows — one, if every
/// effect is affine. The renderer rides the final op on the layer's composite
/// draw (zero extra passes) and gives each earlier op one pooled render-target
/// pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColorOp {
    /// The fused affine transform, applied first.
    pub matrix: ColorMatrix,
    /// The per-channel exponent applied after the matrix; `1.0` means none.
    pub gamma: f32,
}

impl Default for ColorOp {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl ColorOp {
    /// The op that changes nothing.
    pub const IDENTITY: ColorOp = ColorOp {
        matrix: ColorMatrix::IDENTITY,
        gamma: 1.0,
    };

    /// Whether this op would leave every pixel untouched, i.e. it can be
    /// dropped instead of costing a draw.
    pub fn is_identity(&self) -> bool {
        self.matrix.is_identity() && self.gamma == 1.0
    }

    /// This op with a group `opacity` folded into its alpha row.
    ///
    /// Because the math is on straight RGBA, scaling the alpha row — all five
    /// coefficients, offset included — scales exactly the output alpha, and the
    /// shader's repremultiply then scales the color too. So a layer's opacity
    /// rides for free on the op that is already drawing, and no separate tint
    /// is needed on the composite.
    pub fn with_opacity(mut self, opacity: f32) -> ColorOp {
        for v in &mut self.matrix.rows[3] {
            *v *= opacity;
        }
        self
    }

    /// Apply this op to one straight linear RGBA pixel — the CPU mirror of the
    /// fragment shader, used to prove fusion is value-preserving.
    ///
    /// The clamp after the matrix is the shader's, and it is what makes one op
    /// exactly one render-target pass: the matrix runs in full precision, the
    /// result is clamped once, and only then is the gamma applied.
    pub fn apply(&self, src: [f32; 4]) -> [f32; 4] {
        let mut out = self.matrix.apply(src);
        for c in &mut out {
            *c = c.clamp(0.0, 1.0);
        }
        if self.gamma != 1.0 {
            for c in out.iter_mut().take(3) {
                *c = c.powf(self.gamma);
            }
        }
        out
    }
}

/// Compile `effects` into the fewest [`ColorOp`]s that reproduce them, pushing
/// the result onto `out` in application order (§17.3).
///
/// The rule is one line of algebra: affine stages compose into the current
/// matrix, and successive gammas multiply their exponents
/// (`(x^g₁)^g₂ = x^(g₁·g₂)`), so a run only has to close when an affine stage
/// arrives *after* a gamma — at that point the power has already been applied
/// and no single matrix can undo it. Consequences:
///
/// * `Brightness → Contrast → Saturation` → **one** op (zero extra passes: it
///   rides the composite the layer already draws).
/// * `Brightness → Gamma → Saturation` → **two** ops (one extra pass, earned by
///   the one stage the fused form cannot express).
/// * `Gamma → Gamma` → one op with the product exponent.
/// * An all-neutral chain → **no** ops, so the layer needs no offscreen at all.
///
/// `effects` is an iterator, not a slice, so the renderer can fuse a chain
/// straight out of the primitive stream without materializing it; `out` is not
/// cleared, so the caller appends into a reusable arena and remembers the range.
/// Between them the whole compile is allocation-free (§7.1, §28).
pub fn fuse(effects: impl IntoIterator<Item = ColorEffect>, out: &mut Vec<ColorOp>) {
    let mut cur = ColorOp::IDENTITY;
    for effect in effects {
        match effect.stage() {
            Stage::Affine(m) => {
                if cur.gamma != 1.0 {
                    // A power is already baked into `cur`; the matrix has to
                    // run after it, which needs its own evaluation.
                    out.push(cur);
                    cur = ColorOp::IDENTITY;
                }
                cur.matrix = cur.matrix.then(&m);
            }
            Stage::Gamma(g) => cur.gamma *= g,
        }
    }
    if !cur.is_identity() {
        out.push(cur);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A handful of straight-RGBA probes that exercise every channel, the
    /// extremes, and a translucent pixel.
    const PROBES: [[f32; 4]; 5] = [
        [0.0, 0.0, 0.0, 1.0],
        [1.0, 1.0, 1.0, 1.0],
        [0.8, 0.3, 0.1, 1.0],
        [0.2, 0.6, 0.9, 0.5],
        [0.5, 0.5, 0.5, 0.25],
    ];

    /// What one render-target pass does to a pixel: the matrix in full
    /// precision, then the single clamp the unorm attachment imposes.
    fn pass(m: &ColorMatrix, p: [f32; 4]) -> [f32; 4] {
        let mut out = m.apply(p);
        for c in &mut out {
            *c = c.clamp(0.0, 1.0);
        }
        out
    }

    fn assert_close(a: [f32; 4], b: [f32; 4], what: &str) {
        for c in 0..4 {
            assert!(
                (a[c] - b[c]).abs() < 1e-5,
                "{what}: channel {c} differs: {a:?} vs {b:?}"
            );
        }
    }

    /// Every named effect at its neutral parameter is exactly the identity, so
    /// authoring a disabled effect costs nothing at all — no op, no pass.
    #[test]
    fn neutral_parameters_are_the_identity() {
        assert!(ColorMatrix::brightness(1.0).is_identity());
        assert!(ColorMatrix::contrast(1.0).is_identity());
        assert!(ColorMatrix::saturation(1.0).is_identity());
        assert!(ColorMatrix::hue_rotate(0.0).is_identity());
        assert!(ColorMatrix::grayscale(0.0).is_identity());
        assert!(ColorMatrix::sepia(0.0).is_identity());
        assert!(ColorMatrix::invert(0.0).is_identity());
        assert!(ColorMatrix::tint([1.0, 0.0, 0.0], 0.0).is_identity());
    }

    /// Composition means what it says: `a.then(b)` applied to a pixel equals
    /// `b` applied to `a` applied to that pixel. This is the property the whole
    /// fusion scheme rests on.
    #[test]
    fn composition_matches_sequential_application() {
        let a = ColorMatrix::brightness(1.4);
        let b = ColorMatrix::contrast(0.7);
        let c = ColorMatrix::saturation(0.3);
        let fused = a.then(&b).then(&c);
        for probe in PROBES {
            let stepwise = c.apply(b.apply(a.apply(probe)));
            assert_close(fused.apply(probe), stepwise, "three-stage chain");
        }
    }

    /// A run of affine effects — the `Brightness → Contrast → Saturation` case
    /// — compiles to exactly one op, and that op reproduces applying the three
    /// matrices in sequence within a single pass.
    #[test]
    fn affine_run_fuses_into_one_op() {
        let effects = [
            ColorEffect::Brightness(1.2),
            ColorEffect::Contrast(0.8),
            ColorEffect::Saturation(0.5),
        ];
        let mut ops = Vec::new();
        fuse(effects, &mut ops);
        assert_eq!(ops.len(), 1, "three mergeable effects are one op");
        assert_eq!(ops[0].gamma, 1.0);

        let stepwise = |p: [f32; 4]| {
            let m = ColorMatrix::saturation(0.5)
                .apply(ColorMatrix::contrast(0.8).apply(ColorMatrix::brightness(1.2).apply(p)));
            pass(&ColorMatrix::IDENTITY, m)
        };
        for probe in PROBES {
            assert_close(ops[0].apply(probe), stepwise(probe), "fused affine run");
        }
    }

    /// All nine affine effects at once still fuse to one op: length does not
    /// matter, only expressibility.
    #[test]
    fn every_affine_effect_merges_regardless_of_length() {
        let effects = [
            ColorEffect::Brightness(1.1),
            ColorEffect::Contrast(1.2),
            ColorEffect::Saturation(0.9),
            ColorEffect::HueRotate(0.4),
            ColorEffect::Grayscale(0.25),
            ColorEffect::Sepia(0.3),
            ColorEffect::Invert(0.1),
            ColorEffect::ColorMatrix(ColorMatrix::brightness(0.95)),
            ColorEffect::Tint {
                color: [0.2, 0.4, 0.9],
                amount: 0.35,
            },
        ];
        let mut ops = Vec::new();
        fuse(effects, &mut ops);
        assert_eq!(ops.len(), 1, "nine mergeable effects are still one op");
    }

    /// A non-expressible stage in the middle splits the chain — and only there.
    /// The gamma itself rides on the op before it, so `affine → gamma → affine`
    /// is two ops, not three.
    #[test]
    fn a_non_expressible_stage_earns_exactly_one_extra_op() {
        let mut ops = Vec::new();
        fuse(
            [
                ColorEffect::Brightness(1.3),
                ColorEffect::Gamma(2.2),
                ColorEffect::Saturation(0.4),
            ],
            &mut ops,
        );
        assert_eq!(ops.len(), 2, "one gamma splits the run once");
        assert_eq!(ops[0].gamma, 2.2, "the gamma rides the preceding matrix");
        assert!(ops[1].gamma == 1.0 && !ops[1].matrix.is_identity());

        for probe in PROBES {
            let stepwise = {
                let mut p = pass(&ColorMatrix::brightness(1.3), probe);
                for c in p.iter_mut().take(3) {
                    *c = c.powf(2.2);
                }
                pass(&ColorMatrix::saturation(0.4), p)
            };
            let piped = ops[1].apply(ops[0].apply(probe));
            assert_close(piped, stepwise, "split chain");
        }
    }

    /// Consecutive gammas are one power, so a gamma run never splits either.
    #[test]
    fn consecutive_gammas_multiply_into_one_op() {
        let mut ops = Vec::new();
        fuse([ColorEffect::Gamma(2.0), ColorEffect::Gamma(1.5)], &mut ops);
        assert_eq!(ops.len(), 1);
        assert!((ops[0].gamma - 3.0).abs() < 1e-6, "exponents multiply");
        assert!(ops[0].matrix.is_identity());
    }

    /// A chain that computes nothing emits nothing: no op means no forced
    /// offscreen pass for the layer.
    #[test]
    fn a_neutral_chain_emits_no_op() {
        let mut ops = Vec::new();
        fuse(
            [
                ColorEffect::Brightness(1.0),
                ColorEffect::Gamma(1.0),
                ColorEffect::Grayscale(0.0),
                ColorEffect::Gamma(-3.0),
            ],
            &mut ops,
        );
        assert!(ops.is_empty(), "a neutral chain compiles to nothing");
        assert!(fuse_len([]) == 0, "an empty chain compiles to nothing");
    }

    fn fuse_len(effects: impl IntoIterator<Item = ColorEffect>) -> usize {
        let mut ops = Vec::new();
        fuse(effects, &mut ops);
        ops.len()
    }

    /// Grayscale is saturation's complement, and full grayscale collapses every
    /// channel onto the luminance of the source.
    #[test]
    fn grayscale_is_luminance() {
        let m = ColorMatrix::grayscale(1.0);
        for probe in PROBES {
            let out = m.apply(probe);
            let luma = (LUMA_R * probe[0] + LUMA_G * probe[1] + LUMA_B * probe[2]).clamp(0.0, 1.0);
            assert_close(out, [luma, luma, luma, probe[3]], "full grayscale");
        }
    }

    /// Full inversion is the photographic negative and leaves alpha alone.
    #[test]
    fn full_invert_is_the_negative() {
        let m = ColorMatrix::invert(1.0);
        for probe in PROBES {
            let out = m.apply(probe);
            assert_close(
                out,
                [1.0 - probe[0], 1.0 - probe[1], 1.0 - probe[2], probe[3]],
                "full invert",
            );
        }
    }

    /// A full tint replaces the color channels and preserves the silhouette.
    #[test]
    fn full_tint_replaces_color_and_keeps_alpha() {
        let color = [0.1, 0.7, 0.4];
        let m = ColorMatrix::tint(color, 1.0);
        for probe in PROBES {
            let out = m.apply(probe);
            assert_close(out, [color[0], color[1], color[2], probe[3]], "full tint");
        }
    }

    /// Hue rotation preserves luminance (the standard's design goal) and a full
    /// turn returns the source.
    #[test]
    fn hue_rotation_is_luminance_preserving_and_periodic() {
        let probe = [0.8, 0.3, 0.1, 1.0];
        let luma = LUMA_R * probe[0] + LUMA_G * probe[1] + LUMA_B * probe[2];
        for steps in 1..6 {
            let out = ColorMatrix::hue_rotate(steps as f32).apply(probe);
            let out_luma = LUMA_R * out[0] + LUMA_G * out[1] + LUMA_B * out[2];
            assert!(
                (out_luma - luma).abs() < 2e-2,
                "hue rotation must not shift luminance: {out_luma} vs {luma}"
            );
        }
        let full = ColorMatrix::hue_rotate(core::f32::consts::TAU);
        assert_close(full.apply(probe), probe, "a full turn is the identity");
    }

    /// Opacity folds into the alpha row, scaling output alpha exactly and
    /// leaving the color rows (and so the un-premultiplied hue) untouched.
    #[test]
    fn opacity_folds_into_the_alpha_row() {
        let op = ColorOp {
            matrix: ColorMatrix::saturation(0.5),
            gamma: 1.0,
        }
        .with_opacity(0.25);
        for probe in PROBES {
            let full = ColorMatrix::saturation(0.5).apply(probe);
            let out = op.apply(probe);
            assert_close(
                out,
                [full[0], full[1], full[2], (probe[3] * 0.25).clamp(0.0, 1.0)],
                "opacity scales alpha only",
            );
        }
    }

    /// `fuse` appends, so a shared arena can hold several layers' chains back
    /// to back and each layer keeps a range into it.
    #[test]
    fn fuse_appends_into_a_shared_arena() {
        let mut ops = Vec::with_capacity(4);
        fuse([ColorEffect::Brightness(1.5)], &mut ops);
        let first = ops.len();
        fuse(
            [ColorEffect::Gamma(2.0), ColorEffect::Invert(1.0)],
            &mut ops,
        );
        assert_eq!(first, 1);
        assert_eq!(ops.len(), 3, "the second chain appended its two ops");
    }
}
