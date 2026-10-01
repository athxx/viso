//! Lengths that depend on the environment: the five-term [`LengthTerms`]
//! authoring value, the [`LengthEnv`] it resolves against, and the store's
//! side table of nodes whose lengths re-fold when that environment changes.
//!
//! Layout's hot store only holds the folded [`Length`]: a `px`, `sp` or `em`
//! term becomes part of its `fixed` extent before any pass reads it. A node
//! whose length carries such a term keeps its terms here, with the mask of
//! what they read, so a scale-factor, text-scale or font-size change re-folds
//! only the bindings it meets and dirties only the nodes whose folded length
//! moved. A tree of plain `dp` and `%` lengths never touches this table.

use core::ops::{Add, BitOr, BitOrAssign, Div, Mul, Neg, Sub};

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::layout::{LayoutInput, Length};
use crate::node::NodeId;

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

impl Mul<LengthTerms> for f32 {
    type Output = LengthTerms;
    fn mul(self, terms: LengthTerms) -> LengthTerms {
        terms * self
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
}

impl Default for LengthEnv {
    fn default() -> Self {
        Self {
            scale_factor: 1.0,
            text_scale: 1.0,
            base_font_size: 14.0,
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
    fn changed(&self, next: &Self) -> LengthDeps {
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
    /// A container's gap between children. Its percent term counts as `0`.
    pub gap: Option<LengthTerms>,
    /// The node's font size, the typography source of its subtree: `em` and
    /// `%` read the parent's resolved font size.
    pub font_size: Option<LengthTerms>,
}

impl NodeLengths {
    /// What the node's lengths read. Every `font_size` reads the font size,
    /// since changing it re-resolves the `em` lengths beneath.
    fn deps(&self) -> LengthDeps {
        let mut deps = LengthDeps::EMPTY;
        for terms in [self.width, self.height, self.gap].into_iter().flatten() {
            deps |= terms.deps();
        }
        if let Some(font) = self.font_size {
            deps |= font.deps() | LengthDeps::FONT_SIZE;
        }
        deps
    }
}

/// A length a layout pass could not resolve as authored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LengthIssue {
    /// A percent term met an indefinite basis (a parent that fits its content
    /// on that axis, a scroll axis) and fell back.
    IndefiniteBasis,
    /// A length folded to a non-finite value and took its default.
    NonFinite,
}

impl LengthIssue {
    /// The diagnostic code the issue reports as.
    pub const fn code(self) -> &'static str {
        match self {
            LengthIssue::IndefiniteBasis => "E3105",
            LengthIssue::NonFinite => "E3106",
        }
    }
}

/// A node's length issue, as a debug build reports it once per node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LengthWarning {
    /// The node whose length did not resolve.
    pub node: NodeId,
    /// What went wrong.
    pub issue: LengthIssue,
}

/// How often layout met each [`LengthIssue`], counted in every build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LengthStats {
    /// Lengths that folded to a non-finite value (`length_nonfinite`).
    pub nonfinite: u64,
    /// Percent terms that met an indefinite basis.
    pub indefinite_basis: u64,
}

/// One node's bound lengths.
#[derive(Debug, Clone, Copy)]
struct LengthBinding {
    node: NodeId,
    lengths: NodeLengths,
    deps: LengthDeps,
    /// The binding re-folds at the next layout.
    pending: bool,
}

/// The store's environment-dependent lengths: the environment, each bound
/// node by ascending index, and what layout reported.
#[derive(Default)]
pub(crate) struct LengthBindings {
    env: LengthEnv,
    bindings: Vec<LengthBinding>,
    /// Some binding re-folds at the next layout.
    pending: bool,
    /// A font source was bound or dropped: the next layout re-folds every
    /// binding that reads the font size.
    font_moved: bool,
    stats: LengthStats,
    /// The issues reported so far, by node index; a debug build warns of each
    /// once.
    #[cfg(debug_assertions)]
    reported: Vec<(u32, u32, LengthIssue)>,
    #[cfg(debug_assertions)]
    warnings: Vec<LengthWarning>,
}

impl LengthBindings {
    /// Drops every binding and report; the environment and counters stay.
    pub(crate) fn clear(&mut self) {
        self.bindings.clear();
        self.pending = false;
        self.font_moved = false;
        #[cfg(debug_assertions)]
        {
            self.reported.clear();
            self.warnings.clear();
        }
    }

