//! `viso clean` end to end: `target/viso/` goes, `dist/` and everything a link
//! inside points at stay, and a live lock stops it before anything is removed.

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

fn project(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("clean-{label}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("Viso.toml"), "[package]\nname = \"demo\"\n").unwrap();
    dir
}

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

#[test]
fn clean_removes_target_viso_and_nothing_else() {
    let root = project("removes");
    write(&root.join("target/viso/build/0123/app"), &[0; 2048]);
    write(&root.join("target/viso/cache/dsl/x"), b"cache");
    write(&root.join("target/debug/app"), b"cargo's own");
    write(&root.join("dist/macos/app"), b"shipped");
    let outside = root.with_extension("outside");
    write(&outside.join("keep"), b"not ours");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, root.join("target/viso/linked")).unwrap();

    let out = viso(&root, &["clean", "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
    let stdout = text(&out.stdout);
    assert!(stdout.contains(r#""removed":["#), "{stdout}");
    assert!(stdout.contains(r#""freed_bytes":"#), "{stdout}");
    assert!(!root.join("target/viso").exists());
    assert!(root.join("target/debug/app").is_file());
    assert!(root.join("dist/macos/app").is_file());
    assert!(
        outside.join("keep").is_file(),
        "a link is removed, not followed"
    );

    let again = viso(&root, &["clean"]);
    assert_eq!(text(&again.stdout), "nothing to clean\n");
}

#[test]
fn a_live_lock_stops_clean() {
    let root = project("locked");
    write(
        &root.join("target/viso/locks/build-host.lock"),
        b"4123\n1700000000\nbuild cache for `host`\n",
    );
    write(&root.join("target/viso/build/x"), b"x");
    let out = viso(&root, &["clean"]);
    assert_eq!(out.status.code(), Some(3));
    let stderr = text(&out.stderr);
    assert!(stderr.starts_with("error[ENV_LOCK_HELD]: "), "{stderr}");
    assert!(
        stderr.contains("build cache for `host` is held (pid 4123"),
        "{stderr}"
    );
    assert!(
        root.join("target/viso/build/x").is_file(),
        "nothing was removed"
    );
}
