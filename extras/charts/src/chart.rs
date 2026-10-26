//! The [`Chart`] component: a line, area, bar or scatter chart with its
//! axes, its legend and a readout of what the pointer or the keyboard
//! points at.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::{
    AbsoluteInsets, AbsoluteStyle, Align, Axis, BoxStyle, BuildCx, Component, CursorIcon,
    DispatchPhase, EventCx, FlexStyle, FocusChange, FontWeight, Inset, Justify, Key, LeafStyle,
    Length, LiveRegion, NodeId, Placement, PlotMark, PointerButtons, PointerPhase, Rgba, Role,
    SemanticState, Semantics, Shadow, Sizing, Span, StateId, StateValue, StructureCx, TextAlign,
};
use viso_widgets::label;

use crate::model::{ChartKind, Model, Spec, model};
use crate::scale::{format_tick, format_value};
use crate::series::Series;
use crate::style::ChartStyle;

/// How near, in points, the pointer must come to a scatter's dot to read
/// it.
const REACH: f32 = 24.0;

/// What a chart's readout reads, as its handlers are told it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChartPoint {
    /// The series, for a scatter's dot; `None` for a place along `x`, which
    /// every series is read at.
    pub series: Option<usize>,
    /// The category or the place along `x` (in order of `x`), or the dot's
    /// index in its series.
    pub index: usize,
}