    fn find(&self, id: NodeId) -> Option<&LengthBinding> {
        let at = self
            .bindings
            .binary_search_by_key(&id.index(), |b| b.node.index())
            .ok()?;
        let binding = &self.bindings[at];
        (binding.node == id).then_some(binding)
    }

    fn report(&mut self, node: NodeId, issue: LengthIssue) {
        match issue {
            LengthIssue::IndefiniteBasis => self.stats.indefinite_basis += 1,
            LengthIssue::NonFinite => self.stats.nonfinite += 1,
        }
        #[cfg(debug_assertions)]
        {
            let key = (node.index(), node.generation(), issue);
            let order = |e: &(u32, u32, LengthIssue)| (e.0, e.1, e.2 as u8);
            let probe = (key.0, key.1, key.2 as u8);
            if let Err(at) = self.reported.binary_search_by_key(&probe, order) {
                self.reported.insert(at, key);
                self.warnings.push(LengthWarning { node, issue });
            }
        }
        #[cfg(not(debug_assertions))]
        let _ = node;
    }
}

impl NodeStore {
    /// Binds `lengths` to node `id`, replacing what it had bound. The terms
    /// fold at the next layout, once the node's ancestry (and so its font size)
    /// is known; until then the node keeps its built lengths. A no-op for a
    /// stale handle.
    pub fn bind_lengths(&mut self, id: NodeId, lengths: NodeLengths) {
        if !self.arena().is_live(id) {
            return;
        }
        let deps = lengths.deps();
        let binding = LengthBinding {
            node: id,
            lengths,
            deps,
            pending: true,
        };
        let table = self.lengths_mut();
        let font_moved: bool;
        match table
            .bindings
            .binary_search_by_key(&id.index(), |b| b.node.index())
        {
            Ok(at) => {
                let old = table.bindings[at];
                if old.node == id && old.lengths == lengths {
                    return;
                }
                font_moved = old.node != id
                    || old.lengths.font_size.is_some()
                    || lengths.font_size.is_some();
                table.bindings[at] = binding;
            }
            Err(at) => {
                font_moved = lengths.font_size.is_some();
                table.bindings.insert(at, binding);
            }
        }
        table.pending = true;
        // A font source moved: every `em` beneath it may resolve anew.
        table.font_moved |= font_moved;
    }

    /// Drops node `id`'s bound lengths; its folded lengths stay as they are.
    pub fn unbind_lengths(&mut self, id: NodeId) {
        let table = self.lengths_mut();
        if let Ok(at) = table
            .bindings
            .binary_search_by_key(&id.index(), |b| b.node.index())
            && table.bindings[at].node == id
        {
            let font = table.bindings[at].lengths.font_size.is_some();
            table.bindings.remove(at);
            if font {
                table.pending = true;
                table.font_moved = true;
            }
        }
    }

    /// The node's bound lengths, or `None` when it has none.
    pub fn bound_lengths(&self, id: NodeId) -> Option<NodeLengths> {
        self.lengths().find(id).map(|b| b.lengths)
    }

    /// The environment lengths resolve against.
    pub fn length_env(&self) -> LengthEnv {
        self.lengths().env
    }

    /// Moves lengths to environment `env`. Only the bindings whose terms read
    /// what changed re-fold, at the next layout, and only a node whose folded
    /// length moves is dirtied.
    pub fn set_length_env(&mut self, env: LengthEnv) {
        let table = self.lengths_mut();
        let changed = table.env.changed(&env);
        table.env = env;
        if !changed.is_empty() {
            table.mark(changed);
        }
    }

    /// The node's resolved font size in dp: its own `font_size` when it binds
    /// one, else its parent's, and the environment's root font size at the
    /// root. `None` for a stale handle.
    pub fn resolved_font_size(&self, id: NodeId) -> Option<f32> {
        if !self.arena().is_live(id) {
            return None;
        }
        Some(self.font_of(id))
    }

    /// How often layout met each length issue.
    pub fn length_stats(&self) -> LengthStats {
        self.lengths().stats
    }

    /// Drains the length warnings raised since the last call, each node and
    /// issue once for the life of the tree. Always empty in a release build,
    /// which only counts them.
    pub fn take_length_warnings(&mut self) -> Vec<LengthWarning> {
        #[cfg(debug_assertions)]
        {
            std::mem::take(&mut self.lengths_mut().warnings)
        }
        #[cfg(not(debug_assertions))]
        Vec::new()
    }

