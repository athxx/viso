//! A reader that goes away is no failure: `viso … | head` closes the pipe
//! early, and the command still ends as it would, without a panic.

use std::process::{Command, Stdio};

#[test]
fn a_closed_stdout_ends_the_command_cleanly() {
    let dir = std::env::temp_dir().join(format!("viso-cli-stdout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Viso.toml"), "[package]\nname = \"demo\"\n").unwrap();
    for args in [
        &["explain", "BUILD_FAILED"][..],
        &["config", "show"],
        &["config", "show", "--json"],
        &["schema", "Button"],
    ] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let out = Command::new(env!("CARGO_BIN_EXE_viso"))
            .args(args)
            .current_dir(&dir)
            .stdout(writer)
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
        assert_eq!(out.status.code(), Some(0), "{args:?}: {stderr}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
