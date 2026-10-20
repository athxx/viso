//! Layout properties set from values as they change: a view's binding that
//! reads state, an input or the theme delivers its value here, and the node
//! lays out by it as though it had been built with it.
//!
//! Every length goes through the node's bound [`NodeLengths`], so a value in
//! `px`, `sp` or `em` follows the environment as a built one does, and folds
//! with the constant lengths the node was built with. A box rule or the
//! padding keeps the node's rules carried by its lengths, which then hold all
//! of them; the compiler leaves a node's constant rules there for this.

use crate::component::NodeStore;
use crate::grid::{AdaptiveColumns, GridPlacement, GridStyle, TrackSizing};
use crate::layout::{
    Align, AlignItems, Alignment2D, Axis, Justify, LayoutInput, Length, Placement, StackPlacement,
};
use crate::length::{EdgeTerms, InsetTerms, LengthTerms, NodeLengths};
use crate::node::NodeId;

/// How a node sizes on one axis, as a value gives it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BoundSize {
    /// An extent of these terms.
    Fixed(LengthTerms),
    /// A share of the leftover main-axis space.
    Fill {
        /// Its weight.
        weight: f32,
    },
    /// The content's extent within the room.
    Fit,
    /// The narrowest the content lays out in.
    MinContent,
    /// The content's unconstrained extent.
    MaxContent,
}

/// A layout property's value, as a view's binding delivers it.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutValue {
    /// `width`.
    Width(BoundSize),
    /// `height`.
    Height(BoundSize),
    /// `min_width`.
    MinWidth(LengthTerms),
    /// `max_width`, `None` for no bound.
    MaxWidth(Option<LengthTerms>),
    /// `min_height`.
    MinHeight(LengthTerms),
    /// `max_height`, `None` for no bound.
    MaxHeight(Option<LengthTerms>),
    /// `margin`.
    Margin(EdgeTerms),
    /// A container's `padding`.
    Padding(EdgeTerms),
    /// A flex container's `gap`, both of a grid's gaps.
    Gap(LengthTerms),
    /// A grid's `column_gap`.
    ColumnGap(LengthTerms),
    /// A grid's `row_gap`.
    RowGap(LengthTerms),
    /// A text node's `font_size`.
    FontSize(LengthTerms),
    /// A flex container's `axis`.
    Axis(Axis),
    /// A flex container's `justify`.
    Justify(Justify),
    /// A flex container's `align`.
    Align(Align),
    /// A grid's `columns`.
    Columns(Vec<TrackSizing>),
    /// A grid's `rows`.
    Rows(Vec<TrackSizing>),
    /// A grid's `auto_rows`.
    AutoRows(TrackSizing),
    /// A grid's `align_items`.
    AlignItems(AlignItems),
    /// A grid's `adaptive_columns`.
    AdaptiveColumns(Option<AdaptiveColumns>),
    /// A stack's `content_align`.
    ContentAlign(Alignment2D),
    /// A grid child's `grid.column`.
    GridColumn(Option<u16>),
    /// A grid child's `grid.row`.
    GridRow(Option<u16>),
    /// A grid child's `grid.column_span`.
    GridColumnSpan(u16),
    /// A grid child's `grid.row_span`.
    GridRowSpan(u16),
    /// A stack child's `stack.align`.
    StackAlign(Option<Alignment2D>),
    /// A stack child's `stack.layer`.
    StackLayer(i32),
    /// An absolute child's `absolute.top`.
    AbsoluteTop(Option<LengthTerms>),
    /// An absolute child's `absolute.end`.
    AbsoluteEnd(Option<LengthTerms>),
    /// An absolute child's `absolute.bottom`.
    AbsoluteBottom(Option<LengthTerms>),
    /// An absolute child's `absolute.start`.
    AbsoluteStart(Option<LengthTerms>),
}

