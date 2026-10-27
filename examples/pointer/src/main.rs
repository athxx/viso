//! The mouse beyond clicks: an image cursor and pointer lock.
//!
//! The left panel shows a cursor the app draws itself — a ring with a dot,
//! 64 pixels at two pixels to a point — registered once and named like any
//! system shape. The middle panel shows the system's crosshair. Pressing the
//! right panel locks the pointer to it: the cursor hides and stays put, and
//! the mouse's motion arrives as deltas the panel adds up, until Escape or
//! the window losing focus lets it go.

use viso::cursor::{CursorImage, CustomCursor};
use viso::prelude::*;
use viso::ui::{FontSize, PointerPhase, TextStyle};

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
                size: Sizing::fill(),
                style: BoxStyle::solid(rgb(0.12, 0.13, 0.16)),
                ..Default::default()
            },
            |cx| {
                let left = panel(cx, "Image cursor", rgb(0.20, 0.30, 0.50));
                cx.cursor(left, CursorIcon::Custom(ring));
                let middle = panel(cx, "System crosshair", rgb(0.22, 0.24, 0.28));
                cx.cursor(middle, CursorIcon::Crosshair);
                lock_panel(cx);
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
            size: Sizing::fill(),
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

/// A panel that locks the pointer when pressed and shows how far the mouse
/// moved while locked.
fn lock_panel(cx: &mut BuildCx<'_>) {
    let mut status = None;
    let panel = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            gap: 8.0,
            align: Align::Center,
            justify: Justify::Center,
            size: Sizing::fill(),
            style: BoxStyle::solid(rgb(0.36, 0.22, 0.30)).with_radius(12.0),
            ..Default::default()
        },
        |cx| {
            label("Pointer lock")
                .font_size(20.0)
                .color(rgb(0.93, 0.94, 0.97))
                .build(cx);
            let line = cx.leaf(LeafStyle {
                size: Sizing::fixed(260.0, 20.0),
                style: BoxStyle::NONE,
            });
            status = Some(
                cx.text_request(line, status_text("Press to lock".to_owned()))
                    .id(),
            );
        },
    );
    let status = status.expect("the status line was built");
    let id = panel.id();
    cx.on_pointer(panel, move |cx| {
        if cx.pointer().is_some_and(|p| p.phase == PointerPhase::Down) {
            cx.lock_pointer(id);
        }
    });
    // How far the mouse moved since the lock engaged.
    let mut moved = [0.0f32; 2];
    cx.on_key(panel, move |cx| {
        let text = if let Some(delta) = cx.pointer_motion() {
            moved[0] += delta.x;
            moved[1] += delta.y;
            viso::eprintln!(
                "motion {} {} total {} {}",
                delta.x,
                delta.y,
                moved[0],
                moved[1]
            );
            format!("Moved {:.0}, {:.0} · Esc releases", moved[0], moved[1])
        } else if let Some(locked) = cx.pointer_lock_change() {
            viso::eprintln!("pointer lock {locked}");
            moved = [0.0; 2];
            if locked {
                "Locked · move the mouse".to_owned()
            } else {
                "Press to lock".to_owned()
            }
        } else {
            return;
        };
        cx.set_text(status, status_text(text));
    });
}

/// The lock panel's status line showing `text`.
fn status_text(text: String) -> TextRequest {
    TextRequest {
        text,
        font_size: FontSize::Fixed(14.0),
        color: rgb(0.80, 0.82, 0.88),
        soft_wrap: false,
        locale: None,
        style: TextStyle::default(),
        spans: Vec::new(),
    }
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
