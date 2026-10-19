//! Lengths that depend on the environment: the five-term [`LengthTerms`]
//! authoring value, the [`LengthEnv`] it resolves against, and the store's
//! side table of nodes whose lengths re-fold when that environment changes.
//!
//! Layout's hot store only holds the folded [`Length`]: a `px`, `sp` or `em`
//! term becomes part of its `fixed` extent before any pass reads it. A node
//! whose length carries such a term keeps its terms here, with the mask of
//! what they read, so a scale-factor, text-scale or font-size change re-folds
//! only the bindings it meets and dirties only the nodes whose folded length
//! moved. A tree of plain `dp` and `%` lengths never touches this table, nor
//! does a padding of plain `dp` whose `start` and `end` agree.

use crate::component::NodeStore;
use crate::dirty::DirtyClass;
use crate::layout::{AbsoluteInsets, BoxRules, Inset, LayoutInput, Length, Placement};

use crate::node::NodeId;
pub use viso_schema::length::{
    EdgeTerms, InsetTerms, LengthDeps, LengthEnv, LengthTerms, NodeLengths,
};

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

/// One node's bound lengths: what they read, and the slot of `terms` that
/// keeps them.
#[derive(Debug, Clone, Copy)]
struct LengthBinding {
    node: NodeId,
    deps: LengthDeps,
    /// The binding re-folds at the next layout.
    pending: bool,
    slot: u32,
}

/// A binding's terms but its box rules.
#[derive(Debug, Clone, Copy, PartialEq)]
struct SizeTerms {
    width: Option<LengthTerms>,
    height: Option<LengthTerms>,
    gap: Option<LengthTerms>,
    font_size: Option<LengthTerms>,
    padding: Option<EdgeTerms>,
}

/// A binding's terms that few bindings have: its box rules, a grid's row
/// gap and an absolute child's insets.
#[derive(Debug, Clone, Copy, PartialEq)]
struct RareTerms {
    min_width: Option<LengthTerms>,
    max_width: Option<LengthTerms>,
    min_height: Option<LengthTerms>,
    max_height: Option<LengthTerms>,
    margin: Option<EdgeTerms>,
    row_gap: Option<LengthTerms>,
    insets: Option<InsetTerms>,
}

/// No slot of [`LengthBindings::rare_terms`].
const NO_RARE_TERMS: u32 = u32::MAX;