impl NodeStore {
    /// Lays node `id` out by `value`, marking what it moves as the setter of
    /// that property does. A no-op for a stale handle, an unchanged value or
    /// a property the node does not take.
    pub fn set_layout_value(&mut self, id: NodeId, value: LayoutValue) {
        if !self.arena().is_live(id) {
            return;
        }
        match value {
            LayoutValue::Width(size) => self.set_bound_size(id, true, size),
            LayoutValue::Height(size) => self.set_bound_size(id, false, size),
            LayoutValue::MinWidth(terms) => self.edit_rules(id, |l| l.min_width = Some(terms)),
            LayoutValue::MaxWidth(terms) => self.edit_rules(id, |l| l.max_width = terms),
            LayoutValue::MinHeight(terms) => self.edit_rules(id, |l| l.min_height = Some(terms)),
            LayoutValue::MaxHeight(terms) => self.edit_rules(id, |l| l.max_height = terms),
            LayoutValue::Margin(edges) => self.edit_rules(id, |l| l.margin = Some(edges)),
            LayoutValue::Padding(edges) => self.edit_rules(id, |l| l.padding = Some(edges)),
            LayoutValue::Gap(terms) => {
                let grid = self.is_grid(id);
                self.edit_lengths(id, |l| {
                    l.gap = Some(terms);
                    if grid {
                        l.row_gap = Some(terms);
                    }
                });
            }
            LayoutValue::ColumnGap(terms) => self.edit_lengths(id, |l| l.gap = Some(terms)),
            LayoutValue::RowGap(terms) => self.edit_lengths(id, |l| l.row_gap = Some(terms)),
            LayoutValue::FontSize(terms) => self.edit_lengths(id, |l| l.font_size = Some(terms)),
            LayoutValue::Axis(to) => self.edit_arrangement(id, |a| a.0 = to),
            LayoutValue::Justify(to) => self.edit_arrangement(id, |a| a.1 = to),
            LayoutValue::Align(to) => self.edit_arrangement(id, |a| a.2 = to),
            LayoutValue::Columns(tracks) => self.edit_grid(id, |g| g.columns = tracks),
            LayoutValue::Rows(tracks) => self.edit_grid(id, |g| g.rows = tracks),
            LayoutValue::AutoRows(track) => self.edit_grid(id, |g| g.auto_rows = track),
            LayoutValue::AlignItems(to) => self.edit_grid(id, |g| g.align_items = to),
            LayoutValue::AdaptiveColumns(to) => self.edit_grid(id, |g| g.adaptive_columns = to),
            LayoutValue::ContentAlign(to) => self.set_stack_align(id, to),
            LayoutValue::GridColumn(at) => self.edit_cell(id, |c| c.column = at),
            LayoutValue::GridRow(at) => self.edit_cell(id, |c| c.row = at),
            LayoutValue::GridColumnSpan(span) => {
                self.edit_cell(id, |c| c.column_span = span.max(1))
            }
            LayoutValue::GridRowSpan(span) => self.edit_cell(id, |c| c.row_span = span.max(1)),
            LayoutValue::StackAlign(to) => self.edit_stacked(id, |s| s.align = to),
            LayoutValue::StackLayer(layer) => self.edit_stacked(id, |s| s.layer = layer),
            LayoutValue::AbsoluteTop(terms) => self.edit_insets(id, |i| i.top = terms),
            LayoutValue::AbsoluteEnd(terms) => self.edit_insets(id, |i| i.end = terms),
            LayoutValue::AbsoluteBottom(terms) => self.edit_insets(id, |i| i.bottom = terms),
            LayoutValue::AbsoluteStart(terms) => self.edit_insets(id, |i| i.start = terms),
        }
    }

    /// An axis's size: a fixed extent folds from the lengths, any other is the
    /// request itself.
    fn set_bound_size(&mut self, id: NodeId, width: bool, size: BoundSize) {
        let length = match size {
            BoundSize::Fixed(terms) => {
                return self.edit_lengths(id, |l| {
                    *if width { &mut l.width } else { &mut l.height } = Some(terms);
                });
            }
            BoundSize::Fill { weight } => Length::Fill {
                weight: weight.max(0.0),
            },
            BoundSize::Fit => Length::Fit,
            BoundSize::MinContent => Length::MinContent,
            BoundSize::MaxContent => Length::MaxContent,
        };
        self.edit_lengths(id, |l| {
            *if width { &mut l.width } else { &mut l.height } = None;
        });
        let Some(mut request) = self.size_request(id) else {
            return;
        };
        *if width {
            &mut request.width
        } else {
            &mut request.height
        } = length;
        self.set_fixed_size(id, request);
    }

    /// Rewrites the node's bound lengths with `edit`, unbinding them when
    /// nothing is left.
    fn edit_lengths(&mut self, id: NodeId, edit: impl FnOnce(&mut NodeLengths)) {
        let mut lengths = self.bound_lengths(id).unwrap_or_default();
        edit(&mut lengths);
        if lengths == NodeLengths::default() {
            self.unbind_lengths(id);
        } else {
            self.bind_lengths(id, lengths);
        }
    }

    /// [`edit_lengths`](Self::edit_lengths) for a box rule or the padding:
    /// the lengths then carry all of the node's rules, so one that a value
    /// leaves unbounded does not keep its last folded bound.
    fn edit_rules(&mut self, id: NodeId, edit: impl FnOnce(&mut NodeLengths)) {
        self.edit_lengths(id, |lengths| {
            edit(lengths);
            if !lengths.has_rules() {
                lengths.margin = Some(EdgeTerms::default());
            }
        });
    }

    fn edit_arrangement(&mut self, id: NodeId, edit: impl FnOnce(&mut (Axis, Justify, Align))) {
        let LayoutInput::Flex {
            axis,
            justify,
            align,
            ..
        } = self.layout_input(id)
        else {
            return;
        };
        let mut to = (axis, justify, align);
        edit(&mut to);
        self.set_flex_arrangement(id, to);
    }

    fn edit_grid(&mut self, id: NodeId, edit: impl FnOnce(&mut GridStyle)) {
        let Some(mut template) = self.grid_template(id) else {
            return;
        };
        edit(&mut template);
        self.set_grid_template(id, template);
    }

    fn edit_cell(&mut self, id: NodeId, edit: impl FnOnce(&mut GridPlacement)) {
        let mut cell = match self.placement(id) {
            Some(Placement::Grid(cell)) => cell,
            _ => GridPlacement::default(),
        };
        edit(&mut cell);
        self.set_placement(id, Placement::Grid(cell));
    }

    fn edit_stacked(&mut self, id: NodeId, edit: impl FnOnce(&mut StackPlacement)) {
        let mut stacked = match self.placement(id) {
            Some(Placement::Stack(stacked)) => stacked,
            _ => StackPlacement::default(),
        };
        edit(&mut stacked);
        self.set_placement(id, Placement::Stack(stacked));
    }

    fn edit_insets(&mut self, id: NodeId, edit: impl FnOnce(&mut InsetTerms)) {
        self.edit_lengths(id, |lengths| edit(lengths.insets.get_or_insert_default()));
    }
}
