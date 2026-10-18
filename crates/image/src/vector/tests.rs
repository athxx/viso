use super::rasterize;

/// `body` inside a `size × size` document drawn at `width × height`.
fn draw(size: (u32, u32), body: &str, width: u32, height: u32) -> Vec<[u8; 4]> {
    let text = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}">{body}</svg>"#,
        size.0, size.1
    );
    let tree = viso_svg::parse(text.as_bytes()).expect("a valid document");
    rasterize(&tree, width, height)
}

/// `body` in a square document drawn at its own size.
fn square(size: u32, body: &str) -> Vec<[u8; 4]> {
    draw((size, size), body, size, size)
}

fn alpha(image: &[[u8; 4]], width: u32, x: u32, y: u32) -> u8 {
    image[(y * width + x) as usize][3]
}

/// Total alpha coverage, in pixels.
fn area(image: &[[u8; 4]]) -> f32 {
    image.iter().map(|px| px[3] as f32 / 255.0).sum()
}

/// The linear value `byte` of an sRGB channel stores as.
fn linear(byte: u8) -> u8 {
    let c = viso_math::Srgb::from_u8(byte, byte, byte, 255).into_linear_straight();
    (c.r * 255.0).round() as u8
}

#[test]
fn a_rect_has_exact_interior_and_edge_coverage() {
    let image = square(
        10,
        r#"<rect x="2.25" y="3" width="5.25" height="2.5" fill="black"/>"#,
    );
    let a = |x, y| alpha(&image, 10, x, y);
    assert_eq!(a(4, 3), 255);
    assert_eq!(a(4, 4), 255);
    assert_eq!(a(2, 3), 191, "a quarter outside");
    assert_eq!(a(7, 4), 128, "half outside");
    assert_eq!(a(4, 5), 128, "half below");
    assert_eq!(a(2, 5), 96, "three eighths at the corner");
    assert_eq!(a(1, 4), 0);
    assert_eq!(a(8, 4), 0);
    assert_eq!(a(4, 2), 0);
    assert_eq!(a(4, 6), 0);
    assert!((area(&image) - 5.25 * 2.5).abs() < 0.02);
}

#[test]
fn shapes_past_the_image_edge_clip_to_it() {
    let image = square(
        8,
        r#"<rect x="-5" y="2" width="30" height="3" fill="black"/>"#,
    );
    for x in 0..8 {
        for y in 0..8 {
            let inside = (2..5).contains(&y);
            assert_eq!(
                alpha(&image, 8, x, y),
                if inside { 255 } else { 0 },
                "({x}, {y})"
            );
        }
    }
}

