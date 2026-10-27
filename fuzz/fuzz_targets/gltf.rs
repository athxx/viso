//! A self-contained glTF model (`.glb`, or `.gltf` with data URIs): decoding
//! never panics.

#![no_main]

use libfuzzer_sys::fuzz_target;
use viso_render::scene3d::model::ModelAsset;

fuzz_target!(|data: &[u8]| {
    if let Ok(model) = ModelAsset::decode_embedded(data) {
        let _ = model.bytes();
    }
});
