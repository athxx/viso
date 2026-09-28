//! `viso check` end to end: the built binary against scratch projects, asserting
//! the exit code (`Viso_CLI.md` section 7) and what reaches stdout and stderr.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// A scratch project directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("viso-cli-{label}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
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

const MANIFEST: &str = "[package]\nname = \"demo\"\nlanguage = \"1.0\"\n";

#[test]
fn a_clean_project_checks_from_a_nested_directory() {
    let s = Scratch::new("clean");
    s.write("Viso.toml", MANIFEST)
        .write(
            "src/widgets/badge.vs",
            "export record Badge { text: String; }",
        )
        .write(
            "src/main.vs",
            "import widgets::badge::{Badge};\ncomponent App { input b: Badge; view { } }",
        );
    let out = viso(&s.0.join("src/widgets"), &["check"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), "checked `demo`: 2 files, no problems\n");
    assert_eq!(stderr(&out), "");
}

#[test]
fn a_source_error_points_at_its_file_line_and_column() {
    let s = Scratch::new("error");
    s.write("Viso.toml", MANIFEST).write(
        "src/app.vs",
        "component App {\n    input b: Badgee;\n    view { }\n}\n",
    );
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.starts_with("error[E2001]: "), "{err}");
    assert!(
        err.contains(
            " --> src/app.vs:2:14\n  |\n2 |     input b: Badgee;\n  |              ^^^^^^\n"
        ),
        "{err}"
    );
    assert_eq!(stdout(&out), "checked `demo`: 1 file, 1 error\n");
}

#[test]
fn an_unsupported_language_version_points_into_the_manifest() {
    let s = Scratch::new("language");
    s.write(
        "Viso.toml",
        "[package]\nname = \"demo\"\nlanguage = \"9.9\"\n",
    )
    .write("src/app.vs", "component App { view { } }");
    let out = viso(&s.0, &["check"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.starts_with("error[E1001]: "), "{err}");
    assert!(
        err.contains(" --> Viso.toml:3:12\n  |\n3 | language = \"9.9\"\n  |            ^^^^^\n"),
        "{err}"
    );
    assert!(err.contains("= note: set `language = \"1.0\"`"), "{err}");
}

#[test]
fn project_may_come_before_or_after_the_subcommand() {
    let s = Scratch::new("explicit");
    s.write("app/Viso.toml", MANIFEST)
        .write("app/src/app.vs", "component App { view { } }");
    for args in [["--project", "app", "check"], ["check", "--project", "app"]] {
        let out = viso(&s.0, &args);
        assert_eq!(code(&out), 0, "{args:?}: {}", stderr(&out));
        assert_eq!(stdout(&out), "checked `demo`: 1 file, no problems\n");
    }
}

#[test]
fn no_manifest_is_a_config_diagnostic() {
    let s = Scratch::new("missing");
    let out = viso(&s.0, &["check", "--project", "."]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).starts_with("error[C0001]: "),
        "{}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "");
}

#[test]
fn quiet_drops_warnings_but_keeps_the_summary() {
    let s = Scratch::new("quiet");
    s.write("Viso.toml", "[package]\nname = \"demo\"\npackge = 1\n")
        .write("src/app.vs", "component App { view { } }");
    let loud = viso(&s.0, &["check"]);
    assert_eq!(code(&loud), 0);
    assert!(
        stderr(&loud).starts_with("warning[C0004]: "),
        "{}",
        stderr(&loud)
    );
    assert!(
        stderr(&loud).contains("3 | packge = 1\n"),
        "{}",
        stderr(&loud)
    );
    assert_eq!(stdout(&loud), "checked `demo`: 1 file, 1 warning\n");

    let quiet = viso(&s.0, &["-q", "check"]);
    assert_eq!(code(&quiet), 0);
    assert_eq!(stderr(&quiet), "");
    assert_eq!(stdout(&quiet), "checked `demo`: 1 file, 1 warning\n");
}

#[test]
fn usage_errors_exit_2_and_help_and_version_exit_0() {
    let cwd = std::env::temp_dir();
    assert_eq!(code(&viso(&cwd, &["check", "--no-such-flag"])), 2);
    assert_eq!(code(&viso(&cwd, &["no-such-command"])), 2);
    assert_eq!(code(&viso(&cwd, &[])), 2);

    let help = viso(&cwd, &["--help"]);
    assert_eq!(code(&help), 0);
    assert!(stdout(&help).contains("check"), "{}", stdout(&help));
    let version = viso(&cwd, &["--version"]);
    assert_eq!(code(&version), 0);
    assert_eq!(
        stdout(&version),
        format!("viso {}\n", env!("CARGO_PKG_VERSION"))
    );
}
