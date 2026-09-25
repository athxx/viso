//! The portable color raster against a COLR v0/v1 fixture (see
//! `fixtures/make_colr_fixture.py` for what each glyph paints).

use viso_text::raster_a8::rasterize_coverage;
use viso_text::raster_color::rasterize_color;
use viso_text::system_fonts::ColorGlyph;

const COLR: &[u8] = include_bytes!("fixtures/ColrFixture.ttf");

const GLYPH_A: u16 = 34;
const GLYPH_H: u16 = 41;
const GLYPH_O: u16 = 48;
const GLYPH_R: u16 = 51;
const GLYPH_S: u16 = 52;
const GLYPH_T: u16 = 53;
const GLYPH_X: u16 = 57;

fn paint(glyph: u16, ppem: u16) -> ColorGlyph {
    rasterize_color(COLR, 0, glyph, ppem).expect("color glyph")
}

/// Unpremultiplied colors of the (nearly) opaque pixels, with their position.
fn opaque(glyph: &ColorGlyph) -> Vec<(u32, u32, [u8; 3])> {
    let mut out = Vec::new();
    for (i, p) in glyph.rgba.as_chunks::<4>().0.iter().enumerate() {
        if p[3] >= 250 {
            let (x, y) = (i as u32 % glyph.width, i as u32 / glyph.width);
            out.push((x, y, [p[0], p[1], p[2]]));
        }
    }
    out
}

#[test]
fn colr_v0_layer_paints_its_palette_color() {
    let glyph = paint(GLYPH_O, 48);
    assert_eq!(glyph.pixels_per_em, 48);
    assert_eq!(glyph.rgba.len(), (glyph.width * glyph.height * 4) as usize);
    let ink = opaque(&glyph);
    assert!(!ink.is_empty());
    assert!(
        ink.iter()
            .all(|&(_, _, c)| c[0] >= 250 && c[1] <= 5 && c[2] <= 5)
    );
}

#[test]
fn premultiplied_pixels_never_exceed_their_alpha() {
    for gid in [GLYPH_O, GLYPH_H, GLYPH_R, GLYPH_S, GLYPH_T, GLYPH_X] {
        let glyph = paint(gid, 40);
        for p in glyph.rgba.as_chunks::<4>().0 {
            assert!(
                p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3],
                "glyph {gid}: {p:?}"
            );
        }
    }
}

#[test]
fn placement_matches_the_coverage_raster() {
    // A plain layer covers exactly the outline, so its box sits where the A8
    // coverage raster puts the same glyph.
    let color = paint(GLYPH_O, 64);
    let mask = rasterize_coverage(COLR, 0, GLYPH_O, 64.0).expect("outline");
    assert!((color.width as i32 - mask.width as i32).abs() <= 1);
    assert!((color.height as i32 - mask.height as i32).abs() <= 1);
    let top = color.origin_px[1] + color.height as f32;
    assert!(
        (color.origin_px[0] - mask.left).abs() <= 1.0,
        "{:?} vs {}",
        color.origin_px,
        mask.left
    );
    assert!((top - mask.top).abs() <= 1.0, "{top} vs {}", mask.top);
}

#[test]
fn size_scales_the_bitmap() {
    let small = paint(GLYPH_O, 32);
    let large = paint(GLYPH_O, 64);
    let ratio = large.height as f32 / small.height as f32;
    assert!((1.8..=2.2).contains(&ratio), "{ratio}");
}

#[test]
fn linear_gradient_runs_red_to_blue_left_to_right() {
    let glyph = paint(GLYPH_H, 64);
    let ink = opaque(&glyph);
    let left = ink.iter().min_by_key(|p| p.0).expect("ink").2;
    let right = ink.iter().max_by_key(|p| p.0).expect("ink").2;
    assert!(left[0] > left[2], "left {left:?}");
    assert!(right[2] > right[0], "right {right:?}");
}

#[test]
fn radial_gradient_is_green_at_the_center_and_bluer_outward() {
    let glyph = paint(GLYPH_R, 64);
    // The gradient center (700, 750) units, in bitmap px.
    let scale = 64.0 / 2048.0;
    let cx = 700.0 * scale - glyph.origin_px[0];
    let cy = glyph.origin_px[1] + glyph.height as f32 - 750.0 * scale;
    let ink = opaque(&glyph);
    let dist = |p: &(u32, u32, [u8; 3])| {
        let (dx, dy) = (p.0 as f32 + 0.5 - cx, p.1 as f32 + 0.5 - cy);
        dx * dx + dy * dy
    };
    let near = ink
        .iter()
        .min_by(|l, r| dist(l).total_cmp(&dist(r)))
        .expect("ink");
    let far = ink
        .iter()
        .max_by(|l, r| dist(l).total_cmp(&dist(r)))
        .expect("ink");
    let green_share = |c: [u8; 3]| f32::from(c[1]) / (f32::from(c[1]) + f32::from(c[2]) + 1.0);
    assert!(
        green_share(near.2) > green_share(far.2),
        "{near:?} vs {far:?}"
    );
}

#[test]
fn sweep_gradient_spans_its_color_line() {
    let glyph = paint(GLYPH_S, 64);
    let ink = opaque(&glyph);
    assert!(ink.iter().any(|p| p.2[0] > 180 && p.2[2] < 80));
    assert!(ink.iter().any(|p| p.2[2] > 180 && p.2[0] < 80));
}

#[test]
fn source_in_composite_keeps_the_source_color_inside_the_backdrop() {
    let glyph = paint(GLYPH_T, 48);
    let ink = opaque(&glyph);
    assert!(!ink.is_empty());
    assert!(ink.iter().all(|&(_, _, c)| c[1] >= 250 && c[0] <= 5));
    // Nothing outside the T: the bitmap is no wider than the outline.
    let mask = rasterize_coverage(COLR, 0, GLYPH_T, 48.0).expect("outline");
    assert!(glyph.width <= mask.width + 1);
}

#[test]
fn translate_moves_the_origin_by_half_an_em() {
    let ppem = 64;
    let glyph = paint(GLYPH_X, ppem);
    let mask = rasterize_coverage(COLR, 0, GLYPH_X, f32::from(ppem)).expect("outline");
    let shift = glyph.origin_px[0] - mask.left;
    assert!((shift - 32.0).abs() <= 1.0, "{shift}");
}

#[test]
fn a_glyph_without_color_is_none() {
    assert!(rasterize_color(COLR, 0, GLYPH_A, 32).is_none());
}

/// `sbix` PNG strikes through the system emoji face (macOS only; the file is
/// part of every macOS install).
#[cfg(target_os = "macos")]
#[test]
fn sbix_strike_resamples_to_the_requested_size() {
    let Ok(bytes) = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc") else {
        return;
    };
    let face = ttf_parser::Face::parse(&bytes, 0).expect("face");
    let gid = face.glyph_index('😀').expect("grinning face").0;
    for ppem in [20u16, 40, 96] {
        let glyph = rasterize_color(&bytes, 0, gid, ppem).expect("strike");
        assert_eq!(glyph.pixels_per_em, ppem);
        let edge = glyph.width.max(glyph.height) as f32;
        assert!(
            edge >= f32::from(ppem) * 0.7 && edge <= f32::from(ppem) * 1.4,
            "{ppem}: {edge}"
        );
        let colorful = glyph
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[3] > 200 && p[0].abs_diff(p[2]) > 60);
        assert!(colorful);
    }
}