type OnSelect = Box<dyn FnMut(&mut EventCx<'_>, ChartPoint)>;
type XFormat = Rc<dyn Fn(f64) -> String>;
type TickFormat = Rc<dyn Fn(f64, f64) -> String>;

/// A chart of [`Series`]. Construct one with [`line_chart`], [`area_chart`],
/// [`bar_chart`] or [`scatter_chart`].
///
/// Invalidation: pointing at another place moves the readout and the mark
/// (`PAINT`, `LAYOUT` for the readout); hiding a series, or new data
/// through its [`ChartHandle`], redraws the plot and rebuilds the axis
/// labels; a resize repaints the plot, which draws into whatever box it is
/// given.
pub struct Chart {
    label: String,
    kind: ChartKind,
    series: Vec<Series>,
    categories: Option<Vec<String>>,
    stacked: bool,
    smooth: bool,
    y_range: Option<(f64, f64)>,
    legend: Option<bool>,
    max_x_labels: usize,
    style: ChartStyle,
    x_format: XFormat,
    y_format: TickFormat,
    value_format: XFormat,
    on_select: RefCell<Option<OnSelect>>,
    slot: Option<ChartHandleSlot>,
}

fn chart(kind: ChartKind, label: impl Into<String>, series: Vec<Series>) -> Chart {
    Chart {
        label: label.into(),
        kind,
        series,
        categories: None,
        stacked: false,
        smooth: false,
        y_range: None,
        legend: None,
        max_x_labels: 12,
        style: ChartStyle::light(),
        x_format: Rc::new(format_value),
        y_format: Rc::new(format_tick),
        value_format: Rc::new(format_value),
        on_select: RefCell::new(None),
        slot: None,
    }
}

/// A line chart named `label` of `series`: a line through each, and a
/// crosshair reading every series at the place along `x` it is at.
pub fn line_chart(label: impl Into<String>, series: Vec<Series>) -> Chart {
    chart(ChartKind::Line, label, series)
}

/// An area chart: lines over a wash of their color down to zero, or
/// stacked one on another.
pub fn area_chart(label: impl Into<String>, series: Vec<Series>) -> Chart {
    chart(ChartKind::Area, label, series)
}

/// A bar chart: a bar per series in each category, side by side or
/// stacked, grown from zero.
pub fn bar_chart(label: impl Into<String>, series: Vec<Series>) -> Chart {
    chart(ChartKind::Bar, label, series)
}

/// A scatter chart: a dot at each point, the nearest read.
pub fn scatter_chart(label: impl Into<String>, series: Vec<Series>) -> Chart {
    chart(ChartKind::Scatter, label, series)
}

impl Chart {
    /// Name the places along `x`: the series' values fall one per category.
    pub fn categories(mut self, categories: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.categories = Some(categories.into_iter().map(Into::into).collect());
        self
    }

    /// Stack the series one on another (areas and bars).
    pub fn stacked(mut self) -> Self {
        self.stacked = true;
        self
    }

    /// Run lines through their points in a smooth curve that never
    /// overshoots them.
    pub fn smooth(mut self) -> Self {
        self.smooth = true;
        self
    }

    /// Run the `y` axis over `lo..hi` (rounded out to round ticks) whatever
    /// the data.
    pub fn y_range(mut self, lo: f64, hi: f64) -> Self {
        self.y_range = Some((lo, hi));
        self
    }

    /// Show the legend, or not; by default it shows for two series or more.
    pub fn legend(mut self, legend: bool) -> Self {
        self.legend = Some(legend);
        self
    }

    /// Label no more than `count` places along `x`, every few if there are
    /// more.
    pub fn max_x_labels(mut self, count: usize) -> Self {
        self.max_x_labels = count.max(1);
        self
    }

    /// Write numbers along `x` (and the readout's place) with `format`.
    pub fn x_format(mut self, format: impl Fn(f64) -> String + 'static) -> Self {
        self.x_format = Rc::new(format);
        self
    }

    /// Write the ticks along `y` with `format(value, step)`.
    pub fn y_format(mut self, format: impl Fn(f64, f64) -> String + 'static) -> Self {
        self.y_format = Rc::new(format);
        self
    }

    /// Write the values the readout reads with `format`.
    pub fn value_format(mut self, format: impl Fn(f64) -> String + 'static) -> Self {
        self.value_format = Rc::new(format);
        self
    }

    /// Run `handler` with what the readout reads when it is pressed or
    /// Enter is pressed on it.
    pub fn on_select(
        mut self,
        handler: impl FnMut(&mut EventCx<'_>, ChartPoint) + 'static,
    ) -> Self {
        self.on_select = RefCell::new(Some(Box::new(handler)));
        self
    }

    /// Replace the whole [`ChartStyle`].
    pub fn style(mut self, style: ChartStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the chart's own size within its parent.
    pub fn size(mut self, size: Sizing) -> Self {
        self.style.size = size;
        self
    }

    /// Fill `slot` with the chart's handle once it is built.
    pub fn handle(mut self, slot: &ChartHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }
}

/// What the readout reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hover {
    /// A place along `x`.
    Station(usize),
    /// A scatter's dot: the column of its series, and its index there.
    Dot(usize, usize),
}

/// What a frame's structure pass applies, set by the handlers.
mod pending {
    pub const DATA: u8 = 1;
    pub const LEGEND: u8 = 1 << 1;
    pub const HOVER: u8 = 1 << 2;
}

#[derive(Debug, Clone, Copy)]
struct Nodes {
    plot: NodeId,
    y_axis: NodeId,
    x_axis: NodeId,
    tooltip: NodeId,
    legend: Option<NodeId>,
    revision: StateId,
}

/// A chart's state, shared by its handlers, its structure hook and its
/// handle.
struct State {
    kind: ChartKind,
    stacked: bool,
    smooth: bool,
    y_range: Option<(f64, f64)>,
    legend: Option<bool>,
    max_x_labels: usize,
    style: ChartStyle,
    x_format: XFormat,
    y_format: TickFormat,
    value_format: XFormat,
    series: RefCell<Vec<Series>>,
    categories: RefCell<Option<Vec<String>>>,
    hidden: RefCell<Vec<bool>>,
    model: RefCell<Model>,
    hover: Cell<Option<Hover>>,
    nodes: Cell<Option<Nodes>>,
    pending: Cell<u8>,
    on_select: RefCell<Option<OnSelect>>,
}

impl State {
    fn work_out(&self) -> Model {
        let series = self.series.borrow();
        let categories = self.categories.borrow();
        let hidden = self.hidden.borrow();
        let x_format = &*self.x_format;
        let y_format = &*self.y_format;
        model(&Spec {
            kind: self.kind,
            series: &series,
            categories: categories.as_deref(),
            hidden: &hidden,
            stacked: self.stacked,
            smooth: self.smooth,
            y_range: self.y_range,
            style: &self.style,
            max_x_labels: self.max_x_labels,
            x_format,
            y_format,
        })
    }

    fn shows_legend(&self) -> bool {
        self.legend.unwrap_or(self.series.borrow().len() > 1)
    }

    /// Ask the next structure pass to apply `what`.
    fn ask(&self, ev: &mut EventCx<'_>, what: u8) {
        let Some(nodes) = self.nodes.get() else {
            return;
        };
        self.pending.set(self.pending.get() | what);
        let n = match ev.get(nodes.revision) {
            Some(StateValue::Int(n)) => n,
            _ => 0,
        };
        ev.set(nodes.revision, StateValue::Int(n.wrapping_add(1)));
    }

    /// Read `hover`, redrawing the mark and the readout if it moved.
    fn point_at(&self, ev: &mut EventCx<'_>, hover: Option<Hover>) {
        if self.hover.get() != hover {
            self.hover.set(hover);
            self.ask(ev, pending::HOVER);
        }
    }

    fn told(&self, hover: Hover) -> ChartPoint {
        match hover {
            Hover::Station(index) => ChartPoint {
                series: None,
                index,
            },
            Hover::Dot(column, index) => ChartPoint {
                series: self.model.borrow().scatter.get(column).map(|c| c.0),
                index,
            },
        }
    }

    fn select(&self, ev: &mut EventCx<'_>) {
        let Some(hover) = self.hover.get() else {
            return;
        };
        let told = self.told(hover);
        let handler = self.on_select.borrow_mut().take();
        if let Some(mut handler) = handler {
            handler(ev, told);
            let mut slot = self.on_select.borrow_mut();
            if slot.is_none() {
                *slot = Some(handler);
            }
        }
    }

    fn toggle(&self, ev: &mut EventCx<'_>, index: usize) {
        {
            let mut hidden = self.hidden.borrow_mut();
            if hidden.len() <= index {
                hidden.resize(index + 1, false);
            }
            hidden[index] = !hidden[index];
        }
        self.hover.set(None);
        self.ask(ev, pending::DATA | pending::HOVER);
    }
}

/// A built [`Chart`], to give it new data. Cheap to clone: every clone is
/// the same chart.
#[derive(Clone)]
pub struct ChartHandle(Rc<State>);

/// Where a [`Chart`] puts its handle once built.
pub type ChartHandleSlot = Rc<RefCell<Option<ChartHandle>>>;

impl ChartHandle {
    /// Draw `series` in place of what it draws. Series keep their colors by
    /// their place; a hidden series stays hidden.
    pub fn set_series(&self, ev: &mut EventCx<'_>, series: Vec<Series>) {
        let state = &self.0;
        let relabel = {
            let was = state.series.borrow();
            was.len() != series.len() || was.iter().zip(&series).any(|(a, b)| a.name != b.name)
        };
        *state.series.borrow_mut() = series;
        state.hover.set(None);
        let legend = if relabel { pending::LEGEND } else { 0 };
        state.ask(ev, pending::DATA | pending::HOVER | legend);
    }

    /// Name the places along `x` anew, or none.
    pub fn set_categories(&self, ev: &mut EventCx<'_>, categories: Option<Vec<String>>) {
        *self.0.categories.borrow_mut() = categories;
        self.0.hover.set(None);
        self.0.ask(ev, pending::DATA | pending::HOVER);
    }

    /// What the readout reads, if it shows.
    pub fn hovered(&self) -> Option<ChartPoint> {
        self.0.hover.get().map(|h| self.0.told(h))
    }

    /// Whether series `index` is hidden from the legend.
    pub fn is_hidden(&self, index: usize) -> bool {
        self.0.hidden.borrow().get(index).copied().unwrap_or(false)
    }
}

const FIT: Sizing = Sizing {
    width: Length::Fit,
    height: Length::Fit,
};

/// The height of a line of the chart's text.
fn line_height(style: &ChartStyle) -> f32 {
    (style.font_size * 1.4).round()
}

/// The height of the `x` axis under the plot.
fn x_axis_height(style: &ChartStyle) -> f32 {
    line_height(style) + 6.0
}

fn flex(axis: Axis, size: Sizing) -> FlexStyle {
    FlexStyle {
        axis,
        size,
        align: Align::Stretch,
        justify: Justify::Start,
        ..FlexStyle::default()
    }
}

fn pinned(
    top: Option<Span>,
    start: Option<Span>,
    end: Option<Span>,
    bottom: Option<Span>,
) -> Placement {
    Placement::Absolute(AbsoluteInsets {
        top,
        end,
        bottom,
        start,
    })
}

impl Component for Chart {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let state = Rc::new(State {
            kind: self.kind,
            stacked: self.stacked,
            smooth: self.smooth,
            y_range: self.y_range,
            legend: self.legend,
            max_x_labels: self.max_x_labels,
            style,
            x_format: Rc::clone(&self.x_format),
            y_format: Rc::clone(&self.y_format),
            value_format: Rc::clone(&self.value_format),
            series: RefCell::new(self.series.clone()),
            categories: RefCell::new(self.categories.clone()),
            hidden: RefCell::new(vec![false; self.series.len()]),
            model: RefCell::new(Model::default_for(self.kind)),
            hover: Cell::new(None),
            nodes: Cell::new(None),
            pending: Cell::new(0),
            on_select: RefCell::new(self.on_select.borrow_mut().take()),
        });
        *state.model.borrow_mut() = state.work_out();
        let revision = cx.state(StateValue::Int(0));
        let lh = line_height(&style);
        let axis_h = x_axis_height(&style);
        let (mut plot, mut y_axis, mut x_axis, mut tooltip, mut legend) =
            (None, None, None, None, None);
        let mut area = None;
        let root = cx.flex(
            FlexStyle {
                gap: 8.0,
                ..flex(Axis::Column, style.size)
            },
            |cx| {
                if state.shows_legend() {
                    legend = Some(
                        cx.flex(
                            FlexStyle {
                                gap: 16.0,
                                align: Align::Center,
                                ..flex(Axis::Row, FIT)
                            },
                            |cx| build_legend(&state, cx),
                        )
                        .id(),
                    );
                }
                cx.flex(
                    FlexStyle {
                        gap: 8.0,
                        ..flex(Axis::Row, Sizing::fill())
                    },
                    |cx| {
                        y_axis = Some(
                            cx.flex(
                                FlexStyle {
                                    align: Align::End,
                                    padding: Inset {
                                        bottom: axis_h,
                                        ..Inset::default()
                                    },
                                    ..flex(
                                        Axis::Column,
                                        Sizing {
                                            width: Length::Fit,
                                            height: Length::fill(),
                                        },
                                    )
                                },
                                |cx| build_y_labels(&state, cx),
                            )
                            .id(),
                        );
                        cx.flex(flex(Axis::Column, Sizing::fill()), |cx| {
                            let built = cx.absolute(
                                AbsoluteStyle {
                                    padding: Inset {
                                        top: lh / 2.0,
                                        bottom: lh / 2.0,
                                        ..Inset::default()
                                    },
                                    size: Sizing::fill(),
                                    style: BoxStyle::NONE,
                                },
                                |cx| {
                                    let zero = Some(Span::ZERO);
                                    cx.place(pinned(zero, zero, zero, zero));
                                    let model = state.model.borrow();
                                    plot = Some(cx.plot(Sizing::fill(), model.plot.clone()).id());
                                    drop(model);
                                    cx.place(pinned(
                                        Some(Span::ZERO),
                                        Some(Span::ZERO),
                                        None,
                                        None,
                                    ));
                                    let tip = cx.flex(
                                        FlexStyle {
                                            gap: 4.0,
                                            padding: Inset {
                                                left: 10.0,
                                                right: 10.0,
                                                top: 8.0,
                                                bottom: 8.0,
                                            },
                                            style: BoxStyle {
                                                radius: 6.0,
                                                border: viso_ui::Border {
                                                    width: 1.0,
                                                    color: style.tooltip_border,
                                                },
                                                ..BoxStyle::solid(style.tooltip)
                                            },
                                            ..flex(Axis::Column, FIT)
                                        },
                                        |_| {},
                                    );
                                    cx.shadow(
                                        tip,
                                        Shadow::below(Rgba::new(0.0, 0.0, 0.0, 0.12), 4.0, 12.0),
                                    );
                                    cx.set_hidden(tip, true);
                                    cx.semantics(
                                        tip,
                                        Semantics::role(Role::Status).with_live(LiveRegion::Polite),
                                    );
                                    tooltip = Some(tip.id());
                                },
                            );
                            area = Some(built);
                            x_axis = Some(
                                cx.absolute(
                                    AbsoluteStyle {
                                        size: Sizing {
                                            width: Length::fill(),
                                            height: Length::Fixed(axis_h),
                                        },
                                        ..AbsoluteStyle::default()
                                    },
                                    |cx| build_x_labels(&state, cx),
                                )
                                .id(),
                            );
                        });
                    },
                );
            },
        );
        cx.focusable(root, true);
        cx.semantics(
            root,
            Semantics::role(Role::Group).with_label(self.label.clone()),
        );
        let keys = Rc::clone(&state);
        cx.on_key(root, move |ev| key(&keys, ev));
        if let Some(area) = area {
            let pointer = Rc::clone(&state);
            cx.on_pointer(area, move |ev| pointer_at(&pointer, ev));
        }
        let (Some(plot), Some(y_axis), Some(x_axis), Some(tooltip)) =
            (plot, y_axis, x_axis, tooltip)
        else {
            return;
        };
        state.nodes.set(Some(Nodes {
            plot,
            y_axis,
            x_axis,
            tooltip,
            legend,
            revision,
        }));
        let hook = Rc::clone(&state);
        cx.structure(|s| {
            s.store.add_structure_hook([revision], move |s, _| {
                if s.store.arena().is_live(plot) {
                    apply(&hook, s);
                }
            });
        });
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(ChartHandle(Rc::clone(&state)));
        }
    }
}

