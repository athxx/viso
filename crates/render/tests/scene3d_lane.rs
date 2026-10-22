//! The 3D scene lane end to end through the renderer (ADR 0047): a scene draw
//! renders its slot's frame into the slot's target and composites it into its
//! rect, renders again only when the frame or the size changed, culls what the
//! camera cannot see, and grows its target in 64-pixel steps.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId};
use viso_math::{LinearStraight, Quat, Vec3, vec3};
use viso_render::damage::Region;
use viso_render::scene3d::{Camera, SceneFrame, Shape, instance};
use viso_render::{Border, Primitive, Quad, Rect, Renderer, Rgba, SceneDraw, SceneSlot};

const W: u32 = 96;
const H: u32 = 96;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const SLOT: SceneSlot = SceneSlot(0);

struct Kit {
    gpu: CpuRaster,
    surface: SurfaceId,
    r: Renderer,
}

impl Kit {
    fn new() -> Kit {
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([W as f32, H as f32], 1.0);
        Kit { gpu, surface, r }
    }

    fn frame(&mut self, at: Vec3) {
        let mut frame = SceneFrame::new();
        frame.camera = Camera {
            eye: vec3(0.0, 0.0, 6.0),
            target: Vec3::ZERO,
            ..Camera::default()
        };
        frame.background = LinearStraight::new(0.0, 0.0, 1.0, 1.0);
        frame.lighting.direction = vec3(0.0, 0.0, 1.0);
        frame.push(
            Shape::Cube,
            instance(
                at,
                Quat::IDENTITY,
                vec3(1.0, 1.0, 1.0),
                LinearStraight::new(1.0, 0.0, 0.0, 1.0),
            ),
        );
        self.r.swap_scene(SLOT, &mut frame);
    }

    fn present(&mut self, stream: &[Primitive]) -> Vec<u8> {
        self.r.upload(&mut self.gpu, stream);
        self.r
            .submit(&mut self.gpu, self.surface, CLEAR, [W as f32, H as f32]);
        self.gpu.read_pixels_bgra8(self.surface)
    }
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { x, y, w, h }
}

fn stream(at: Rect, revision: u64) -> Vec<Primitive> {
    vec![
        Primitive::Quad(Quad {
            rect: rect(0.0, 0.0, W as f32, H as f32),
            color: Rgba::new(0.0, 0.0, 0.0, 1.0),
            radius: 0.0,
            border: Border::NONE,
        }),
        Primitive::Scene(SceneDraw {
            rect: at,
            slot: SLOT,
            revision,
            mask: None,
        }),
    ]
}

/// Requires two surfaces to be pixel-identical, naming where they differ.
#[track_caller]
fn same(a: &[u8], b: &[u8], what: &str) {
    let diffs: Vec<(usize, &[u8], &[u8])> = a
        .chunks(4)
        .zip(b.chunks(4))
        .enumerate()
        .filter(|(_, (p, q))| p != q)
        .map(|(i, (p, q))| (i, p, q))
        .collect();
    assert!(
        diffs.is_empty(),
        "{what}: {} pixels differ, first {:?}",
        diffs.len(),
        &diffs[..diffs.len().min(4)]
    );
}

/// The RGB at `(x, y)` of a BGRA8 surface.
fn at(px: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * W + x) * 4) as usize;
    [px[i + 2], px[i + 1], px[i]]
}

#[test]
fn a_scene_draw_composites_its_lit_scene_into_its_rect() {
    let mut k = Kit::new();
    k.frame(Vec3::ZERO);
    let px = k.present(&stream(rect(10.0, 20.0, 70.0, 50.0), 1));
    let centre = at(&px, 45, 45);
    assert!(
        centre[0] > 200 && centre[0] > centre[1] + 80 && centre[0] > centre[2] + 80,
        "the red cube faces the camera and the light: {centre:?}"
    );
    let corner = at(&px, 12, 22);
    assert!(
        corner[2] > 200 && corner[0] < 40,
        "the blue background fills the rest of the rect: {corner:?}"
    );
    assert_eq!(at(&px, 5, 5), [0, 0, 0], "outside the rect is the 2D scene");
    assert_eq!(at(&px, 85, 45), [0, 0, 0]);
    let stats = k.r.take_scene_stats();
    assert_eq!((stats.passes, stats.draws, stats.instances), (1, 1, 1));
}

#[test]
fn a_scene_renders_again_only_when_its_frame_or_size_changes() {
    let mut k = Kit::new();
    let at_rect = rect(10.0, 20.0, 70.0, 50.0);
    k.frame(Vec3::ZERO);
    k.present(&stream(at_rect, 1));
    assert_eq!(k.r.take_scene_stats().allocations, 1);

    // The same frame: nothing renders and nothing on the surface changed.
    k.present(&stream(at_rect, 1));
    assert_eq!(k.r.take_scene_stats().passes, 0);
    assert_eq!(k.r.last_damage(), &Region::Empty);

    // A new frame with a new revision renders and redraws its rect.
    k.frame(vec3(1.0, 0.0, 0.0));
    let moved = k.present(&stream(at_rect, 2));
    assert_eq!(k.r.take_scene_stats().passes, 1);
    match k.r.last_damage() {
        Region::Rects(rects) => {
            let r = rects.iter().copied().reduce(Rect::union).unwrap();
            assert!(r.x <= 10.0 && r.y <= 20.0 && r.x + r.w >= 80.0 && r.y + r.h >= 70.0);
        }
        other => assert_eq!(other, &Region::Full),
    }
    let mut fresh = Kit::new();
    fresh.frame(vec3(1.0, 0.0, 0.0));
    same(
        &moved,
        &fresh.present(&stream(at_rect, 1)),
        "the kept surface",
    );

    // A resize within the 64-pixel step renders without reallocating.
    k.present(&stream(rect(10.0, 20.0, 76.0, 50.0), 2));
    let stats = k.r.take_scene_stats();
    assert_eq!((stats.passes, stats.allocations), (1, 0));
    // Past it, the target is reallocated.
    k.present(&stream(rect(0.0, 0.0, 96.0, 70.0), 2));
    assert_eq!(k.r.take_scene_stats().allocations, 1);
}

