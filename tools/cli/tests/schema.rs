//! `viso schema` end to end: the built binary's text and JSON forms and exit
//! codes (`Viso_CLI.md` section 18).

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

#[test]
fn a_widget_schema_prints_its_properties_and_events() {
    let output = viso(&["schema", "Button"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(text.starts_with("viso::widgets::Button\n"), "{text}");
    let text_row = text
        .lines()
        .find(|l| l.starts_with("  text "))
        .expect("text row");
    assert!(text_row.contains(" String "), "{text_row}");
    assert!(
        text_row.ends_with("invalidates: MEASURE|LAYOUT|PAINT|SEMANTICS"),
        "{text_row}"
    );
    assert!(text.contains("\nEvents\n"), "{text}");
    assert!(
        text.lines()
            .any(|l| l.starts_with("  click ") && l.contains("ClickEvent"))
    );
}

#[test]
fn json_is_one_result_event_with_the_schema_object() {
    let output = viso(&["schema", "viso::widgets::Button", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[0].contains(r#""type":"result""#), "{}", lines[0]);
    assert!(
        lines[0].contains(r#""payload":{"kind":"component","symbol":"viso::widgets::Button","version":"1","inputs":["#),
        "{}",
        lines[0]
    );
    assert!(lines[1].contains(r#""type":"summary""#));
    assert!(lines[1].contains(r#""exit_code":0"#));
}

#[test]
fn a_member_and_a_native_symbol_are_queried() {
    let text = stdout(&viso(&["schema", "Button.text"]));
    assert!(text.starts_with("viso::widgets::Button.text\n"), "{text}");
    assert!(!text.contains("Events"), "{text}");

    let text = stdout(&viso(&["schema", "viso::clipboard::write_text"]));
    assert!(
        text.contains("action write_text(text: String) -> ()"),
        "{text}"
    );
    assert!(text.contains("requires: clipboard.write"), "{text}");
}

#[test]
fn search_lists_matching_symbols_and_members() {
    let output = viso(&["schema", "--search", "elapsed"]);
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        stdout(&output),
        "viso::time::Stopwatch.elapsed_ms  action elapsed_ms(this: Stopwatch) -> F64\n"
    );
    let json = stdout(&viso(&["schema", "--search", "elapsed", "--json"]));
    assert!(json.contains(r#""payload":{"query":"elapsed","matches":[{"path":"viso::time::Stopwatch.elapsed_ms","kind":"native_type""#), "{json}");
}

#[test]
fn an_unknown_symbol_is_a_diagnostic_and_exit_1() {
    let output = viso(&["schema", "Buton"]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8_lossy(&output.stderr).into_owned() + &stdout(&output);
    assert!(
        text.contains("error[E2001]: no schema symbol is named `Buton`"),
        "{text}"
    );
    assert!(
        text.contains("did you mean `viso::widgets::Button`?"),
        "{text}"
    );

    let json = stdout(&viso(&["schema", "Buton", "--json"]));
    assert!(json.contains(r#""type":"diagnostic""#), "{json}");
    assert!(json.contains(r#""code":"E2001""#), "{json}");
    assert!(json.contains(r#""exit_code":1"#), "{json}");
}