fn axis_label(style: &ChartStyle, text: String, align: TextAlign) -> viso_widgets::Label {
    label(text)
        .font_size(style.font_size)
        .color(style.muted)
        .line_height(line_height(style))
        .align(align)
        .ellipsis()
}

/// The `y` axis's labels, top to bottom.
fn build_y_labels(state: &State, cx: &mut BuildCx<'_>) {
    let style = state.style;
    let model = state.model.borrow();
    for (k, text) in model.y_labels.iter().rev().enumerate() {
        // Even room between the labels: the first at the top, the last at
        // the bottom, each on its tick.
        if k > 0 {
            cx.leaf(LeafStyle {
                size: Sizing {
                    width: Length::Fixed(0.0),
                    height: Length::fill(),
                },
                style: BoxStyle::NONE,
            });
        }
        axis_label(&style, text.clone(), TextAlign::End)
            .size(Sizing {
                width: Length::Fit,
                height: Length::Fixed(line_height(&style)),
            })
            .build(cx);
    }
}

/// The `x` axis's labels, each centered under its place.
fn build_x_labels(state: &State, cx: &mut BuildCx<'_>) {
    let style = state.style;
    let model = state.model.borrow();
    for l in &model.x_labels {
        cx.place(pinned(
            Some(Span::dp(6.0)),
            Some(Span {
                fixed: 0.0,
                pct: l.center - l.width / 2.0,
            }),
            None,
            None,
        ));
        axis_label(&style, l.text.clone(), TextAlign::Center)
            .size(Sizing {
                width: Length::Relative {
                    fixed: 0.0,
                    pct: l.width,
                },
                height: Length::Fixed(line_height(&style)),
            })
            .build(cx);
    }
}

