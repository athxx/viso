//! Every question the CLI asks is asked only at a terminal: off one, each
//! command that would ask fails with the structured error and the flag or
//! command that answers it, and never reads the "yes" waiting on its stdin.

mod stand_in;

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

/// `viso <args> --json` in `cwd` with `home` as its Viso home, an answer
/// of yes to anything waiting on stdin, which stays open.
fn answered(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .arg("--json")
        .current_dir(cwd)
        .env("VISO_HOME", home)
        .env("HOME", home)
        .env("CARGO_TARGET_DIR", cwd.join("target"))
        .env_remove("ANDROID_HOME")
        .env_remove("ANDROID_SDK_ROOT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(b"y\n1\ny\n");
    let out = child.wait_with_output().unwrap();
    drop(stdin);
    out
}

fn code_of(out: &Output) -> String {
    let json = stand_in::text(&out.stdout);
    json.lines()
        .find(|line| line.contains("\"type\":\"diagnostic\""))
        .and_then(|line| stand_in::field(line, "code"))
        .unwrap_or_else(|| panic!("no diagnostic in {json}"))
        .to_owned()
}

#[test]
fn no_question_is_asked_off_a_terminal() {
    let root = stand_in::project("prompts");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();

    // The licenses of what `android use` downloads.
    let out = answered(&root, &home, &["android", "use", "36"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(code_of(&out), "ENV_ANDROID_LICENSE");
    assert!(stand_in::text(&out.stdout).contains("--accept-licenses"));
    assert!(!home.join("android/downloads").exists());

    // Whether to create the standard emulator when there is none.
    let out = answered(&root, &home, &["run", "android"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        stand_in::text(&out.stdout)
    );
    assert_eq!(code_of(&out), "DEVICE_MISSING");
    assert!(stand_in::text(&out.stdout).contains("viso android emulator create"));
    assert!(!home.join("android/emulators").exists());

    // Which emulator to run on when there are several and no default.
    let emulators = home.join("android/emulators");
    fs::create_dir_all(&emulators).unwrap();
    for id in ["pixel", "tablet"] {
        fs::write(
            emulators.join(format!("{id}.toml")),
            format!("preset = \"phone\"\nsystem = \"android-36\"\ndevice = \"viso_{id}\"\n"),
        )
        .unwrap();
    }
    let out = answered(&root, &home, &["run", "android"]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        stand_in::text(&out.stdout)
    );
    assert_eq!(code_of(&out), "DEVICE_AMBIGUOUS");
    assert!(stand_in::text(&out.stdout).contains("viso run android --device pixel"));

    let _ = fs::remove_dir_all(&root);
}