    /// Records that node `index`'s length hit `issue`.
    pub(crate) fn report_length_issue(&mut self, index: u32, issue: LengthIssue) {
        if let Some(node) = self.arena().live_id(index) {
            self.lengths_mut().report(node, issue);
        }
    }

    /// The node's resolved font size, found by walking up to the nearest font
    /// source. A source that folds to a non-finite size inherits its parent's.
    fn font_of(&self, id: NodeId) -> f32 {
        let table = self.lengths();
        let mut node = Some(id);
        while let Some(at) = node {
            if let Some(font) = table.find(at).and_then(|b| b.lengths.font_size) {
                return font
                    .fold_font(&table.env, self.parent_font(at, font))
                    .unwrap_or_else(|| self.parent_font(at, LengthTerms::em(1.0)));
            }
            node = self.parent(at);
        }
        table.env.root_font_size()
    }

    /// The font size `font`, bound at `id`, reads as its parent's: only an
    /// `em` or `%` term walks up.
    fn parent_font(&self, id: NodeId, font: LengthTerms) -> f32 {
        match self.parent(id) {
            Some(parent) if font.em != 0.0 || font.pct != 0.0 => self.font_of(parent),
            _ => self.lengths().env.root_font_size(),
        }
    }

    /// Folds every pending binding into the layout store, dirtying each node
    /// whose folded length moved. Layout calls it first, so a pass never reads
    /// an unfolded length; a store with nothing pending returns at once.
    pub(crate) fn fold_lengths(&mut self) {
        if !self.lengths().pending {
            return;
        }
        let table = self.lengths_mut();
        if std::mem::take(&mut table.font_moved) {
            table.mark(LengthDeps::FONT_SIZE);
        }
        let mut at = 0;
        while at < self.lengths().bindings.len() {
            let binding = self.lengths().bindings[at];
            if !self.arena().is_live(binding.node) {
                self.lengths_mut().bindings.remove(at);
                continue;
            }
            at += 1;
            if !binding.pending {
                continue;
            }
            self.lengths_mut().bindings[at - 1].pending = false;
            self.fold_binding(binding);
        }
        self.lengths_mut().pending = false;
    }

    fn fold_binding(&mut self, binding: LengthBinding) {
        let id = binding.node;
        let env = self.lengths().env;
        let font = self.font_of(id);
        if let Some(own) = binding.lengths.font_size
            && own.fold_font(&env, self.parent_font(id, own)).is_none()
        {
            self.lengths_mut().report(id, LengthIssue::NonFinite);
        }
        let mut class = DirtyClass::EMPTY;
        let lengths = binding.lengths;
        for (terms, width) in [(lengths.width, true), (lengths.height, false)] {
            let Some(terms) = terms else {
                continue;
            };
            let folded = terms.fold(&env, font).unwrap_or_else(|| {
                self.lengths_mut().report(id, LengthIssue::NonFinite);
                Length::Fit
            });
            let size = self.layout_input_mut(id).size_mut();
            let slot = if width {
                &mut size.width
            } else {
                &mut size.height
            };
            if *slot != folded {
                *slot = folded;
                class = class | DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT;
            }
        }
        if let Some(terms) = lengths.gap {
            if terms.pct != 0.0 {
                self.lengths_mut().report(id, LengthIssue::IndefiniteBasis);
            }
            let folded = match terms.fixed(&env, font) {
                Some(gap) => gap.max(0.0),
                None => {
                    self.lengths_mut().report(id, LengthIssue::NonFinite);
                    0.0
                }
            };
            let moved = match self.layout_input_mut(id) {
                LayoutInput::Flex { gap, .. } => {
                    let moved = *gap != folded;
                    *gap = folded;
                    moved
                }
                LayoutInput::Grid {
                    column_gap,
                    row_gap,
                    ..
                } => {
                    let moved = *column_gap != folded || *row_gap != folded;
                    *column_gap = folded;
                    *row_gap = folded;
                    moved
                }
                _ => false,
            };
            if moved {
                class = class | DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT;
            }
        }
        if class != DirtyClass::EMPTY {
            self.mark_dirty(id, class);
        }
    }
}