/// A short stroke, a square or a dot in series `index`'s color, as its
/// marks are.
fn key_mark(state: &State, cx: &mut BuildCx<'_>, color: Rgba, line: bool) {
    let (w, h, r) = match (state.kind, line) {
        (_, true) | (ChartKind::Line, _) => (12.0, 2.0, 0.0),
        (ChartKind::Scatter, _) => (8.0, 8.0, 4.0),
        _ => (10.0, 10.0, 2.0),
    };
    cx.leaf(LeafStyle {
        size: Sizing::fixed(w, h),
        style: BoxStyle {
            radius: r,
            ..BoxStyle::solid(color)
        },
    });
}

/// The legend: each series' mark and name, a press or Space hiding or
/// showing it.
fn build_legend(state: &Rc<State>, cx: &mut BuildCx<'_>) {
    let style = state.style;
    let series = state.series.borrow();
    let hidden = state.hidden.borrow();
    for (i, s) in series.iter().enumerate() {
        let color = Model::color(&series, &style, i);
        let shown = !hidden.get(i).copied().unwrap_or(false);
        let item = cx.flex(
            FlexStyle {
                gap: 6.0,
                align: Align::Center,
                ..flex(Axis::Row, FIT)
            },
            |cx| {
                key_mark(state, cx, color, false);
                label(s.name.clone())
                    .font_size(style.font_size)
                    .color(style.text_secondary)
                    .build(cx);
            },
        );
        cx.focusable(item, true);
        cx.cursor(item, CursorIcon::Pointer);
        cx.semantics(
            item,
            Semantics::role(Role::CheckBox).with_label(s.name.clone()),
        );
        cx.semantic_state(item, SemanticState::checked(shown));
        cx.opacity(item, if shown { 1.0 } else { 0.4 });
        let press = Rc::clone(state);
        cx.on_pointer(item, move |ev| {
            if ev.phase() != DispatchPhase::Capture
                && let Some(p) = ev.pointer()
                && p.phase == PointerPhase::Up
                && (!ev.direct() || ev.still())
            {
                press.toggle(ev, i);
                ev.stop_propagation();
            }
        });
        let keys = Rc::clone(state);
        cx.on_key(item, move |ev| {
            if ev.phase() != DispatchPhase::Capture
                && let Some(k) = ev.key()
                && k.pressed
                && !k.repeat
                && matches!(k.key, Key::Space | Key::Enter)
            {
                keys.toggle(ev, i);
                ev.stop_propagation();
            }
        });
    }
}

