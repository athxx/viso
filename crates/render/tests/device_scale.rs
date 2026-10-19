//! A scene drawn at device scale 2 is the same picture as the scene with every
//! length doubled drawn at scale 1: clips cut on physical pixels, and an
//! offscreen layer's texels are physical, so a translucent or blurred layer is
//! as sharp as the surface it lands on. The same holds on the Metal device.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso_render::{
    AnalyticLine, AnalyticShadow, Border, Corners, FrostedMaterial, LayerClip, LineCap, LineJoin,
    Path, PathCmd, Point, Primitive, Quad, Rect, Renderer, Rgba, ShadowShape,
};

const W: u32 = 48;
const H: u32 = 40;

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn quad(r: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color,
        radius: 3.0,
        border: Border::NONE,
    })
}

fn layer(clip: Rect, opacity: f32, blur_sigma: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity,
        blur_sigma,
        backdrop_sigma: 0.0,
    })
}

/// A clipped quad, a translucent layer of two overlapping quads and a blurred
/// one, in logical points over a `W`×`H` window.
fn scene() -> Vec<Primitive> {
    let red = Rgba::new(0.9, 0.2, 0.1, 1.0);
    let blue = Rgba::new(0.1, 0.3, 0.9, 1.0);
    vec![
        quad(rect(0.0, 0.0, 48.0, 40.0), Rgba::new(0.1, 0.1, 0.1, 1.0)),
        layer(rect(2.5, 3.0, 11.0, 9.5), 1.0, 0.0),
        quad(rect(0.0, 0.0, 20.0, 20.0), red),
        Primitive::LayerEnd,
        layer(rect(16.0, 2.0, 30.0, 18.0), 0.5, 0.0),
        quad(rect(17.0, 3.0, 16.0, 12.0), red),
        quad(rect(25.0, 7.0, 16.0, 12.0), blue),
        Primitive::LayerEnd,
        layer(rect(4.0, 22.0, 40.0, 16.0), 1.0, 2.0),
        quad(rect(10.0, 25.0, 14.0, 9.0), blue),
        Primitive::LayerEnd,
    ]
}

/// A soft shadow, a line, a concave path, a backdrop blur and a frosted
/// panel: the lanes whose reach or coverage is measured in pixels.
fn lanes() -> Vec<Primitive> {
    let white = Rgba::new(0.95, 0.95, 0.9, 1.0);
    let point = |x, y| Point { x, y };
    vec![
        quad(rect(0.0, 0.0, 48.0, 40.0), Rgba::new(0.2, 0.25, 0.3, 1.0)),
        quad(rect(30.0, 4.0, 4.0, 32.0), white),
        Primitive::AnalyticShadow(AnalyticShadow {
            rect: rect(4.0, 4.0, 14.0, 10.0),
            color: Rgba::new(0.0, 0.0, 0.0, 0.8),
            radius: Corners::uniform(3.0),
            offset: [1.5, 2.0],
            sigma: 2.5,
            spread: 0.5,
            shape: ShadowShape::RoundedBox,
            inner: false,
        }),
        Primitive::AnalyticLine(AnalyticLine {
            p0: point(3.0, 20.0),
            p1: point(22.0, 27.5),
            width: 1.5,
            color: white,
            cap: LineCap::Round,
            join: LineJoin::Round,
            miter_limit: 4.0,
            border: Border::NONE,
        }),
        Primitive::Path(Box::new(Path {
            cmds: vec![
                PathCmd::MoveTo(point(4.0, 30.0)),
                PathCmd::LineTo(point(20.0, 30.0)),
                PathCmd::LineTo(point(12.0, 34.0)),
                PathCmd::LineTo(point(20.0, 38.0)),
                PathCmd::LineTo(point(4.0, 38.0)),
                PathCmd::Close,
            ],
            fill: Some(white),
            stroke: None,
            shadow: None,
        })),
        Primitive::Layer(LayerClip {
            clip: rect(26.0, 8.0, 14.0, 10.0),
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 2.0,
        }),
        Primitive::LayerEnd,
        Primitive::Frosted(Box::new(FrostedMaterial::new(
            rect(26.0, 22.0, 16.0, 12.0),
            Corners::uniform(4.0),
            2.5,
        ))),
    ]
}

