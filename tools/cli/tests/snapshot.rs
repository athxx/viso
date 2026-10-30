//! `viso snapshot` end to end. What fails before a build — a target other
//! than headless, nothing to take, a scenario that does not parse, two
//! scenarios taking one snapshot — runs on scratch projects. Capturing,
//! comparing and updating build a probe app headless, so that test is
//! ignored by default:
//!
//! `cargo test -p viso-cli --test snapshot -- --ignored --nocapture`.

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
        let dir = std::env::temp_dir().join(format!(
            "viso-cli-snapshot-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let s = Self(dir);
        s.write(
            "Viso.toml",
            "[package]\nname = \"demo\"\nlanguage = \"1.0\"\n",
        )
        .write(
            "src/main.vs",
            "export component App { view { Text(\"Hi\") } }\n",
        );
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

#[test]
fn only_headless_takes_snapshots_here() {
    let s = Scratch::new("target");
    let out = viso(&s.0, &["snapshot", "compare", "--target", "ios", "--json"]);
    assert_eq!(code(&out), 3);
    assert!(
        stdout(&out).contains(r#""code":"ENV_SNAPSHOT_TARGET""#),
        "{}",
        stdout(&out)
    );
}

#[test]
fn nothing_to_take_is_the_environment_s() {
    let s = Scratch::new("nothing");
    let out = viso(&s.0, &["snapshot", "capture"]);
    assert_eq!(code(&out), 3);
    assert!(
        stderr(&out).contains("error[ENV_TEST_INPUT]: no `tests/ui` directory"),
        "{}",
        stderr(&out)
    );
    s.write("tests/ui/plain.tape", "expect \"Hi\"\n");
    let out = viso(&s.0, &["snapshot", "compare"]);
    assert_eq!(code(&out), 3);
    assert!(
        stderr(&out).contains("no scenario in `tests/ui` takes a snapshot"),
        "{}",
        stderr(&out)
    );
    let out = viso(&s.0, &["snapshot", "capture", "Nope"]);
    assert_eq!(code(&out), 3);
    assert!(
        stderr(&out).contains("no scenario `Nope` (`tests/ui/Nope.tape`) and no component `Nope`"),
        "{}",
        stderr(&out)
    );
    let out = viso(&s.0, &["snapshot", "update", "plain", "--filter", "wide"]);
    assert_eq!(code(&out), 3);
    assert!(
        stderr(&out).contains("the scenario `plain` takes no snapshot whose name contains `wide`"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn scenarios_are_checked_before_a_build() {
    let s = Scratch::new("invalid");
    s.write("tests/ui/bad.tape", "expect snapshot \"a b\"\n")
        .write("tests/ui/one.tape", "expect snapshot \"home\"\n")
        .write(
            "tests/ui/two.tape",
            "expect \"Hi\"\nexpect snapshot \"home\"\n",
        );
    let out = viso(&s.0, &["snapshot", "compare"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.contains("error[TEST_INPUT_INVALID]"), "{err}");
    assert!(err.contains("tests/ui/bad.tape:1"), "{err}");
    assert!(
        err.contains("the scenario `one` takes a snapshot named `home` too"),
        "{err}"
    );
    assert!(err.contains("tests/ui/two.tape:2"), "{err}");
    assert!(!err.contains("Compiling"), "nothing is built: {err}");
}

#[test]
fn a_component_shown_alone_needs_defaults_for_its_inputs() {
    let s = Scratch::new("needy");
    s.write(
        "src/tag.vs",
        "export component Tag {\n    input text: String;\n    view { Text(text) }\n}\n",
    );
    let out = viso(&s.0, &["snapshot", "capture", "Tag"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.contains("error[TEST_INPUT_INVALID]"), "{err}");
    assert!(
        err.contains(
            "the component `tag::Tag` cannot be shown alone: it needs `text`, which has no default"
        ),
        "{err}"
    );
    assert!(err.contains("src/tag.vs:1"), "{err}");
    assert!(!err.contains("Compiling"), "nothing is built: {err}");
}

#[test]
fn update_snapshots_is_the_ui_domain_s() {
    let s = Scratch::new("domain");
    let out = viso(&s.0, &["test", "game", "--update-snapshots"]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("viso test ui --update-snapshots"));
}

/// The lines of `stdout` that are events of `kind`.
fn events<'a>(stdout: &'a str, kind: &str) -> Vec<&'a str> {
    let tag = format!(r#""type":"{kind}""#);
    stdout.lines().filter(|l| l.contains(&tag)).collect()
}

#[test]
#[ignore = "builds a probe app headless"]
fn snapshots_are_captured_compared_and_updated() {
    let app = support::project(
        "snapshot",
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
                b"export component App {\n    state on = false;\n    view {\n        Column {\n            \
                  Stack { width: 120dp; height: 40dp; background: if on { #ff3300 } else { #0055ff }; }\n            \
                  Button(\"Flip\") { width: 120dp; height: 40dp; on click { on = !on; } }\n        }\n    }\n}\n",
            ),
            (
                "src/badge.vs",
                b"export component Badge {\n    input wide: Bool = true;\n    view {\n        \
                  Stack { width: 80dp; height: 30dp; background: if wide { #22aa44 } else { #000000 }; }\n    }\n}\n",
            ),
            (
                "tests/ui/flip.tape",
                // A `wait` with the app idle still plays the step after it.
                b"surface 320x200@1\nwait 2\nexpect snapshot \"off\"\nclick \"Flip\"\nexpect snapshot \"on\"\n",
            ),
        ],
    );
    let _ = fs::remove_dir_all(app.join("tests/snapshots"));
    let target = app.parent().unwrap().join("target");
    let viso = |args: &[&str]| -> Output {
        Command::new(env!("CARGO_BIN_EXE_viso"))
            .args(args)
            .current_dir(&app)
            .env("CARGO_TARGET_DIR", &target)
            .output()
            .unwrap()
    };

    let out = viso(&["snapshot", "compare", "--json"]);
    assert_eq!(code(&out), 1, "{}", stderr(&out));
    let json = stdout(&out);
    let tests = events(&json, "test");
    assert_eq!(tests.len(), 2, "{}", stdout(&out));
    assert!(tests.iter().all(|t| t.contains(r#""status":"fail""#)));
    assert!(stdout(&out).contains(r#""code":"TEST_SNAPSHOT_MISSING""#));

    let out = viso(&["snapshot", "capture", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let json = stdout(&out);
    let written = events(&json, "artifact");
    assert_eq!(written.len(), 4, "{}", stdout(&out));
    assert!(written[0].contains(r#""path":"tests/snapshots/off.headless.png""#));
    let sidecar = fs::read_to_string(app.join("tests/snapshots/off.headless.json")).unwrap();
    assert!(sidecar.contains(r#""surface": "320x200@1""#), "{sidecar}");
    assert!(sidecar.contains(r#""size": [320, 200]"#), "{sidecar}");

    let out = viso(&["snapshot", "compare"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(stdout(&out).contains("test result: ok. 2 passed"));

    // The two snapshots differ, so taking `on` where `off` was departs.
    let scenario =
        "surface 320x200@1\nclick \"Flip\"\nexpect snapshot \"off\"\nexpect snapshot \"on\"\n";
    fs::write(app.join("tests/ui/flip.tape"), scenario).unwrap();
    let out = viso(&["snapshot", "compare", "--json"]);
    assert_eq!(code(&out), 1);
    let json = stdout(&out);
    let failed = events(&json, "test");
    let off = failed
        .iter()
        .find(|t| t.contains(r#""name":"off""#))
        .unwrap();
    assert!(
        off.contains(r#""diff":"target/viso/snapshots/off.headless.diff.png""#),
        "{off}"
    );
    assert!(
        app.join("target/viso/snapshots/off.headless.diff.png")
            .is_file()
    );

    let out = viso(&["test", "ui"]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("error[TEST_SNAPSHOT_MISMATCH]"));

    let out = viso(&["snapshot", "update", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        events(&stdout(&out), "artifact").len(),
        2,
        "only `off` is rewritten"
    );

    let out = viso(&["test", "ui"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));

    // A component alone, then held with the scenarios' snapshots.
    let out = viso(&["snapshot", "capture", "Badge", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        stdout(&out).contains(r#""path":"tests/snapshots/Badge.headless.png""#),
        "{}",
        stdout(&out)
    );
    let sidecar = fs::read_to_string(app.join("tests/snapshots/Badge.headless.json")).unwrap();
    assert!(
        sidecar.contains(r#""component": "badge::Badge""#),
        "{sidecar}"
    );
    let out = viso(&["snapshot", "compare", "--json"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let json = stdout(&out);
    let tests = events(&json, "test");
    assert_eq!(tests.len(), 3, "{json}");
    assert!(
        tests.iter().all(|t| t.contains(r#""status":"pass""#)),
        "{json}"
    );
}
