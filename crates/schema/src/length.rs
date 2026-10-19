//! Lengths in their authored units, and the environment they fold against.

use core::ops::{Add, BitOr, BitOrAssign, Div, Mul, Neg, Sub};

use crate::env::LayoutDirection;
use crate::layout::{AbsoluteInsets, BoxRules, Inset, Length, Span, SpanInset};

/// A length as five terms, each in its own unit: `dp + px + sp + em + pct`.
///
/// `pct` is a fraction of the property's percent basis (`0.5` is `50%`); the
/// other four resolve to dp through the node's [`LengthEnv`] and resolved font
/// size. Arithmetic is term-wise, so `LengthTerms::pct(100.0) - LengthTerms::dp(32.0)`
/// is `100% - 32dp`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LengthTerms {
    /// Logical pixels.
    pub dp: f32,
    /// Physical pixels: `1px` is `1 / scale_factor` dp.
    pub px: f32,
    /// Scaled pixels: `1sp` follows the text scale.
    pub sp: f32,
    /// The resolved font size; inside `font_size` itself, the parent's.
    pub em: f32,
    /// A fraction of the percent basis.
    pub pct: f32,
}

impl LengthTerms {
    /// No length.
    pub const ZERO: Self = Self {
        dp: 0.0,
        px: 0.0,
        sp: 0.0,
        em: 0.0,
        pct: 0.0,
    };

    /// `v` logical pixels.
    pub const fn dp(v: f32) -> Self {
        Self {
            dp: v,
            ..Self::ZERO
        }
    }

    /// `v` physical pixels.
    pub const fn px(v: f32) -> Self {
        Self {
            px: v,
            ..Self::ZERO
        }
    }

    /// `v` scaled pixels.
    pub const fn sp(v: f32) -> Self {
        Self {
            sp: v,
            ..Self::ZERO
        }
    }

    /// `v` times the resolved font size.
    pub const fn em(v: f32) -> Self {
        Self {
            em: v,
            ..Self::ZERO
        }
    }

    /// A percentage of the basis: `LengthTerms::pct(50.0)` is `50%`.
    pub const fn pct(percent: f32) -> Self {
        Self {
            pct: percent / 100.0,
            ..Self::ZERO
        }
    }

    /// What the terms read besides the basis: the scale factor for `px`, the
    /// text scale for `sp`, the font size for `em`.
    pub fn deps(self) -> LengthDeps {
        let mut deps = LengthDeps::EMPTY;
        if self.px != 0.0 {
            deps |= LengthDeps::SCALE_FACTOR;
        }
        if self.sp != 0.0 {
            deps |= LengthDeps::TEXT_SCALE;
        }
        if self.em != 0.0 {
            deps |= LengthDeps::FONT_SIZE;
        }
        deps
    }

    /// The extent the non-percent terms fold to, in dp, at `font` dp per em:
    /// `None` when it is not finite.
    #[inline]
    pub fn fixed(self, env: &LengthEnv, font: f32) -> Option<f32> {
        let fixed = self.dp + self.px / env.scale_factor + env.sp(self.sp) + self.em * font;
        fixed.is_finite().then_some(fixed)
    }

    /// The layout length these terms fold to at `font` dp per em: a `Fixed`
    /// extent, never negative, when there is no percent term, and a `Relative`
    /// one otherwise. `None` when a term folds to a non-finite value.
    pub fn fold(self, env: &LengthEnv, font: f32) -> Option<Length> {
        let fixed = self.fixed(env, font)?;
        if self.pct == 0.0 {
            return Some(Length::Fixed(fixed.max(0.0)));
        }
        self.pct.is_finite().then_some(Length::Relative {
            fixed,
            pct: self.pct,
        })
    }

    /// The box-rule span these terms fold to at `font` dp per em: the dp the
    /// non-percent terms make, and the percentage. `None` when not finite.
    pub fn fold_span(self, env: &LengthEnv, font: f32) -> Option<Span> {
        let fixed = self.fixed(env, font)?;
        self.pct.is_finite().then_some(Span {
            fixed,
            pct: self.pct,
        })
    }