/// What the readout reads at fraction `(ux, uy)` of a plot `w` by `h`.
fn hover_at(model: &Model, ux: f32, uy: f32, w: f32, h: f32) -> Option<Hover> {
    if model.kind == ChartKind::Scatter {
        let mut best: Option<(f32, Hover)> = None;
        for (c, (_, points)) in model.scatter.iter().enumerate() {
            for (k, p) in points.iter().enumerate() {
                let d = ((p.2.x - ux) * w).hypot((p.2.y - uy) * h);
                if d <= REACH && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, Hover::Dot(c, k)));
                }
            }
        }
        return best.map(|b| b.1);
    }
    let n = model.stations.len();
    if n == 0 {
        return None;
    }
    let k = if model.categorical {
        ((ux * n as f32).floor().max(0.0) as usize).min(n - 1)
    } else {
        let after = model.stations.partition_point(|&s| s < ux);
        match (after.checked_sub(1), (after < n).then_some(after)) {
            (Some(a), Some(b)) => {
                if ux - model.stations[a] <= model.stations[b] - ux {
                    a
                } else {
                    b
                }
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => return None,
        }
    };
    Some(Hover::Station(k))
}

fn pointer_at(state: &State, ev: &mut EventCx<'_>) {
    if ev.phase() == DispatchPhase::Capture {
        return;
    }
    let (Some(p), Some(nodes)) = (ev.pointer().copied(), state.nodes.get()) else {
        return;
    };
    match p.phase {
        PointerPhase::Leave => {
            if !ev.direct() {
                state.point_at(ev, None);
            }
            return;
        }
        PointerPhase::Up => {
            if p.buttons == PointerButtons::NONE && (!ev.direct() || ev.still()) {
                state.select(ev);
            }
            return;
        }
        PointerPhase::Move | PointerPhase::Enter | PointerPhase::Down => {}
    }
    if ev.direct() && p.phase != PointerPhase::Down {
        return;
    }
    let Some(r) = ev.rect_of(nodes.plot) else {
        return;
    };
    if r.w <= 0.0 || r.h <= 0.0 {
        return;
    }
    let ux = if ev.reads_rtl() {
        (r.x + r.w - p.x) / r.w
    } else {
        (p.x - r.x) / r.w
    };
    let uy = 1.0 - (p.y - r.y) / r.h;
    let hover = if (0.0..=1.0).contains(&ux) {
        hover_at(&state.model.borrow(), ux, uy, r.w, r.h)
    } else {
        None
    };
    state.point_at(ev, hover);
}

