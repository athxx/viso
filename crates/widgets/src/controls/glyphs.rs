//! The small drawn glyphs controls show: checks, chevrons, a radio's dot.

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

/// A radio item's dot: a filled circle.
pub(crate) fn dot(cx: &mut BuildCx<'_>, ink: Rgba) -> Handle {
    let leaf = cx.leaf(LeafStyle {
        size: Size::fixed(GLYPH, GLYPH),
        style: BoxStyle::NONE,
    });
    let (c, r) = (GLYPH * 0.5, 3.5);
    // A circle as four cubic quarters.
    let k = 0.552_284_8 * r;
    let cmds = vec![
        PathCmd::MoveTo(Point::new(c + r, c)),
        PathCmd::CubicTo(
            Point::new(c + r, c + k),
            Point::new(c + k, c + r),
            Point::new(c, c + r),
        ),
        PathCmd::CubicTo(
            Point::new(c - k, c + r),
            Point::new(c - r, c + k),
            Point::new(c - r, c),
        ),
        PathCmd::CubicTo(
            Point::new(c - r, c - k),
            Point::new(c - k, c - r),
            Point::new(c, c - r),
        ),
        PathCmd::CubicTo(
            Point::new(c + k, c - r),
            Point::new(c + r, c - k),
            Point::new(c + r, c),
        ),
        PathCmd::Close,
    ];
    cx.path(leaf, cmds, Some(ink), None, Vec2::new(GLYPH, GLYPH))
}
