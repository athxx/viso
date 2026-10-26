//! `viso explain` end to end: a compiler, configuration and CLI code each in
//! text and JSON, a lowercase code, an unknown one and a Rust one, and the
//! list. It needs no project.

use std::process::{Command, Output};

fn viso(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(std::env::temp_dir())
        .output()
        .expect("run viso")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("utf-8 stderr")
}

#[test]
fn a_compiler_code_prints_its_explanation_example_and_fix() {
    let output = viso(&["explain", "E3101"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.starts_with("E3101 Unknown Property\nerror, compiler, view\n"),
        "{text}"
    );
    for part in [
        "\nExample:\n    component ",
        "\nFix:\n  ",
        "\nSuggested actions:\n  viso schema <Component>\n",
    ] {
        assert!(text.contains(part), "{part:?} in {text}");
    }
    assert_eq!(
        stdout(&viso(&["explain", "e3101"])),
        text,
        "case does not matter"
    );
}

#[test]
fn json_is_one_result_with_the_whole_entry() {
    let text = stdout(&viso(&["explain", "E2103", "--json"]));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    let result = lines[0];
    assert!(result.contains(r#""type":"result""#), "{result}");
    for key in [
        r#""payload":{"code":"E2103","producer":"compiler","severity":"error","stage":"type","title":"Type Mismatch","explanation":""#,
        r#""example_kind":"source","fix":""#,
        r#""suggested_commands":["#,
    ] {
        assert!(result.contains(key), "{key} in {result}");
    }
    assert!(lines[1].contains(r#""exit_code":0"#));

    let config = stdout(&viso(&["explain", "C0004", "--json"]));
    assert!(
        config.contains(r#""code":"C0004","producer":"config""#),
        "{config}"
    );
    let cli = stdout(&viso(&["explain", "ENV_LOCK_HELD", "--json"]));
    assert!(
        cli.contains(r#""code":"ENV_LOCK_HELD","producer":"cli""#),
        "{cli}"
    );
    assert!(cli.contains(r#""example":null"#), "{cli}");
}

#[test]
fn an_unknown_code_suggests_the_nearest_and_exits_1() {
    let output = viso(&["explain", "E3110"]);
    assert_eq!(output.status.code(), Some(1));
    let err = stderr(&output);
    assert!(
        err.contains("error[EXPLAIN_UNKNOWN]: no diagnostic code is named `E3110`"),
        "{err}"
    );
    assert!(err.contains("did you mean `E3101`?"), "{err}");

    let output = viso(&["explain", "E0308"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("rustc --explain E0308"),
        "{}",
        stderr(&output)
    );

    let json = stdout(&viso(&["explain", "ENV_LOK_HELD", "--json"]));
    assert!(json.contains(r#""code":"EXPLAIN_UNKNOWN""#), "{json}");
    assert!(json.contains("did you mean `ENV_LOCK_HELD`?"), "{json}");
    assert!(json.contains(r#""exit_code":1"#), "{json}");
}

#[test]
fn the_list_has_every_producer() {
    let output = viso(&["explain", "--list"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    for code in [
        "E1001 ",
        "E3002 ",
        "C0012 ",
        "C0014 ",
        "CLI_USAGE ",
        "EXPLAIN_UNKNOWN ",
    ] {
        assert!(
            text.lines().any(|l| l.starts_with(code)),
            "{code} in {text}"
        );
    }
    assert!(
        text.lines()
            .any(|l| l.starts_with("E3002 ") && l.ends_with("(reserved)"))
    );
    let json = stdout(&viso(&["explain", "--list", "--json"]));
    assert!(
        json.contains(r#""payload":{"codes":[{"code":"E1001","producer":"compiler""#),
        "{json}"
    );
    assert!(
        viso(&["explain"]).status.code() == Some(2),
        "a code or --list"
    );
}
