//! glTF models drawn through the scene lane: a model uploaded into the
//! renderer, posed and pushed into a frame, draws its meshes where its node
//! hierarchy places them.

use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle, SurfaceId};
use viso_math::{LinearStraight, Mat4, Quat, Vec3, vec3};
use viso_render::scene3d::model::{ModelAsset, Pose};
use viso_render::scene3d::{
    AmbientOcclusion, Camera, Lighting, SceneFrame, SceneModel, Shape, Sky, instance,
};
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
    let (lo, hi) = model.bounds(pose).expect("the model draws something");
    (Vec3::from_array(lo), Vec3::from_array(hi))
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
        // Occlusion at the model's scale.
        frame.lighting.occlusion = AmbientOcclusion {
            radius: radius * 0.15,
            bias: radius * 0.004,
            ..AmbientOcclusion::default()
        };
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let shot = |kit: &mut Kit, frame: &mut SceneFrame, revision, file: String| {
            let px = kit.present(frame, revision);
            let image = viso_image::snapshot::Image::from_bgra8(640, 480, &px);
            std::fs::write(std::path::Path::new(&out).join(file), image.to_png()).unwrap();
        };
        // And under an afternoon sun's sky, lit by the sun.
        let sky = Sky {
            sun: vec3(-0.6, 0.45, 0.35),
            ..Sky::default()
        };
        let mut sky_frame = frame.clone();
        sky_frame.lighting = Lighting {
            occlusion: frame.lighting.occlusion,
            ..Lighting::sunlit(&sky)
        };
        sky_frame.sky = Some(sky);
        shot(&mut kit, &mut frame, 1, format!("{name}.png"));
        shot(&mut kit, &mut sky_frame, 2, format!("{name}-sky.png"));
    }
}

/// `json` and its binary chunk `bin` as a `.glb`.
fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
    let mut json = json.as_bytes().to_vec();
    json.resize(json.len().next_multiple_of(4), b' ');
    let mut bin = bin.to_vec();
    bin.resize(bin.len().next_multiple_of(4), 0);
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    for word in [
        0x4654_6c67u32,
        2,
        total as u32,
        json.len() as u32,
        0x4e4f_534a,
    ] {
        out.extend(word.to_le_bytes());
    }
    out.extend(&json);
    out.extend((bin.len() as u32).to_le_bytes());
    out.extend(0x004e_4942u32.to_le_bytes());
    out.extend(&bin);
    out
}

/// A glTF binary chunk being written, with its views and accessors.
#[derive(Default)]
struct Bin {
    bytes: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
}

impl Bin {
    /// A view of `bytes`, appended.
    fn view(&mut self, bytes: &[u8]) -> usize {
        let at = self.bytes.len();
        self.bytes.extend(bytes);
        self.bytes.resize(self.bytes.len().next_multiple_of(4), 0);
        self.views.push(format!(
            r#"{{"buffer":0,"byteOffset":{at},"byteLength":{}}}"#,
            bytes.len()
        ));
        self.views.len() - 1
    }

