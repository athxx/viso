//! `viso doctor` end to end: the checks a target adds, a gap failing the
//! command, and `--fix` running the fix it names, against a stand-in Rust
//! toolchain whose sysroot is a scratch directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// A scratch directory.
fn scratch(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("doctor-{label}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn doctor(cwd: &Path, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_viso"));
    command.arg("doctor").args(args).current_dir(cwd);
    for (name, value) in env {
        command.env(name, value);
    }
    command.output().unwrap()
}

/// The `doctor_check` event of check `name`.
fn check<'a>(stdout: &'a str, name: &str) -> Option<&'a str> {
    stdout.lines().find(|line| {
        line.contains("\"type\":\"doctor_check\"") && line.contains(&format!("\"name\":\"{name}\""))
    })
}

#[test]
fn a_target_unavailable_here_fails_the_doctor() {
    let dir = scratch("unavailable");
    let out = doctor(&dir, &["web-hybrid", "--json"], &[]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(3), "{stdout}");
    let target = check(&stdout, "target").unwrap_or_else(|| panic!("{stdout}"));
    assert!(target.contains("\"status\":\"error\""), "{target}");
    assert!(target.contains("\"code\":\"C0011\""), "{target}");
    let project = check(&stdout, "project").unwrap();
    assert!(
        project.contains("\"status\":\"warn\""),
        "no project is a warning: {project}"
    );
    assert!(
        project.contains("\"suggestion\":\"viso new <name>\""),
        "{project}"
    );
}

#[cfg(unix)]
#[test]
fn a_missing_target_is_a_gap_that_fix_closes() {
    use std::os::unix::fs::PermissionsExt;

    let dir = scratch("fix");
    let sysroot = dir.join("sysroot");
    fs::create_dir_all(sysroot.join("lib/rustlib")).unwrap();
    let bin = dir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let script = |name: &str, body: String| {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let rustc = script(
        "rustc",
        format!(
            "case \"$1\" in\n  -vV) echo 'release: 9.0.0'; echo 'host: test-host' ;;\n  \
             --print) echo '{}' ;;\nesac\n",
            sysroot.display()
        ),
    );
    script(
        "rustup",
        format!(
            "[ \"$1 $2\" = 'target add' ] && mkdir -p '{}/lib/rustlib/'\"$3\"\n",
            sysroot.display()
        ),
    );
    let path = std::env::join_paths(
        std::iter::once(bin.clone())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let env = [("RUSTC", rustc.as_os_str()), ("PATH", path.as_os_str())];

    let out = doctor(&dir, &["web-gpu", "--json"], &env);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(3), "{stdout}");
    let wasm = check(&stdout, "wasm32").unwrap();
    assert!(wasm.contains("\"status\":\"missing\""), "{wasm}");
    assert!(wasm.contains("\"code\":\"ENV_RUST_TARGET\""), "{wasm}");
    assert!(
        wasm.contains("\"suggestion\":\"rustup target add wasm32-unknown-unknown\""),
        "{wasm}"
    );
    // The host doctor only mentions it.
    let host = doctor(&dir, &["--json"], &env);
    let host_out = text(&host.stdout);
    assert!(
        check(&host_out, "wasm32")
            .unwrap()
            .contains("\"status\":\"warn\"")
    );

    let fixed = doctor(&dir, &["web-gpu", "--fix"], &env);
    let stdout = text(&fixed.stdout);
    let stderr = text(&fixed.stderr);
    assert!(
        stderr.contains("Running `rustup target add wasm32-unknown-unknown`"),
        "{stderr}"
    );
    assert!(
        stdout.contains("[ok] WebAssembly target: `wasm32-unknown-unknown` installed"),
        "{stdout}"
    );
    assert!(sysroot.join("lib/rustlib/wasm32-unknown-unknown").is_dir());

    // A toolchain older than the framework's is an error.
    let old = script(
        "old-rustc",
        "echo 'release: 1.2.0'; echo 'host: test-host'\n".to_owned(),
    );
    let out = doctor(&dir, &["--json"], &[("RUSTC", old.as_os_str())]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(3), "{stdout}");
    let rust = check(&stdout, "rust").unwrap();
    assert!(rust.contains("\"code\":\"ENV_RUST_TOOLCHAIN\""), "{rust}");
    assert!(rust.contains("older than"), "{rust}");
}