impl LengthBindings {
    /// Marks every binding reading `deps` to re-fold at the next layout.
    fn mark(&mut self, deps: LengthDeps) {
        for binding in &mut self.bindings {
            if binding.deps.intersects(deps) {
                binding.pending = true;
                self.pending = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENV: LengthEnv = LengthEnv {
        scale_factor: 2.0,
        text_scale: 1.5,
        base_font_size: 14.0,
    };

    #[test]
    fn terms_fold_each_unit_to_dp() {
        let terms = LengthTerms::dp(10.0)
            + LengthTerms::px(4.0)
            + LengthTerms::sp(2.0)
            + LengthTerms::em(0.5);
        assert_eq!(
            terms.fold(&ENV, 20.0),
            Some(Length::Fixed(10.0 + 2.0 + 3.0 + 10.0))
        );
        let mixed = LengthTerms::pct(100.0) - LengthTerms::px(8.0);
        assert_eq!(
            mixed.fold(&ENV, 20.0),
            Some(Length::Relative {
                fixed: -4.0,
                pct: 1.0
            })
        );
        assert_eq!(
            LengthTerms::dp(-3.0).fold(&ENV, 20.0),
            Some(Length::Fixed(0.0))
        );
        assert_eq!((LengthTerms::em(1.0) * f32::INFINITY).fold(&ENV, 1.0), None);
    }

    #[test]
    fn a_font_size_reads_the_parent_for_em_and_percent() {
        let em = LengthTerms::em(1.2);
        let pct = LengthTerms::pct(120.0);
        assert_eq!(em.fold_font(&ENV, 10.0), pct.fold_font(&ENV, 10.0));
        assert_eq!(LengthTerms::sp(16.0).fold_font(&ENV, 10.0), Some(24.0));
        assert_eq!(ENV.root_font_size(), 21.0);
    }

    #[test]
    fn deps_follow_the_non_zero_terms() {
        assert!(LengthTerms::dp(4.0).deps().is_empty());
        assert!(LengthTerms::pct(50.0).deps().is_empty());
        let deps = (LengthTerms::px(1.0) + LengthTerms::em(1.0)).deps();
        assert!(deps.intersects(LengthDeps::SCALE_FACTOR));
        assert!(deps.intersects(LengthDeps::FONT_SIZE));
        assert!(!deps.intersects(LengthDeps::TEXT_SCALE));
    }

    use crate::component::{BuildCx, FlexStyle, LeafStyle};
    use crate::layout::{Align, Axis, Size};
    use viso_render::Rect;

    fn surface() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        }
    }

    fn leaf(width: Length) -> LeafStyle {
        LeafStyle {
            size: Size {
                width,
                height: Length::Fixed(10.0),
            },
            ..LeafStyle::default()
        }
    }

    /// A column whose font size is `column_font`, holding a leaf whose width is
    /// `width`; returns the store, the root, the column and the leaf.
    fn tree(
        column_font: Option<LengthTerms>,
        width: LengthTerms,
    ) -> (NodeStore, NodeId, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let (mut column, mut item) = (None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                let inner = cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        align: Align::Start,
                        size: Size {
                            width: Length::Fit,
                            height: Length::fill(),
                        },
                        ..FlexStyle::default()
                    },
                    |cx| {
                        let handle = cx.leaf(leaf(Length::Fit));
                        cx.bind_lengths(
                            handle,
                            NodeLengths {
                                width: Some(width),
                                ..NodeLengths::default()
                            },
                        );
                        item = Some(handle.id());
                    },
                );
                cx.bind_lengths(
                    inner,
                    NodeLengths {
                        font_size: column_font,
                        ..NodeLengths::default()
                    },
                );
                column = Some(inner.id());
            })
            .id()
        };
        (store, root, column.unwrap(), item.unwrap())
    }

    fn width_of(store: &mut NodeStore, root: NodeId, id: NodeId) -> f32 {
        let mut scratch = Vec::new();
        let mut redo = Vec::new();
        store.relayout_dirty(root, surface(), &mut scratch, &mut redo);
        store.bounds(id).w
    }

    #[test]
    fn a_bound_length_folds_at_layout_and_follows_the_environment() {
        let (mut store, root, _, item) = tree(None, LengthTerms::px(40.0) + LengthTerms::sp(10.0));
        store.layout(root, surface(), &mut Vec::new());
        assert_eq!(store.bounds(item).w, 50.0);
        store.clear_dirty();
        store.set_length_env(LengthEnv {
            scale_factor: 2.0,
            ..LengthEnv::default()
        });
        assert_eq!(width_of(&mut store, root, item), 30.0);
        store.clear_dirty();
        store.set_length_env(LengthEnv {
            scale_factor: 2.0,
            text_scale: 2.0,
            ..LengthEnv::default()
        });
        assert_eq!(width_of(&mut store, root, item), 40.0);
    }

    #[test]
    fn em_reads_the_nearest_font_source_up_the_ancestry() {
        let (mut store, root, column, item) =
            tree(Some(LengthTerms::dp(20.0)), LengthTerms::em(2.0));
        assert_eq!(width_of(&mut store, root, item), 40.0);
        assert_eq!(store.resolved_font_size(item), Some(20.0));
        assert_eq!(store.resolved_font_size(root), Some(14.0));
        // A relative font size reads its parent's: the root's 14sp.
        store.bind_lengths(
            column,
            NodeLengths {
                font_size: Some(LengthTerms::pct(150.0)),
                ..NodeLengths::default()
            },
        );
        assert_eq!(width_of(&mut store, root, item), 42.0);
    }

    #[test]
    fn an_absolute_font_size_stops_a_text_scale_change() {
        let (mut store, root, column, item) =
            tree(Some(LengthTerms::dp(20.0)), LengthTerms::em(2.0));
        store.layout(root, surface(), &mut Vec::new());
        store.clear_dirty();
        store.set_length_env(LengthEnv {
            text_scale: 2.0,
            ..LengthEnv::default()
        });
        let (mut scratch, mut redo) = (Vec::new(), Vec::new());
        let (measured, _) = store.relayout_dirty(root, surface(), &mut scratch, &mut redo);
        assert_eq!(measured, 0, "no folded length moved");
        assert_eq!(store.bounds(item).w, 40.0);
        let _ = column;
    }

    #[test]
    fn a_non_finite_length_takes_its_default_and_is_counted() {
        let (mut store, root, _, item) =
            tree(None, LengthTerms::dp(1.0) + LengthTerms::em(f32::INFINITY));
        store.layout(root, surface(), &mut Vec::new());
        assert_eq!(
            crate::layout::LayoutTree::input(&store, item.index())
                .size()
                .width,
            Length::Fit
        );
        assert_eq!(store.length_stats().nonfinite, 1);
        let warnings = store.take_length_warnings();
        if cfg!(debug_assertions) {
            assert_eq!(
                warnings,
                vec![LengthWarning {
                    node: item,
                    issue: LengthIssue::NonFinite
                }]
            );
            assert_eq!(warnings[0].issue.code(), "E3106");
        }
    }

    #[test]
    fn a_percent_on_an_indefinite_basis_falls_back_and_warns() {
        // The column fits its content along its Row parent, so a leaf's width
        // percentage has no basis: a pure one fits, a mixed one
        // keeps its fixed term.
        let (mut store, root, _, item) = tree(None, LengthTerms::pct(50.0));
        store.layout(root, surface(), &mut Vec::new());
        assert_eq!(store.bounds(item).w, 0.0, "a bare leaf fits to 0");
        assert_eq!(store.length_stats().indefinite_basis, 1);
        let (mut store, root, _, item) = tree(None, LengthTerms::pct(50.0) + LengthTerms::dp(12.0));
        store.layout(root, surface(), &mut Vec::new());
        assert_eq!(store.bounds(item).w, 12.0);
        if cfg!(debug_assertions) {
            let warnings = store.take_length_warnings();
            assert_eq!(warnings.len(), 1);
            assert_eq!(warnings[0].issue.code(), "E3105");
            store.layout(root, surface(), &mut Vec::new());
            assert!(
                store.take_length_warnings().is_empty(),
                "each node warns once"
            );
        }
    }

    #[test]
    fn a_percent_on_a_definite_basis_resolves() {
        let mut store = NodeStore::new();
        let mut item = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                item = Some(cx.leaf(leaf(Length::pct(25.0))).id());
            })
            .id()
        };
        store.layout(root, surface(), &mut Vec::new());
        assert_eq!(store.bounds(item.unwrap()).w, 100.0);
        assert_eq!(store.length_stats().indefinite_basis, 0);
    }

    #[test]
    fn only_a_changed_environment_meets_dependencies() {
        let env = LengthEnv::default();
        assert!(env.changed(&env).is_empty());
        let scaled = LengthEnv {
            scale_factor: 2.0,
            ..env
        };
        let changed = env.changed(&scaled);
        assert!(changed.intersects(LengthDeps::SCALE_FACTOR));
        assert!(!changed.intersects(LengthDeps::TEXT_SCALE));
    }
}