    /// The font size these terms fold to as a `font_size` value under a parent
    /// of `parent_font` dp: `em` and `%` both read the parent. Never negative;
    /// `None` when not finite.
    pub fn fold_font(self, env: &LengthEnv, parent_font: f32) -> Option<f32> {
        let terms = Self {
            em: self.em + self.pct,
            pct: 0.0,
            ..self
        };
        Some(terms.fixed(env, parent_font)?.max(0.0))
    }
}

impl From<Length> for LengthTerms {
    /// The terms of a folded length; `Fill` and `Fit` have none.
    fn from(length: Length) -> Self {
        match length {
            Length::Fixed(v) => Self::dp(v),
            Length::Relative { fixed, pct } => Self {
                dp: fixed,
                pct,
                ..Self::ZERO
            },
            Length::Fill { .. } | Length::Fit => Self::ZERO,
        }
    }
}

/// A box's four edges as terms: `start` and `end` are the edges the layout
/// direction begins and ends a line at, left and right in a left-to-right
/// layout.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeTerms {
    pub top: LengthTerms,
    pub end: LengthTerms,
    pub bottom: LengthTerms,
    pub start: LengthTerms,
}

impl EdgeTerms {
    /// `terms` on every edge.
    pub const fn all(terms: LengthTerms) -> Self {
        Self {
            top: terms,
            end: terms,
            bottom: terms,
            start: terms,
        }
    }

    /// What the edges read: their terms', and the layout direction when
    /// `start` and `end` differ.
    pub fn deps(self) -> LengthDeps {
        let mut deps = self.top.deps() | self.end.deps() | self.bottom.deps() | self.start.deps();
        if self.start != self.end {
            deps |= LengthDeps::DIRECTION;
        }
        deps
    }

    /// The physical inset the edges fold to at `font` dp per em, each edge
    /// never negative; a percent term is left to [`pct`](Self::pct). `None`
    /// when an edge is not finite.
    pub fn fold(self, env: &LengthEnv, font: f32) -> Option<Inset> {
        let edge = |terms: LengthTerms| Some(terms.fixed(env, font)?.max(0.0));
        let [left, top, right, bottom] = self.physical(env.direction);
        Some(Inset {
            left: edge(left)?,
            top: edge(top)?,
            right: edge(right)?,
            bottom: edge(bottom)?,
        })
    }

    /// The physical edges' percentages under `direction`.
    pub fn pct(self, direction: LayoutDirection) -> Inset {
        let [left, top, right, bottom] = self.physical(direction);
        Inset {
            left: left.pct,
            top: top.pct,
            right: right.pct,
            bottom: bottom.pct,
        }
    }

    /// The physical spans the edges fold to at `font` dp per em — a margin,
    /// whose edges may be negative. `None` when an edge is not finite.
    pub fn fold_spans(self, env: &LengthEnv, font: f32) -> Option<SpanInset> {
        let [left, top, right, bottom] = self.physical(env.direction);
        Some(SpanInset {
            left: left.fold_span(env, font)?,
            top: top.fold_span(env, font)?,
            right: right.fold_span(env, font)?,
            bottom: bottom.fold_span(env, font)?,
        })
    }

    /// The edges as left, top, right, bottom under `direction`.
    fn physical(self, direction: LayoutDirection) -> [LengthTerms; 4] {
        let (left, right) = match direction {
            LayoutDirection::Ltr => (self.start, self.end),
            LayoutDirection::Rtl => (self.end, self.start),
        };
        [left, self.top, right, self.bottom]
    }

    /// Whether an edge has a percent term.
    pub fn has_pct(self) -> bool {
        [self.top, self.end, self.bottom, self.start]
            .iter()
            .any(|terms| terms.pct != 0.0)
    }
}

/// An `Absolute` child's insets as terms, `None` for an edge it is not
/// pinned to.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct InsetTerms {
    pub top: Option<LengthTerms>,
    pub end: Option<LengthTerms>,
    pub bottom: Option<LengthTerms>,
    pub start: Option<LengthTerms>,
}

impl InsetTerms {
    /// What the edges read.
    pub fn deps(self) -> LengthDeps {
        let mut deps = LengthDeps::EMPTY;
        for terms in [self.top, self.end, self.bottom, self.start]
            .into_iter()
            .flatten()
        {
            deps |= terms.deps();
        }
        deps
    }

