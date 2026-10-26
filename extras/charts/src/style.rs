//! How a chart looks: its series colors, its ink and its marks.

use viso_math::Srgb;
use viso_ui::{Length, Rgba, Sizing};

fn hex(v: u32) -> Rgba {
    Srgb::from_rgba32((v << 8) | 0xff).into_linear_straight()
}

/// How a chart looks. Logical points.
///
/// The series colors are eight hues in a fixed order, chosen so neighbours
/// stay apart under the common color-vision deficiencies; a series keeps
/// its slot's color whichever others are shown. Text never takes a series
/// color: a mark beside it does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChartStyle {
    /// The chart's own size within its parent.
    pub size: Sizing,
    /// The series colors, in order.
    pub palette: [Rgba; 8],
    /// What the chart sits on; dots are ringed with it and touching bars
    /// parted by it.
    pub surface: Rgba,
    /// Values, and the rest of the text.
    pub text: Rgba,
    pub text_secondary: Rgba,
    /// Axis labels.
    pub muted: Rgba,
    /// Gridlines, and the line at zero.
    pub grid: Rgba,
    pub baseline: Rgba,
    /// What shows behind the band under the pointer.
    pub hover: Rgba,
    /// The readout's box.
    pub tooltip: Rgba,
    pub tooltip_border: Rgba,
    pub font_size: f32,
    /// A line's width, and how strongly an area under it is washed.
    pub line_width: f32,
    pub area_alpha: f32,
    /// A bar's rounded value end, its widest, and the gap between touching
    /// bars and stacked segments.
    pub bar_radius: f32,
    pub bar_width: f32,
    pub gap: f32,
    /// A dot's radius.
    pub dot_radius: f32,
}

impl ChartStyle {
    /// On a light surface.
    pub fn light() -> Self {
        ChartStyle {
            size: Sizing {
                width: Length::fill(),
                height: Length::Fixed(240.0),
            },
            palette: [
                hex(0x2a78d6),
                hex(0xeb6834),
                hex(0x1baf7a),
                hex(0xeda100),
                hex(0xe87ba4),
                hex(0x008300),
                hex(0x6250d6),
                hex(0xe34948),
            ],
            surface: hex(0xfcfcfb),
            text: hex(0x0b0b0b),
            text_secondary: hex(0x52514e),
            muted: hex(0x898781),
            grid: hex(0xe1e0d9),
            baseline: hex(0xc3c2b7),
            hover: Rgba::new(0.0, 0.0, 0.0, 0.04),
            tooltip: hex(0xffffff),
            tooltip_border: Rgba::new(0.0, 0.0, 0.0, 0.1),
            font_size: 12.0,
            line_width: 2.0,
            area_alpha: 0.1,
            bar_radius: 4.0,
            bar_width: 24.0,
            gap: 2.0,
            dot_radius: 4.0,
        }
    }

    /// On a dark surface: the same hues stepped for it.
    pub fn dark() -> Self {
        ChartStyle {
            palette: [
                hex(0x3987e5),
                hex(0xd95926),
                hex(0x199e70),
                hex(0xc98500),
                hex(0xd55181),
                hex(0x008300),
                hex(0x9085e9),
                hex(0xe66767),
            ],
            surface: hex(0x1a1a19),
            text: hex(0xf0efec),
            text_secondary: hex(0xc3c2b7),
            muted: hex(0x898781),
            grid: hex(0x2c2c2a),
            baseline: hex(0x383835),
            hover: Rgba::new(1.0, 1.0, 1.0, 0.05),
            tooltip: hex(0x262624),
            tooltip_border: Rgba::new(1.0, 1.0, 1.0, 0.1),
            ..ChartStyle::light()
        }
    }

    /// Series `index`'s color: its slot's, the slots taken in turn past the
    /// eighth.
    pub fn color(&self, index: usize) -> Rgba {
        self.palette[index % self.palette.len()]
    }
}

impl Default for ChartStyle {
    fn default() -> Self {
        ChartStyle::light()
    }
}
