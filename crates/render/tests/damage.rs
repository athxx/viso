//! Damage-driven present (§30.3): a frame redraws only what changed, and what
//! it shows is exactly what drawing the whole frame shows.
//!
//! Each case presents a scene on a renderer that keeps its surface, changes
//! it, presents again, and requires the surface to be pixel-identical to the
//! changed scene drawn from scratch on a fresh surface — while the kept one
//! redrew only part of it, or nothing.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId};
use viso_render::damage::Region;
use viso_render::{
    Border, LayerClip, Primitive, Quad, Rect, Renderer, Rgba, TransformSlot, TransformUpdate,
};

const W: u32 = 96;
const H: u32 = 96;
const CLEAR: [f32; 4] = [0.05, 0.05, 0.1, 1.0];

struct Kept {
    gpu: CpuRaster,
    surface: SurfaceId,
    r: Renderer,
}

impl Kept {
    fn new() -> Kept {
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([W as f32, H as f32]);
        Kept { gpu, surface, r }
    }

    fn upload(&mut self, scene: &[Primitive]) {
        self.r.upload(&mut self.gpu, scene);
    }

    fn present(&mut self) -> Vec<u8> {
        self.r
            .submit(&mut self.gpu, self.surface, CLEAR, [W as f32, H as f32]);
        self.gpu.read_pixels_bgra8(self.surface)
    }
}

