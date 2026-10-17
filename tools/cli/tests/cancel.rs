//! Ctrl-C in the middle of a long command: `build`, `package` and `test`
//! interrupted while Cargo runs a build script that never ends. Each exits
//! 130 with a `cancelled` summary, leaves no process it started running, no
//! lock held and no half-written output.

#![cfg(unix)]

mod stand_in;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The stand-in project, packageable: a bundle id beside its name.
fn project(label: &str) -> std::path::PathBuf {
    let root = stand_in::project(label);
    fs::write(
        root.join("Viso.toml"),
        "[package]\nname = \"probe\"\nversion = \"1.2.0\"\nbundle_id = \"dev.viso.probe\"\n",
    )
    .unwrap();
    root
}

/// A build script that writes its pid where it is told, then waits forever.
const HANGING_BUILD: &str = "fn main() {\n    \
    let pid = std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\")).join(\"build.pid\");\n    \
    std::fs::write(pid, std::process::id().to_string()).unwrap();\n    \
    loop { std::thread::sleep(std::time::Duration::from_secs(60)); }\n}\n";

fn alive(pid: libc::pid_t) -> bool {
    // SAFETY: signal 0 only asks whether the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Runs `viso <args> --json` in `root`, interrupts it once the build script
/// runs, and checks what it leaves.
fn interrupted(root: &Path, args: &[&str]) {
    let pid_file = root.join("build.pid");
    let _ = fs::remove_file(&pid_file);
    let mut child = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .arg("--json")
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let lines = std::thread::spawn(move || {
        BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });
    let started = Instant::now();
    let script: libc::pid_t = loop {
        if let Some(pid) = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|t| t.trim().parse().ok())
        {
            break pid;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "`viso {}` ended before its build script ran",
            args.join(" ")
        );
        assert!(
            started.elapsed() < Duration::from_secs(300),
            "the build script did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let viso = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: a plain call on the child not yet reaped.
    unsafe { libc::kill(viso, libc::SIGINT) };
    let status = child.wait().unwrap();
    let lines = lines.join().unwrap();
    let what = args.join(" ");
    assert_eq!(status.code(), Some(130), "`viso {what}`: {lines:#?}");
    let summary = lines.last().expect("a summary");
    assert!(
        summary.contains("\"type\":\"summary\"") && summary.contains("\"status\":\"cancelled\""),
        "`viso {what}`: {summary}"
    );
    let gone = Instant::now();
    while alive(script) {
        assert!(
            gone.elapsed() < Duration::from_secs(5),
            "`viso {what}` left its build script running"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let locks = root.join("target/viso/locks");
    let held: Vec<_> = fs::read_dir(&locks)
        .map(|dir| dir.filter_map(Result::ok).map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(held.is_empty(), "`viso {what}` left {held:?} held");
    if let Ok(dist) = fs::read_dir(root.join("dist")) {
        let staged: Vec<_> = dist
            .filter_map(Result::ok)
            .flat_map(|target| fs::read_dir(target.path()).into_iter().flatten())
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with('.'))
            .collect();
        assert!(staged.is_empty(), "`viso {what}` left {staged:?}");
    }
}

#[test]
fn a_long_command_interrupted_ends_everything_it_started() {
    let root = project("cancel");
    fs::write(root.join("build.rs"), HANGING_BUILD).unwrap();
    interrupted(&root, &["build", "headless"]);
    interrupted(&root, &["package", "--signing", "off"]);
    interrupted(&root, &["test"]);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_build_or_package_meeting_a_held_lock_fails_at_once_naming_its_holder() {
    let root = project("held");
    let locks = root.join("target/viso/locks");
    fs::create_dir_all(&locks).unwrap();
    for (args, lock, held) in [
        (
            &["build", "headless"][..],
            "build-headless.lock",
            "build cache for `headless`",
        ),
        (
            &["package", "--signing", "off"],
            "package-host.lock",
            "package output for `host`",
        ),
    ] {
        let path = locks.join(lock);
        fs::write(&path, format!("4123\n1700000000\n{held}\n")).unwrap();
        let started = Instant::now();
        let out = stand_in::viso(&root, args);
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "it did not wait"
        );
        assert_eq!(
            out.status.code(),
            Some(3),
            "{}",
            stand_in::text(&out.stderr)
        );
        let stderr = stand_in::text(&out.stderr);
        assert!(
            stderr.contains("ENV_LOCK_HELD") && stderr.contains("pid 4123"),
            "{stderr}"
        );
        assert!(path.is_file(), "a lock it does not hold is left alone");
        fs::remove_file(&path).unwrap();
    }
    let _ = fs::remove_dir_all(&root);
}
