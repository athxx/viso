//! glTF models drawn through the scene lane: a model uploaded into the
//! renderer, posed and pushed into a frame, draws its meshes where its node
//! hierarchy places them.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId};
use viso_math::{LinearStraight, Mat4, Quat, Vec3, vec3};
use viso_render::scene3d::model::{ModelAsset, Pose};
use viso_render::scene3d::{Camera, SceneFrame, SceneModel, Shape, instance};
use viso_render::{Primitive, Rect, Renderer, SceneDraw, SceneSlot};

const SLOT: SceneSlot = SceneSlot(0);

struct Kit {
    gpu: CpuRaster,
    surface: SurfaceId,
    r: Renderer,
    size: [u32; 2],
}

impl Kit {
    fn new(w: u32, h: u32) -> Kit {
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, w, h);
        let format = gpu.surface_format(surface);
        let mut r = Renderer::new(&mut gpu, format);
        r.set_surface_size([w as f32, h as f32], 1.0);
        Kit {
            gpu,
            surface,
            r,
            size: [w, h],
        }
    }

    fn present(&mut self, frame: &mut SceneFrame, revision: u64) -> Vec<u8> {
        self.r.swap_scene(SLOT, frame);
        let [w, h] = self.size.map(|n| n as f32);
        let stream = [Primitive::Scene(SceneDraw {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            },
            slot: SLOT,
            revision,
            mask: None,
        })];
        self.r.upload(&mut self.gpu, &stream);
        self.r
            .submit(&mut self.gpu, self.surface, [0.0, 0.0, 0.0, 1.0], [w, h]);
        self.gpu.read_pixels_bgra8(self.surface)
    }
}

/// The box around `model`'s meshes in `pose`.
fn bounds(model: &SceneModel, pose: &Pose) -> (Vec3, Vec3) {
    let rig = model.rig();
    let mut lo = vec3(f32::MAX, f32::MAX, f32::MAX);
    let mut hi = vec3(f32::MIN, f32::MIN, f32::MIN);
    for &n in &rig.order {
        let node = &rig.nodes[n as usize];
        let Some(mesh) = node.mesh else { continue };
        let (a, b) = rig.mesh_bounds[mesh as usize];
        let world = if node.skin.is_some() {
            Mat4::IDENTITY
        } else {
            pose.world[n as usize]
        };
        for i in 0..8 {
            let p = vec3(
                if i & 1 == 0 { a[0] } else { b[0] },
                if i & 2 == 0 { a[1] } else { b[1] },
                if i & 4 == 0 { a[2] } else { b[2] },
            );
            let q = world.transform_point(p);
            lo = vec3(lo.x.min(q.x), lo.y.min(q.y), lo.z.min(q.z));
            hi = vec3(hi.x.max(q.x), hi.y.max(q.y), hi.z.max(q.z));
        }
    }
    (lo, hi)
}

/// A frame looking at `model` from its front and a little above.
fn framed(model: &SceneModel, pose: &Pose) -> SceneFrame {
    let (lo, hi) = bounds(model, pose);
    let centre = (lo + hi) * 0.5;
    let radius = (hi - lo).length() * 0.5;
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: centre + vec3(0.25, 0.2, 1.0).normalize() * (radius * 2.2),
        target: centre,
        near: radius * 0.05,
        ..Camera::default()
    };
    frame.background = LinearStraight::new(0.2, 0.22, 0.26, 1.0);
    frame.push_model(
        model,
        pose,
        &Mat4::IDENTITY,
        LinearStraight::new(1.0, 1.0, 1.0, 1.0),
    );
    frame
}

/// A red triangle, a child of a node lifted two units.
fn triangle() -> Vec<u8> {
    let json = br#"{"asset":{"version":"2.0"},"scenes":[{"nodes":[0]}],
"nodes":[{"translation":[0,2,0],"children":[1]},{"mesh":0}],
"meshes":[{"primitives":[{"attributes":{"POSITION":0},"material":0}]}],
"materials":[{"pbrMetallicRoughness":{"baseColorFactor":[1,0,0,1]}}],
"accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}],
"bufferViews":[{"buffer":0,"byteLength":36}],
"buffers":[{"byteLength":36,"uri":"data:application/octet-stream;base64,AACAvwAAgL8AAAAAAACAPwAAgL8AAAAAAAAAAAAAgD8AAAAA"}]}"#;
    json.to_vec()
}