    /// The insets the edges fold to at `font` dp per em. `None` when an edge
    /// is not finite.
    pub fn fold(self, env: &LengthEnv, font: f32) -> Option<AbsoluteInsets> {
        let edge = |terms: Option<LengthTerms>| match terms {
            Some(terms) => terms.fold_span(env, font).map(Some),
            None => Some(None),
        };
        Some(AbsoluteInsets {
            top: edge(self.top)?,
            end: edge(self.end)?,
            bottom: edge(self.bottom)?,
            start: edge(self.start)?,
        })
    }
}

impl Add for LengthTerms {
    type Output = Self;
    fn add(self, rhs: Self) -> Self {
        Self {
            dp: self.dp + rhs.dp,
            px: self.px + rhs.px,
            sp: self.sp + rhs.sp,
            em: self.em + rhs.em,
            pct: self.pct + rhs.pct,
        }
    }
}

impl Sub for LengthTerms {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self + -rhs
    }
}

impl Neg for LengthTerms {
    type Output = Self;
    fn neg(self) -> Self {
        self * -1.0
    }
}

impl Mul<f32> for LengthTerms {
    type Output = Self;
    fn mul(self, k: f32) -> Self {
        Self {
            dp: self.dp * k,
            px: self.px * k,
            sp: self.sp * k,
            em: self.em * k,
            pct: self.pct * k,
        }
    }
}

impl Div<f32> for LengthTerms {
    type Output = Self;
    fn div(self, k: f32) -> Self {
        Self {
            dp: self.dp / k,
            px: self.px / k,
            sp: self.sp / k,
            em: self.em / k,
            pct: self.pct / k,
        }
    }
}

/// What a length reads from its environment, as a bitset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LengthDeps(u8);

impl LengthDeps {
    /// Reads nothing.
    pub const EMPTY: Self = Self(0);
    /// Reads the surface's scale factor (`px`).
    pub const SCALE_FACTOR: Self = Self(1 << 0);
    /// Reads the text scale (`sp`).
    pub const TEXT_SCALE: Self = Self(1 << 1);
    /// Reads the resolved font size (`em`, and `%` inside `font_size`).
    pub const FONT_SIZE: Self = Self(1 << 2);
    /// Reads the layout direction (a padding whose `start` and `end` differ).
    pub const DIRECTION: Self = Self(1 << 3);

