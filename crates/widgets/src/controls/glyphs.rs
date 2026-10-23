//! The small drawn glyphs controls show: checks, chevrons, a radio's dot, a
//! magnifier and a cross.

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, FlexStyle, Handle, Justify, LeafStyle, LineCap, LineJoin,
    PathCmd, Point, Rgba, Size, Stroke, Vec2,
};

/// The side of a glyph's box.
pub(crate) const GLYPH: f32 = 16.0;

/// A box of a glyph's side, `inner` centred in it.
pub(crate) fn glyph_slot(cx: &mut BuildCx<'_>, inner: impl FnOnce(&mut BuildCx<'_>)) -> Handle {
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            align: Align::Center,
            justify: Justify::Center,
            size: Size::fixed(GLYPH, GLYPH),
            ..FlexStyle::default()
        },
        inner,
    )
}

/// A stroked glyph of `cmds` in a box of the glyph's side.
fn stroked(cx: &mut BuildCx<'_>, cmds: Vec<PathCmd>, ink: Rgba, width: f32) -> Handle {
    let leaf = cx.leaf(LeafStyle {
        size: Size::fixed(GLYPH, GLYPH),
        style: BoxStyle::NONE,
    });
    let stroke = Stroke {
        cap: LineCap::Round,
        join: LineJoin::Round,
        ..Stroke::new(width, ink)
    };
    cx.path(leaf, cmds, None, Some(stroke), Vec2::new(GLYPH, GLYPH))
}

/// A check mark.
pub(crate) fn check(cx: &mut BuildCx<'_>, ink: Rgba) -> Handle {
    let cmds = vec![
        PathCmd::MoveTo(Point::new(3.5, 8.5)),
        PathCmd::LineTo(Point::new(6.5, 11.5)),
        PathCmd::LineTo(Point::new(12.5, 4.5)),
    ];
    stroked(cx, cmds, ink, 1.8)
}

/// A chevron pointing toward the end of lines: a submenu's.
pub(crate) fn chevron_end(cx: &mut BuildCx<'_>, ink: Rgba) -> Handle {
    let cmds = vec![
        PathCmd::MoveTo(Point::new(6.0, 4.0)),
        PathCmd::LineTo(Point::new(10.0, 8.0)),
        PathCmd::LineTo(Point::new(6.0, 12.0)),
    ];
    stroked(cx, cmds, ink, 1.5)
}

/// A chevron pointing down: a list that opens below.
pub(crate) fn chevron_down(cx: &mut BuildCx<'_>, ink: Rgba) -> Handle {
    let cmds = vec![
        PathCmd::MoveTo(Point::new(4.0, 6.0)),
        PathCmd::LineTo(Point::new(8.0, 10.0)),
        PathCmd::LineTo(Point::new(12.0, 6.0)),
    ];
    stroked(cx, cmds, ink, 1.5)
}

/// Two chevrons, up over down: a list that opens over its button.
pub(crate) fn chevrons_up_down(cx: &mut BuildCx<'_>, ink: Rgba) -> Handle {
    let cmds = vec![
        PathCmd::MoveTo(Point::new(5.0, 6.0)),
        PathCmd::LineTo(Point::new(8.0, 3.0)),
        PathCmd::LineTo(Point::new(11.0, 6.0)),
        PathCmd::MoveTo(Point::new(5.0, 10.0)),
        PathCmd::LineTo(Point::new(8.0, 13.0)),
        PathCmd::LineTo(Point::new(11.0, 10.0)),
    ];
    stroked(cx, cmds, ink, 1.5)
}

/// A circle of radius `r` about `(x, y)`, as four cubic quarters.
fn circle(x: f32, y: f32, r: f32) -> [PathCmd; 6] {
    let k = 0.552_284_8 * r;
    [
        PathCmd::MoveTo(Point::new(x + r, y)),
        PathCmd::CubicTo(
            Point::new(x + r, y + k),
            Point::new(x + k, y + r),
            Point::new(x, y + r),
        ),
        PathCmd::CubicTo(
            Point::new(x - k, y + r),
            Point::new(x - r, y + k),
            Point::new(x - r, y),
        ),
        PathCmd::CubicTo(
            Point::new(x - r, y - k),
            Point::new(x - k, y - r),
            Point::new(x, y - r),
        ),
        PathCmd::CubicTo(
            Point::new(x + k, y - r),
            Point::new(x + r, y - k),
            Point::new(x + r, y),
        ),
        PathCmd::Close,
    ]
}

/// A radio item's dot: a filled circle.
pub(crate) fn dot(cx: &mut BuildCx<'_>, ink: Rgba) -> Handle {
    let leaf = cx.leaf(LeafStyle {
        size: Size::fixed(GLYPH, GLYPH),
        style: BoxStyle::NONE,
    });
    let c = GLYPH * 0.5;
    let cmds = circle(c, c, 3.5).to_vec();
    cx.path(leaf, cmds, Some(ink), None, Vec2::new(GLYPH, GLYPH))
}

/// A magnifier: a lens with its handle toward the bottom end.
pub(crate) fn magnifier(cx: &mut BuildCx<'_>, ink: Rgba) -> Handle {
    let mut cmds = circle(7.0, 7.0, 4.5).to_vec();
    cmds.extend([
        PathCmd::MoveTo(Point::new(10.4, 10.4)),
        PathCmd::LineTo(Point::new(13.5, 13.5)),
    ]);
    stroked(cx, cmds, ink, 1.6)
}

/// A cross on a filled disc: what clears a field.
pub(crate) fn clear(cx: &mut BuildCx<'_>, disc: Rgba, ink: Rgba) -> Handle {
    cx.flex(
        FlexStyle {
            axis: Axis::Row,
            align: Align::Center,
            justify: Justify::Center,
            size: Size::fixed(GLYPH, GLYPH),
            style: BoxStyle::solid(disc).with_radius(GLYPH * 0.5),
            ..FlexStyle::default()
        },
        |cx| {
            let (a, b) = (GLYPH * 0.5 - 2.75, GLYPH * 0.5 + 2.75);
            let cmds = vec![
                PathCmd::MoveTo(Point::new(a, a)),
                PathCmd::LineTo(Point::new(b, b)),
                PathCmd::MoveTo(Point::new(b, a)),
                PathCmd::LineTo(Point::new(a, b)),
            ];
            stroked(cx, cmds, ink, 1.5);
        },
    )
}