/// The store's environment-dependent lengths: the environment, each bound
/// node by ascending index, and what layout reported.
#[derive(Default)]
pub(crate) struct LengthBindings {
    env: LengthEnv,
    bindings: Vec<LengthBinding>,
    /// Each binding's terms, with its slot of `rare_terms` or
    /// [`NO_RARE_TERMS`]; `free_terms` lists the slots a dropped binding left.
    terms: Vec<(SizeTerms, u32)>,
    free_terms: Vec<u32>,
    rare_terms: Vec<RareTerms>,
    free_rare_terms: Vec<u32>,
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
        self.terms.clear();
        self.free_terms.clear();
        self.rare_terms.clear();
        self.free_rare_terms.clear();
        self.pending = false;
        self.font_moved = false;
        #[cfg(debug_assertions)]
        {
            self.reported.clear();
            self.warnings.clear();
        }
    }

    /// The lengths kept in `slot`.
    fn lengths(&self, slot: u32) -> NodeLengths {
        let (size, rules) = self.terms[slot as usize];
        let mut lengths = NodeLengths {
            width: size.width,
            height: size.height,
            gap: size.gap,
            font_size: size.font_size,
            padding: size.padding,
            ..NodeLengths::default()
        };
        if rules != NO_RARE_TERMS {
            let rules = self.rare_terms[rules as usize];
            lengths.min_width = rules.min_width;
            lengths.max_width = rules.max_width;
            lengths.min_height = rules.min_height;
            lengths.max_height = rules.max_height;
            lengths.margin = rules.margin;
            lengths.row_gap = rules.row_gap;
            lengths.insets = rules.insets;
        }
        lengths
    }

    /// The font size kept in `slot`.
    fn font_size(&self, slot: u32) -> Option<LengthTerms> {
        self.terms[slot as usize].0.font_size
    }

    /// Keeps `lengths` in a free slot, or `slot` when one is given.
    fn keep(&mut self, lengths: &NodeLengths, slot: Option<u32>) -> u32 {
        let size = SizeTerms {
            width: lengths.width,
            height: lengths.height,
            gap: lengths.gap,
            font_size: lengths.font_size,
            padding: lengths.padding,
        };
        let rare = RareTerms {
            min_width: lengths.min_width,
            max_width: lengths.max_width,
            min_height: lengths.min_height,
            max_height: lengths.max_height,
            margin: lengths.margin,
            row_gap: lengths.row_gap,
            insets: lengths.insets,
        };
        let has_rare = rare.min_width.is_some()
            || rare.max_width.is_some()
            || rare.min_height.is_some()
            || rare.max_height.is_some()
            || rare.margin.is_some()
            || rare.row_gap.is_some()
            || rare.insets.is_some();
        let old_rare = slot.map_or(NO_RARE_TERMS, |slot| self.terms[slot as usize].1);
        let rare_slot = match (has_rare, old_rare) {
            (false, NO_RARE_TERMS) => NO_RARE_TERMS,
            (false, old) => {
                self.free_rare_terms.push(old);
                NO_RARE_TERMS
            }
            (true, NO_RARE_TERMS) => match self.free_rare_terms.pop() {
                Some(free) => {
                    self.rare_terms[free as usize] = rare;
                    free
                }
                None => {
                    self.rare_terms.push(rare);
                    (self.rare_terms.len() - 1) as u32
                }
            },
            (true, old) => {
                self.rare_terms[old as usize] = rare;
                old
            }
        };
        match slot.or_else(|| self.free_terms.pop()) {
            Some(slot) => {
                self.terms[slot as usize] = (size, rare_slot);
                slot
            }
            None => {
                self.terms.push((size, rare_slot));
                (self.terms.len() - 1) as u32
            }
        }
    }

    /// Frees `slot` and the rare terms it holds.
    fn release(&mut self, slot: u32) {
        let rules = self.terms[slot as usize].1;
        if rules != NO_RARE_TERMS {
            self.free_rare_terms.push(rules);
        }
        self.free_terms.push(slot);
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
        let table = self.lengths_mut();
        let font_moved: bool;
        match table
            .bindings
            .binary_search_by_key(&id.index(), |b| b.node.index())
        {
            Ok(at) => {
                let old = table.bindings[at];
                if old.node == id && table.lengths(old.slot) == lengths {
                    return;
                }
                font_moved = old.node != id
                    || table.font_size(old.slot).is_some()
                    || lengths.font_size.is_some();
                let slot = table.keep(&lengths, Some(old.slot));
                table.bindings[at] = LengthBinding {
                    node: id,
                    deps,
                    pending: true,
                    slot,
                };
            }
            Err(at) => {
                font_moved = lengths.font_size.is_some();
                let slot = table.keep(&lengths, None);
                table.bindings.insert(
                    at,
                    LengthBinding {
                        node: id,
                        deps,
                        pending: true,
                        slot,
                    },
                );
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
            let slot = table.bindings.remove(at).slot;
            let font = table.font_size(slot).is_some();
            table.release(slot);
            if font {
                table.pending = true;
                table.font_moved = true;
            }
        }
    }

    /// The node's bound lengths, or `None` when it has none.
    pub fn bound_lengths(&self, id: NodeId) -> Option<NodeLengths> {
        let table = self.lengths();
        table.find(id).map(|b| table.lengths(b.slot))
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
        let rescaled = table.env.scale_factor != env.scale_factor;
        table.env = env;
        if !changed.is_empty() {
            table.mark(changed);
        }
        // Displacements snap to the device grid, so a new scale moves them.
        if rescaled {
            self.request_repaint();
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
            if let Some(font) = table.find(at).and_then(|b| table.font_size(b.slot)) {
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
                let table = self.lengths_mut();
                table.bindings.remove(at);
                table.release(binding.slot);
                continue;
            }
            at += 1;
            if !binding.pending {
                continue;
            }
            self.lengths_mut().bindings[at - 1].pending = false;
            let lengths = self.lengths().lengths(binding.slot);
            self.fold_binding(binding.node, &lengths);
        }
        self.lengths_mut().pending = false;
    }

    fn fold_binding(&mut self, id: NodeId, lengths: &NodeLengths) {
        let env = self.lengths().env;
        let font = self.font_of(id);
        if let Some(own) = lengths.font_size
            && own.fold_font(&env, self.parent_font(id, own)).is_none()
        {
            self.lengths_mut().report(id, LengthIssue::NonFinite);
        }
        let mut class = DirtyClass::EMPTY;
        let mut requested = false;
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
                requested = true;
            }
        }
        if requested {
            self.request_moved(id);
        }
        for (terms, rows) in [(lengths.gap, false), (lengths.row_gap, true)] {
            let Some(terms) = terms else {
                continue;
            };
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
            let slot = match self.layout_input_mut(id) {
                LayoutInput::Flex { gap, .. } if !rows => gap,
                LayoutInput::Grid { column_gap, .. } if !rows => column_gap,
                LayoutInput::Grid { row_gap, .. } => row_gap,
                _ => continue,
            };
            if *slot != folded {
                *slot = folded;
                class = class | DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT;
            }
        }
        if let Some(rules) = lengths.rules(&env, font) {
            let rules = rules.unwrap_or_else(|| {
                self.lengths_mut().report(id, LengthIssue::NonFinite);
                BoxRules::NONE
            });
            self.set_box_rules(id, rules);
        }
        if let Some(edges) = lengths.padding {
            let folded = edges.fold(&env, font).unwrap_or_else(|| {
                self.lengths_mut().report(id, LengthIssue::NonFinite);
                Inset::default()
            });
            let moved = match self.layout_input_mut(id) {
                LayoutInput::Flex { padding, .. }
                | LayoutInput::Grid { padding, .. }
                | LayoutInput::Stack { padding, .. }
                | LayoutInput::Absolute { padding, .. } => {
                    let moved = *padding != folded;
                    *padding = folded;
                    moved
                }
                _ => false,
            };
            if moved {
                class = class | DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT;
            }
        }
        if let Some(insets) = lengths.insets {
            let folded = insets.fold(&env, font).unwrap_or_else(|| {
                self.lengths_mut().report(id, LengthIssue::NonFinite);
                AbsoluteInsets::NONE
            });
            self.set_placement(id, Placement::Absolute(folded));
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
    use crate::adaptive::LayoutDirection;

    const ENV: LengthEnv = LengthEnv {
        scale_factor: 2.0,
        text_scale: 1.5,
        base_font_size: 14.0,
        direction: LayoutDirection::Ltr,
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
    fn a_padding_s_start_and_end_follow_the_layout_direction() {
        let (mut store, root, column, item) = tree(None, LengthTerms::dp(10.0));
        let sided = EdgeTerms {
            start: LengthTerms::dp(6.0),
            top: LengthTerms::sp(2.0),
            ..EdgeTerms::default()
        };
        store.bind_lengths(
            column,
            NodeLengths {
                padding: Some(sided),
                ..NodeLengths::default()
            },
        );
        store.layout(root, surface(), &mut Vec::new());
        let column_box = store.bounds(column);
        assert_eq!(store.bounds(item).x - column_box.x, 6.0);
        assert_eq!(store.bounds(item).y - column_box.y, 2.0);
        store.clear_dirty();
        store.set_length_env(LengthEnv {
            direction: LayoutDirection::Rtl,
            ..LengthEnv::default()
        });
        let (mut scratch, mut redo) = (Vec::new(), Vec::new());
        store.relayout_dirty(root, surface(), &mut scratch, &mut redo);
        assert!(matches!(
            *store.layout_input_mut(column),
            LayoutInput::Flex { padding, .. } if padding == Inset { right: 6.0, top: 2.0, ..Inset::default() }
        ));

        // Edges that agree read no direction.
        assert!(
            !EdgeTerms::all(LengthTerms::dp(4.0))
                .deps()
                .intersects(LengthDeps::DIRECTION)
        );
        assert!(
            sided
                .deps()
                .intersects(LengthDeps::DIRECTION | LengthDeps::TEXT_SCALE)
        );
        let cut = EdgeTerms::all(LengthTerms::dp(-3.0)).fold(&LengthEnv::default(), 14.0);
        assert_eq!(cut, Some(Inset::default()), "never negative");
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
