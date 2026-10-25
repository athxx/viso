//! The mouse beyond clicks: an image cursor.
//!
//! The left panel shows a cursor the app draws itself — a ring with a dot,
//! 64 pixels at two pixels to a point — registered once and named like any
//! system shape. The right panel shows the system's crosshair.

use viso::cursor::{CursorImage, CustomCursor};
use viso::prelude::*;
use viso::render::Rgba;
use viso::ui::{Align, Axis, BoxStyle, Component, Inset, Justify, Size};

struct Pointer;

impl Application for Pointer {
    fn new(_cx: &mut AppCx) -> Self {
        Pointer
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let ring = CustomCursor::new(ring_image());
        cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 16.0,
                padding: Inset::all(16.0),
                size: Size::fill(),
                style: BoxStyle::solid(rgb(0.12, 0.13, 0.16)),
                ..Default::default()
            },
            |cx| {
                let left = panel(cx, "Image cursor", rgb(0.20, 0.30, 0.50));
                cx.cursor(left, CursorIcon::Custom(ring));
                let right = panel(cx, "System crosshair", rgb(0.22, 0.24, 0.28));
                cx.cursor(right, CursorIcon::Crosshair);
            },
        );
    }
}

/// A panel filling its share of the row, `title` centred in it.
fn panel(cx: &mut BuildCx<'_>, title: &str, fill: Rgba) -> viso::ui::Handle {
    cx.flex(
        FlexStyle {
            align: Align::Center,
            justify: Justify::Center,
            size: Size::fill(),
            style: BoxStyle::solid(fill).with_radius(12.0),
            ..Default::default()
        },
        |cx| {
            label(title)
                .font_size(20.0)
                .color(rgb(0.93, 0.94, 0.97))
                .build(cx);
        },
    )
}

/// A 64-pixel ring with a dot in its middle, drawn two pixels to a point:
/// white with a dark rim so it shows on any background, pointing from its
/// centre.
fn ring_image() -> CursorImage {
    const SIDE: u32 = 64;
    let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    let c = SIDE as f32 / 2.0;
    for y in 0..SIDE {
        for x in 0..SIDE {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            // Coverage of the ring (radius 22–28) and the dot (radius 4).
            let ring = (3.0 - (d - 25.0).abs()).clamp(0.0, 1.0);
            let dot = (4.0 - d).clamp(0.0, 1.0);
            let rim = (4.5 - (d - 25.0).abs())
                .clamp(0.0, 1.0)
                .max((5.5 - d).clamp(0.0, 1.0));
            let white = ring.max(dot);
            let a = rim.max(white);
            let v = if a > 0.0 {
                (white / a * 255.0) as u8
            } else {
                0
            };
            rgba.extend_from_slice(&[v, v, v, (a * 255.0) as u8]);
        }
    }
    CursorImage::new(SIDE, SIDE, rgba, [SIDE / 2, SIDE / 2])
        .expect("a 64-pixel image with its hotspot inside")
        .with_scale(2.0)
}

fn rgb(r: f32, g: f32, b: f32) -> Rgba {
    Rgba { r, g, b, a: 1.0 }
}

fn main() {
    viso::run::<Pointer>();
}
