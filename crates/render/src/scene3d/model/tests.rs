use super::*;

/// A little-endian byte buffer of floats and integers.
#[derive(Default)]
struct Bin(Vec<u8>);

impl Bin {
    fn f32s(&mut self, v: &[f32]) -> (usize, usize) {
        let at = self.0.len();
        for f in v {
            self.0.extend_from_slice(&f.to_le_bytes());
        }
        (at, self.0.len() - at)
    }

    fn u16s(&mut self, v: &[u16]) -> (usize, usize) {
        let at = self.0.len();
        for x in v {
            self.0.extend_from_slice(&x.to_le_bytes());
        }
        while !self.0.len().is_multiple_of(4) {
            self.0.push(0);
        }
        (at, v.len() * 2)
    }
}

fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
    let mut json = json.as_bytes().to_vec();
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    let mut bin = bin.to_vec();
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let total = 12 + 8 + json.len() + 8 + bin.len();
    let mut out = Vec::new();
    out.extend_from_slice(&0x4654_6c67u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x4e4f_534au32.to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(&0x004e_4942u32.to_le_bytes());
    out.extend_from_slice(&bin);
    out
}

/// A two-node model: a root holding a child that draws a triangle, the
/// child spinning a quarter turn about `y` over one second, and a
/// two-joint skin over the triangle.
fn spinner() -> Vec<u8> {
    let mut bin = Bin::default();
    let pos = bin.f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
    let idx = bin.u16s(&[0, 1, 2]);
    let times = bin.f32s(&[0.0, 1.0]);
    let s = std::f32::consts::FRAC_1_SQRT_2;
    let rot = bin.f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, s, 0.0, s]);
    let joints = bin.u16s(&[0, 1, 0, 0, 0, 1, 0, 0, 1, 0, 0, 0]);
    let weights = bin.f32s(&[1.0, 0.0, 0.0, 0.0, 0.5, 0.5, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0]);
    let mut ibm = Vec::new();
    for t in [[0.0f32, 0.0, 0.0], [0.0, -1.0, 0.0]] {
        ibm.extend_from_slice(&[1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        ibm.extend_from_slice(&[t[0], t[1], t[2], 1.0]);
    }
    let ibm = bin.f32s(&ibm);
    let view =
        |(o, l): (usize, usize)| format!(r#"{{"buffer":0,"byteOffset":{o},"byteLength":{l}}}"#);
    let json = format!(
        r#"{{
  "asset": {{"version": "2.0"}},
  "scene": 0,
  "scenes": [{{"nodes": [0]}}],
  "nodes": [
    {{"name": "root", "translation": [0, 2, 0], "children": [1, 2]}},
    {{"name": "child", "mesh": 0, "skin": 0}},
    {{"name": "bone", "translation": [0, 1, 0]}}
  ],
  "meshes": [{{"primitives": [{{"attributes": {{"POSITION": 0, "JOINTS_0": 4, "WEIGHTS_0": 5}}, "indices": 1, "material": 0}}]}}],
  "materials": [{{"name": "red", "pbrMetallicRoughness": {{"baseColorFactor": [1, 0, 0, 1], "metallicFactor": 0.25}}}}],
  "skins": [{{"joints": [1, 2], "inverseBindMatrices": 6}}],
  "animations": [{{"name": "spin", "channels": [{{"sampler": 0, "target": {{"node": 1, "path": "rotation"}}}}],
                   "samplers": [{{"input": 2, "output": 3}}]}}],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"}},
    {{"bufferView": 1, "componentType": 5123, "count": 3, "type": "SCALAR"}},
    {{"bufferView": 2, "componentType": 5126, "count": 2, "type": "SCALAR"}},
    {{"bufferView": 3, "componentType": 5126, "count": 2, "type": "VEC4"}},
    {{"bufferView": 4, "componentType": 5123, "count": 3, "type": "VEC4"}},
    {{"bufferView": 5, "componentType": 5126, "count": 3, "type": "VEC4"}},
    {{"bufferView": 6, "componentType": 5126, "count": 2, "type": "MAT4"}}
  ],
  "bufferViews": [{}, {}, {}, {}, {}, {}, {}],
  "buffers": [{{"byteLength": {}}}]
}}"#,
        view(pos),
        view(idx),
        view(times),
        view(rot),
        view(joints),
        view(weights),
        view(ibm),
        bin.0.len()
    );
    glb(&json, &bin.0)
}

