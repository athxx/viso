//! Charts: line, area, bar and scatter charts of named series, and
//! sparklines.
//!
//! A chart's plot is one node drawn by the UI's plot content: its series
//! are worked out once into fractions of the plot, and the plot maps them
//! into whatever box layout gives it, so a resize repaints it without
//! rebuilding anything, and a line of a hundred thousand points draws as
//! many as its box has pixel columns can show. Round ticks label the `y`
//! axis (compact from a thousand up: `2K`, `1.5M`), categories or round
//! ticks the `x` axis; gridlines are hairlines, recessive, with a stronger
//! line at zero.
//!
//! Series take eight hues in a fixed order, chosen so neighbours stay
//! apart under the common color-vision deficiencies (light and dark
//! steps, [`ChartStyle::light`] and [`ChartStyle::dark`]); a series keeps
//! its color when another is hidden. Two series or more get a legend,
//! each entry a check box that hides or shows its series. Lines are 2
//! points, round-joined; areas a wash of their line's color; bars at most
//! 24 points wide with a rounded value end, 2 points apart; dots ringed in
//! the surface color.
//!
//! The pointer reads the chart: on a line or an area a crosshair at the
//! nearest place along `x` reads every series there, on bars the band
//! under it, on a scatter the nearest dot within 24 points; a finger's tap
//! does the same. The chart is one tab stop: Left and Right (mirrored read
//! right to left), Home and End move the readout, Enter selects, Escape
//! hides it. The readout is a polite live region, so an assistive
//! technology reads what it shows as it moves.
//!
//! ```
//! use viso_charts::{bar_chart, line_chart, series, sparkline};
//! use viso_ui::{BindingTable, BuildCx, Component, NodeStore, SemanticProjector, StateStore, TextEdits, VirtualLists};
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! line_chart(
//!     "Revenue",
//!     vec![series("2025", [12.0, 15.0, 14.0, 19.0]), series("2026", [14.0, 18.0, 21.0, 24.0])],
//! )
//! .categories(["Q1", "Q2", "Q3", "Q4"])
//! .smooth()
//! .build(&mut cx);
//! bar_chart("Signups", vec![series("Web", [3.0, 5.0]), series("App", [2.0, 4.0])])
//!     .categories(["May", "June"])
//!     .stacked()
//!     .build(&mut cx);
//! sparkline("Latency", [41.0, 38.0, 44.0, 52.0, 47.0]).build(&mut cx);
//! ```

mod chart;
mod model;
mod scale;
mod series;
mod sparkline;
mod style;

pub use chart::{
    Chart, ChartHandle, ChartHandleSlot, ChartPoint, area_chart, bar_chart, line_chart,
    scatter_chart,
};
pub use model::ChartKind;
pub use scale::{format_tick, format_value};
pub use series::{Series, series, series_xy};
pub use sparkline::{Sparkline, sparkline};
pub use style::ChartStyle;
