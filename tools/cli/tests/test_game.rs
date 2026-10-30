//! `viso test game` end to end: scratch game projects whose scenarios pass,
//! fail and replay tapes, asserting exit codes, the human lines and the JSON
//! `trace` and `test` events. Drawing a run's frame sheet builds a probe app
//! headless, so that test is ignored by default:
//!
//! `cargo test -p viso-cli --test test_game -- --ignored --nocapture`.

#[allow(
    dead_code,
    reason = "the dev-session half of the support is not used here"
)]
mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("viso-cli-game-{label}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let s = Self(dir);
        s.write(
            "Viso.toml",
            "[package]\nname = \"demo\"\nlanguage = \"1.0\"\n",
        )
        .write("src/main.vs", GAME);
        s
    }

    fn write(&self, relative: &str, text: &str) -> &Self {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        self
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn viso(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// The JSON events of `type`.
fn events<'a>(out: &'a str, kind: &str) -> Vec<&'a str> {
    let tag = format!("\"type\":\"{kind}\"");
    out.lines().filter(|l| l.contains(&tag)).collect()
}

const GAME: &str = r#"
import viso::game::{FixedUpdate, FixedFrame, InputAction};

export system Player implements FixedUpdate {
    @probe state jumps = 0;
    @probe state airborne = false;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) {
            jumps += 1;
            airborne = true;
        }
        if frame.input.released(InputAction::jump) { airborne = false; }
    }
}
"#;

const JUMP: &str = "seed 7\n2: press jump\n3: expect Player.airborne == true\n4: release jump\n6: expect Player.jumps == 1\n";