fn close(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4)
}

#[test]
fn a_glb_reads_its_mesh_material_and_hierarchy() {
    let model = ModelAsset::decode_embedded(&spinner()).unwrap();
    assert_eq!(model.meshes.len(), 1);
    let p = &model.meshes[0].primitives[0];
    assert_eq!(p.indices, [0, 1, 2]);
    assert_eq!(p.positions.len(), 3);
    assert!(p.skinned());
    assert_eq!(p.weights[1], [0.5, 0.5, 0.0, 0.0]);
    // No normals in the file: flat ones, facing +z for this winding.
    assert!(p.normals.iter().all(|&n| close(n, [0.0, 0.0, 1.0])));
    assert_eq!(model.materials[0].base_color, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(model.materials[0].metallic, 0.25);
    let rig = &model.rig;
    assert_eq!(rig.roots, [0]);
    assert_eq!(rig.order, [0, 1, 2]);
    assert_eq!(rig.nodes[1].parent, Some(0));
    assert_eq!(rig.node("bone"), Some(2));
}

#[test]
fn an_animation_turns_its_node_and_the_pose_composes_down_the_tree() {
    let model = ModelAsset::decode_embedded(&spinner()).unwrap();
    let rig = &model.rig;
    let spin = rig.animation("spin").unwrap();
    assert_eq!(rig.animations[spin].duration, 1.0);
    let mut pose = rig.rest_pose();
    let tip = |pose: &Pose| {
        let p = pose.world[1].transform_point(vec3(1.0, 0.0, 0.0));
        [p.x, p.y, p.z]
    };
    assert!(close(tip(&pose), [1.0, 2.0, 0.0]), "{:?}", tip(&pose));
    rig.animate(&mut pose, spin, 1.0, 1.0);
    pose.solve(rig);
    // A quarter turn about y takes +x to -z, under the root's lift.
    assert!(close(tip(&pose), [0.0, 2.0, -1.0]), "{:?}", tip(&pose));
    rig.animate(&mut pose, spin, 0.5, 1.0);
    pose.solve(rig);
    let h = std::f32::consts::FRAC_1_SQRT_2;
    assert!(close(tip(&pose), [h, 2.0, -h]), "halfway is an eighth turn");
}

#[test]
fn joint_matrices_undo_the_bind_pose() {
    let model = ModelAsset::decode_embedded(&spinner()).unwrap();
    let rig = &model.rig;
    let pose = rig.rest_pose();
    let mut joints = Vec::new();
    rig.joint_matrices(&pose, 0, &mut joints);
    assert_eq!(joints.len(), 2);
    // At rest a vertex lands where the joints' world transforms put the bind
    // pose: lifted by the root.
    let p = joints[1].transform_point(vec3(0.0, 1.0, 0.0));
    assert!(close([p.x, p.y, p.z], [0.0, 3.0, 0.0]), "{p:?}");
    let reach = &rig.skins[0].reach;
    assert_eq!(reach.len(), 2);
    assert!(reach[0].1 > 0.0 && reach[1].1 > 0.0);
}

#[test]
fn a_gltf_reads_its_buffer_from_a_data_uri_and_refuses_to_leave_its_directory() {
    let positions: Vec<u8> = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, -1.0]
        .iter()
        .flat_map(|f| f.to_le_bytes())
        .collect();
    let b64 = {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for c in positions.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for k in 0..4 {
                if k <= c.len() {
                    s.push(T[((n >> (18 - 6 * k)) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        s
    };
    let json = |uri: &str| {
        format!(
            r#"{{"asset":{{"version":"2.0"}},"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
"nodes":[{{"mesh":0}}],"accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3"}}],
"bufferViews":[{{"buffer":0,"byteLength":36}}],"buffers":[{{"byteLength":36,"uri":"{uri}"}}]}}"#
        )
    };
    let embedded = json(&format!("data:application/octet-stream;base64,{b64}"));
    let model = ModelAsset::decode_embedded(embedded.as_bytes()).unwrap();
    assert_eq!(model.meshes[0].primitives[0].positions[2], [0.0, 0.0, -1.0]);
    assert_eq!(model.rig.roots, [0], "no scene: the parentless nodes");

    let asked = std::cell::RefCell::new(Vec::new());
    let mut resolve = |uri: &str| {
        asked.borrow_mut().push(uri.to_owned());
        Some(positions.clone())
    };
    let external = json("mesh%20data.bin");
    assert!(ModelAsset::decode(external.as_bytes(), &mut resolve).is_ok());
    for escape in ["../secret.bin", "/etc/passwd", "C:/x.bin", "a/../../b.bin"] {
        assert!(
            ModelAsset::decode(json(escape).as_bytes(), &mut resolve).is_err(),
            "{escape}"
        );
    }
    assert_eq!(
        *asked.borrow(),
        ["mesh data.bin"],
        "an escaping URI is never read"
    );
}