#[test]
fn a_model_draws_its_mesh_where_its_nodes_place_it() {
    let asset = ModelAsset::decode_embedded(&triangle()).unwrap();
    let mut kit = Kit::new(64, 64);
    let model = kit.r.create_scene_model(&mut kit.gpu, &asset);
    let pose = model.rig().rest_pose();
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: vec3(0.0, 2.0, 5.0),
        target: vec3(0.0, 2.0, 0.0),
        ..Camera::default()
    };
    frame.lighting.direction = vec3(0.0, 0.0, 1.0);
    frame.push_model(
        &model,
        &pose,
        &Mat4::IDENTITY,
        LinearStraight::new(1.0, 1.0, 1.0, 1.0),
    );
    let px = kit.present(&mut frame, 1);
    let rgb = |x: u32, y: u32| {
        let i = ((y * 64 + x) * 4) as usize;
        [px[i + 2], px[i + 1], px[i]]
    };
    // Lifted to the camera's height, the triangle covers the centre.
    let c = rgb(32, 34);
    // A rough red metal (the glTF default metalness), lit.
    assert!(c[0] > 120 && c[1] < 30 && c[2] < 30, "{c:?}");
    // Lowered by a pushed transform, it leaves the centre.
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: vec3(0.0, 2.0, 5.0),
        target: vec3(0.0, 2.0, 0.0),
        ..Camera::default()
    };
    frame.push_model(
        &model,
        &pose,
        &Mat4::from_translation(vec3(0.0, -4.0, 0.0)),
        LinearStraight::new(1.0, 1.0, 1.0, 1.0),
    );
    let px = kit.present(&mut frame, 2);
    let i = ((34 * 64 + 32) * 4) as usize;
    assert!(px[i + 2] < 20, "moved away");
    kit.r.release_scene_model(&mut kit.gpu, &model);
}

/// Draws every model in `VISO_GLTF_SAMPLES` into a PNG under
/// `VISO_SCENE_SHOTS`; skipped without both.
#[test]
fn sample_models_draw() {
    let (Some(dir), Some(out)) = (
        std::env::var_os("VISO_GLTF_SAMPLES"),
        std::env::var_os("VISO_SCENE_SHOTS"),
    ) else {
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("glb") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let asset = ModelAsset::decode_embedded(&bytes).unwrap();
        let mut kit = Kit::new(640, 480);
        let model = kit.r.create_scene_model(&mut kit.gpu, &asset);
        let mut pose = model.rig().rest_pose();
        if !model.rig().animations.is_empty() {
            let d = model.rig().animations[0].duration;
            model.rig().animate(&mut pose, 0, d * 0.3, 1.0);
            pose.solve(model.rig());
        }
        let mut frame = framed(&model, &pose);
        // A ground under it, lit from the left so its shadow falls in view.
        let (lo, hi) = bounds(&model, &pose);
        let (centre, radius) = ((lo + hi) * 0.5, (hi - lo).length() * 0.5);
        let thick = radius * 0.02;
        frame.push(
            Shape::Cube,
            instance(
                vec3(centre.x, lo.y - thick, centre.z),
                Quat::IDENTITY,
                vec3(radius * 3.0, thick, radius * 3.0),
                LinearStraight::new(0.7, 0.7, 0.7, 1.0),
            ),
        );
        frame.lighting.direction = vec3(-0.6, 0.8, 0.35);
        let px = kit.present(&mut frame, 1);
        let image = viso_image::snapshot::Image::from_bgra8(640, 480, &px);
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        std::fs::write(
            std::path::Path::new(&out).join(format!("{name}.png")),
            image.to_png(),
        )
        .unwrap();
    }
}