#[test]
fn a_circle_covers_its_area() {
    let image = square(32, r#"<circle cx="16" cy="16" r="12.3" fill="black"/>"#);
    let expected = core::f32::consts::PI * 12.3 * 12.3;
    assert!(
        (area(&image) - expected).abs() < expected * 0.002,
        "{}",
        area(&image)
    );
    assert_eq!(alpha(&image, 32, 16, 16), 255);
    assert_eq!(alpha(&image, 32, 1, 1), 0);
}

#[test]
fn the_fill_rule_decides_an_overlapping_subpath() {
    let shape = |rule| {
        square(
            16,
            &format!(r#"<path fill-rule="{rule}" d="M2 2 H14 V14 H2 Z M5 5 H11 V11 H5 Z"/>"#),
        )
    };
    let nonzero = shape("nonzero");
    let evenodd = shape("evenodd");
    assert_eq!(alpha(&nonzero, 16, 8, 8), 255);
    assert_eq!(alpha(&evenodd, 16, 8, 8), 0);
    assert_eq!(alpha(&nonzero, 16, 3, 8), 255);
    assert_eq!(alpha(&evenodd, 16, 3, 8), 255);
    assert!((area(&evenodd) - (144.0 - 36.0)).abs() < 0.02);
}

#[test]
fn a_stroke_covers_its_width_centered_on_the_line() {
    let image = square(
        20,
        r#"<path d="M0 10 H20" stroke="black" stroke-width="3"/>"#,
    );
    let column: Vec<u8> = (0..20).map(|y| alpha(&image, 20, 10, y)).collect();
    let mut expected = [0u8; 20];
    expected[8] = 128;
    expected[9] = 255;
    expected[10] = 255;
    expected[11] = 128;
    assert_eq!(column, expected);
}

#[test]
fn caps_extend_the_ends() {
    let line = |cap| {
        let body = format!(
            r#"<path d="M5 10 H15" stroke="black" stroke-width="4" stroke-linecap="{cap}"/>"#
        );
        area(&square(20, &body))
    };
    assert!((line("butt") - 40.0).abs() < 0.02);
    assert!((line("square") - 56.0).abs() < 0.02);
    let round = 40.0 + core::f32::consts::PI * 4.0;
    assert!((line("round") - round).abs() < 0.1, "{}", line("round"));
}

#[test]
fn a_corner_joins_mitered_or_beveled() {
    let corner = |join: &str, limit: f32| {
        let body = format!(
            r#"<path d="M4 16 H16 V4" fill="none" stroke="black" stroke-width="4"
                stroke-linejoin="{join}" stroke-miterlimit="{limit}"/>"#
        );
        square(20, &body)
    };
    let miter = corner("miter", 4.0);
    assert_eq!(alpha(&miter, 20, 17, 17), 255);
    assert_eq!(alpha(&miter, 20, 16, 17), 255);
    let bevel = corner("bevel", 4.0);
    assert_eq!(alpha(&bevel, 20, 17, 17), 0);
    assert_eq!(alpha(&bevel, 20, 16, 17), 128);
    assert_eq!(alpha(&bevel, 20, 15, 15), 255);
    // A right angle's miter is √2 half-widths long, past a limit of 1.
    let limited = corner("miter", 1.0);
    assert_eq!(limited, bevel);
    let round = corner("round", 4.0);
    let quarter = core::f32::consts::PI * 4.0 / 4.0;
    assert!(
        (area(&round) - (area(&bevel) - 2.0 + quarter)).abs() < 0.05,
        "{} {}",
        area(&round),
        area(&bevel)
    );
}

#[test]
fn dashes_chop_the_stroke() {
    let line = |offset: f32| {
        let body = format!(
            r#"<path d="M0 10 H20" stroke="black" stroke-width="2"
                stroke-dasharray="4 2" stroke-dashoffset="{offset}"/>"#
        );
        let image = square(20, &body);
        (0..20)
            .map(|x| alpha(&image, 20, x, 10) / 255)
            .collect::<Vec<u8>>()
    };
    assert_eq!(
        line(0.0),
        [1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1]
    );
    assert_eq!(
        line(1.0),
        [1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1, 1, 0, 0, 1, 1, 1]
    );
}

#[test]
fn a_stroke_over_itself_does_not_blend_twice() {
    let image = square(
        16,
        r#"<path d="M2 6 H14 V14 H8 V2" fill="none" stroke="black"
            stroke-opacity="0.5" stroke-width="2"/>"#,
    );
    let half = alpha(&image, 16, 4, 5);
    assert_eq!(half, 128);
    assert_eq!(
        alpha(&image, 16, 7, 5),
        half,
        "where the stroke crosses itself"
    );
    assert_eq!(alpha(&image, 16, 8, 6), half);
    assert_eq!(alpha(&image, 16, 14, 6), half, "at a corner");
    let dashed = square(
        16,
        r#"<path d="M2 8 H14 M8 2 V14" stroke="black" stroke-opacity="0.5"
            stroke-width="2" stroke-dasharray="20 1"/>"#,
    );
    assert_eq!(alpha(&dashed, 16, 7, 7), 128, "two subpaths crossing");
}

#[test]
fn group_opacity_and_transforms_apply() {
    let image = square(
        10,
        r##"<g opacity="0.5" transform="translate(5 0)">
            <rect width="5" height="10" fill="#ff0000"/></g>"##,
    );
    assert_eq!(image[5], [128, 0, 0, 128]);
    assert_eq!(image[9 * 10 + 9], [128, 0, 0, 128]);
    assert_eq!(image[4], [0, 0, 0, 0]);

    let scaled = square(
        20,
        r#"<g transform="scale(2)"><path d="M0 5 H10" stroke="black" stroke-width="1"/></g>"#,
    );
    let column: Vec<u8> = (8..12).map(|y| alpha(&scaled, 20, 10, y)).collect();
    assert_eq!(column, [0, 255, 255, 0]);

    let fixed = square(
        20,
        r#"<g transform="scale(2)"><path d="M0 5 H10" stroke="black" stroke-width="1"
            vector-effect="non-scaling-stroke"/></g>"#,
    );
    let column: Vec<u8> = (8..12).map(|y| alpha(&fixed, 20, 10, y)).collect();
    assert_eq!(column, [0, 128, 128, 0]);
}

#[test]
fn the_document_box_stretches_to_the_image() {
    let image = draw(
        (10, 10),
        r#"<rect width="5" height="5" fill="black"/>"#,
        20,
        10,
    );
    assert_eq!(alpha(&image, 20, 9, 4), 255);
    assert_eq!(alpha(&image, 20, 10, 4), 0);
    assert_eq!(alpha(&image, 20, 9, 5), 0);
    assert!((area(&image) - 50.0).abs() < 0.02);

    // A stroke widens by the geometric mean of the stretch: 1 × √(4 · 1) = 2.
    let line = draw((10, 10), r#"<path d="M0 5 H10" stroke="black"/>"#, 40, 10);
    assert!((area(&line) - 80.0).abs() < 0.05, "{}", area(&line));
}

#[test]
fn colors_convert_as_the_renderer_converts_them() {
    let fill = |color: &str| {
        square(
            4,
            &format!(r#"<rect width="4" height="4" fill="{color}"/>"#),
        )[5]
    };
    assert_eq!(fill("#ff0000"), [255, 0, 0, 255]);
    let gray = linear(0x80);
    assert_eq!(gray, 55);
    assert_eq!(fill("#808080"), [gray, gray, gray, 255]);
    let half = square(
        4,
        r##"<rect width="4" height="4" fill="#ff0000" fill-opacity="0.5"/>"##,
    );
    assert_eq!(half[5], [128, 0, 0, 128]);
    let over = square(
        4,
        r##"<rect width="4" height="4" fill="#0000ff"/>
            <rect width="4" height="4" fill="#ff0000" fill-opacity="0.5"/>"##,
    );
    assert_eq!(over[5], [128, 0, 127, 255]);
}

#[test]
fn unsupported_paints_and_empty_images_draw_nothing() {
    let image = square(
        8,
        r#"<defs><linearGradient id="g"><stop offset="0" stop-color="red"/><stop offset="1" stop-color="blue"/></linearGradient></defs>
           <rect width="8" height="8" fill="url(#g)"/>"#,
    );
    assert!(image.iter().all(|px| *px == [0, 0, 0, 0]));
    let tree =
        viso_svg::parse(br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#)
            .unwrap();
    assert!(rasterize(&tree, 0, 4).is_empty());
}
