//! A game run drawn as a sheet of frames through the software raster: the
//! terrain, the Kit's composite models, a confetti burst and a debug box,
//! each tile stamped with its tick, held to a golden image.

use viso::__test_support::render_game_sheet;
use viso_behavior::game::InputTape;
use viso_behavior::native::Natives;
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;
use viso_image::snapshot::{Image, compare};
use viso_view::headless::{GameSheet, HeadlessSurface};

fn sheet() -> GameSheet {
    let compiled = compile_file_for(
        include_str!("fixtures/sheet.vs"),
        &Origin {
            package: "app".into(),
            module: vec!["sheet".into()],
            language: None,
            ..Default::default()
        },
        Natives::standard(),
        TargetProfile::default(),
    );
    assert!(!compiled.has_errors(), "{:#?}", compiled.diagnostics);
    let module = compiled.behavior.bytecode().expect("bytecode");
    GameSheet {
        module: module.encode(),
        tape: InputTape::new(7, 60).encode(),
        frames: 48,
        tiles: 4,
        tile: HeadlessSurface {
            width: 320,
            height: 180,
            scale: 1.0,
        },
        output: String::new(),
    }
}

fn golden_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/game_sheet.png")
}

#[test]
fn a_game_run_draws_as_a_sheet_of_its_frames() {
    let image = render_game_sheet(&sheet()).expect("the sheet draws");
    // Two by two tiles of 320 × 180 with 4-pixel gaps.
    assert_eq!(
        (image.width, image.height),
        (2 * 320 + 3 * 4, 2 * 180 + 3 * 4)
    );
    assert_eq!(
        render_game_sheet(&sheet()).expect("again"),
        image,
        "a run draws the same every time"
    );
    let path = golden_path();
    if std::env::var("BLESS").is_ok() {
        std::fs::write(&path, image.to_png()).unwrap();
        eprintln!("blessed golden: {}", path.display());
        return;
    }
    let golden = Image::from_png(&std::fs::read(&path).expect("the golden exists; BLESS=1"))
        .expect("the golden decodes");
    let comparison = compare(&golden, &image, 0.05);
    assert!(
        comparison.same_size && comparison.differing == 0,
        "the sheet departs from its golden: {comparison:?}"
    );
}

#[test]
fn a_sheet_reports_a_module_that_does_not_load() {
    let mut bad = sheet();
    bad.module = vec![0xff; 4];
    let error = render_game_sheet(&bad).expect_err("not a module");
    assert!(error.contains("does not load"), "{error}");
}
