//! What a chart shows, worked out from its data: its scales, its axis
//! labels, the plot's layers, and the values at each place along `x` a
//! readout reads.

use std::sync::Arc;

use viso_ui::{Plot, PlotBar, PlotGrid, PlotLayer, Point, Rgba};

use crate::scale::{Ticks, nice, within};
use crate::series::Series;
use crate::style::ChartStyle;

/// What a chart draws its series as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Line,
    Area,
    Bar,
    Scatter,
}

/// What a model is worked out from.
pub(crate) struct Spec<'a> {
    pub kind: ChartKind,
    pub series: &'a [Series],
    pub categories: Option<&'a [String]>,
    pub hidden: &'a [bool],
    pub stacked: bool,
    pub smooth: bool,
    pub y_range: Option<(f64, f64)>,
    pub style: &'a ChartStyle,
    pub max_x_labels: usize,
    pub x_format: &'a dyn Fn(f64) -> String,
    pub y_format: &'a dyn Fn(f64, f64) -> String,
}

/// A label along the `x` axis: centered at `center`, `width` wide, both
/// fractions of the plot's width.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct AxisLabel {
    pub center: f32,
    pub width: f32,
    pub text: String,
}

/// A series shown, and its value at each station: the value read, and
/// where its mark sits in the plot.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Column {
    pub series: usize,
    pub at: Vec<Option<(f64, Point)>>,
}

/// A scatter series shown: its index, and each point's `x`, `y` and place
/// in the plot.
pub(crate) type Scatter = (usize, Vec<(f64, f64, Point)>);

/// A chart's data worked out.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Model {
    pub kind: ChartKind,
    /// Whether `x` is a run of categories (bands) rather than numbers.
    pub categorical: bool,
    /// The places along `x` a readout stops at, where each falls in the
    /// plot, and how each is labelled.
    pub stations: Vec<f32>,
    pub station_labels: Vec<String>,
    pub y: Ticks,
    pub y_labels: Vec<String>,
    pub x_labels: Vec<AxisLabel>,
    /// The series shown, by station; for a scatter, their points.
    pub columns: Vec<Column>,
    pub scatter: Vec<Scatter>,
    pub plot: Plot,
}

impl Model {
    /// The color series `index` is drawn in.
    pub fn color(series: &[Series], style: &ChartStyle, index: usize) -> Rgba {
        series[index].color.unwrap_or_else(|| style.color(index))
    }

    /// The band a category station spans, as fractions of the plot's width.
    pub fn band(&self, station: usize) -> (f32, f32) {
        let n = self.stations.len().max(1) as f32;
        (station as f32 / n, (station + 1) as f32 / n)
    }
}

fn faint(color: Rgba, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..color }
}