    /// Whether any dependency of `other` is set.
    #[inline]
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Whether no dependency is set.
    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for LengthDeps {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for LengthDeps {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// The environment a length resolves against.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LengthEnv {
    /// Physical pixels per dp on the node's surface.
    pub scale_factor: f32,
    /// The text scale; `c sp` is `c × text_scale` dp.
    pub text_scale: f32,
    /// The root's font size, in sp.
    pub base_font_size: f32,
    /// The direction a line runs in: which physical edges `start` and `end`
    /// are.
    pub direction: LayoutDirection,
}

impl Default for LengthEnv {
    fn default() -> Self {
        Self {
            scale_factor: 1.0,
            text_scale: 1.0,
            base_font_size: 14.0,
            direction: LayoutDirection::Ltr,
        }
    }
}

impl LengthEnv {
    /// `c` sp in dp.
    #[inline]
    pub fn sp(&self, c: f32) -> f32 {
        c * self.text_scale
    }

    /// The root's resolved font size, in dp.
    #[inline]
    pub fn root_font_size(&self) -> f32 {
        self.sp(self.base_font_size)
    }

    /// The dependencies a move from `self` to `next` meets. A font size
    /// depends on the root font and on whatever the font sources between read,
    /// so any change meets it.
    pub fn changed(&self, next: &Self) -> LengthDeps {
        let mut deps = LengthDeps::EMPTY;
        if self.scale_factor != next.scale_factor {
            deps |= LengthDeps::SCALE_FACTOR;
        }
        if self.text_scale != next.text_scale {
            deps |= LengthDeps::TEXT_SCALE;
        }
        if !deps.is_empty() || self.base_font_size != next.base_font_size {
            deps |= LengthDeps::FONT_SIZE;
        }
        if self.direction != next.direction {
            deps |= LengthDeps::DIRECTION;
        }
        deps
    }
}

/// The environment-dependent lengths bound to one node. A `None` property keeps
/// the node's built value.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NodeLengths {
    /// The node's width.
    pub width: Option<LengthTerms>,
    /// The node's height.
    pub height: Option<LengthTerms>,
    /// A container's gap between children: a Flex's main-axis gap, a Grid's
    /// column gap. Its percent term counts as `0`.
    pub gap: Option<LengthTerms>,
    /// The node's font size, the typography source of its subtree: `em` and
    /// `%` read the parent's resolved font size.
    pub font_size: Option<LengthTerms>,
    /// A container's padding; a percent term reads the parent's content box.
    pub padding: Option<EdgeTerms>,
    /// The node's least width.
    pub min_width: Option<LengthTerms>,
    /// The node's greatest width.
    pub max_width: Option<LengthTerms>,
    /// The node's least height.
    pub min_height: Option<LengthTerms>,
    /// The node's greatest height.
    pub max_height: Option<LengthTerms>,
    /// The node's margin.
    pub margin: Option<EdgeTerms>,
    /// A Grid's row gap. Its percent term counts as `0`.
    pub row_gap: Option<LengthTerms>,
    /// An `Absolute` child's insets; a percent term reads the container's
    /// content box.
    pub insets: Option<InsetTerms>,
}

impl NodeLengths {
    /// What the node's lengths read. Every `font_size` reads the font size,
    /// since changing it re-resolves the `em` lengths beneath.
    pub fn deps(&self) -> LengthDeps {
        let mut deps = LengthDeps::EMPTY;
        let terms = [
            self.width,
            self.height,
            self.gap,
            self.row_gap,
            self.min_width,
            self.max_width,
            self.min_height,
            self.max_height,
        ];
        for terms in terms.into_iter().flatten() {
            deps |= terms.deps();
        }
        for edges in [self.padding, self.margin].into_iter().flatten() {
            deps |= edges.deps();
        }
        if let Some(insets) = self.insets {
            deps |= insets.deps();
        }
        if let Some(font) = self.font_size {
            deps |= font.deps() | LengthDeps::FONT_SIZE;
        }
        deps
    }
}

impl NodeLengths {
    /// Whether the lengths carry the node's box rules: a bound size bound or
    /// margin, or a padding with a percent term. Such lengths carry all of
    /// them, so the node's rules are what they fold to.
    pub fn has_rules(&self) -> bool {
        self.min_width.is_some()
            || self.max_width.is_some()
            || self.min_height.is_some()
            || self.max_height.is_some()
            || self.margin.is_some()
            || self.padding.is_some_and(EdgeTerms::has_pct)
    }

    /// The box rules the lengths fold to at `font` dp per em, `None` when
    /// they carry none ([`has_rules`](Self::has_rules)); `Some(None)` when a
    /// term is not finite. A bound is never negative.
    pub fn rules(&self, env: &LengthEnv, font: f32) -> Option<Option<BoxRules>> {
        if !self.has_rules() {
            return None;
        }
        let span = |terms: Option<LengthTerms>, none: Span| match terms {
            Some(terms) => terms.fold_span(env, font).map(|span| Span {
                fixed: span.fixed.max(0.0),
                ..span
            }),
            None => Some(none),
        };
        let fold = || {
            Some(BoxRules {
                min_width: span(self.min_width, Span::ZERO)?,
                max_width: span(self.max_width, Span::UNBOUNDED)?,
                min_height: span(self.min_height, Span::ZERO)?,
                max_height: span(self.max_height, Span::UNBOUNDED)?,
                margin: match self.margin {
                    Some(edges) => edges.fold_spans(env, font)?,
                    None => SpanInset::ZERO,
                },
                padding_pct: self
                    .padding
                    .map_or(Inset::default(), |edges| edges.pct(env.direction)),
            })
        };
        Some(fold())
    }
}

impl Mul<LengthTerms> for f32 {
    type Output = LengthTerms;
    fn mul(self, terms: LengthTerms) -> LengthTerms {
        terms * self
    }
}
