//! `viso lsp` end to end: it runs the `viso-lsp` beside it with the editor's
//! stdio and its exit code, and fails as the environment's when there is
//! none.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A directory holding a copy of `viso`, alone.
fn install(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("lsp-{label}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let name = if cfg!(windows) { "viso.exe" } else { "viso" };
    fs::copy(env!("CARGO_BIN_EXE_viso"), dir.join(name)).unwrap();
    dir
}

fn viso(dir: &Path, args: &[&str]) -> Output {
    let name = if cfg!(windows) { "viso.exe" } else { "viso" };
    Command::new(dir.join(name))
        .args(args)
        .current_dir(dir)
        .env("PATH", dir.join("empty"))
        .output()
        .expect("run viso")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn without_a_server_it_fails_as_the_environment() {
    let dir = install("missing");
    let out = viso(&dir, &["lsp", "--stdio"]);
    assert_eq!(out.status.code(), Some(3));
    let stderr = text(&out.stderr);
    assert!(
        stderr.contains("error[ENV_LSP]: no `viso-lsp` next to `viso` or on `PATH`"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn json_has_no_form_here() {
    let dir = install("json");
    let out = viso(&dir, &["lsp", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stdout).contains(r#""code":"CLI_USAGE""#));
}

#[cfg(unix)]
#[test]
fn the_server_beside_it_gets_the_stdio_and_gives_the_exit() {
    use std::os::unix::fs::PermissionsExt;
    let dir = install("beside");
    let server = dir.join("viso-lsp");
    fs::write(&server, "#!/bin/sh\necho \"server $*\"\nexit 7\n").unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o755)).unwrap();
    let out = viso(&dir, &["lsp", "--stdio"]);
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(text(&out.stdout), "server --stdio\n");
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
}

/// The doctor's `lsp` check, as a `doctor_check` payload.
fn lsp_check(dir: &Path) -> String {
    let out = viso(dir, &["doctor", "--json"]);
    text(&out.stdout)
        .lines()
        .find(|line| line.contains(r#""name":"lsp""#))
        .unwrap_or_else(|| panic!("no lsp check: {}", text(&out.stdout)))
        .to_owned()
}

#[cfg(unix)]
#[test]
fn the_doctor_checks_the_server_is_there_and_of_the_cli_version() {
    use std::os::unix::fs::PermissionsExt;
    let dir = install("doctor");
    let check = lsp_check(&dir);
    assert!(check.contains(r#""status":"warn""#), "{check}");
    assert!(
        check.contains("cargo install viso-lsp --version"),
        "{check}"
    );

    let server = dir.join("viso-lsp");
    let answer = |version: &str| {
        fs::write(&server, format!("#!/bin/sh\necho \"viso-lsp {version}\"\n")).unwrap();
        fs::set_permissions(&server, fs::Permissions::from_mode(0o755)).unwrap();
    };
    answer("9.9.9");
    let check = lsp_check(&dir);
    assert!(check.contains(r#""status":"warn""#), "{check}");
    assert!(check.contains("viso-lsp 9.9.9, this CLI is"), "{check}");

    answer(env!("CARGO_PKG_VERSION"));
    let check = lsp_check(&dir);
    assert!(check.contains(r#""status":"ok""#), "{check}");
}