pub(crate) fn model(spec: &Spec<'_>) -> Model {
    let style = spec.style;
    let shown: Vec<usize> = (0..spec.series.len())
        .filter(|&i| !spec.hidden.get(i).copied().unwrap_or(false))
        .collect();
    let categorical = spec.categories.is_some() || spec.kind == ChartKind::Bar;
    let stacked = spec.stacked && matches!(spec.kind, ChartKind::Area | ChartKind::Bar);

    // The stations along x, raw.
    let (station_x, x_ticks): (Vec<f64>, Option<Ticks>) = if categorical {
        let n = spec.categories.map(|c| c.len()).unwrap_or_else(|| {
            spec.series
                .iter()
                .map(|s| s.points.len())
                .max()
                .unwrap_or(0)
        });
        ((0..n).map(|i| i as f64).collect(), None)
    } else {
        let mut xs: Vec<f64> = shown
            .iter()
            .flat_map(|&i| spec.series[i].points.iter().map(|p| p.0))
            .filter(|x| x.is_finite())
            .collect();
        xs.sort_by(f64::total_cmp);
        xs.dedup();
        let (lo, hi) = (
            xs.first().copied().unwrap_or(0.0),
            xs.last().copied().unwrap_or(1.0),
        );
        let ticks = if spec.kind == ChartKind::Scatter {
            nice(lo, hi, 5)
        } else {
            within(lo, hi, 5)
        };
        (xs, Some(ticks))
    };
    let n = station_x.len();
    let ux = |x: f64| -> f32 {
        match &x_ticks {
            Some(t) => t.unit(x),
            None => ((x + 0.5) / n.max(1) as f64) as f32,
        }
    };
    let stations: Vec<f32> = station_x.iter().map(|&x| ux(x)).collect();

    // Each shown series' value at each station.
    let value_at = |series: &Series, k: usize| -> Option<f64> {
        let v = if categorical {
            series.points.get(k).map(|p| p.1)
        } else {
            let x = station_x[k];
            series
                .points
                .binary_search_by(|p| p.0.total_cmp(&x))
                .ok()
                .map(|i| series.points[i].1)
        };
        v.filter(|v| v.is_finite())
    };
    let raw: Vec<Vec<Option<f64>>> = shown
        .iter()
        .map(|&i| (0..n).map(|k| value_at(&spec.series[i], k)).collect())
        .collect();
    // Stacked, where each series' mark ends and starts.
    let mut tops = vec![vec![None; n]; shown.len()];
    let mut bases = vec![vec![0.0; n]; shown.len()];
    if stacked {
        let (mut up, mut down) = (vec![0.0; n], vec![0.0; n]);
        for (j, values) in raw.iter().enumerate() {
            for k in 0..n {
                let v = values[k].unwrap_or(0.0);
                let under = if v < 0.0 && spec.kind == ChartKind::Bar {
                    &mut down
                } else {
                    &mut up
                };
                bases[j][k] = under[k];
                under[k] += v;
                tops[j][k] = values[k].map(|_| under[k]);
            }
        }
    } else {
        tops = raw.clone();
    }

    // The y domain.
    let mut ys: Vec<f64> = if spec.kind == ChartKind::Scatter {
        shown
            .iter()
            .flat_map(|&i| spec.series[i].points.iter().map(|p| p.1))
            .filter(|y| y.is_finite())
            .collect()
    } else {
        tops.iter().flatten().flatten().copied().collect()
    };
    if stacked {
        ys.extend(bases.iter().flatten().copied());
    }
    if matches!(spec.kind, ChartKind::Bar | ChartKind::Area) {
        ys.push(0.0);
    }
    let (lo, hi) = spec.y_range.unwrap_or_else(|| {
        let lo = ys.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if lo.is_finite() { (lo, hi) } else { (0.0, 1.0) }
    });
    let y = nice(lo, hi, 5);
    let uy = |v: f64| y.unit(v);
    let base = uy(0.0f64.clamp(y.lo, y.hi));

    // The layers.
    let mut layers = Vec::new();
    let mut columns = Vec::new();
    let mut scatter = Vec::new();
    let mut bars = Vec::new();
    let count = shown.len();
    for (j, &i) in shown.iter().enumerate() {
        let series = &spec.series[i];
        let color = Model::color(spec.series, style, i);
        let at: Vec<Option<(f64, Point)>> = (0..n)
            .map(|k| {
                let v = raw[j][k]?;
                let top = tops[j][k].unwrap_or(v);
                Some((v, Point::new(stations[k], uy(top))))
            })
            .collect();
        match spec.kind {
            ChartKind::Line | ChartKind::Area => {
                let points: Arc<[Point]> = if categorical || stacked {
                    (0..n)
                        .map(|k| Point::new(stations[k], tops[j][k].map_or(f32::NAN, uy)))
                        .collect()
                } else {
                    series
                        .points
                        .iter()
                        .map(|&(x, v)| {
                            Point::new(ux(x), if v.is_finite() { uy(v) } else { f32::NAN })
                        })
                        .collect()
                };
                if spec.kind == ChartKind::Line {
                    layers.push(PlotLayer::Line {
                        points,
                        color,
                        width: style.line_width,
                        smooth: spec.smooth,
                    });
                } else {
                    let bottom = stacked.then(|| {
                        (0..n)
                            .map(|k| Point::new(stations[k], uy(bases[j][k])))
                            .collect::<Arc<[Point]>>()
                    });
                    let alpha = if stacked {
                        style.area_alpha * 2.5
                    } else {
                        style.area_alpha
                    };
                    layers.push(PlotLayer::Area {
                        top: points,
                        bottom,
                        base,
                        fill: faint(color, alpha),
                        line: Some((color, style.line_width)),
                        smooth: spec.smooth,
                    });
                }
            }
            ChartKind::Bar => {
                for k in 0..n {
                    let Some(v) = raw[j][k] else {
                        continue;
                    };
                    let (x0, x1) = (k as f32 / n as f32, (k + 1) as f32 / n as f32);
                    let pad = (x1 - x0) * 0.1;
                    let (y0, y1) = if stacked {
                        (uy(bases[j][k]), uy(bases[j][k] + v))
                    } else {
                        (base, uy(v))
                    };
                    bars.push((
                        k,
                        v,
                        PlotBar {
                            x0: x0 + pad,
                            x1: x1 - pad,
                            y0,
                            y1,
                            color,
                            round: !stacked,
                            slot: if stacked { 0 } else { j as u16 },
                            slots: if stacked { 1 } else { count as u16 },
                        },
                    ));
                }
            }
            ChartKind::Scatter => {
                let points: Vec<(f64, f64, Point)> = series
                    .points
                    .iter()
                    .filter(|p| p.0.is_finite() && p.1.is_finite())
                    .map(|&(x, v)| (x, v, Point::new(ux(x), uy(v))))
                    .collect();
                layers.push(PlotLayer::Dots {
                    points: points.iter().map(|p| p.2).collect(),
                    radius: style.dot_radius,
                    color,
                    ring: Some(style.surface),
                });
                scatter.push((i, points));
            }
        }
        columns.push(Column { series: i, at });
    }
    if spec.kind == ChartKind::Bar {
        if stacked {
            // The outermost segment each way of each band is rounded.
            for k in 0..n {
                for up in [true, false] {
                    let last = bars
                        .iter()
                        .rposition(|(b, v, _)| *b == k && (*v >= 0.0) == up && *v != 0.0);
                    if let Some(last) = last {
                        bars[last].2.round = true;
                    }
                }
            }
        }
        layers.push(PlotLayer::Bars {
            bars: bars.into_iter().map(|b| b.2).collect(),
            base,
            radius: style.bar_radius,
            max_width: style.bar_width,
            gap: style.gap,
        });
    }

    // The axes.
    let y_labels = y
        .values
        .iter()
        .map(|&v| (spec.y_format)(v, y.step))
        .collect();
    let station_labels: Vec<String> = match spec.categories {
        Some(c) => c.to_vec(),
        None => station_x.iter().map(|&x| (spec.x_format)(x)).collect(),
    };
    let x_labels = match &x_ticks {
        None => {
            let every = n.div_ceil(spec.max_x_labels.max(1)).max(1);
            (0..n)
                .step_by(every)
                .map(|k| AxisLabel {
                    center: stations[k],
                    width: every as f32 / n as f32,
                    text: station_labels[k].clone(),
                })
                .collect()
        }
        Some(t) => {
            let width = (t.step / (t.hi - t.lo).max(f64::MIN_POSITIVE)) as f32;
            t.values
                .iter()
                .map(|&v| AxisLabel {
                    center: t.unit(v),
                    width,
                    text: (spec.x_format)(v),
                })
                .collect()
        }
    };
    let grid = PlotGrid {
        rows: y.values.iter().map(|&v| uy(v)).collect(),
        columns: Arc::from([]),
        color: style.grid,
        axis: (y.lo <= 0.0 && y.hi >= 0.0).then_some((base, style.baseline)),
    };
    Model {
        kind: spec.kind,
        categorical,
        stations,
        station_labels,
        y,
        y_labels,
        x_labels,
        columns,
        scatter,
        plot: Plot {
            grid,
            layers: layers.into(),
            mark: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scale::{format_tick, format_value};
    use crate::series::{series, series_xy};

    fn spec<'a>(
        kind: ChartKind,
        series: &'a [Series],
        hidden: &'a [bool],
        stacked: bool,
        style: &'a ChartStyle,
    ) -> Spec<'a> {
        Spec {
            kind,
            series,
            categories: None,
            hidden,
            stacked,
            smooth: false,
            y_range: None,
            style,
            max_x_labels: 12,
            x_format: &format_value,
            y_format: &format_tick,
        }
    }

    #[test]
    fn bars_group_by_band_and_stack_rounding_the_outer_segment() {
        let style = ChartStyle::light();
        let data = [series("a", [3.0, 5.0]), series("b", [2.0, -1.0])];
        let m = model(&spec(ChartKind::Bar, &data, &[], false, &style));
        assert!(m.categorical);
        assert_eq!(m.stations, [0.25, 0.75]);
        assert_eq!((m.y.lo, m.y.hi), (-1.0, 5.0));
        let PlotLayer::Bars { bars, .. } = &m.plot.layers[0] else {
            unreachable!()
        };
        assert_eq!(bars.len(), 4);
        assert_eq!((bars[0].slot, bars[0].slots), (0, 2));
        assert_eq!((bars[2].slot, bars[2].slots), (1, 2));
        assert!(bars.iter().all(|b| b.round));

        let m = model(&spec(ChartKind::Bar, &data, &[], true, &style));
        let PlotLayer::Bars { bars, .. } = &m.plot.layers[0] else {
            unreachable!()
        };
        // Band 0: a (0..3) then b (3..5); band 1: a up to 5, b down to -1.
        assert_eq!((m.y.lo, m.y.hi), (-1.0, 5.0), "the stacks top and bottom");
        assert!(!bars[0].round && bars[2].round, "only the outer segment");
        assert!(bars[1].round && bars[3].round, "each way");
        assert_eq!(m.columns[1].at[0].map(|c| c.0), Some(2.0), "the value read");
    }

    #[test]
    fn a_hidden_series_leaves_the_domain_and_keeps_its_color() {
        let style = ChartStyle::light();
        let data = [series("a", [1.0, 2.0]), series("b", [100.0, 200.0])];
        let m = model(&spec(ChartKind::Line, &data, &[true, false], false, &style));
        assert_eq!(m.columns.len(), 1);
        assert_eq!(m.columns[0].series, 1);
        let PlotLayer::Line { color, .. } = &m.plot.layers[0] else {
            unreachable!()
        };
        assert_eq!(*color, style.color(1));
        assert!(m.y.lo >= 100.0 - 20.0, "{:?}", m.y);
    }

    #[test]
    fn numeric_x_reads_values_at_each_x_any_series_has() {
        let style = ChartStyle::light();
        let data = [
            series_xy("a", [(0.0, 1.0), (10.0, 3.0)]),
            series_xy("b", [(5.0, 2.0), (10.0, 4.0)]),
        ];
        let m = model(&spec(ChartKind::Line, &data, &[], false, &style));
        assert!(!m.categorical);
        assert_eq!(m.stations, [0.0, 0.5, 1.0]);
        assert_eq!(m.columns[0].at[1], None, "a has no value at 5");
        assert_eq!(m.columns[1].at[1].map(|c| c.0), Some(2.0));
        assert_eq!(m.station_labels, ["0", "5", "10"]);
        assert_eq!(m.x_labels.first().map(|l| l.center), Some(0.0));
    }

    #[test]
    fn many_categories_label_every_few() {
        let style = ChartStyle::light();
        let data = [series("a", (0..100).map(f64::from))];
        let m = model(&spec(ChartKind::Bar, &data, &[], false, &style));
        assert_eq!(m.x_labels.len(), 12, "every 9th of 100");
        assert_eq!(m.y_labels.first().map(String::as_str), Some("0"));
    }
}
