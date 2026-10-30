//! The primary property (§50.1): `Type(value)` binds it, the body may not
//! (`E3109`, with the rewrite as its fix), a type without one takes no
//! argument (`E3101`), and a component names its own with `@primary`.

use viso_dsl::Severity;
use viso_dsl::frontend::{Origin, compile_file};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    }
}

/// The error and warning codes compiling `source` reports.
fn codes(source: &str) -> Vec<&'static str> {
    compile_file(source, &origin())
        .diagnostics
        .iter()
        .filter(|d| d.severity != Severity::Note)
        .map(|d| d.code)
        .collect()
}

/// `source` with the edits of `fix` made.
fn apply(source: &str, fix: &viso_dsl::diag::Fix) -> String {
    let mut edits = fix.edits.clone();
    edits.sort_by_key(|e| std::cmp::Reverse(e.range.start()));
    let mut out = source.to_owned();
    for edit in edits {
        let (start, end) = (
            edit.range.start().to_u32() as usize,
            edit.range.end().to_u32() as usize,
        );
        out.replace_range(start..end, &edit.replacement);
    }
    out
}

fn view(items: &str) -> String {
    format!(
        "export component C {{\n    state n = 1\n    view {{\n        Column {{\n{items}\n        }}\n    }}\n}}\n"
    )
}

#[test]
fn the_argument_binds_the_primary_property() {
    for items in [
        r#"Text("Hello")"#,
        r#"Text("{n}") { color: #888888 }"#,
        r#"Button("Save") { on click { n += 1 } }"#,
        r#"CheckBox("Remember") { checked: true }"#,
        "Spinner",
        "Text(\"a\"); Text(\"b\")",
    ] {
        assert_eq!(codes(&view(items)), Vec::<&str>::new(), "{items}");
    }
}

#[test]
fn the_body_does_not_bind_the_primary_property() {
    let source = view(r#"Text { text: "Hello" }"#);
    let compiled = compile_file(&source, &origin());
    let diagnostic = compiled
        .diagnostics
        .iter()
        .find(|d| d.code == "E3109")
        .expect("E3109");
    let fixed = apply(&source, &diagnostic.fixes[0]);
    assert!(fixed.contains(r#"Text("Hello")"#), "{fixed}");
    assert_eq!(codes(&fixed), Vec::<&str>::new(), "{fixed}");

    let source = view("Text {\n    text: \"Hello\"\n    color: #888888\n}");
    let compiled = compile_file(&source, &origin());
    let diagnostic = compiled
        .diagnostics
        .iter()
        .find(|d| d.code == "E3109")
        .unwrap();
    let fixed = apply(&source, &diagnostic.fixes[0]);
    assert!(
        fixed.contains("Text(\"Hello\") {\n    color: #888888\n}"),
        "{fixed}"
    );
    assert_eq!(codes(&fixed), Vec::<&str>::new(), "{fixed}");

    assert_eq!(codes(&view(r#"Text("a") { text: "b" }"#)), ["E3109"]);
}

#[test]
fn a_type_without_a_primary_property_takes_no_argument() {
    assert_eq!(codes(&view(r#"Column("x")"#)), ["E3101"]);
    assert_eq!(
        codes(&view(r#"Card("x")"#).replace(
            "component C",
            "component Card {\n    view { Column {} }\n}\ncomponent C"
        )),
        ["E3101"]
    );
}

#[test]
fn a_component_names_its_primary_input() {
    let card = r#"
component Card {
    @primary input title: String
    input subtitle: String = ""
    view { Column { Text(title); Text(subtitle) } }
}
"#;
    let source = format!("{card}{}", view(r#"Card("Profile") { subtitle: "{n}" }"#));
    assert_eq!(codes(&source), Vec::<&str>::new());
    let source = format!("{card}{}", view(r#"Card { title: "Profile" }"#));
    assert_eq!(codes(&source), ["E3109"]);

    let twice = "component D {\n    @primary input a: String\n    @primary input b: String\n    view { Column {} }\n}\n";
    assert_eq!(codes(twice), ["E3109"]);
    let state = "component D {\n    @primary state a = 1\n    view { Column {} }\n}\n";
    assert_eq!(codes(state), ["E3109"]);
}
