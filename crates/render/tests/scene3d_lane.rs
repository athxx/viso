//! The 3D scene lane end to end through the renderer: a scene draw
//! renders its slot's frame into the slot's target and composites it into its
//! rect, renders again only when the frame or the size changed, culls what the
//! camera cannot see, grows its target in 64-pixel steps, draws unlit meshes
//! in their own color, blends sprites over the meshes they are in front
//! of, moves a skinned mesh's vertices by its joints, and culls a mirrored
//! instance's far side.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId};
use viso_math::{LinearStraight, Mat4, Quat, Vec3, vec3};
use viso_render::damage::Region;
use viso_render::scene3d::{
    AmbientOcclusion, Camera, MaterialId, SceneFrame, SceneSprite, Shadows, Shape, SkinnedVertex,
    Sky, instance,
};
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

/// A frame seen from `z = 6` toward the origin on a black background, lit
/// head on.
fn facing_frame() -> SceneFrame {
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: vec3(0.0, 0.0, 6.0),
        target: Vec3::ZERO,
        ..Camera::default()
    };
    frame.background = LinearStraight::new(0.0, 0.0, 0.0, 1.0);
    frame.lighting.direction = vec3(0.0, 1.0, 0.0);
    frame
}

#[test]
fn a_mirrored_instance_shows_its_outside() {
    // A cube is its own mirror image: mirrored or not, the same faces show,
    // lit the same.
    let draw = |x_scale: f32| {
        let mut k = Kit::new();
        let mut frame = facing_frame();
        frame.lighting.direction = vec3(0.5, 0.6, 1.0);
        frame.push(
            Shape::Cube,
            instance(
                Vec3::ZERO,
                Quat::from_axis_angle(vec3(0.3, 1.0, 0.1).normalize(), 0.7),
                vec3(x_scale, 1.0, 1.0),
                LinearStraight::new(0.8, 0.5, 0.3, 1.0),
            ),
        );
        k.r.swap_scene(SLOT, &mut frame);
        k.present(&stream(rect(0.0, 0.0, W as f32, H as f32), 1))
    };
    let plain = draw(1.0);
    let mirrored = draw(-1.0);
    let lit = plain
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[2] > 40)
        .count();
    assert!(lit > (W * H / 10) as usize, "the cube shows");
    same(&plain, &mirrored, "a mirrored cube");
}

#[test]
fn an_unlit_mesh_shows_its_own_color() {
    let mut k = Kit::new();
    let mut frame = facing_frame();
    let one = vec3(1.0, 1.0, 1.0);
    let color = LinearStraight::new(0.2, 0.6, 0.4, 1.0);
    // Lit from above only, a lit cube's front face is dim; an unlit one is not.
    frame.push_unlit(
        Shape::Cube,
        instance(vec3(-1.5, 0.0, 0.0), Quat::IDENTITY, one * 0.5, color),
    );
    frame.push(
        Shape::Cube,
        instance(vec3(1.5, 0.0, 0.0), Quat::IDENTITY, one * 0.5, color),
    );
    k.r.swap_scene(SLOT, &mut frame);
    let px = k.present(&stream(rect(0.0, 0.0, W as f32, H as f32), 1));
    let encode = |c: f32| (c.powf(1.0 / 2.2) * 255.0).round() as i32;
    let unlit = at(&px, 30, 48);
    let want = [encode(0.2), encode(0.6), encode(0.4)];
    for (got, want) in unlit.iter().zip(want) {
        assert!((i32::from(*got) - want).abs() <= 2, "{unlit:?} vs {want:?}");
    }
    let lit = at(&px, 66, 48);
    assert!(lit[1] < unlit[1], "the lit face is in shadow: {lit:?}");
}

