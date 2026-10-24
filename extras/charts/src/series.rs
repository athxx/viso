//! A chart's data: named series of values or of points.

use viso_ui::Rgba;

/// A named run of data a chart draws. Construct one with [`series`] (values
/// at the chart's categories, or at `0, 1, 2, …`) or [`series_xy`] (points).
/// A value that is not a number leaves a gap.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    pub(crate) name: String,
    pub(crate) points: Vec<(f64, f64)>,
    pub(crate) color: Option<Rgba>,
}

/// A series named `name` of `values`, one per category in order.
pub fn series(name: impl Into<String>, values: impl IntoIterator<Item = f64>) -> Series {
    Series {
        name: name.into(),
        points: values
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i as f64, v))
            .collect(),
        color: None,
    }
}

/// A series named `name` of `(x, y)` points; a line joins them in order of
/// `x`.
pub fn series_xy(name: impl Into<String>, points: impl IntoIterator<Item = (f64, f64)>) -> Series {
    let mut points: Vec<(f64, f64)> = points.into_iter().collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    Series {
        name: name.into(),
        points,
        color: None,
    }
}

impl Series {
    /// Draw it in `color` in place of its slot's.
    pub fn color(mut self, color: Rgba) -> Self {
        self.color = Some(color);
        self
    }

    /// Its name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Its points: `(index, value)` for a series of values.
    pub fn points(&self) -> &[(f64, f64)] {
        &self.points
    }
}