#[test]
fn hostile_files_fail_without_reading_out_of_bounds() {
    let good = spinner();
    // Every truncation and every single-byte corruption of the header and
    // JSON either decodes or fails; none panics.
    for cut in (0..good.len()).step_by(7) {
        let _ = ModelAsset::decode_embedded(&good[..cut]);
    }
    for at in (0..good.len().min(1200)).step_by(3) {
        let mut bad = good.clone();
        bad[at] = bad[at].wrapping_add(0x5b);
        let _ = ModelAsset::decode_embedded(&bad);
    }
    let cyclic = r#"{"asset":{"version":"2.0"},"nodes":[{"children":[1]},{"children":[0]}],"scenes":[{"nodes":[0]}]}"#;
    assert!(ModelAsset::decode_embedded(cyclic.as_bytes()).is_err());
    for count in ["4000000000", "20000000"] {
        let huge = format!(
            r#"{{"asset":{{"version":"2.0"}},"accessors":[{{"componentType":5126,"count":{count},"type":"VEC3"}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}]}}"#
        );
        assert!(
            ModelAsset::decode_embedded(huge.as_bytes()).is_err(),
            "{count}"
        );
    }
    let v1 = r#"{"asset":{"version":"1.0"}}"#;
    assert!(ModelAsset::decode_embedded(v1.as_bytes()).is_err());
    let draco =
        r#"{"asset":{"version":"2.0"},"extensionsRequired":["KHR_draco_mesh_compression"]}"#;
    assert!(ModelAsset::decode_embedded(draco.as_bytes()).is_err());
}

#[test]
fn cubic_spline_keys_pass_through_their_values() {
    let channel = Channel {
        node: 0,
        path: ChannelPath::Translation,
        interpolation: Interpolation::CubicSpline,
        times: vec![0.0, 2.0],
        // in, value, out per key.
        values: vec![
            0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, //
            0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ],
    };
    assert_eq!(sample(&channel, 3, 0.0)[0], 1.0);
    assert_eq!(sample(&channel, 3, 2.0)[0], 3.0);
    // Flat tangents: the midpoint is the mean, eased.
    assert!((sample(&channel, 3, 1.0)[0] - 2.0).abs() < 1e-6);
    assert!(sample(&channel, 3, 0.5)[0] < 1.5);
}

/// Every `.glb` and `.gltf` in the directory `VISO_GLTF_SAMPLES` names
/// decodes; skipped without it.
#[test]
fn sample_models_decode() {
    let Some(dir) = std::env::var_os("VISO_GLTF_SAMPLES") else {
        return;
    };
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext, "glb" | "gltf") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let base = path.parent().unwrap().to_owned();
        let mut resolve = |uri: &str| std::fs::read(base.join(uri)).ok();
        let model = ModelAsset::decode(&bytes, &mut resolve)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let prims: usize = model.meshes.iter().map(|m| m.primitives.len()).sum();
        let images = model.images.iter().filter(|i| i.width > 0).count();
        eprintln!(
            "{}: {} meshes, {prims} primitives, {} materials, {images}/{} images, {} nodes, {} skins, {} animations",
            path.file_name().unwrap().to_string_lossy(),
            model.meshes.len(),
            model.materials.len(),
            model.images.len(),
            model.rig.nodes.len(),
            model.rig.skins.len(),
            model.rig.animations.len(),
        );
        let mut pose = model.rig.rest_pose();
        for a in 0..model.rig.animations.len() {
            model
                .rig
                .animate(&mut pose, a, model.rig.animations[a].duration * 0.5, 1.0);
            pose.solve(&model.rig);
            assert!(pose.world.iter().all(|m| m.v.iter().all(|f| f.is_finite())));
        }
    }
}
