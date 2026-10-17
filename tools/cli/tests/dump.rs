//! `viso dump` end to end: an invalid file reports its diagnostics and still
//! dumps, a selection that names nothing fails, and `--out` writes the file.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn project(label: &str, source: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("dump-{label}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Viso.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(dir.join("src/app.vs"), source).unwrap();
    dir
}

fn viso(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("run viso")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

const APP: &str = "export component App {\n    state count = 0;\n    view {\n        Text { text: label; }\n    }\n}\n";

#[test]
fn an_invalid_file_reports_its_diagnostics_then_dumps_what_lowered() {
    let dir = project("invalid", APP);
    let out = viso(&dir, &["dump", "hir", "src/app.vs"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("error[E2001]"), "{stderr}");
    assert_eq!(stderr.matches("error[E2001]").count(), 1, "{stderr}");
    let stdout = text(&out.stdout);
    assert!(stdout.starts_with("hir 1\n"), "{stdout}");
    assert!(stdout.contains("  state count: I64"), "{stdout}");

    let out = viso(&dir, &["dump", "ui-ir", "App", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = text(&out.stdout);
    let types: Vec<&str> = stdout
        .lines()
        .filter_map(|l| l.split(r#""type":""#).nth(1)?.split('"').next())
        .collect();
    assert_eq!(types, ["diagnostic", "result", "summary"], "{stdout}");
}

#[test]
fn a_selection_that_names_nothing_fails() {
    let dir = project("unknown", "export component App { view { Text {} } }\n");
    let out = viso(&dir, &["dump", "hir", "Ap"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("error[E2001]: no declaration of the package is named `Ap`"),
        "{stderr}"
    );
    assert!(
        stderr.contains("did you mean `demo::app::App`?"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());

    let out = viso(&dir, &["dump", "ast", "src/missing.vs"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stderr).contains("error[DUMP_INPUT]"));
}

#[test]
fn out_writes_the_dump_to_a_file() {
    let dir = project("out", "export component App { view { Text {} } }\n");
    let target = dir.join("graph.json");
    let out = viso(
        &dir,
        &["dump", "module-graph", "--out", "graph.json", "--json"],
    );
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let written = fs::read_to_string(&target).unwrap();
    assert!(
        written.starts_with(r#"{"dump_version":1,"sections":[{"ir":{"package":"demo","#),
        "{written}"
    );
    assert!(
        text(&out.stdout)
            .contains(r#""payload":{"kind":"module-graph","input":null,"out":"graph.json"}"#)
    );

    // A directory that is not there is made; one where the file would go
    // cannot be written over.
    let out = viso(&dir, &["dump", "ast", "--out", "new/dir/ast.txt"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(dir.join("new/dir/ast.txt").is_file());
    let out = viso(&dir, &["dump", "ast", "--out", "new"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).contains("error[ENV_DUMP_WRITE]"));
    assert!(
        fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().ends_with(".tmp")),
        "a failed write leaves nothing beside it"
    );
}