/// The readout's places in order along `x`: each station, or each dot.
fn stops(model: &Model) -> Vec<Hover> {
    if model.kind == ChartKind::Scatter {
        let mut dots: Vec<(f32, Hover)> = model
            .scatter
            .iter()
            .enumerate()
            .flat_map(|(c, (_, points))| {
                points
                    .iter()
                    .enumerate()
                    .map(move |(k, p)| (p.2.x, Hover::Dot(c, k)))
            })
            .collect();
        dots.sort_by(|a, b| a.0.total_cmp(&b.0));
        dots.into_iter().map(|d| d.1).collect()
    } else {
        (0..model.stations.len()).map(Hover::Station).collect()
    }
}

fn key(state: &State, ev: &mut EventCx<'_>) {
    if let Some(change) = ev.focus_change() {
        if change == FocusChange::Lost {
            state.point_at(ev, None);
        }
        return;
    }
    if ev.phase() == DispatchPhase::Capture {
        return;
    }
    let Some(k) = ev.key().copied().filter(|k| k.pressed) else {
        return;
    };
    let stops = stops(&state.model.borrow());
    if stops.is_empty() {
        return;
    }
    let at = state
        .hover
        .get()
        .and_then(|h| stops.iter().position(|&s| s == h));
    let last = stops.len() - 1;
    // Left and Right follow the screen: read right to left, Left goes on.
    let on = |key: Key| {
        matches!(
            (key, ev.reads_rtl()),
            (Key::Right, false) | (Key::Left, true)
        )
    };
    let to = match k.key {
        Key::Left | Key::Right => Some(match at {
            None => {
                if on(k.key) {
                    0
                } else {
                    last
                }
            }
            Some(i) if on(k.key) => (i + 1).min(last),
            Some(i) => i.saturating_sub(1),
        }),
        Key::Home => Some(0),
        Key::End => Some(last),
        Key::Escape if at.is_some() => {
            state.point_at(ev, None);
            ev.stop_propagation();
            return;
        }
        Key::Enter | Key::Space if at.is_some() && !k.repeat => {
            state.select(ev);
            ev.stop_propagation();
            return;
        }
        _ => None,
    };
    if let Some(to) = to {
        state.point_at(ev, Some(stops[to]));
        ev.stop_propagation();
    }
}