#[test]
fn a_sprite_blends_over_what_is_behind_it_and_hides_behind_what_is_in_front() {
    let mut k = Kit::new();
    let mut frame = facing_frame();
    let red = LinearStraight::new(1.0, 0.0, 0.0, 1.0);
    frame.push(
        Shape::Cube,
        instance(Vec3::ZERO, Quat::IDENTITY, vec3(3.0, 3.0, 0.1), red),
    );
    let white = LinearStraight::new(1.0, 1.0, 1.0, 0.5);
    // In front of the wall, and one behind it.
    frame.push_sprite(SceneSprite::new(vec3(-1.2, 0.0, 1.0), 0.8, white));
    frame.push_sprite(SceneSprite::new(vec3(1.2, 0.0, -1.0), 0.8, white));
    k.r.swap_scene(SLOT, &mut frame);
    let px = k.present(&stream(rect(0.0, 0.0, W as f32, H as f32), 1));
    let stats = k.r.take_scene_stats();
    assert_eq!(
        (stats.draws, stats.instances),
        (2, 3),
        "a wall and a sprite batch"
    );

    let wall = at(&px, 48, 20);
    assert!(wall[0] > 100 && wall[1] < 20, "the bare wall: {wall:?}");
    // The front sprite's centre: half white over the wall's red.
    let front = at(&px, 23, 48);
    assert!(
        front[1] > 100 && front[0] >= wall[0],
        "the sprite over the wall: {front:?}"
    );
    // Toward its rim it fades back to the wall.
    let rim = at(&px, 23 + 12, 48);
    assert!(rim[1] < front[1], "it fades to its rim: {rim:?} {front:?}");
    // The sprite behind the wall is hidden by its depth.
    let behind = at(&px, 64, 48);
    assert!(behind[1] < 20, "hidden behind the wall: {behind:?}");
}

