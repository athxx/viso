//! Liquid glass: a frosted surface whose bevelled rim bends what lies behind
//! it inward and catches a thin glint from its light, and acrylic's
//! luminosity layer — each its own stage of the one material composite a
//! frosted panel already costs.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, TextureFormat};
use viso_render::{
    Border, Corners, FrostedMaterial, Glint, Lens, Luminosity, Primitive, Quad, Rect, Renderer,
    Rgba,
};

const W: u32 = 96;
const H: u32 = 64;

fn quad(rect: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect,
        color,
        radius: 0.0,
        border: Border::NONE,
    })
}

/// Black and white stripes 8 wide, a backdrop whose bend shows.
fn stripes() -> Vec<Primitive> {
    let mut out = vec![quad(
        Rect::new(0.0, 0.0, W as f32, H as f32),
        Rgba::new(0.0, 0.0, 0.0, 1.0),
    )];
    for x in (0..W).step_by(16) {
        out.push(quad(
            Rect::new(x as f32, 0.0, 8.0, H as f32),
            Rgba::new(1.0, 1.0, 1.0, 1.0),
        ));
    }
    out
}

/// The surface over the stripes, blurred by the little a lensed one is.
fn glass(lens: Lens, glint: Glint) -> Primitive {
    let mut surface = FrostedMaterial::new(
        Rect::new(16.0, 8.0, 64.0, 48.0),
        Corners::uniform(12.0),
        1.25,
    );
    surface.lens = lens;
    surface.glint = glint;
    Primitive::Frosted(Box::new(surface))
}

fn plain() -> Primitive {
    glass(Lens::NONE, Glint::NONE)
}

/// The picture of the stripes under `over`, as straight linear RGB rows.
fn picture(over: Option<Primitive>) -> (Vec<u8>, viso_render::FrameStats) {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format: TextureFormat = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([W as f32, H as f32], 1.0);
    let mut prims = stripes();
    prims.extend(over);
    r.upload(&mut gpu, &prims);
    let stats = r.frame_stats();
    r.submit(
        &mut gpu,
        surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    (gpu.read_pixels_bgra8(surface), stats)
}

fn red(px: &[u8], x: u32, y: u32) -> u8 {
    px[((y * W + x) * 4 + 2) as usize]
}

/// The row `y` of `px` across the surface, red channel.
fn row(px: &[u8], y: u32) -> Vec<u8> {
    (16..80).map(|x| red(px, x, y)).collect()
}

const LENS: Lens = Lens {
    refraction: 10.0,
    bevel: 16.0,
    dispersion: 0.0,
};

#[test]
fn the_rim_bends_the_backdrop_and_the_middle_shows_it_straight() {
    let (plain, _) = picture(Some(plain()));
    let (lensed, stats) = picture(Some(glass(LENS, Glint::NONE)));
    assert_eq!(
        stats.backdrop_captures, 1,
        "unblurred, it still reads its backdrop once"
    );
    // The middle of the surface is past the bevel: unbent.
    assert_eq!(red(&lensed, 48, 32), red(&plain, 48, 32));
    // The left rim pulls the stripes in from further inside.
    assert_ne!(
        row(&lensed, 32)[..10],
        row(&plain, 32)[..10],
        "{:?}",
        row(&lensed, 32)
    );
}

#[test]
fn the_glint_is_a_thin_line_on_the_walls_along_the_light() {
    // The light from straight above: the top and bottom walls catch it, the
    // top one, facing it, the more.
    let lit = Glint {
        strength: 1.0,
        light: 0.0,
        width: 1.5,
    };
    let (plain, _) = picture(Some(plain()));
    let (shone, _) = picture(Some(glass(Lens::NONE, lit)));
    // On a black stripe (x 40..48): the edge rows brighten, the top more.
    let at = |px: &[u8], x, y| red(px, x, y) as i32;
    let near = at(&shone, 44, 8) - at(&plain, 44, 8);
    let far = at(&shone, 44, 55) - at(&plain, 44, 55);
    assert!(near > 40, "{near}");
    assert!(near > far + 20, "{near} {far}");
    assert!(far > 10, "the far wall glints faintly: {far}");
    // The middle of a side wall, across the light's axis, does not.
    assert_eq!(at(&shone, 16, 32), at(&plain, 16, 32), "a side wall");
    // A line: eight points in, past its bleed, nothing.
    assert_eq!(at(&shone, 44, 16), at(&plain, 44, 16), "past the line");
}

#[test]
fn the_luminosity_layer_evens_the_backdrop_light() {
    let mut surface = FrostedMaterial::new(Rect::new(16.0, 8.0, 64.0, 48.0), Corners::SHARP, 2.0);
    surface.luminosity = Luminosity {
        amount: 1.0,
        luminance: 0.5,
    };
    let (px, _) = picture(Some(Primitive::Frosted(Box::new(surface))));
    // A white stripe and a black one come out at one light.
    let (white, black) = (red(&px, 36, 32) as i32, red(&px, 44, 32) as i32);
    assert!((white - black).abs() < 24, "{white} {black}");
}
