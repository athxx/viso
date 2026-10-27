//! Writes the starting corpus of the fuzz targets in `fuzz/`: packages, a
//! module and packets the compiler and a session produce, and the image, SVG,
//! glTF, font and text samples the decoders read. Run by the fuzz workflow
//! before fuzzing; ignored otherwise.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use viso_behavior::game::{RollbackSession, Scheduler, SessionConfig};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Vm};
use viso_dsl::aot::{emit_package, emit_view_package};
use viso_dsl::frontend::{Origin, compile_file, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};
use viso_dsl::hotreload::plan_view;
use viso_ende::Encode;

const VIEW: &str = r#"
component Card {
    state count = 0;
    state open = false;
    view {
        Column {
            gap: 8dp;
            Text { text: format("{} taps", count); }
            Button { text: "Tap"; on click { count += 1; } }
            if open {
                Text { text: "open"; }
            }
            for i in [1, 2, 3] key i {
                Text { text: format("row {}", i); }
            }
        }
    }
}
"#;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
<defs><linearGradient id="g"><stop offset="0" stop-color="#f80"/><stop offset="1" stop-color="#08f"/></linearGradient></defs>
<rect x="2" y="2" width="28" height="28" rx="4" fill="url(#g)"/>
<path d="M4 16 C 8 4, 24 4, 28 16 S 20 28, 16 28 Z" stroke="#000" stroke-width="2" fill="none"/>
<circle cx="16" cy="16" r="5" opacity="0.5"/>
</svg>"##;

const TEXTS: [&str; 5] = [
    "Hello, world! A line\nand another.",
    "שלום עולם and مرحبا 123",
    "日本語のテキストと한국어",
    "ภาษาไทยไม่มีช่องว่าง",
    "e\u{301}👩‍👩‍👧 flag 🇩🇪 tab\tend",
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn write(target: &str, name: &str, bytes: &[u8]) {
    let dir = root().join("fuzz/corpus").join(target);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), bytes).unwrap();
}

fn copy(target: &str, files: &[&str]) {
    for file in files {
        let bytes = std::fs::read(root().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        let name = Path::new(file).file_name().unwrap().to_str().unwrap();
        write(target, name, &bytes);
    }
}

fn origin(module: &str) -> Origin {
    Origin {
        package: "fuzz".to_owned(),
        module: vec![module.to_owned()],
        ..Default::default()
    }
}

fn game_module() -> Rc<Module> {
    let profile = TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    };
    let source = std::fs::read_to_string(root().join("fuzz/game.vs")).unwrap();
    let compiled = compile_file_for(&source, &origin("game"), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn session(module: &Rc<Module>, local: u32) -> RollbackSession {
    let mut vm = Vm::new(module.clone(), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let game = Scheduler::with_seed(vm, 1).expect("a started game");
    RollbackSession::new(game, SessionConfig::new(2, local)).expect("a session")
}

#[test]
#[ignore = "writes the fuzz corpus's seeds"]
fn fuzz_seeds() {
    let plan = plan_view(VIEW, &origin("view")).expect("the view plans");
    write("aot_package", "card", &emit_package(&plan).encode_to_vec());
    write(
        "view_package",
        "card",
        &emit_view_package(&plan).encode_to_vec(),
    );

    let view = compile_file(VIEW, &origin("view"));
    let errors: Vec<_> = view.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = view.behavior.bytecode().expect("verified bytecode");
    write("behavior_module", "card", &module.encode());
    let game = game_module();
    write("behavior_module", "game", &game.encode());

    let (mut a, mut b) = (session(&game, 1), session(&game, 0));
    for step in 0..3 {
        a.advance(4);
        b.advance(4);
        let packet = a.packet_for(0);
        b.receive(&packet).expect("a packet");
        write("game_packet", &format!("step-{step}"), &packet);
    }

    copy(
        "raster",
        &[
            "crates/image/tests/fixtures/anim.png",
            "crates/image/tests/fixtures/anim.gif",
            "crates/image/tests/fixtures/anim.webp",
            "crates/image/tests/fixtures/two_alpha.webp",
            "crates/image/tests/fixtures/two_lossy.webp",
            "crates/image/tests/fixtures/two.avif",
            "crates/image/tests/fixtures/two.bmp",
            "crates/image/tests/fixtures/two.qoi",
            "crates/image/src/fixtures/two_tone.jpg",
        ],
    );
    copy("svg", &["examples/images/assets/icons/star.svg"]);
    write("svg", "shapes.svg", SVG.as_bytes());
    copy(
        "font",
        &[
            "crates/render/src/fixtures/DejaVuSans-subset.ttf",
            "crates/macros/tests/fixtures/VisoSym-subset.otf",
            "crates/text/tests/fixtures/ColrFixture.ttf",
        ],
    );
    for (i, text) in TEXTS.iter().enumerate() {
        write("text", &format!("text-{i}"), text.as_bytes());
    }
    write("gltf", "triangle.gltf", triangle().as_bytes());
}

/// One triangle whose positions lie in a data URI.
fn triangle() -> String {
    let positions: Vec<u8> = [-1.0f32, -1.0, 0.0, 1.0, -1.0, 0.0, 0.0, 1.0, 0.0]
        .iter()
        .flat_map(|v| v.to_le_bytes())
        .collect();
    let data = viso_ende::base64::encode(&positions);
    format!(
        r#"{{"asset":{{"version":"2.0"}},"scenes":[{{"nodes":[0]}}],"nodes":[{{"mesh":0}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":0}}}}]}}],
"accessors":[{{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[-1,-1,0],"max":[1,1,0]}}],
"bufferViews":[{{"buffer":0,"byteLength":36}}],
"buffers":[{{"byteLength":36,"uri":"data:application/octet-stream;base64,{data}"}}]}}"#
    )
}