/// Free `parent`'s children and build new ones with `build`.
fn rebuild(s: &mut StructureCx<'_>, parent: NodeId, build: impl FnOnce(&mut BuildCx<'_>)) {
    let kids: Vec<NodeId> = s.store.arena().children(parent).collect();
    let mut scratch = Vec::new();
    for kid in kids {
        s.store.free_tree(kid, s.effects, &mut scratch);
    }
    let mut cx = BuildCx::with_parent(s.store, s.states, s.bindings, parent);
    build(&mut cx);
}

/// Apply what the handlers asked since the last pass.
fn apply(state: &Rc<State>, s: &mut StructureCx<'_>) {
    let what = state.pending.replace(0);
    let Some(nodes) = state.nodes.get() else {
        return;
    };
    if what & pending::DATA != 0 {
        let model = state.work_out();
        let mark = s.store.plot(nodes.plot).and_then(|p| p.mark.clone());
        s.store.set_plot(
            nodes.plot,
            viso_ui::Plot {
                mark,
                ..model.plot.clone()
            },
        );
        *state.model.borrow_mut() = model;
        rebuild(s, nodes.y_axis, |cx| build_y_labels(state, cx));
        rebuild(s, nodes.x_axis, |cx| build_x_labels(state, cx));
        if let Some(legend) = nodes.legend {
            if what & pending::LEGEND != 0 {
                rebuild(s, legend, |cx| build_legend(state, cx));
            } else {
                let hidden = state.hidden.borrow();
                let items: Vec<NodeId> = s.store.arena().children(legend).collect();
                for (i, item) in items.into_iter().enumerate() {
                    let shown = !hidden.get(i).copied().unwrap_or(false);
                    s.store.set_opacity(item, if shown { 1.0 } else { 0.4 });
                    s.store
                        .set_semantic_state(item, SemanticState::checked(shown));
                }
            }
        }
    }
    if what & (pending::HOVER | pending::DATA) != 0 {
        show_hover(state, s, nodes);
    }
    s.bindings.collect(s.store, s.states);
}