/// `prims` with every length scaled by `s`.
fn scaled(prims: &[Primitive], s: f32) -> Vec<Primitive> {
    let r = |r: Rect| rect(r.x * s, r.y * s, r.w * s, r.h * s);
    let p = |p: Point| Point {
        x: p.x * s,
        y: p.y * s,
    };
    let corners = |c: Corners| Corners {
        left_top: c.left_top * s,
        right_top: c.right_top * s,
        right_bottom: c.right_bottom * s,
        left_bottom: c.left_bottom * s,
    };
    prims
        .iter()
        .map(|prim| match prim {
            Primitive::Quad(q) => Primitive::Quad(Quad {
                rect: r(q.rect),
                radius: q.radius * s,
                ..*q
            }),
            Primitive::Layer(l) => Primitive::Layer(LayerClip {
                clip: r(l.clip),
                blur_sigma: l.blur_sigma * s,
                backdrop_sigma: l.backdrop_sigma * s,
                ..*l
            }),
            Primitive::AnalyticShadow(sh) => Primitive::AnalyticShadow(AnalyticShadow {
                rect: r(sh.rect),
                radius: corners(sh.radius),
                offset: [sh.offset[0] * s, sh.offset[1] * s],
                sigma: sh.sigma * s,
                spread: sh.spread * s,
                ..*sh
            }),
            Primitive::AnalyticLine(l) => Primitive::AnalyticLine(AnalyticLine {
                p0: p(l.p0),
                p1: p(l.p1),
                width: l.width * s,
                ..*l
            }),
            Primitive::Path(path) => {
                let cmds = path
                    .cmds
                    .iter()
                    .map(|c| match *c {
                        PathCmd::MoveTo(a) => PathCmd::MoveTo(p(a)),
                        PathCmd::LineTo(a) => PathCmd::LineTo(p(a)),
                        PathCmd::QuadTo(a, b) => PathCmd::QuadTo(p(a), p(b)),
                        PathCmd::CubicTo(a, b, c) => PathCmd::CubicTo(p(a), p(b), p(c)),
                        PathCmd::Close => PathCmd::Close,
                    })
                    .collect();
                Primitive::Path(Box::new(Path {
                    cmds,
                    ..(**path).clone()
                }))
            }
            Primitive::Frosted(m) => Primitive::Frosted(Box::new(FrostedMaterial {
                rect: r(m.rect),
                radius: corners(m.radius),
                backdrop_sigma: m.backdrop_sigma * s,
                ..**m
            })),
            other => other.clone(),
        })
        .collect()
}

/// `prims` drawn at device `scale` onto a surface of the window's physical
/// size, read back.
fn draw(prims: &[Primitive], scale: f32, physical: [u32; 2]) -> Vec<u8> {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, physical[0], physical[1]);
    let format = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([physical[0] as f32, physical[1] as f32], scale);
    r.upload(&mut gpu, prims);
    let viewport = [physical[0] as f32 / scale, physical[1] as f32 / scale];
    r.submit(&mut gpu, surface, [0.0, 0.0, 0.0, 1.0], viewport);
    gpu.read_pixels_bgra8(surface)
}

/// The largest channel difference between two images, and how many texels
/// differ by more than `tolerance`.
fn diff(a: &[u8], b: &[u8], tolerance: u8) -> (u8, usize) {
    assert_eq!(a.len(), b.len());
    let mut worst = 0;
    let mut off = 0;
    for (pa, pb) in a.chunks(4).zip(b.chunks(4)) {
        let d = pa
            .iter()
            .zip(pb)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        worst = worst.max(d);
        if d > tolerance {
            off += 1;
        }
    }
    (worst, off)
}

/// `prims` at scale 2 against their doubled twin at scale 1.
fn same_at_two(prims: &[Primitive]) {
    let physical = [2 * W, 2 * H];
    let at_two = draw(prims, 2.0, physical);
    let doubled = draw(&scaled(prims, 2.0), 1.0, physical);
    let (worst, off) = diff(&at_two, &doubled, 2);
    assert_eq!(off, 0, "{off} texels differ, by up to {worst}");
}