/// The scene lane draws the same picture on the Metal device as on the CPU
/// raster: exactly without ambient occlusion; with it, within the noise of
/// its point-sampled taps, a tap on a texel's edge reading one texel on one
/// and its neighbour on the other.
#[cfg(target_vendor = "apple")]
#[test]
fn metal_draws_what_the_cpu_raster_draws() {
    use viso_gpu::{MetalBackend, TextureDesc, TextureFormat};

    fn image<B: GpuBackend>(
        gpu: &mut B,
        sky: Option<Sky>,
        occlusion: AmbientOcclusion,
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
        frame.sky = sky;
        frame.lighting.occlusion = occlusion;
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
        // Mirrored, so its far side is culled by the other winding.
        frame.push(
            Shape::Cone,
            instance(
                vec3(0.0, 0.0, -2.0),
                turn,
                vec3(-1.0, 1.0, 1.0),
                color(0.9, 0.8, 0.2),
            ),
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
        frame.push_unlit(
            Shape::Cube,
            instance(
                vec3(0.0, 1.4, 0.0),
                Quat::IDENTITY,
                vec3(1.5, 0.05, 0.05),
                color(0.2, 1.0, 0.3),
            ),
        );
        for (x, z) in [(-0.4, 1.5), (0.5, 1.0), (0.0, -0.5)] {
            frame.push_sprite(SceneSprite::new(
                vec3(x, 0.4, z),
                0.6,
                LinearStraight::new(1.0, 0.8, 0.4, 0.7),
            ));
        }
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

    // The second under a low sun's sky, its disc in view behind the scene.
    let low_sun = Sky {
        sun: vec3(-0.6, 0.12, -1.0),
        ..Sky::default()
    };
    let ao = AmbientOcclusion::default();
    for (name, sky, occlusion, tolerance) in [
        ("lane-shapes", None, AmbientOcclusion::NONE, 3),
        ("lane-shapes-ao", None, ao, 16),
        ("lane-sky", Some(low_sun), ao, 16),
    ] {
        let cpu = image(&mut CpuRaster::new(), sky, occlusion, |gpu, t| {
            gpu.read_texture(t)
        });
        let metal = image(&mut MetalBackend::new(), sky, occlusion, |gpu, t| {
            gpu.read_texture(t)
        });
        if let Some(dir) = std::env::var_os("VISO_SCENE_SHOTS") {
            for (who, px) in [("cpu", &cpu), ("metal", &metal)] {
                let png = viso_image::snapshot::Image::from_bgra8(W, H, px).to_png();
                let path = std::path::Path::new(&dir).join(format!("{name}-{who}.png"));
                std::fs::write(path, png).unwrap();
            }
        }
        let off = cpu
            .as_chunks::<4>()
            .0
            .iter()
            .zip(metal.as_chunks::<4>().0)
            .filter(|(a, b)| {
                a.iter()
                    .zip(b.iter())
                    .any(|(x, y)| x.abs_diff(*y) > tolerance)
            })
            .count();
        assert!(
            off * 100 < (W * H) as usize,
            "{name}: {off} of {} pixels differ",
            W * H
        );
    }
}

/// A cube over a ground slab, lit from up and to the left, seen from the
/// front and above, casting with `shadows`.
fn shadowed_frame(k: &mut Kit, shadows: Shadows) {
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: vec3(0.0, 4.0, 6.0),
        target: Vec3::ZERO,
        ..Camera::default()
    };
    frame.background = LinearStraight::new(0.0, 0.0, 1.0, 1.0);
    frame.lighting.direction = vec3(-1.0, 1.2, 0.0);
    frame.lighting.shadows = shadows;
    let white = LinearStraight::new(0.8, 0.8, 0.8, 1.0);
    frame.push(
        Shape::Cube,
        instance(
            vec3(0.0, 0.0, 0.0),
            Quat::IDENTITY,
            vec3(0.6, 0.6, 0.6),
            white,
        ),
    );
    frame.push(
        Shape::Cube,
        instance(
            vec3(0.0, -0.8, 0.0),
            Quat::IDENTITY,
            vec3(4.0, 0.2, 4.0),
            white,
        ),
    );
    k.r.swap_scene(SLOT, &mut frame);
}

#[test]
fn a_cube_shadows_the_ground_beside_it() {
    let full = rect(0.0, 0.0, W as f32, H as f32);
    let mut lit = Kit::new();
    shadowed_frame(&mut lit, Shadows::NONE);
    let lit_px = lit.present(&stream(full, 1));
    let stats = lit.r.take_scene_stats();
    assert_eq!((stats.shadow_passes, stats.shadow_draws), (0, 0));

    let mut shadowed = Kit::new();
    shadowed_frame(&mut shadowed, Shadows::default());
    let shadowed_px = shadowed.present(&stream(full, 1));
    if let Some(dir) = std::env::var_os("VISO_SCENE_SHOTS") {
        let png = viso_image::snapshot::Image::from_bgra8(W, H, &shadowed_px).to_png();
        std::fs::write(std::path::Path::new(&dir).join("lane-shadow.png"), png).unwrap();
    }
    let stats = shadowed.r.take_scene_stats();
    // One caster pass; both meshes are one cube mesh in one material, so one
    // draw per cascade that they reach.
    assert_eq!(stats.shadow_passes, 1);
    assert!((1..=3).contains(&stats.shadow_draws), "{stats:?}");
    assert_eq!(stats.passes, 1);

    // The ground to the cube's right, away from the light, is darker; the
    // ground to its left is not.
    let right = (at(&lit_px, 66, 60), at(&shadowed_px, 66, 60));
    let left = (at(&lit_px, 26, 60), at(&shadowed_px, 26, 60));
    assert!(right.1[0] + 20 < right.0[0], "shadowed: {right:?}");
    assert!(left.1[0].abs_diff(left.0[0]) <= 1, "lit: {left:?}");

    // The same frame again renders nothing, shadows included.
    shadowed.present(&stream(full, 1));
    let stats = shadowed.r.take_scene_stats();
    assert_eq!((stats.passes, stats.shadow_passes), (0, 0));
}

/// A red cube whose top follows joint 1, `lift` along `x`, seen from the
/// front: its skinned mesh, drawn by `push_skinned` in the lit material.
fn skinned_frame(k: &mut Kit, lift: f32, bounds: ([f32; 3], [f32; 3])) {
    let (vertices, indices) = Shape::Cube.geometry();
    let skinned: Vec<SkinnedVertex> = vertices
        .iter()
        .map(|&v| {
            let top = v.position[1] > 0.0;
            SkinnedVertex::new(v, [u16::from(top), 0, 0, 0], [1.0, 0.0, 0.0, 0.0])
        })
        .collect();
    let indices: Vec<u32> = indices.iter().map(|&i| u32::from(i)).collect();
    let mesh =
        k.r.create_scene_skinned_mesh(&mut k.gpu, &skinned, &indices);
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: vec3(0.0, 0.0, 6.0),
        target: Vec3::ZERO,
        ..Camera::default()
    };
    frame.background = LinearStraight::new(0.0, 0.0, 1.0, 1.0);
    frame.lighting.direction = vec3(0.0, 0.0, 1.0);
    let joints = [Mat4::IDENTITY, Mat4::from_translation(vec3(lift, 0.0, 0.0))];
    frame.push_skinned(
        mesh,
        MaterialId::LIT,
        instance(
            Vec3::ZERO,
            Quat::IDENTITY,
            vec3(1.0, 1.0, 1.0),
            LinearStraight::new(1.0, 0.0, 0.0, 1.0),
        ),
        &joints,
        bounds,
    );
    k.r.swap_scene(SLOT, &mut frame);
}