#[test]
fn a_passing_scenario_reports_ok_with_its_snapshot_hash() {
    let s = Scratch::new("pass");
    s.write("tests/game/jump.tape", JUMP);
    let out = viso(&s.0, &["test", "game"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.starts_with("test game jump ... ok (7 frames, seed 7, snapshot "),
        "{text}"
    );
    assert!(
        text.ends_with("test result: ok. 1 passed; 0 failed; 0 skipped\n"),
        "{text}"
    );

    let out = viso(&s.0, &["test", "game", "jump", "--json"]);
    assert_eq!(code(&out), 0);
    let json = stdout(&out);
    let trace = events(&json, "trace");
    assert_eq!(trace.len(), 7);
    assert!(
        trace[2].contains(r#""payload":{"scenario":"jump","tick":2,"probes":{"Player.jumps":1,"Player.airborne":true}}"#),
        "{}",
        trace[2]
    );
    let test = events(&json, "test");
    assert_eq!(test.len(), 1);
    assert!(
        test[0].contains(r#""name":"jump","domain":"game","status":"pass""#),
        "{}",
        test[0]
    );
    assert!(test[0].contains(r#""seed":7,"frames":7"#), "{}", test[0]);
    assert!(test[0].contains(r#""snapshot_hash":""#), "{}", test[0]);
    assert!(test[0].contains(r#""entities":[]"#), "{}", test[0]);

    // The same build and tape give the same hash; another seed is still
    // the same game here, which draws nothing.
    let again = viso(&s.0, &["test", "game", "jump", "--json"]);
    let hash = |json: &str| {
        let test = events(json, "test")[0].to_owned();
        let at = test.find("\"snapshot_hash\":\"").unwrap() + 17;
        test[at..at + 16].to_owned()
    };
    assert_eq!(hash(&stdout(&again)), hash(&json));
}

#[test]
fn an_unmet_expectation_fails_at_its_line() {
    let s = Scratch::new("fail");
    s.write(
        "tests/game/jump.tape",
        &JUMP.replace("jumps == 1", "jumps == 2"),
    );
    let out = viso(&s.0, &["test", "game"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(
        err.starts_with(
            "error[TEST_EXPECTATION]: after tick 6: expected `Player.jumps` == 2, found 1"
        ),
        "{err}"
    );
    assert!(err.contains(" --> tests/game/jump.tape:5:1"), "{err}");
    assert!(stdout(&out).ends_with("test result: FAILED. 0 passed; 1 failed; 0 skipped\n"));

    let out = viso(&s.0, &["test", "game", "--json"]);
    let json = stdout(&out);
    assert!(events(&json, "test")[0].contains(r#""status":"fail""#));
    assert_eq!(events(&json, "diagnostic").len(), 1);
}

#[test]
fn a_tape_replaces_the_scenarios_input_and_a_seed_its_seed() {
    let s = Scratch::new("tape");
    s.write("tests/game/jump.tape", JUMP)
        .write("runs/late.tape", "seed 9\n5: press jump\n");
    // The scenario's expectations now see a late jump.
    let out = viso(
        &s.0,
        &[
            "test",
            "game",
            "jump",
            "--tape",
            "runs/late.tape",
            "--seed",
            "3",
        ],
    );
    assert_eq!(code(&out), 1);
    assert!(stdout(&out).contains("seed 3"), "{}", stdout(&out));
    // A tape on its own runs under its file's name, for as long as it lasts.
    let out = viso(
        &s.0,
        &["test", "game", "--tape", "runs/late.tape", "--frames", "9"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).starts_with("test game late ... ok (9 frames, seed 9"));
}

#[test]
fn a_missing_scenario_or_probe_is_reported() {
    let s = Scratch::new("missing");
    let out = viso(&s.0, &["test", "game", "nope"]);
    assert_eq!(code(&out), 3);
    assert!(
        stderr(&out).starts_with(
            "error[ENV_TEST_INPUT]: no scenario `nope`: `tests/game/nope.tape` does not exist"
        ),
        "{}",
        stderr(&out)
    );
    s.write("tests/game/bad.tape", "1: expect Player.score == 0\n");
    let out = viso(&s.0, &["test", "game", "bad"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).starts_with(
            "error[TEST_INPUT_INVALID]: line 1: `Player.score` is not a `@probe` state"
        ),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_sheet_is_the_game_domain_s() {
    let s = Scratch::new("sheet-usage");
    let out = viso(&s.0, &["test", "ui", "--sheet", "sheet.png"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("`--sheet` is an option of game scenarios"),
        "{}",
        stderr(&out)
    );
}

#[test]
#[ignore = "builds a probe app headless"]
fn a_run_is_drawn_as_a_sheet_of_its_frames() {
    let app = support::project(
        "test_game_sheet",
        &[
            (
                "src/main.rs",
                b"use viso::prelude::*;\nstruct App;\nimpl Application for App {\n    \
                  fn new(_cx: &mut AppCx) -> Self { App }\n    \
                  fn build(&mut self, cx: &mut BuildCx<'_>) { viso::view!(\"app.vs\")(cx); }\n}\n\
                  fn main() { viso::run::<App>(); }\n",
            ),
            (
                "src/app.vs",
                b"export component App { view { Text(\"Hi\") } }\n",
            ),
            (
                "src/game.vs",
                b"import viso::game::quick::{QuickGame, QuickStart, QuickFrame};\n\
                  import viso::game::{SpawnDesc, InputAxis};\n\
                  import viso::game::kit::{CameraRig, Terrain, Prefab};\n\
                  import viso::math::Vec3F32;\n\
                  export system Walk implements QuickGame {\n    \
                  @probe state ticks = 0;\n    \
                  action start(cx: QuickStart) {\n        \
                  cx.kit.terrain(Terrain::flat(40.0));\n        \
                  let h = cx.spawn(SpawnDesc::prefab(Prefab::hero).at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32)));\n        \
                  cx.kit.camera(CameraRig::third_person(h).distance(6.0));\n    }\n    \
                  action fixed(frame: QuickFrame) { ticks += 1; }\n}\n",
            ),
            ("tests/game/walk.tape", b"seed 3\n23: expect Walk.ticks == 24\n"),
        ],
    );
    let target = app.parent().unwrap().join("target");
    let _ = fs::remove_dir_all(app.join("out"));
    let out = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(["test", "game", "walk", "--sheet", "out/walk.png", "--json"])
        .current_dir(&app)
        .env("CARGO_TARGET_DIR", &target)
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}\n{}", stdout(&out), stderr(&out));
    let json = stdout(&out);
    let artifacts = events(&json, "artifact");
    assert!(
        artifacts
            .iter()
            .any(|a| a.contains(r#""kind":"sheet","path":"out/walk.png""#)),
        "{json}"
    );
    let test = events(&json, "test");
    assert!(
        test[0].contains(
            r#""sheet":{"path":"out/walk.png","ticks":[1,3,5,7,9,11,13,15,17,19,21,23]}"#
        ),
        "{}",
        test[0]
    );
    let png = fs::read(app.join("out/walk.png")).expect("the sheet");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    // Twelve 480 × 270 tiles, four to a row, 4-pixel gaps.
    let size = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    assert_eq!((size(16), size(20)), (4 * 480 + 5 * 4, 3 * 270 + 4 * 4));
}