#[test]
fn clips_and_layers_at_scale_two_draw_what_their_doubled_twin_draws() {
    same_at_two(&scene());
}

#[test]
fn shadows_lines_paths_and_backdrops_at_scale_two_draw_what_their_doubled_twin_draws() {
    let lanes = lanes();
    for k in 2..lanes.len() {
        let mut one = lanes[..2].to_vec();
        one.push(lanes[k].clone());
        if matches!(lanes[k], Primitive::Layer(_)) {
            one.push(Primitive::LayerEnd);
        }
        if !matches!(lanes[k], Primitive::LayerEnd) {
            same_at_two(&one);
        }
    }
    same_at_two(&lanes);
}

/// The offscreen texel bytes one frame of `prims` binds at device `scale`.
fn target_bytes(prims: &[Primitive], scale: f32, physical: [u32; 2]) -> usize {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, physical[0], physical[1]);
    let format = gpu.surface_format(surface);
    let mut r = Renderer::new(&mut gpu, format);
    r.set_surface_size([physical[0] as f32, physical[1] as f32], scale);
    r.upload(&mut gpu, prims);
    r.frame_stats().transient_target_bytes
}

#[test]
fn offscreen_targets_count_physical_bytes() {
    let physical = [2 * W, 2 * H];
    let at_two = target_bytes(&scene(), 2.0, physical);
    assert!(at_two > 0);
    assert_eq!(at_two, target_bytes(&scaled(&scene(), 2.0), 1.0, physical));
}

#[test]
fn scale_one_is_the_scene_as_painted() {
    let physical = [W, H];
    let plain = draw(&scene(), 1.0, physical);
    let again = draw(&scaled(&scene(), 1.0), 1.0, physical);
    assert_eq!(diff(&plain, &again, 0), (0, 0));
}

/// `prims` drawn at device `scale` into a Metal render target of the window's
/// physical size, read back.
#[cfg(target_vendor = "apple")]
fn draw_on_metal(prims: &[Primitive], scale: f32, physical: [u32; 2]) -> Vec<u8> {
    use viso_gpu::{MetalBackend, TextureDesc, TextureFormat};
    let mut gpu = MetalBackend::new();
    let mut r = Renderer::new(&mut gpu, TextureFormat::Bgra8Unorm);
    r.set_surface_size([physical[0] as f32, physical[1] as f32], scale);
    r.upload(&mut gpu, prims);
    let target = gpu.create_texture(&TextureDesc {
        width: physical[0],
        height: physical[1],
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "device-scale-target",
    });
    let viewport = [physical[0] as f32 / scale, physical[1] as f32 / scale];
    r.render_to_texture(&mut gpu, target, [0.0, 0.0, 0.0, 1.0], viewport);
    gpu.read_texture(target)
}

/// On Metal the scale-2 scene is its doubled twin, and its edge drift from the
/// headless raster grows no faster than the texel count.
#[cfg(target_vendor = "apple")]
#[test]
fn metal_at_scale_two_draws_what_its_doubled_twin_draws() {
    let physical = [2 * W, 2 * H];
    for prims in [scene(), lanes()] {
        let at_two = draw_on_metal(&prims, 2.0, physical);
        let doubled = draw_on_metal(&scaled(&prims, 2.0), 1.0, physical);
        assert_eq!(at_two.len(), (4 * physical[0] * physical[1]) as usize);
        let one = [W, H];
        let drift_one = diff(&draw_on_metal(&prims, 1.0, one), &draw(&prims, 1.0, one), 4).1;
        let drift_two = diff(&at_two, &draw(&prims, 2.0, physical), 4).1;
        assert!(
            drift_two <= 4 * drift_one,
            "Metal drifts from headless on {drift_two} texels at scale 2, {drift_one} at scale 1"
        );
        let (worst, off) = diff(&at_two, &doubled, 2);
        assert_eq!(off, 0, "{off} texels differ on Metal, by up to {worst}");
    }
}
