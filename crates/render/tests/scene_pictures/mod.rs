//! The scene lane pictures each device draws as the CPU raster does: lit
//! shapes, a mirrored cone, an unlit bar and blended sprites, without and
//! with ambient occlusion, and under a low sun's sky.

#![allow(dead_code)]

use viso_gpu::{GpuBackend, TextureDesc, TextureFormat, TextureId};
use viso_math::{LinearStraight, Quat, Vec3, vec3};
use viso_render::scene3d::{
    AmbientOcclusion, Camera, SceneFrame, SceneSprite, Shape, Sky, instance,
};
use viso_render::{Primitive, Rect, Renderer, SceneDraw, SceneSlot};

/// The side of a picture, in pixels.
pub const SIDE: u32 = 96;

/// One picture: its name, its sky and occlusion, and how far a channel may
/// differ from the CPU raster's (ambient occlusion's point-sampled taps read
/// a texel's neighbour on one device where another reads the texel).
#[derive(Clone, Copy)]
pub struct LanePicture {
    pub name: &'static str,
    pub sky: Option<Sky>,
    pub occlusion: AmbientOcclusion,
    pub tolerance: u8,
}

/// Every picture.
pub fn lane_pictures() -> [LanePicture; 3] {
    // The last under a low sun's sky, its disc in view behind the scene.
    let low_sun = Sky {
        sun: vec3(-0.6, 0.12, -1.0),
        ..Sky::default()
    };
    let ao = AmbientOcclusion::default();
    let picture = |name, sky, occlusion, tolerance| LanePicture {
        name,
        sky,
        occlusion,
        tolerance,
    };
    [
        picture("lane-shapes", None, AmbientOcclusion::NONE, 3),
        picture("lane-shapes-ao", None, ao, 16),
        picture("lane-sky", Some(low_sun), ao, 16),
    ]
}

/// `picture` drawn by `gpu` into a new BGRA8 texture.
pub fn draw_lane<B: GpuBackend>(gpu: &mut B, picture: &LanePicture) -> TextureId {
    let side = SIDE as f32;
    let mut r = Renderer::new(gpu, TextureFormat::Bgra8Unorm);
    r.set_surface_size([side, side], 1.0);
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: vec3(3.0, 2.5, 5.0),
        target: Vec3::ZERO,
        ..Camera::default()
    };
    frame.background = LinearStraight::new(0.5, 0.6, 0.8, 1.0);
    frame.sky = picture.sky;
    frame.lighting.occlusion = picture.occlusion;
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
    let slot = SceneSlot(0);
    r.swap_scene(slot, &mut frame);
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: side,
        h: side,
    };
    r.upload(
        gpu,
        &[Primitive::Scene(SceneDraw {
            rect,
            slot,
            revision: 1,
            mask: None,
        })],
    );
    let target = gpu.create_texture(&TextureDesc {
        width: SIDE,
        height: SIDE,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "lane",
    });
    r.render_to_texture(gpu, target, [0.0, 0.0, 0.0, 1.0], [side, side]);
    target
}