#[test]
fn a_patched_revision_renders_the_new_frame() {
    let mut k = Kit::new();
    let at_rect = rect(10.0, 20.0, 70.0, 50.0);
    k.frame(Vec3::ZERO);
    k.present(&stream(at_rect, 1));
    k.r.take_scene_stats();
    k.frame(vec3(1.0, 0.0, 0.0));
    let next = stream(at_rect, 2);
    assert!(k.r.patch(&mut k.gpu, &next, &[1]), "a scene draw patches");
    k.r.submit(&mut k.gpu, k.surface, CLEAR, [W as f32, H as f32]);
    assert_eq!(k.r.take_scene_stats().passes, 1);
    let mut fresh = Kit::new();
    fresh.frame(vec3(1.0, 0.0, 0.0));
    same(
        &k.gpu.read_pixels_bgra8(k.surface),
        &fresh.present(&next),
        "the patched surface",
    );
}

#[test]
fn what_the_camera_cannot_see_is_culled() {
    let mut k = Kit::new();
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: vec3(0.0, 0.0, 6.0),
        target: Vec3::ZERO,
        ..Camera::default()
    };
    let red = LinearStraight::new(1.0, 0.0, 0.0, 1.0);
    let one = vec3(1.0, 1.0, 1.0);
    for position in [Vec3::ZERO, vec3(0.0, 0.0, 20.0), vec3(50.0, 0.0, 0.0)] {
        frame.push(Shape::Sphere, instance(position, Quat::IDENTITY, one, red));
    }
    frame.push(
        Shape::Cube,
        instance(vec3(0.0, -60.0, 0.0), Quat::IDENTITY, one, red),
    );
    k.r.swap_scene(SLOT, &mut frame);
    k.present(&stream(rect(0.0, 0.0, 64.0, 64.0), 1));
    let stats = k.r.take_scene_stats();
    assert_eq!(
        (stats.draws, stats.instances, stats.culled),
        (1, 1, 3),
        "one sphere in view; the cube batch is culled whole"
    );
}

#[test]
fn a_released_scene_draws_nothing() {
    let mut k = Kit::new();
    k.frame(Vec3::ZERO);
    let s = stream(rect(10.0, 20.0, 70.0, 50.0), 1);
    k.present(&s);
    k.r.release_scene(&mut k.gpu, SLOT);
    let px = k.present(&stream(rect(10.0, 20.0, 70.0, 50.0), 2));
    assert_eq!(at(&px, 45, 45), [0, 0, 0]);
}

/// The scene lane draws the same picture on the Metal device as on the CPU
/// raster.
#[cfg(target_vendor = "apple")]
#[test]
fn metal_draws_what_the_cpu_raster_draws() {
    use viso_gpu::{MetalBackend, TextureDesc, TextureFormat};

    fn image<B: GpuBackend>(
        gpu: &mut B,
        read: impl Fn(&mut B, viso_gpu::TextureId) -> Vec<u8>,
    ) -> Vec<u8> {
        let mut r = Renderer::new(gpu, TextureFormat::Bgra8Unorm);
        r.set_surface_size([W as f32, H as f32], 1.0);
        let mut frame = SceneFrame::new();
        frame.camera = Camera {
            eye: vec3(3.0, 2.5, 5.0),
            target: Vec3::ZERO,
            ..Camera::default()
        };
        frame.background = LinearStraight::new(0.5, 0.6, 0.8, 1.0);
        let one = vec3(1.0, 1.0, 1.0);
        let turn = Quat::from_axis_angle(vec3(0.0, 1.0, 0.0), 0.6);
        let color = |r, g, b| LinearStraight::new(r, g, b, 1.0);
        frame.push(
            Shape::Cube,
            instance(vec3(-1.2, 0.0, 0.0), turn, one, color(0.9, 0.2, 0.1)),
        );
        frame.push(
            Shape::Sphere,
            instance(vec3(1.2, 0.0, 0.0), turn, one, color(0.1, 0.6, 0.9)),
        );
        frame.push(
            Shape::Cone,
            instance(vec3(0.0, 0.0, -2.0), turn, one, color(0.9, 0.8, 0.2)),
        );
        frame.push(
            Shape::Plane,
            instance(
                vec3(0.0, -1.0, 0.0),
                Quat::IDENTITY,
                vec3(6.0, 1.0, 6.0),
                color(0.3, 0.5, 0.3),
            ),
        );
        r.swap_scene(SLOT, &mut frame);
        r.upload(gpu, &stream(rect(0.0, 0.0, W as f32, H as f32), 1));
        let target = gpu.create_texture(&TextureDesc {
            width: W,
            height: H,
            format: TextureFormat::Bgra8Unorm,
            render_target: true,
            label: "out",
        });
        r.render_to_texture(gpu, target, CLEAR, [W as f32, H as f32]);
        read(gpu, target)
    }

    let cpu = image(&mut CpuRaster::new(), |gpu, t| gpu.read_texture(t));
    let metal = image(&mut MetalBackend::new(), |gpu, t| gpu.read_texture(t));
    let off = cpu
        .as_chunks::<4>()
        .0
        .iter()
        .zip(metal.as_chunks::<4>().0)
        .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 3))
        .count();
    assert!(
        off * 100 < (W * H) as usize,
        "{off} of {} pixels differ",
        W * H
    );
}
