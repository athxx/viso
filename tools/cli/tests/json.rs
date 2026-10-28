//! The `--json` contract across commands: every line one event in the one
//! envelope, `seq` counting up from 0, a `summary` last and only once whose
//! `exit_code` is the process's, and every diagnostic code one the registry
//! explains — held for every command the help lists but `lsp` and
//! `completion`, each invoked the quick way, a failure as good as a success.

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

/// The events `viso --json <args>` writes, and its exit code.
fn events(dir: &Path, args: &[&str]) -> (Vec<JsonValue>, i32) {
    let out = Command::new(env!("CARGO_BIN_EXE_viso"))
        .arg("--json")
        .args(args)
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
        &["profile"],
        &["profile", "headless", "--seconds", "1"],
        &["inspect", "tree"],
        &["inspect", "node", "Button >"],
        &["inspect", "frame", "--window", "1"],
        &["inspect", "events", "--seconds", "1"],
        &["inspect", "query", "Button"],
        &["inspect"],
        &["clean"],
        &["new"],
        &["doctor", "web-hybrid"],
        &["dump", "hir", "src/app.vs"],
        &["config", "path"],
        &["android", "list"],
        &["android", "use", "1"],
        &["android", "doctor"],
        &["android", "adb", "--device", "no-such-profile", "devices"],
        &["android", "emulator", "list"],
        &["android", "emulator", "create"],
        &["android", "emulator", "start", "no-such-profile"],
        &["android", "emulator", "stop", "no-such-profile"],
        &["android", "emulator", "delete", "no-such-profile"],
        &["ios", "list"],
        &["ios", "use", "not-a-runtime"],
        &["ios", "doctor"],
        &["ios", "simulator", "list"],
        &["ios", "simulator", "create"],
        &["ios", "simulator", "start", "no-such-profile"],
        &["ios", "simulator", "stop", "no-such-profile"],
        &["ios", "simulator", "delete", "no-such-profile"],
        &["game", "peek"],
        &["game", "record"],
        &["game", "replay"],
        &["run", "web-hybrid"],
        &["serve", "web-hybrid"],
        &["snapshot", "capture"],
        &["snapshot", "compare"],
        &["snapshot", "update"],
        &["test", "ui"],
    ];
    let mut codes = Vec::new();
    let mut covered = std::collections::BTreeSet::new();
    for args in commands {
        let (events, exit) = events(&dir, args);
        if let Some(command) = events
            .first()
            .and_then(|e| e.get("command"))
            .and_then(JsonValue::as_str)
        {
            covered.insert(command.to_owned());
        }
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

    // Every command the help lists is held to the contract.
    let listed = leaves(&[]);
    for path in [
        "new",
        "android adb",
        "ios simulator delete",
        "inspect",
        "inspect events",
        "snapshot update",
    ] {
        assert!(
            listed.iter().any(|p| p == path),
            "the help lists no `{path}`: {listed:?}"
        );
    }
    let missing: Vec<String> = listed
        .into_iter()
        .filter(|path| !["lsp", "completion"].contains(&path.as_str()))
        .filter(|path| !covered.contains(path))
        .collect();
    assert!(missing.is_empty(), "not held to the contract: {missing:?}");

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

/// The command paths under `viso <path>` the help lists, each with no
/// subcommands of its own, or with one it runs bare (`inspect`).
fn leaves(path: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(path)
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(out.stdout).unwrap();
    let mut names = Vec::new();
    let mut listing = false;
    for line in help.lines() {
        let heading = line == "Commands:"
            || (!line.is_empty()
                && line
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c == ' ' || c == '/'));
        if heading {
            listing = true;
        } else if line.is_empty() {
            listing = false;
        } else if listing
            && let Some(name) = line.strip_prefix("  ")
            && let Some(name) = name.split_whitespace().next().filter(|n| *n != "help")
        {
            names.push(name.to_owned());
        }
    }
    let here = path.join(" ");
    if names.is_empty() {
        return vec![here];
    }
    let mut leaves = Vec::new();
    if here == "inspect" {
        leaves.push(here);
    }
    for name in &names {
        let mut next = path.to_vec();
        next.push(name);
        leaves.extend(self::leaves(&next));
    }
    leaves
}

/// `line` without what differs between two runs of the same command: the
/// envelope's clock and session, and the summary's elapsed time.
fn steady(line: &str) -> String {
    let mut line = line.to_owned();
    for key in ["\"timestamp_ms\":", "\"elapsed_ms\":", "\"session_id\":"] {
        if let Some(at) = line.find(key) {
            let value = at + key.len();
            let end = if line[value..].starts_with('"') {
                value + 1 + line[value + 1..].find('"').unwrap() + 1
            } else {
                value + line[value..].find(|c: char| !c.is_ascii_digit()).unwrap()
            };
            line.replace_range(value..end, "_");
        }
    }
    line
}

#[test]
fn the_commands_an_agent_asks_answer_the_same_twice() {
    let dir = std::env::temp_dir().join(format!("viso-json-steady-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    project(&dir);
    for args in [
        &["check"][..],
        &["schema", "Button"],
        &["schema", "--search", "button"],
        &["dump", "hir", "src/app.vs"],
        &["explain", "E2001"],
        &["fmt", "--check"],
        &["config", "show"],
    ] {
        let run = || {
            let out = Command::new(env!("CARGO_BIN_EXE_viso"))
                .arg("--json")
                .args(args)
                .current_dir(&dir)
                .env_remove("VISO_PROFILE")
                .output()
                .unwrap();
            String::from_utf8(out.stdout)
                .unwrap()
                .lines()
                .map(steady)
                .collect::<Vec<_>>()
        };
        let first = run();
        assert!(!first.is_empty(), "{args:?}");
        assert_eq!(first, run(), "{args:?}");
        for line in &first {
            assert!(!line.contains("\x1b["), "{args:?}: color in {line}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
