//! `viso test game` end to end: scratch game projects whose scenarios pass,
//! fail and replay tapes, asserting exit codes, the human lines and the JSON
//! `trace` and `test` events.

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
        text.ends_with("test result: ok. 1 passed; 0 failed\n"),
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
    assert!(stdout(&out).ends_with("test result: FAILED. 0 passed; 1 failed\n"));

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
