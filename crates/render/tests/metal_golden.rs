//! The golden scenes rendered on the Metal device (macOS, or the iOS
//! simulator) through the frozen MSL, compared with the headless reference.

#![cfg(target_vendor = "apple")]

mod golden_scenes;

use golden_scenes::{GoldenScene, assert_device_matches, render_to_target};
use viso_gpu::MetalBackend;

#[test]
fn golden_scenes_match_on_metal() {
    let mut gpu = MetalBackend::new();
    for scene in GoldenScene::ALL {
        let target = render_to_target(&mut gpu, scene);
        let pixels = gpu.read_texture(target);
        assert_device_matches(scene, &pixels);
    }
}
