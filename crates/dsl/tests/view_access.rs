//! Accessibility and localization findings: a node of a non-interactive
//! widget that handles a pointer or key gesture needs a role, an accessible
//! name (`E3704`) and a keyboard path (`E3708`); a Localizable property shows
//! no concatenation and no literal `format` (`E3705`). Each is a warning, an
//! error under the strict profile, which also reports literal text.

use viso_behavior::native::Natives;
use viso_dsl::diag::Severity;
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["access".into()],
        language: None,
    }
}

/// Each finding compiling `view` reports under `profile`: its code and
/// whether it is an error.
fn findings(view: &str, profile: TargetProfile) -> Vec<(&'static str, bool)> {
    let source = format!(
        "export component C {{\n    state n = 0;\n    state name = \"a\";\n    view {{ {view} }}\n}}\n"
    );
    let compiled = compile_file_for(&source, &origin(), Natives::standard(), profile);
    compiled
        .diagnostics
        .iter()
        .map(|d| (d.code, d.severity == Severity::Error))
        .collect()
}

fn lenient(view: &str) -> Vec<&'static str> {
    let found = findings(view, TargetProfile::default());
    assert!(found.iter().all(|(_, error)| !error), "{found:?}");
    found.into_iter().map(|(code, _)| code).collect()
}

const CLEAN: [&str; 0] = [];

#[test]
fn an_interactive_node_needs_a_role_a_name_and_a_keyboard_path() {
    assert_eq!(
        lenient(
            "Row { semantics.role: Role::button; focusable: true; \
             Text { text: \"Go\"; } on click { n += 1; } }"
        ),
        CLEAN
    );
    assert_eq!(
        lenient(
            "Row { focusable: true; semantics.label: Option::Some(\"Go\"); on click { n += 1; } }"
        ),
        ["E3704"],
        "no role"
    );
    assert_eq!(
        lenient(
            "Row { semantics.role: Role::group; focusable: true; Text { text: \"Go\"; } on click { n += 1; } }"
        ),
        ["E3704"],
        "`group` is no role"
    );
    assert_eq!(
        lenient("Row { semantics.role: Role::button; focusable: true; on click { n += 1; } }"),
        ["E3704"],
        "no name"
    );
    assert_eq!(
        lenient(
            "Row { semantics.role: Role::button; Text { text: \"Go\"; } on click { n += 1; } }"
        ),
        ["E3708"],
        "not focusable"
    );
    assert_eq!(
        lenient(
            "Row { semantics.role: Role::button; focusable: true; Text { text: \"Go\"; } \
             on tap { n += 1; } }"
        ),
        ["E3708"],
        "a tap has no keyboard path"
    );
    assert_eq!(
        lenient("Row { on pointer_down { n += 1; } }"),
        CLEAN,
        "a raw pointer handler is no gesture"
    );
    assert_eq!(
        lenient("Button { text: \"Go\"; on click { n += 1; } }"),
        CLEAN,
        "a standard interactive widget"
    );
}

#[test]
fn localizable_text_is_no_concatenation_or_literal_format() {
    assert_eq!(lenient("Text { text: \"Inbox\"; }"), CLEAN);
    assert_eq!(lenient("Text { text: format(\"{}\", n); }"), CLEAN);
    assert_eq!(lenient("Text { text: name; }"), CLEAN);
    assert_eq!(lenient("Text { text: \"Hi \" + name; }"), ["E3705"]);
    assert_eq!(lenient("Text { text: (name + \"!\"); }"), ["E3705"]);
    assert_eq!(lenient("Text { text: format(\"{} new\", n); }"), ["E3705"]);
    assert_eq!(lenient("Text { text: format(\"{}{}\", n, n); }"), ["E3705"]);
    assert_eq!(
        lenient("TextInput { placeholder: \"Name: \" + name; }"),
        ["E3705"]
    );
    assert_eq!(
        lenient("Text { text: \"a\"; semantics.hint: Option::Some(\"x\" + name); }"),
        CLEAN,
        "an `Option` is no joined text"
    );
}

#[test]
fn a_strict_profile_makes_the_findings_errors() {
    let strict = TargetProfile {
        a11y_strict: true,
        i18n_strict: true,
        ..TargetProfile::default()
    };
    assert_eq!(
        findings(
            "Row { on click { n += 1; } Text { text: name; } }",
            strict.clone()
        ),
        [("E3704", true), ("E3708", true)]
    );
    assert_eq!(
        findings("Text { text: \"Inbox\"; }", strict.clone()),
        [("E3705", true)],
        "literal text too"
    );
    assert_eq!(findings("Text { text: name; }", strict), []);
}
