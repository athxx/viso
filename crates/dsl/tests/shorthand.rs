//! Enum shorthand (U1.3): where a property or record field expects an enum
//! (or its `Option`), a bare variant name stands for the qualified one,
//! through `if`/`match` tails; a name that is also a value is `E2002`.

mod support;

use support::{Rt, origin};
use viso_dsl::Severity;
use viso_dsl::frontend::compile_file;

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
fn a_bare_variant_stands_for_the_expected_enums() {
    for items in [
        "Row { align: center; justify: end }",
        "Row { justify: if n > 0 { end } else { start } }",
        "Row { justify: match n { 0 => start, _ => center } }",
        "Text(\"a\") { font_weight: semibold; align: center; overflow: ellipsis }",
        "Text(\"a\") { width: fill; height: fit }",
        "Text(\"a\") { width: fill { weight: 2.0 } }",
        "Text(\"a\") { semantics.role: heading; semantics.live: polite }",
        "Text(\"a\") { width: if n > 0 { fill } else { fit } }",
        "Row { align: Align::center; width: Sizing::fill {} }",
    ] {
        assert_eq!(codes(&view(items)), Vec::<&str>::new(), "{items}");
    }
}

#[test]
fn a_record_field_takes_the_shorthand_too() {
    let source = r#"
enum Tone { calm; loud }
record Look { tone: Tone = Tone::calm; size: I64 = 1 }
export component C {
    state look = Look { tone: loud }
    view { Column { Text(if look.tone == Tone::loud { "!" } else { "." }) } }
}
"#;
    assert_eq!(codes(source), Vec::<&str>::new());
}

#[test]
fn a_style_and_a_component_input_take_the_shorthand() {
    let source = r#"
enum Tone { calm; loud }
component Badge {
    input tone: Tone = Tone::calm
    view { Text(if tone == Tone::loud { "!" } else { "." }) }
}
style Centered for Row {
    align: center
    justify: start
    when hover { justify: end }
}
export component C {
    view { Row { styles: [Centered]; Badge { tone: loud } } }
}
"#;
    assert_eq!(codes(source), Vec::<&str>::new());
}

#[test]
fn shorthand_needs_an_expected_enum() {
    // A `let`, a function argument and a computed initializer expect nothing.
    let source = "enum Tone { calm; loud }\nfn f(t: Tone) -> I64 { 1 }\nexport component C {\n    computed a = f(loud)\n    view { Column {} }\n}\n";
    assert_eq!(codes(source), ["E2001"]);
    let source =
        "export component C {\n    computed a: Align = center\n    view { Column {} }\n}\n";
    assert_eq!(codes(source), ["E2001"]);
    // Parentheses read the value.
    assert_eq!(codes(&view("Row { align: (center) }")), ["E2001"]);
}

#[test]
fn an_unknown_name_suggests_the_variants() {
    let compiled = compile_file(&view("Row { align: cnter }"), &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].code, "E2001");
    assert!(
        errors[0].fixes.iter().any(|f| f.title.contains("center")),
        "{:?}",
        errors[0].fixes
    );
    // A name of another enum's variant is no variant here.
    assert_eq!(codes(&view("Row { align: semibold }")), ["E2001"]);
}

#[test]
fn a_value_named_like_a_variant_is_ambiguous() {
    let source = view("Row { align: center }").replace("state n = 1", "state center = 1");
    let compiled = compile_file(&source, &origin());
    let diagnostic = compiled
        .diagnostics
        .iter()
        .find(|d| d.code == "E2002")
        .expect("E2002");
    assert_eq!(diagnostic.fixes.len(), 2);
    let variant = apply(&source, &diagnostic.fixes[0]);
    assert!(variant.contains("align: Align::center"), "{variant}");
    assert_eq!(codes(&variant), Vec::<&str>::new());
    let value = apply(&source, &diagnostic.fixes[1]);
    assert!(value.contains("align: (center)"), "{value}");
    assert_eq!(codes(&value), ["E2103"], "an `Int` is no `Align`");
}

const STYLED: &str = r#"
export component Styled {
    state heavy = false
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { heavy = !heavy } }
            Text("Title") {
                font_weight: if heavy { bold } else { regular }
                align: center
            }
            Row {
                width: fill
                justify: if heavy { end } else { start }
                Text { width: 10dp; height: 10dp }
            }
        }
    }
}
"#;

#[test]
fn shorthand_runs_as_the_qualified_variant() {
    let qualified = STYLED
        .replace("{ bold }", "{ FontWeight::bold }")
        .replace("{ regular }", "{ FontWeight::regular }")
        .replace("align: center", "align: TextAlign::center")
        .replace("width: fill", "width: Sizing::fill")
        .replace("{ end }", "{ Justify::end }")
        .replace("{ start }", "{ Justify::start }");
    let place = |source: &str, packaged: bool| {
        let mut rt = if packaged {
            Rt::packaged(source)
        } else {
            Rt::mount(source)
        };
        assert_eq!(rt.fault(), None);
        rt.relayout();
        let before: Vec<_> = rt
            .descendants()
            .iter()
            .map(|n| rt.store.bounds(*n))
            .collect();
        rt.click(0);
        rt.relayout();
        let after: Vec<_> = rt
            .descendants()
            .iter()
            .map(|n| rt.store.bounds(*n))
            .collect();
        (before, after)
    };
    for packaged in [false, true] {
        let (short, long) = (place(STYLED, packaged), place(&qualified, packaged));
        assert_eq!(short, long);
        assert_ne!(short.0, short.1, "the justify flips");
    }
}
