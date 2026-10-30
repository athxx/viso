//! `viso fmt` end to end: rewrites what changes and nothing else, `--check`
//! writes nothing and fails, and a file that does not parse is left alone.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn viso(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

const MESSY: &str = "component App {\n  view {\n      Text(\"hi\")\n  }\n}\n";
const TIDY: &str = "component App {\n    view {\n        Text(\"hi\")\n    }\n}\n";
const BROKEN: &str = "component Broken {\n  view {\n    Text(\"open; })";

fn project(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("fmt-{label}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src/pages")).unwrap();
    fs::write(dir.join("Viso.toml"), "[package]\nname = \"demo\"\n").unwrap();
    fs::write(dir.join("src/app.vs"), MESSY).unwrap();
    fs::write(dir.join("src/pages/home.vs"), TIDY).unwrap();
    dir
}

#[test]
fn check_writes_nothing_and_fails_then_fmt_rewrites_only_what_changes() {
    let root = project("rewrite");
    let check = viso(&root, &["fmt", "--check", "--json"]);
    assert_eq!(check.status.code(), Some(1));
    let stdout = text(&check.stdout);
    assert!(
        stdout.contains(r#""payload":{"file":"src/app.vs","changed":true,"line":2}"#),
        "{stdout}"
    );
    assert!(!stdout.contains("home.vs"), "{stdout}");
    assert_eq!(fs::read_to_string(root.join("src/app.vs")).unwrap(), MESSY);

    let home = root.join("src/pages/home.vs");
    let before = fs::metadata(&home).unwrap().modified().unwrap();
    let fmt = viso(&root.join("src"), &["fmt"]);
    assert_eq!(fmt.status.code(), Some(0), "{}", text(&fmt.stderr));
    assert_eq!(text(&fmt.stdout), "formatted src/app.vs\n");
    assert_eq!(text(&fmt.stderr), "1 of 3 files formatted\n");
    assert_eq!(fs::read_to_string(root.join("src/app.vs")).unwrap(), TIDY);
    assert_eq!(fs::metadata(&home).unwrap().modified().unwrap(), before);

    assert_eq!(viso(&root, &["fmt", "--check"]).status.code(), Some(0));
}

#[test]
fn a_file_that_does_not_parse_is_reported_and_left_alone() {
    let root = project("broken");
    fs::write(root.join("src/broken.vs"), BROKEN).unwrap();
    let out = viso(&root, &["fmt", "src/broken.vs", "src/app.vs"]);
    assert_eq!(out.status.code(), Some(1));
    let stderr = text(&out.stderr);
    assert!(stderr.contains("--> src/broken.vs:3:"), "{stderr}");
    assert_eq!(
        fs::read_to_string(root.join("src/broken.vs")).unwrap(),
        BROKEN
    );
    assert_eq!(fs::read_to_string(root.join("src/app.vs")).unwrap(), TIDY);
}

#[test]
fn the_manifest_is_normalized_and_its_comments_kept() {
    let root = project("manifest");
    fs::write(
        root.join("Viso.toml"),
        "# demo  \n[package]\nname=\"demo\"   # the name\n\n\n",
    )
    .unwrap();
    let out = viso(&root, &["fmt"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("formatted Viso.toml\n"));
    assert_eq!(
        fs::read_to_string(root.join("Viso.toml")).unwrap(),
        "# demo\n[package]\nname = \"demo\" # the name\n"
    );
}

#[test]
fn a_missing_file_is_an_environment_failure() {
    let root = project("missing");
    let out = viso(&root, &["fmt", "src/nope.vs"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).starts_with("error[ENV_SOURCE_UNREADABLE]"));
}