/// The mark over the plot and the readout for what the pointer or the
/// keyboard points at.
fn show_hover(state: &Rc<State>, s: &mut StructureCx<'_>, nodes: Nodes) {
    let style = state.style;
    let model = state.model.borrow();
    let series = state.series.borrow();
    let Some(hover) = state.hover.get() else {
        s.store.set_plot_mark(nodes.plot, None);
        s.store.set_hidden(nodes.tooltip, true);
        return;
    };
    let value = &*state.value_format;
    let mut mark = PlotMark {
        radius: style.dot_radius,
        ring: style.surface,
        color: style.baseline,
        ..PlotMark::default()
    };
    // The readout's title, its rows (color, value, name), and where it is.
    let (title, rows, at) = match hover {
        Hover::Station(k) => {
            if model.kind == ChartKind::Bar {
                mark.band = Some(model.band(k));
                mark.color = style.hover;
            } else {
                mark.line = model.stations.get(k).copied();
            }
            let mut rows = Vec::new();
            for column in &model.columns {
                if let Some(Some((v, p))) = column.at.get(k) {
                    let color = Model::color(&series, &style, column.series);
                    if model.kind != ChartKind::Bar {
                        mark.dots.push((*p, color));
                    }
                    rows.push((color, value(*v), series[column.series].name.clone()));
                }
            }
            let x = model.stations.get(k).copied().unwrap_or(0.0);
            let title = model.station_labels.get(k).cloned().unwrap_or_default();
            (title, rows, (x, None))
        }
        Hover::Dot(c, k) => {
            let Some((i, points)) = model.scatter.get(c) else {
                return;
            };
            let Some(&(x, y, p)) = points.get(k) else {
                return;
            };
            let color = Model::color(&series, &style, *i);
            mark.dots.push((p, color));
            mark.radius = style.dot_radius + 1.5;
            let text = format!("{}, {}", (state.x_format)(x), value(y));
            (
                series[*i].name.clone(),
                vec![(color, text, String::new())],
                (p.x, Some(p.y)),
            )
        }
    };
    s.store.set_plot_mark(nodes.plot, Some(mark));
    let spoken = rows
        .iter()
        .map(|(_, v, name)| {
            if name.is_empty() {
                v.clone()
            } else {
                format!("{name} {v}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    s.store.set_semantics(
        nodes.tooltip,
        Semantics::role(Role::Status)
            .with_live(LiveRegion::Polite)
            .with_label(format!("{title}: {spoken}")),
    );
    let (x, y) = at;
    let near = |pct: f32| Some(Span { fixed: 12.0, pct });
    let (start, end) = if x <= 0.5 {
        (near(x), None)
    } else {
        (None, near(1.0 - x))
    };
    let (top, bottom) = match y {
        Some(y) if y > 0.5 => (near(1.0 - y), None),
        Some(y) => (None, near(y)),
        None => (Some(Span::dp(4.0)), None),
    };
    s.store.set_placement(
        nodes.tooltip,
        Placement::Absolute(AbsoluteInsets {
            top,
            end,
            bottom,
            start,
        }),
    );
    let kind = state.kind;
    drop(series);
    drop(model);
    rebuild(s, nodes.tooltip, |cx| {
        label(title)
            .font_size(style.font_size)
            .color(style.text_secondary)
            .build(cx);
        for (color, v, name) in rows {
            cx.flex(
                FlexStyle {
                    gap: 6.0,
                    align: Align::Center,
                    ..flex(Axis::Row, FIT)
                },
                |cx| {
                    key_mark(state, cx, color, kind != ChartKind::Scatter);
                    label(v)
                        .font_size(style.font_size)
                        .color(style.text)
                        .weight(FontWeight(600))
                        .build(cx);
                    if !name.is_empty() {
                        label(name)
                            .font_size(style.font_size)
                            .color(style.text_secondary)
                            .build(cx);
                    }
                },
            );
        }
    });
    s.store.set_hidden(nodes.tooltip, false);
}

impl Model {
    /// An empty model of `kind`, before its data is worked out.
    fn default_for(kind: ChartKind) -> Model {
        Model {
            kind,
            categorical: false,
            stations: Vec::new(),
            station_labels: Vec::new(),
            y: crate::scale::nice(0.0, 1.0, 5),
            y_labels: Vec::new(),
            x_labels: Vec::new(),
            columns: Vec::new(),
            scatter: Vec::new(),
            plot: viso_ui::Plot::default(),
        }
    }
}