    /// An accessor of four elements `spec` describes over `bytes`.
    fn accessor(&mut self, bytes: &[u8], spec: &str) -> usize {
        let v = self.view(bytes);
        self.accessors
            .push(format!(r#"{{"bufferView":{v},"count":4,{spec}}}"#));
        self.accessors.len() - 1
    }
}

/// A square of side 2 facing `+z`, unlit in base color `factor`, its four
/// corners (bottom left, bottom right, top right, top left) colored
/// `colors` (normalized bytes) when given; its base color texture `image`
/// (a PNG, sampled at its nearest texel) read through texture coordinate
/// set `uv_set` when given, whose first set is the square's corner at
/// `(0, 0)` everywhere and whose second spans it. `skinned` binds every
/// vertex to one joint at rest.
fn square(
    factor: [f32; 4],
    colors: Option<[[u8; 4]; 4]>,
    image: Option<(&[u8], u32)>,
    skinned: bool,
) -> Vec<u8> {
    let mut b = Bin::default();
    let mut attrs = Vec::new();
    let floats = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
    let corners = [
        -1.0, -1.0, 0.0, 1.0, -1.0, 0.0, 1.0, 1.0, 0.0, -1.0, 1.0, 0.0,
    ];
    let a = b.accessor(
        &floats(&corners),
        r#""componentType":5126,"type":"VEC3","min":[-1,-1,0],"max":[1,1,0]"#,
    );
    attrs.push(format!(r#""POSITION":{a}"#));
    let a = b.accessor(
        &floats(&[0.0, 0.0, 1.0].repeat(4)),
        r#""componentType":5126,"type":"VEC3""#,
    );
    attrs.push(format!(r#""NORMAL":{a}"#));
    if let Some(colors) = colors {
        let a = b.accessor(
            colors.as_flattened(),
            r#""componentType":5121,"normalized":true,"type":"VEC4""#,
        );
        attrs.push(format!(r#""COLOR_0":{a}"#));
    }
    let mut material = format!(r#""baseColorFactor":{factor:?}"#);
    let mut textures = String::new();
    if let Some((png, uv_set)) = image {
        let a = b.accessor(&floats(&[0.0; 8]), r#""componentType":5126,"type":"VEC2""#);
        attrs.push(format!(r#""TEXCOORD_0":{a}"#));
        let a = b.accessor(
            &floats(&[0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0]),
            r#""componentType":5126,"type":"VEC2""#,
        );
        attrs.push(format!(r#""TEXCOORD_1":{a}"#));
        let v = b.view(png);
        material.push_str(&format!(
            r#","baseColorTexture":{{"index":0,"texCoord":{uv_set}}}"#
        ));
        textures = format!(
            r#","images":[{{"bufferView":{v},"mimeType":"image/png"}}],
"samplers":[{{"magFilter":9728,"minFilter":9728,"wrapS":33071,"wrapT":33071}}],
"textures":[{{"source":0,"sampler":0}}]"#
        );
    }
    let mut skin = String::new();
    let mut nodes = r#"{"mesh":0}"#.to_string();
    if skinned {
        let a = b.accessor(&[0u8; 16], r#""componentType":5121,"type":"VEC4""#);
        attrs.push(format!(r#""JOINTS_0":{a}"#));
        let a = b.accessor(
            &floats(&[1.0, 0.0, 0.0, 0.0].repeat(4)),
            r#""componentType":5126,"type":"VEC4""#,
        );
        attrs.push(format!(r#""WEIGHTS_0":{a}"#));
        nodes = r#"{"mesh":0,"skin":0},{}"#.to_string();
        skin = r#","skins":[{"joints":[1]}]"#.to_string();
    }
    let indices: Vec<u8> = [0u16, 1, 2, 0, 2, 3]
        .iter()
        .flat_map(|i| i.to_le_bytes())
        .collect();
    let v = b.view(&indices);
    b.accessors.push(format!(
        r#"{{"bufferView":{v},"count":6,"componentType":5123,"type":"SCALAR"}}"#
    ));
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"extensionsUsed":["KHR_materials_unlit"],
"scenes":[{{"nodes":[0]}}],"nodes":[{nodes}]{skin},
"meshes":[{{"primitives":[{{"attributes":{{{attrs}}},"indices":{i},"material":0}}]}}],
"materials":[{{"pbrMetallicRoughness":{{{material}}},"extensions":{{"KHR_materials_unlit":{{}}}}}}]{textures},
"accessors":[{accessors}],"bufferViews":[{views}],"buffers":[{{"byteLength":{len}}}]}}"#,
        attrs = attrs.join(","),
        i = b.accessors.len() - 1,
        accessors = b.accessors.join(","),
        views = b.views.join(","),
        len = b.bytes.len(),
    );
    glb(&json, &b.bytes)
}

/// `asset` seen from its front, unshadowed and unoccluded, drawn by `gpu`
/// into a `w`×`h` BGRA8 texture read back by `read`: framed on the sphere
/// of `radius` about the origin, or else on its bounds.
fn front_view<B: GpuBackend>(
    gpu: &mut B,
    asset: &ModelAsset,
    [w, h]: [u32; 2],
    radius: Option<f32>,
    read: impl Fn(&mut B, viso_gpu::TextureId) -> Vec<u8>,
) -> Vec<u8> {
    use viso_gpu::{TextureDesc, TextureFormat};
    let mut r = Renderer::new(gpu, TextureFormat::Bgra8Unorm);
    r.set_surface_size([w as f32, h as f32], 1.0);
    let model = r.create_scene_model(gpu, asset);
    let pose = model.rig().rest_pose();
    let (lo, hi) = bounds(&model, &pose);
    let (centre, radius) = match radius {
        Some(r) => (Vec3::ZERO, r),
        None => ((lo + hi) * 0.5, (hi - lo).length() * 0.5),
    };
    let mut frame = SceneFrame::new();
    frame.camera = Camera {
        eye: centre + vec3(0.0, 0.0, radius * 2.2),
        target: centre,
        near: radius * 0.05,
        ..Camera::default()
    };
    frame.background = LinearStraight::new(0.2, 0.22, 0.26, 1.0);
    frame.lighting.direction = vec3(-0.4, 0.5, 1.0);
    frame.lighting.occlusion = AmbientOcclusion::NONE;
    frame.push_model(
        &model,
        &pose,
        &Mat4::IDENTITY,
        LinearStraight::new(1.0, 1.0, 1.0, 1.0),
    );
    r.swap_scene(SLOT, &mut frame);
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: w as f32,
        h: h as f32,
    };
    let stream = [Primitive::Scene(SceneDraw {
        rect,
        slot: SLOT,
        revision: 1,
        mask: None,
    })];
    r.upload(gpu, &stream);
    let target = gpu.create_texture(&TextureDesc {
        width: w,
        height: h,
        format: TextureFormat::Bgra8Unorm,
        render_target: true,
        label: "model",
    });
    r.render_to_texture(gpu, target, [0.0, 0.0, 0.0, 1.0], [w as f32, h as f32]);
    read(gpu, target)
}

/// `asset`'s front view on the CPU raster, and on the Metal device where
/// there is one, which must draw the same.
fn front_views(asset: &ModelAsset, size: [u32; 2], radius: Option<f32>, name: &str) -> Vec<u8> {
    let cpu = front_view(&mut CpuRaster::new(), asset, size, radius, |gpu, t| {
        gpu.read_texture(t)
    });
    let mut shots = vec![("cpu", cpu.clone())];
    #[cfg(target_vendor = "apple")]
    {
        let metal = front_view(
            &mut viso_gpu::MetalBackend::new(),
            asset,
            size,
            radius,
            |gpu, t| gpu.read_texture(t),
        );
        let off = cpu
            .as_chunks::<4>()
            .0
            .iter()
            .zip(metal.as_chunks::<4>().0)
            .filter(|(a, b)| a.iter().zip(b.iter()).any(|(x, y)| x.abs_diff(*y) > 3))
            .count();
        shots.push(("metal", metal));
        assert!(
            off * 100 < (size[0] * size[1]) as usize,
            "{name}: {off} pixels differ between the CPU raster and Metal"
        );
    }
    if let Some(dir) = std::env::var_os("VISO_SCENE_SHOTS") {
        std::fs::create_dir_all(&dir).unwrap();
        for (who, px) in shots {
            let png = viso_image::snapshot::Image::from_bgra8(size[0], size[1], &px).to_png();
            std::fs::write(
                std::path::Path::new(&dir).join(format!("{name}-{who}.png")),
                png,
            )
            .unwrap();
        }
    }
    cpu
}

/// The radius [`square`] is framed by.
const SQUARE: f32 = 1.42;

/// The color at `(x, y)` of a `w`-wide BGRA8 image.
fn rgb_at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * w + x) * 4) as usize;
    [px[i + 2], px[i + 1], px[i]]
}

#[test]
fn vertex_colors_multiply_the_base_color() {
    const S: [u32; 2] = [64, 64];
    let red = [255, 0, 0, 255];
    let blue = [0, 0, 255, 255];
    // The left corners red, the right ones blue; the factor halves red.
    for (skinned, name) in [(false, "model-colors"), (true, "model-colors-skinned")] {
        let bytes = square(
            [0.5, 1.0, 1.0, 1.0],
            Some([red, blue, blue, red]),
            None,
            skinned,
        );
        let asset = ModelAsset::decode_embedded(&bytes).unwrap();
        assert_eq!(
            asset.meshes[0].primitives[0].colors[1],
            [0.0, 0.0, 1.0, 1.0]
        );
        let px = front_views(&asset, S, Some(SQUARE), name);
        // Unlit into a linear target: half red at the left edge, blending
        // to blue at the right.
        let left = rgb_at(&px, S[0], 14, 32);
        let right = rgb_at(&px, S[0], 50, 32);
        assert!(
            (110..=128).contains(&left[0]) && left[1] == 0 && left[2] < 30,
            "{name} {left:?}"
        );
        assert!(
            right[2] > 220 && right[0] < 15 && right[1] == 0,
            "{name} {right:?}"
        );
    }
    // All white, the square needs no colors on the device and draws as
    // its factor.
    let white = [255; 4];
    let bytes = square([0.5, 1.0, 1.0, 1.0], Some([white; 4]), None, false);
    let asset = ModelAsset::decode_embedded(&bytes).unwrap();
    let px = front_views(&asset, S, Some(SQUARE), "model-white");
    assert_eq!(rgb_at(&px, S[0], 32, 32), [128, 255, 255]);
}

#[test]
fn a_map_reads_the_texture_coordinate_set_it_names() {
    const S: [u32; 2] = [64, 64];
    // Two texels: red on the left, green on the right.
    let png =
        viso_image::snapshot::Image::from_bgra8(2, 1, &[0, 0, 255, 255, 0, 255, 0, 255]).to_png();
    for (uv_set, skinned) in [(0, false), (1, false), (1, true)] {
        let bytes = square([1.0; 4], None, Some((&png, uv_set)), skinned);
        let asset = ModelAsset::decode_embedded(&bytes).unwrap();
        let name = format!("model-uv{uv_set}{}", if skinned { "-skinned" } else { "" });
        let px = front_views(&asset, S, Some(SQUARE), &name);
        let (left, right) = (rgb_at(&px, S[0], 20, 32), rgb_at(&px, S[0], 44, 32));
        assert_eq!(left, [255, 0, 0], "{name}");
        // The first set holds the corner's texel everywhere; the second
        // spans the image.
        let want = if uv_set == 1 {
            [0, 255, 0]
        } else {
            [255, 0, 0]
        };
        assert_eq!(right, want, "{name}");
    }
}

/// Draws every model in `VISO_GLTF_SAMPLES` from its front on the CPU
/// raster and on the Metal device, which must agree, into PNGs under
/// `VISO_SCENE_SHOTS`; skipped without both.
#[test]
fn sample_models_draw_alike_on_every_device() {
    let (Some(dir), Some(_)) = (
        std::env::var_os("VISO_GLTF_SAMPLES"),
        std::env::var_os("VISO_SCENE_SHOTS"),
    ) else {
        return;
    };
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("glb") {
            continue;
        }
        let asset = ModelAsset::decode_embedded(&std::fs::read(&path).unwrap()).unwrap();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        front_views(&asset, [480, 360], None, &format!("front-{name}"));
    }
}
