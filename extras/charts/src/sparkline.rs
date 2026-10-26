//! The [`Sparkline`]: a word-sized line of a series' trend, with no axes.

use std::sync::Arc;

use viso_ui::{
    BuildCx, Component, Length, Plot, PlotGrid, PlotLayer, Point, Rgba, Role, Semantics, Sizing,
};

use crate::scale::format_value;
use crate::style::ChartStyle;

/// A small line of `values` over their own range, the last one dotted.
/// Construct one with [`sparkline`].
///
/// To an assistive technology it is an image named by its label and what
/// it shows: where the values start and end and their lowest and highest.
pub struct Sparkline {
    label: String,
    values: Vec<f64>,
    color: Option<Rgba>,
    area: bool,
    size: Sizing,
    style: ChartStyle,
}

/// A sparkline named `label` of `values`.
pub fn sparkline(label: impl Into<String>, values: impl IntoIterator<Item = f64>) -> Sparkline {
    Sparkline {
        label: label.into(),
        values: values.into_iter().collect(),
        color: None,
        area: false,
        size: Sizing {
            width: Length::Fixed(96.0),
            height: Length::Fixed(24.0),
        },
        style: ChartStyle::light(),
    }
}

impl Sparkline {
    /// Draw it in `color` in place of the first series color.
    pub fn color(mut self, color: Rgba) -> Self {
        self.color = Some(color);
        self
    }

    /// Wash the region under the line.
    pub fn area(mut self) -> Self {
        self.area = true;
        self
    }

    /// Set its size within its parent.
    pub fn size(mut self, size: Sizing) -> Self {
        self.size = size;
        self
    }

    /// Take its colors and widths from `style`.
    pub fn style(mut self, style: ChartStyle) -> Self {
        self.style = style;
        self
    }

    fn plot(&self) -> Plot {
        let style = &self.style;
        let color = self.color.unwrap_or(style.palette[0]);
        let finite = self.values.iter().copied().filter(|v| v.is_finite());
        let lo = finite.clone().fold(f64::INFINITY, f64::min);
        let hi = finite.fold(f64::NEG_INFINITY, f64::max);
        let n = self.values.len();
        // Room above and below for the line's width and the end dot.
        let (pad, span) = (0.12, if hi > lo { hi - lo } else { 1.0 });
        let uy = |v: f64| -> f32 {
            if v.is_finite() {
                (pad + (1.0 - 2.0 * pad) * (v - lo) / span) as f32
            } else {
                f32::NAN
            }
        };
        let points: Arc<[Point]> = self
            .values
            .iter()
            .enumerate()
            .map(|(i, &v)| Point::new(i as f32 / (n.max(2) - 1) as f32, uy(v)))
            .collect();
        let width = (style.line_width * 0.75).max(1.0);
        let mut layers = Vec::with_capacity(2);
        if self.area {
            layers.push(PlotLayer::Area {
                top: Arc::clone(&points),
                bottom: None,
                base: 0.0,
                fill: Rgba {
                    a: style.area_alpha * 1.5,
                    ..color
                },
                line: Some((color, width)),
                smooth: false,
            });
        } else {
            layers.push(PlotLayer::Line {
                points: Arc::clone(&points),
                color,
                width,
                smooth: false,
            });
        }
        if let Some(&end) = points.iter().rev().find(|p| p.y.is_finite()) {
            layers.push(PlotLayer::Dots {
                points: Arc::from([end]),
                radius: 2.5,
                color,
                ring: None,
            });
        }
        Plot {
            grid: PlotGrid::default(),
            layers: layers.into(),
            mark: None,
        }
    }

    /// What it says to an assistive technology after its label.
    fn summary(&self) -> String {
        let finite: Vec<f64> = self
            .values
            .iter()
            .copied()
            .filter(|v| v.is_finite())
            .collect();
        let (Some(first), Some(last)) = (finite.first(), finite.last()) else {
            return String::new();
        };
        let lo = finite.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = finite.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        format!(
            "{} → {} ({} – {})",
            format_value(*first),
            format_value(*last),
            format_value(lo),
            format_value(hi)
        )
    }
}

impl Component for Sparkline {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let plot = cx.plot(self.size, self.plot());
        let summary = self.summary();
        let label = if summary.is_empty() {
            self.label.clone()
        } else {
            format!("{}: {summary}", self.label)
        };
        cx.semantics(plot, Semantics::role(Role::Image).with_label(label));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_runs_over_its_own_range_and_dots_its_last_value() {
        let line = sparkline("Visits", [3.0, 7.0, f64::NAN, 5.0]);
        let plot = line.plot();
        let PlotLayer::Line { points, .. } = &plot.layers[0] else {
            unreachable!()
        };
        assert_eq!(points[0].y, 0.12, "the lowest at the bottom, with room");
        assert!((points[1].y - 0.88).abs() < 1e-6);
        assert!(points[2].y.is_nan(), "a gap");
        let PlotLayer::Dots { points, .. } = &plot.layers[1] else {
            unreachable!()
        };
        assert_eq!(points[0].x, 1.0);
        assert_eq!(line.summary(), "3 → 5 (3 – 7)");
    }
}
