//! The `--json` contract across commands: every line one event in the one
//! envelope, `seq` counting up from 0, a `summary` last and only once whose
//! `exit_code` is the process's, and every diagnostic code one the registry
//! explains.

use std::path::Path;
use std::process::Command;

use viso_ende::JsonValue;

/// A project with one source that does not check, and one that does.
fn project(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("Viso.toml"),
        "[package]\nname = \"demo\"\nversion = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/app.vs"),
        "component App {\n    view {\n        Text { text: missing; }\n    }\n}\n",
    )
    .unwrap();
}

/// The events `viso <args> --json` writes, and its exit code.
fn events(dir: &Path, args: &[&str]) -> (Vec<JsonValue>, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .arg("--json")
        .current_dir(dir)
        .env_remove("VISO_PROFILE")
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let events = stdout
        .lines()
        .map(|line| JsonValue::parse(line).unwrap_or_else(|e| panic!("{args:?}: `{line}`: {e:?}")))
        .collect();
    (events, out.status.code().unwrap())
}

#[test]
fn every_command_keeps_the_event_contract() {
    let dir = std::env::temp_dir().join(format!("viso-cli-json-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    project(&dir);
    let commands: &[&[&str]] = &[
        &["explain", "BUILD_FAILED"],
        &["explain", "NOPE_NOT_A_CODE"],
        &["config", "show"],
        &["config", "validate"],
        &["config", "get", "no.such.key"],
        &["check"],
        &["fmt", "--check"],
        &["schema", "Button"],
        &["package", "web-gpu", "--dry-run"],
        &["package", "--profile", "dev"],
        &["build", "--release", "--profile", "dev"],
        &["export", "html", "--static"],
        &["export", "solid"],
        &["clean"],
    ];
    let mut codes = Vec::new();
    for args in commands {
        let (events, exit) = events(&dir, args);
        assert!(!events.is_empty(), "{args:?} wrote nothing");
        for (seq, event) in events.iter().enumerate() {
            assert_eq!(
                event.get("schema").and_then(JsonValue::as_str),
                Some("viso.cli.event"),
                "{args:?}"
            );
            assert_eq!(
                event.get("schema_version").and_then(JsonValue::as_f64),
                Some(1.0)
            );
            assert_eq!(
                event.get("seq").and_then(JsonValue::as_f64),
                Some(seq as f64),
                "{args:?}: seq counts up from 0"
            );
            for field in ["type", "session_id", "command"] {
                assert!(
                    event.get(field).and_then(JsonValue::as_str).is_some(),
                    "{args:?}: {field}"
                );
            }
            assert!(
                event
                    .get("timestamp_ms")
                    .and_then(JsonValue::as_f64)
                    .is_some()
            );
            if event.get("type").and_then(JsonValue::as_str) == Some("diagnostic") {
                let code = event
                    .get("payload")
                    .and_then(|p| p.get("code"))
                    .and_then(JsonValue::as_str)
                    .expect("a diagnostic has its code");
                codes.push(code.to_owned());
            }
        }
        let summaries = events
            .iter()
            .filter(|e| e.get("type").and_then(JsonValue::as_str) == Some("summary"))
            .count();
        assert_eq!(summaries, 1, "{args:?}: one summary");
        let last = events.last().unwrap();
        assert_eq!(
            last.get("type").and_then(JsonValue::as_str),
            Some("summary"),
            "{args:?}"
        );
        assert_eq!(
            last.get("payload")
                .and_then(|p| p.get("exit_code"))
                .and_then(JsonValue::as_f64),
            Some(f64::from(exit)),
            "{args:?}: the summary's exit is the process's"
        );
    }

    // Every code reported is one `viso explain` knows.
    codes.sort();
    codes.dedup();
    assert!(codes.len() >= 3, "{codes:?}");
    for code in &codes {
        let out = Command::new(env!("CARGO_BIN_EXE_viso"))
            .args(["explain", code])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "`{code}` is not in the registry"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
