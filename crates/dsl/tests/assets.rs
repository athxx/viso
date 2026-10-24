//! Assets in `.vs`: `asset("…")` names a file under the package's `assets/`,
//! checked when compiling and typed by its format — an `ImageAsset` for a
//! raster, which an `Image` shows, an `SvgAsset` for an SVG, which an `Svg`
//! shows, a `VideoAsset` for a video, which a `Video` plays. Every misuse is
//! `E3713`.

use std::path::PathBuf;
use std::rc::Rc;

use viso_behavior::native::Natives;
use viso_dsl::assets::{AssetCatalog, AssetUse};
use viso_dsl::frontend::{Compiled, Origin, compile_file, compile_file_for};
use viso_dsl::hir::TargetProfile;
use viso_ui::resource::{AssetFormat, ResourceId};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    }
}

/// A catalog in `dir` holding a PNG, a JPEG, an SVG and an MP4.
fn catalog(dir: PathBuf) -> AssetCatalog {
    AssetCatalog::of(
        dir,
        [
            ("images/logo.png".to_owned(), AssetFormat::Png),
            ("images/photo.jpg".to_owned(), AssetFormat::Jpeg),
            ("icons/close.svg".to_owned(), AssetFormat::Svg),
            ("clips/intro.mp4".to_owned(), AssetFormat::Mp4),
        ],
    )
}

fn compile_with(source: &str, catalog: AssetCatalog) -> Compiled {
    let profile = TargetProfile {
        assets: Some(Rc::new(catalog)),
        ..TargetProfile::default()
    };
    compile_file_for(source, &origin(), Natives::standard(), profile)
}

fn compile(source: &str) -> Compiled {
    compile_with(source, catalog(PathBuf::from("/nonexistent/assets")))
}

fn errors(compiled: &Compiled) -> Vec<(&str, &str)> {
    compiled
        .errors()
        .map(|d| (d.code, d.message.as_str()))
        .collect()
}

