//! `parse_svg` conversion tests: minimal SVG documents whose lowered
//! [`Primitive`] stream, coordinates, and fill/stroke are checked exactly.
//!
//! Colors are asserted in straight-linear space (Viso's `Rgba`), the same
//! conversion the adapter applies: a usvg u8 sRGB channel run through the sRGB
//! transfer function. Pure black/white/primaries at the sRGB extremes map to
//! the linear extremes (0.0/1.0), so those are exact; a mid-tone is checked
//! against the transfer function to nail the pipeline.

use viso_math::srgb_to_linear;
use viso_render::{LineCap, LineJoin, PathCmd, Point, Primitive};
use viso_svg::{SvgScene, parse_svg};

/// The single lowered path of a scene, or a panic if the scene isn't exactly
/// one `Primitive::Path`.
fn only_path(scene: &SvgScene) -> &viso_render::Path {
    assert_eq!(scene.prims.len(), 1, "expected exactly one primitive");
    match &scene.prims[0] {
        Primitive::Path(p) => p,
        other => panic!("expected Primitive::Path, got {other:?}"),
    }
}

#[test]
fn rect_becomes_a_closed_filled_path() {
    // usvg converts a `<rect>` into a path: a move + three lines + close,
    // walking the corners. Fill is solid red at full opacity.
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
        <rect x="10" y="20" width="30" height="40" fill="red"/>
    </svg>"#;
    let scene = parse_svg(svg).expect("valid svg");
    assert_eq!(scene.size, (100.0, 100.0));

    let path = only_path(&scene);

    // Solid red: sRGB (255,0,0) → linear (1,0,0), full alpha.
    let fill = path.fill.expect("rect has a fill");
    assert_eq!((fill.r, fill.g, fill.b, fill.a), (1.0, 0.0, 0.0, 1.0));
    assert!(path.stroke.is_none(), "unstroked rect has no stroke");

    // The outline visits the four corners and closes. usvg may start at any
    // corner and wind either way, so assert the corner *set* the anchors touch
    // plus the closed shape, not a fixed vertex order.
    let mut pts: Vec<(f32, f32)> = path
        .cmds
        .iter()
        .filter_map(|c| match *c {
            PathCmd::MoveTo(p) | PathCmd::LineTo(p) => Some((p.x, p.y)),
            _ => None,
        })
        .collect();
    pts.sort_by(|a, b| a.partial_cmp(b).unwrap());
    pts.dedup();
    assert_eq!(
        pts,
        vec![(10.0, 20.0), (10.0, 60.0), (40.0, 20.0), (40.0, 60.0)]
    );
    assert!(
        matches!(path.cmds.last(), Some(PathCmd::Close)),
        "rect outline is closed"
    );
}

#[test]
fn line_path_carries_fill_and_stroke() {
    // An explicit two-point path with both a fill and a stroke. The stroke
    // attributes map straight across; coordinates are untouched (identity
    // transform).
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50">
        <path d="M 5 5 L 45 25" fill="#00ff00"
              stroke="#0000ff" stroke-width="4"
              stroke-linecap="round" stroke-linejoin="bevel"/>
    </svg>"##;
    let scene = parse_svg(svg).expect("valid svg");
    let path = only_path(&scene);

    assert_eq!(
        path.cmds,
        vec![
            PathCmd::MoveTo(Point::new(5.0, 5.0)),
            PathCmd::LineTo(Point::new(45.0, 25.0)),
        ]
    );

    let fill = path.fill.expect("green fill");
    assert_eq!((fill.r, fill.g, fill.b, fill.a), (0.0, 1.0, 0.0, 1.0));

    let stroke = path.stroke.expect("blue stroke");
    assert_eq!(
        (
            stroke.color.r,
            stroke.color.g,
            stroke.color.b,
            stroke.color.a
        ),
        (0.0, 0.0, 1.0, 1.0)
    );
    assert_eq!(stroke.width, 4.0);
    assert_eq!(stroke.cap, LineCap::Round);
    assert_eq!(stroke.join, LineJoin::Bevel);
}

#[test]
fn transform_and_viewbox_are_baked_into_coordinates() {
    // A `viewBox` scaling the 0..10 user space onto a 100px document (×10) plus
    // a `translate(1,2)` on the group. usvg resolves both into each path's
    // absolute transform; the adapter bakes it into the emitted points. A point
    // at user (3,4) under translate(1,2) then ×10 lands at ((3+1)*10,(4+2)*10)
    // = (40,60).
    let svg =
        br#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 10 10">
        <g transform="translate(1,2)">
            <path d="M 3 4 L 5 6" fill="black"/>
        </g>
    </svg>"#;
    let scene = parse_svg(svg).expect("valid svg");
    assert_eq!(scene.size, (100.0, 100.0));

    let path = only_path(&scene);
    assert_eq!(
        path.cmds,
        vec![
            PathCmd::MoveTo(Point::new(40.0, 60.0)),
            PathCmd::LineTo(Point::new(60.0, 80.0)),
        ]
    );
}

#[test]
fn fill_opacity_scales_the_alpha() {
    // `fill-opacity` multiplies the paint alpha; a solid mid-gray at 50% opacity
    // checks both the sRGB transfer on the channel and the alpha scaling.
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
        <rect x="0" y="0" width="10" height="10" fill="#808080" fill-opacity="0.5"/>
    </svg>"##;
    let scene = parse_svg(svg).expect("valid svg");
    let path = only_path(&scene);

    let fill = path.fill.expect("gray fill");
    let expected = srgb_to_linear(0x80 as f32 / 255.0);
    assert!((fill.r - expected).abs() < 1e-6);
    assert!((fill.g - expected).abs() < 1e-6);
    assert!((fill.b - expected).abs() < 1e-6);
    // 0.5 opacity → alpha byte 128 → 128/255.
    assert!((fill.a - 128.0 / 255.0).abs() < 1e-6);
}

#[test]
fn dashed_stroke_carries_the_pattern() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="50" height="50">
        <path d="M 0 0 L 50 0" fill="none" stroke="black"
              stroke-width="2" stroke-dasharray="6 3" stroke-dashoffset="1"/>
    </svg>"#;
    let scene = parse_svg(svg).expect("valid svg");
    let path = only_path(&scene);

    assert!(path.fill.is_none(), "fill=none leaves no fill");
    let stroke = path.stroke.expect("dashed stroke");
    let dash = stroke.dash.expect("dash pattern present");
    assert_eq!(dash.len, 2);
    assert_eq!(&dash.segments[..2], &[6.0, 3.0]);
    assert_eq!(dash.offset, 1.0);
}

#[test]
fn gradient_only_path_is_skipped() {
    // A path whose only paint is a gradient has no solid color we represent this
    // round, so it produces no primitive rather than a mis-colored one.
    let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
        <defs>
            <linearGradient id="g"><stop offset="0" stop-color="red"/>
                <stop offset="1" stop-color="blue"/></linearGradient>
        </defs>
        <rect x="0" y="0" width="10" height="10" fill="url(#g)"/>
    </svg>"##;
    let scene = parse_svg(svg).expect("valid svg");
    assert!(scene.prims.is_empty(), "gradient-only path is skipped");
}

#[test]
fn invalid_bytes_are_a_parse_error() {
    let err = parse_svg(b"not an svg at all").unwrap_err();
    assert!(err.to_string().contains("invalid SVG"));
}