#[test]
fn a_skinned_mesh_follows_its_joints() {
    let full = rect(0.0, 0.0, W as f32, H as f32);
    let posed_bounds = ([-1.0, -1.0, -1.0], [2.5, 1.0, 1.0]);
    let mut rest = Kit::new();
    skinned_frame(&mut rest, 0.0, posed_bounds);
    let rest_px = rest.present(&stream(full, 1));
    let mut posed = Kit::new();
    skinned_frame(&mut posed, 1.5, posed_bounds);
    let posed_px = posed.present(&stream(full, 1));
    if let Some(dir) = std::env::var_os("VISO_SCENE_SHOTS") {
        let png = viso_image::snapshot::Image::from_bgra8(W, H, &posed_px).to_png();
        std::fs::write(std::path::Path::new(&dir).join("lane-skinned.png"), png).unwrap();
    }
    let stats = posed.r.take_scene_stats();
    assert_eq!((stats.draws, stats.instances, stats.culled), (1, 1, 0));
    // The cube's top right of its rest place is red only once its top
    // joint moved it there; its bottom stays where it was.
    let red = |px: &[u8], x, y| {
        let c = at(px, x, y);
        u32::from(c[0]) > u32::from(c[2]) + 60
    };
    let (top_right, bottom) = ((70, 34), (48, 60));
    assert!(!red(&rest_px, top_right.0, top_right.1));
    assert!(red(&posed_px, top_right.0, top_right.1));
    assert!(red(&rest_px, bottom.0, bottom.1) && red(&posed_px, bottom.0, bottom.1));

    // Bounds out of view cull it.
    let mut away = Kit::new();
    skinned_frame(&mut away, 0.0, ([40.0, 0.0, 0.0], [41.0, 1.0, 1.0]));
    away.present(&stream(full, 1));
    let stats = away.r.take_scene_stats();
    assert_eq!((stats.draws, stats.culled), (0, 1), "{stats:?}");
}

/// A cube resting on a ground slab, lit head on so no shadow falls,
/// occluded by `occlusion`.
fn resting_cube(k: &mut Kit, occlusion: AmbientOcclusion) {
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: vec3(0.0, 2.5, 5.0),
        target: vec3(0.0, -0.5, 0.0),
        ..Camera::default()
    };
    frame.background = LinearStraight::new(0.0, 0.0, 0.0, 1.0);
    frame.lighting.direction = vec3(0.0, 0.0, 1.0);
    frame.lighting.shadows = Shadows::NONE;
    frame.lighting.occlusion = occlusion;
    let white = LinearStraight::new(0.8, 0.8, 0.8, 1.0);
    frame.push(
        Shape::Cube,
        instance(Vec3::ZERO, Quat::IDENTITY, vec3(0.7, 0.7, 0.7), white),
    );
    frame.push(
        Shape::Cube,
        instance(
            vec3(0.0, -0.9, 0.0),
            Quat::IDENTITY,
            vec3(4.0, 0.2, 4.0),
            white,
        ),
    );
    k.r.swap_scene(SLOT, &mut frame);
}

#[test]
fn ambient_occlusion_shades_where_the_cube_meets_the_ground() {
    let full = rect(0.0, 0.0, W as f32, H as f32);
    let mut open = Kit::new();
    resting_cube(&mut open, AmbientOcclusion::NONE);
    let open_px = open.present(&stream(full, 1));
    assert_eq!(open.r.take_scene_stats().occlusion_passes, 0);
    let mut occluded = Kit::new();
    resting_cube(
        &mut occluded,
        AmbientOcclusion {
            strength: 1.0,
            ..AmbientOcclusion::default()
        },
    );
    let occluded_px = occluded.present(&stream(full, 1));
    if let Some(dir) = std::env::var_os("VISO_SCENE_SHOTS") {
        let png = viso_image::snapshot::Image::from_bgra8(W, H, &occluded_px).to_png();
        std::fs::write(std::path::Path::new(&dir).join("lane-occlusion.png"), png).unwrap();
    }
    let stats = occluded.r.take_scene_stats();
    assert_eq!((stats.occlusion_passes, stats.passes), (1, 1));
    // Occlusion only ever darkens, and darkens the ground along the cube's
    // base most.
    let mut darker = 0;
    let mut most = 0;
    for (a, b) in open_px
        .as_chunks::<4>()
        .0
        .iter()
        .zip(occluded_px.as_chunks::<4>().0)
    {
        assert!(b[1] <= a[1] + 1, "occlusion never brightens: {a:?} {b:?}");
        let d = a[1].saturating_sub(b[1]);
        most = most.max(d);
        if d >= 3 {
            darker += 1;
        }
    }
    assert!(
        darker > 20 && most >= 5,
        "{darker} pixels occluded, at most by {most}"
    );
    let side = (at(&open_px, 32, 55), at(&occluded_px, 32, 55));
    assert!(side.1[1] + 4 <= side.0[1], "beside the base: {side:?}");
    let corner = (at(&open_px, 6, 88), at(&occluded_px, 6, 88));
    assert!(
        corner.1[1].abs_diff(corner.0[1]) <= 2,
        "open ground: {corner:?}"
    );
}