#[test]
fn an_asset_is_typed_by_its_format_and_shown_by_its_node() {
    let source = "export component Card {
    state dark = false;
    state photo = asset(\"images/photo.jpg\");
    view {
        Column {
            Image {
                source: if dark { asset(\"images//logo.png\") } else { photo };
                fit: ImageFit::cover;
                width: 64dp;
            }
            Svg { source: asset(\"icons/close.svg\"); tint: #ff0000; }
        }
    }
}";
    let compiled = compile(source);
    assert_eq!(errors(&compiled), []);
    let use_of = |path: &str, format| AssetUse {
        id: ResourceId::of(path),
        path: path.to_owned(),
        format,
    };
    let mut expected = vec![
        use_of("images/logo.png", AssetFormat::Png),
        use_of("images/photo.jpg", AssetFormat::Jpeg),
        use_of("icons/close.svg", AssetFormat::Svg),
    ];
    expected.sort();
    assert_eq!(compiled.assets, expected, "each asset once, by spelling");
    // The value is the resource: a constant the behavior builds.
    let program = format!("{:?}", compiled.behavior);
    let id = ResourceId::of("images/photo.jpg").0.cast_signed();
    assert!(program.contains(&id.to_string()), "{program}");
}

#[test]
fn an_image_reports_its_source_loading() {
    let source = "export component Card {
    state ready = false;
    state broken = false;
    state wide = 0dp;
    view {
        Column {
            Image {
                source: asset(\"images/photo.jpg\");
                on loaded(size) { ready = true; wide = size.width; }
                on failed { broken = true; }
            }
            Svg { source: asset(\"icons/close.svg\"); on loaded(size) { ready = true; } }
        }
    }
}";
    assert_eq!(errors(&compile(source)), []);
    let elsewhere =
        "export component C { state n = 0; view { Column { on loaded(size) { n = 1; } } } }";
    assert!(
        !errors(&compile(elsewhere)).is_empty(),
        "only a picture loads"
    );
}

#[test]
fn a_video_plays_its_asset_or_url_and_writes_back_its_playing() {
    let source = "export component Clip {
    state playing = true;
    state at = 0ms;
    state done = false;
    view {
        Column {
            Video {
                source: asset(\"clips/intro.mp4\");
                bind playing <=> playing;
                looping: false;
                muted: true;
                volume: 0.5;
                rate: 1.5;
                fit: ImageFit::cover;
                on progress(p) { at = p.position; }
                on ended { done = true; }
                on loaded(size) { done = false; }
            }
            Video { url: \"https://example.com/live.m3u8\"; playing: true; }
        }
    }
}";
    let compiled = compile(source);
    assert_eq!(errors(&compiled), []);
    assert!(
        compiled
            .assets
            .iter()
            .any(|a| a.path == "clips/intro.mp4" && a.format == AssetFormat::Mp4)
    );
    let wrong = "export component C { view { Video { source: asset(\"images/logo.png\"); } } }";
    assert_eq!(
        compile(wrong).errors().map(|d| d.code).collect::<Vec<_>>(),
        ["E2103"]
    );
    let wrong = "export component C { view { Image { source: asset(\"clips/intro.mp4\"); } } }";
    assert_eq!(
        compile(wrong).errors().map(|d| d.code).collect::<Vec<_>>(),
        ["E2103"]
    );
    let built = "export component C { view { Video { source: VideoAsset { resource: 1 }; } } }";
    assert_eq!(
        compile(built).errors().map(|d| d.code).collect::<Vec<_>>(),
        ["E3713"]
    );
}

#[test]
fn a_node_takes_only_its_kind_of_asset() {
    let wrong = "export component C { view { Image { source: asset(\"icons/close.svg\"); } } }";
    let compiled = compile(wrong);
    assert_eq!(
        compiled.errors().map(|d| d.code).collect::<Vec<_>>(),
        ["E2103"]
    );
    let wrong = "export component C { view { Svg { source: asset(\"images/logo.png\"); } } }";
    let compiled = compile(wrong);
    assert_eq!(
        compiled.errors().map(|d| d.code).collect::<Vec<_>>(),
        ["E2103"]
    );
    let wrong = "export component C { view { Image { source: \"images/logo.png\"; } } }";
    let compiled = compile(wrong);
    assert_eq!(
        compiled.errors().map(|d| d.code).collect::<Vec<_>>(),
        ["E2103"]
    );
}

/// The one error compiling a view showing `value` raises.
fn misuse(value: &str, catalog: AssetCatalog) -> (String, String, Vec<String>) {
    let source = format!(
        "export component C {{ state name = \"logo.png\"; view {{ Image {{ source: {value}; }} }} }}"
    );
    let compiled = compile_with(&source, catalog);
    let found: Vec<_> = compiled.errors().collect();
    assert_eq!(found.len(), 1, "{value}: {found:#?}");
    let fixes = found[0]
        .fixes
        .iter()
        .flat_map(|f| f.edits.iter().map(|e| e.replacement.clone()))
        .collect();
    (found[0].code.to_owned(), found[0].message.clone(), fixes)
}

#[test]
fn every_misuse_of_an_asset_is_reported() {
    let none = || catalog(PathBuf::from("/nonexistent/assets"));
    let (code, message, fixes) = misuse("asset(\"images/lgo.png\")", none());
    assert_eq!(code, "E3713");
    assert!(
        message.contains("`assets/images/lgo.png` does not exist"),
        "{message}"
    );
    assert_eq!(fixes, ["\"images/logo.png\""]);

    for outside in ["\"../secret.png\"", "\"/etc/logo.png\"", "\"\""] {
        let (code, message, _) = misuse(&format!("asset({outside})"), none());
        assert_eq!(code, "E3713");
        assert!(message.contains("outside `assets/`"), "{message}");
    }
    let (code, message, _) = misuse("asset(name)", none());
    assert!(
        code == "E3713" && message.contains("string literal"),
        "{message}"
    );
    // Not interpolation, which the language does not have: an invalid escape,
    // and a path that is not the literal it reads as.
    let source = "export component C { state name = \"logo.png\"; view { Image { source: asset(\"\\{name}\"); } } }";
    let compiled = compile_with(source, none());
    assert_eq!(
        compiled.errors().map(|d| d.code).collect::<Vec<_>>(),
        ["E1205", "E3713"]
    );
    let (code, message, _) = misuse("asset(\"a.png\", \"b.png\")", none());
    assert!(code == "E3713" && message.contains("one path"), "{message}");

    // A file present in no asset format.
    let dir = std::env::temp_dir().join(format!("viso-dsl-assets-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("notes.txt"), "not an image").unwrap();
    let scanned = AssetCatalog::scan(&dir).unwrap();
    assert_eq!(scanned.files().count(), 0);
    let (code, message, _) = misuse("asset(\"notes.txt\")", scanned);
    assert!(
        code == "E3713" && message.contains("not an image, SVG or video"),
        "{message}"
    );
    std::fs::remove_dir_all(&dir).unwrap();

    // Only `asset` makes an asset value.
    let (code, message, _) = misuse("ImageAsset { resource: 7 }", none());
    assert!(
        code == "E3713" && message.contains("comes from `asset"),
        "{message}"
    );
}

#[test]
fn a_compile_without_assets_says_so() {
    let source = "export component C { view { Image { source: asset(\"logo.png\"); } } }";
    let compiled = compile_file(source, &origin());
    let found = errors(&compiled);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].0 == "E3713" && found[0].1.contains("no `assets/`"),
        "{found:?}"
    );
}

#[test]
fn a_scan_indexes_assets_by_their_bytes_and_path() {
    let dir = std::env::temp_dir().join(format!("viso-dsl-scan-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("icons/small")).unwrap();
    std::fs::write(dir.join("logo.data"), b"\x89PNG\r\n\x1a\nrest").unwrap();
    std::fs::write(
        dir.join("icons/small/x.svg"),
        "<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
    )
    .unwrap();
    std::fs::write(dir.join("icons/readme.md"), "# icons").unwrap();
    let scanned = AssetCatalog::scan(&dir).unwrap();
    let files: Vec<_> = scanned.files().collect();
    assert_eq!(
        files,
        [
            ("icons/small/x.svg", AssetFormat::Svg),
            ("logo.data", AssetFormat::Png)
        ]
    );
    assert_eq!(
        scanned.lookup("./icons\\small/x.svg").map(|(p, ..)| p),
        Ok("icons/small/x.svg".to_owned())
    );
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(AssetCatalog::scan(&dir).unwrap().files().count(), 0);
}