/// `scene` drawn whole on a fresh surface.
fn fresh(scene: &[Primitive]) -> Vec<u8> {
    let mut k = Kept::new();
    k.upload(scene);
    k.present()
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn quad(r: Rect, color: Rgba) -> Primitive {
    Primitive::Quad(Quad {
        rect: r,
        color,
        radius: 2.0,
        border: Border::NONE,
    })
}

fn layer(clip: Rect, opacity: f32) -> Primitive {
    Primitive::Layer(LayerClip {
        clip,
        opacity,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    })
}

/// Rows of translucent quads, so a redraw that drew one twice would show.
fn rows(n: usize, changed: Option<usize>) -> Vec<Primitive> {
    (0..n)
        .map(|i| {
            let alpha = if changed == Some(i) { 0.9 } else { 0.5 };
            quad(
                rect(4.0, 4.0 + i as f32 * 8.0, 80.0, 6.0),
                Rgba::new(0.9, 0.4 + i as f32 * 0.03, 0.2, alpha),
            )
        })
        .collect()
}

/// Requires two frames to be pixel-identical, naming where they differ.
#[track_caller]
fn same(a: &[u8], b: &[u8]) {
    let diffs: Vec<(u32, u32, [u8; 4], [u8; 4])> = a
        .chunks(4)
        .zip(b.chunks(4))
        .enumerate()
        .filter(|(_, (p, q))| p != q)
        .map(|(i, (p, q))| {
            let i = i as u32;
            (i % W, i / W, p.try_into().unwrap(), q.try_into().unwrap())
        })
        .collect();
    assert!(
        diffs.is_empty(),
        "{} pixels differ, first {:?}",
        diffs.len(),
        &diffs[..diffs.len().min(6)]
    );
}

fn redrew(r: &Renderer) -> f32 {
    match r.last_damage() {
        Region::Empty => 0.0,
        Region::Full => (W * H) as f32,
        Region::Rects(rects) => rects.iter().map(|r| r.w * r.h).sum(),
    }
}

#[test]
fn an_unchanged_frame_is_not_submitted() {
    let mut k = Kept::new();
    k.upload(&rows(8, None));
    let first = k.present();
    assert_eq!(
        k.r.last_damage(),
        &Region::Full,
        "the first frame draws whole"
    );
    k.upload(&rows(8, None));
    let again = k.present();
    assert_eq!(k.r.last_damage(), &Region::Empty);
    assert_eq!(k.r.damage_stats().skipped_frames, 1);
    assert_eq!(first, again);
}

#[test]
fn a_changed_row_redraws_that_row_only_and_matches_a_whole_frame() {
    let mut k = Kept::new();
    k.upload(&rows(10, None));
    k.present();
    let changed = rows(10, Some(4));
    k.upload(&changed);
    let partial = k.present();
    let Region::Rects(rects) = *k.r.last_damage() else {
        panic!("one row is a partial redraw: {:?}", k.r.last_damage());
    };
    assert_eq!(rects.len(), 1);
    assert!(redrew(&k.r) < 0.1 * (W * H) as f32, "{rects:?}");
    same(&partial, &fresh(&changed));
    assert_eq!(k.r.damage_stats().partial_frames, 1);
}

#[test]
fn an_inserted_row_damages_where_it_lands_not_everything_after() {
    let mut k = Kept::new();
    let mut scene = rows(10, None);
    k.upload(&scene);
    k.present();
    scene.insert(
        3,
        quad(rect(60.0, 60.0, 10.0, 10.0), Rgba::new(0.0, 1.0, 0.0, 0.7)),
    );
    k.upload(&scene);
    let partial = k.present();
    assert!(
        redrew(&k.r) < 0.05 * (W * H) as f32,
        "{:?}",
        k.r.last_damage()
    );
    same(&partial, &fresh(&scene));

    scene.remove(3);
    k.upload(&scene);
    same(&k.present(), &fresh(&scene));
}

#[test]
fn a_moved_root_damages_where_it_was_and_where_it_is() {
    let scene = |dy: f32, root: bool| {
        let mut out = vec![quad(
            rect(0.0, 0.0, 96.0, 96.0),
            Rgba::new(0.2, 0.2, 0.2, 1.0),
        )];
        out.push(layer(rect(10.0, 10.0, 40.0, 40.0), 1.0));
        if root {
            out.push(Primitive::Transform(TransformSlot(0)));
        }
        out.push(quad(
            rect(12.0, 12.0 + dy, 20.0, 10.0),
            Rgba::new(1.0, 0.0, 0.0, 0.8),
        ));
        out.push(quad(
            rect(12.0, 30.0 + dy, 20.0, 10.0),
            Rgba::new(0.0, 0.0, 1.0, 0.8),
        ));
        if root {
            out.push(Primitive::TransformEnd);
        }
        out.push(Primitive::LayerEnd);
        out.push(quad(
            rect(70.0, 70.0, 10.0, 10.0),
            Rgba::new(1.0, 1.0, 1.0, 1.0),
        ));
        out
    };
    let mut k = Kept::new();
    k.r.upload_with_transforms(&mut k.gpu, &scene(0.0, true), &[[0.0, 0.0]]);
    k.present();
    assert_eq!(k.r.set_transforms(&[[0.0, -6.0]]), TransformUpdate::Moved);
    let moved = k.present();
    assert!(
        redrew(&k.r) <= 50.0 * 50.0,
        "the clip bounds it: {:?}",
        k.r.last_damage()
    );
    same(&moved, &fresh(&scene(-6.0, false)));
    k.present();
    assert_eq!(
        k.r.last_damage(),
        &Region::Empty,
        "a held position is no damage"
    );
}

#[test]
fn a_translucent_layer_s_content_damages_its_composite() {
    let scene = |inner: Rgba| {
        vec![
            quad(rect(0.0, 0.0, 96.0, 96.0), Rgba::new(0.3, 0.3, 0.3, 1.0)),
            layer(rect(20.0, 20.0, 40.0, 40.0), 0.5),
            quad(rect(22.0, 22.0, 30.0, 30.0), inner),
            quad(rect(30.0, 30.0, 30.0, 30.0), Rgba::new(0.0, 0.0, 1.0, 1.0)),
            Primitive::LayerEnd,
        ]
    };
    let mut k = Kept::new();
    k.upload(&scene(Rgba::new(1.0, 0.0, 0.0, 1.0)));
    k.present();
    let changed = scene(Rgba::new(0.0, 1.0, 0.0, 1.0));
    k.upload(&changed);
    let partial = k.present();
    assert!(
        matches!(k.r.last_damage(), Region::Rects(_)),
        "{:?}",
        k.r.last_damage()
    );
    same(&partial, &fresh(&changed));
}

#[test]
fn a_new_clear_color_or_size_redraws_whole() {
    let mut k = Kept::new();
    k.upload(&rows(4, None));
    k.present();
    k.r.submit(
        &mut k.gpu,
        k.surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    assert_eq!(k.r.last_damage(), &Region::Full);
    k.r.damage_all();
    k.r.submit(
        &mut k.gpu,
        k.surface,
        [0.0, 0.0, 0.0, 1.0],
        [W as f32, H as f32],
    );
    assert_eq!(k.r.last_damage(), &Region::Full);
}

#[test]
fn most_of_the_surface_changing_is_a_whole_frame() {
    let mut k = Kept::new();
    k.upload(&[quad(
        rect(0.0, 0.0, 90.0, 90.0),
        Rgba::new(1.0, 0.0, 0.0, 1.0),
    )]);
    k.present();
    let changed = [quad(
        rect(0.0, 0.0, 90.0, 90.0),
        Rgba::new(0.0, 1.0, 0.0, 1.0),
    )];
    k.upload(&changed);
    same(&k.present(), &fresh(&changed));
    assert_eq!(k.r.last_damage(), &Region::Full);
    assert_eq!(k.r.damage_stats().full_frame_fallbacks, 2);
}

#[test]
fn partial_redraw_off_draws_every_frame_whole() {
    let mut k = Kept::new();
    k.r.set_partial_redraw(false);
    k.upload(&rows(4, None));
    k.present();
    k.present();
    assert_eq!(k.r.last_damage(), &Region::Full);
    assert_eq!(k.r.damage_stats().skipped_frames, 0);
}
