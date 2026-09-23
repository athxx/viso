//! Golden test: render each golden scene through the full renderer → headless
//! rasterizer pipeline and compare the pixels against its committed baseline.
//!
//! The baselines are raw BGRA8 byte dumps (top-left origin) under `golden/`.
//! Set `BLESS=1` to (re)generate them. Comparison is per-channel with a small
//! tolerance so it survives trivial rounding differences. The device backends'
//! tests render the same scenes (`golden_scenes`) against this reference.

mod golden_scenes;

use golden_scenes::{GoldenScene, diff, reference};

/// Per-channel tolerance (in 0..=255) for the headless comparison.
const TOL: u8 = 2;

fn check(scene: GoldenScene) {
    let actual = reference(scene);
    if std::env::var("BLESS").is_ok() {
        let path = scene.golden_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        eprintln!("blessed golden: {}", path.display());
        return;
    }
    let d = diff(&actual, &scene.golden(), TOL);
    assert!(
        d.worst <= TOL,
        "{scene:?} golden mismatch: max per-channel diff {} at {:?} exceeds tolerance {TOL}",
        d.worst,
        d.first_over
    );
}

#[test]
fn quad_scene_matches_golden() {
    check(GoldenScene::Quad);
}

#[test]
fn image_family_scene_matches_golden() {
    check(GoldenScene::ImageFamily);
}

#[test]
fn path_scene_matches_golden() {
    check(GoldenScene::Path);
}

#[test]
fn stroke_scene_matches_golden() {
    check(GoldenScene::Stroke);
}
