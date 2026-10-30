//! Strings with holes (§17): each hole's value shows as text, `{{`/`}}` are
//! braces, an empty hole or a value without `Display` is `E2108`, a string
//! with holes is no literal where one is required, and the formatter keeps
//! one as written.

use std::rc::Rc;

use viso_behavior::{Budget, Value, Vm};
use viso_dsl::Severity;
use viso_dsl::format::format;
use viso_dsl::frontend::{Origin, compile_file};
use viso_dsl::syntax::grammar::{Entry, IncrementalParse};
use viso_dsl::syntax::{Edit, TextRange, TextSize};

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

/// What `fn shown(..)` of component `C` in `source` returns for `args`.
fn shown(source: &str, args: &[Value]) -> String {
    let compiled = compile_file(source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let index = module.component("C").expect("component");
    let mut vm = Vm::new(module, Budget::default());
    let mut instance = vm.instantiate(index, []).expect("instantiate");
    let layout = vm.module().layout(index);
    let chunk = layout.member("shown").expect("member");
    let value = vm.call(&mut instance, chunk, args).expect("runs").value;
    value.as_str().expect("a string").to_owned()
}

#[test]
fn each_hole_shows_its_value_between_the_text() {
    let source = r#"
record P { x: I64; y: F64 }
component C {
    state p = P { x: 3, y: 1.5 }
    fn shown(n: I64, name: String) -> String {
        "{name}: {n} of {n * 2}, at ({p.x}, {p.y}) {{braces}} {if n > 1 { "many" } else { "one" }}"
    }
    view { Column {} }
}
"#;
    assert_eq!(
        shown(source, &[Value::Int(2), Value::Str(Rc::new("Ada".into()))]),
        "Ada: 2 of 4, at (3, 1.5) {braces} many"
    );
    let nested = r#"
component C {
    fn shown(n: I64) -> String { "<{"[{n}]"}>" }
    view { Column {} }
}
"#;
    assert_eq!(shown(nested, &[Value::Int(7)]), "<[7]>");
}

#[test]
fn a_hole_needs_a_displayable_value() {
    let with = |body: &str| {
        format!(
            "record P {{ x: I64 }}\ncomponent C {{\n    state p = P {{ x: 1 }}\n    fn f() -> String {{ {body} }}\n    view {{ Column {{}} }}\n}}\n"
        )
    };
    assert_eq!(codes(&with(r#""{p.x}""#)), Vec::<&str>::new());
    assert_eq!(codes(&with(r#""{p}""#)), ["E2108"]);
    assert_eq!(codes(&with(r#""{[1]}""#)), ["E2108"]);
    assert_eq!(codes(&with(r#""{}""#)), ["E2108"]);
    assert_eq!(codes(&with(r#""{nope}""#)), ["E2001"]);
    // A lone `}` is written `}}`.
    assert_eq!(codes(&with(r#""a } b""#)), ["E1205"]);
}

#[test]
fn a_string_with_holes_is_no_literal_where_one_is_required() {
    let preserve = r#"
component C {
    state on = true
    state id = "a"
    view { if on preserve "{id}" { Column {} } }
}
"#;
    assert_eq!(codes(preserve), ["E3301"]);
    let asset = r#"
component C {
    state n = 1
    view { Image { source: asset("{n}.png") } }
}
"#;
    assert_eq!(codes(asset), ["E3713"], "an asset path is a literal");
}

#[test]
fn localizable_text_with_holes_and_words_is_e3705() {
    let view = |text: &str| {
        format!("component C {{\n    state n = 1\n    view {{ Text {{ text: {text} }} }}\n}}\n")
    };
    assert_eq!(codes(&view(r#""{n} items""#)), ["E3705"]);
    assert_eq!(codes(&view(r#""{n}{n}""#)), ["E3705"]);
    assert_eq!(codes(&view(r#""{n}""#)), Vec::<&str>::new());
}

#[test]
fn the_formatter_keeps_a_string_with_holes_as_written() {
    let source = "fn f(n: I64) -> String {\n    \"a {n}  b {  n * 2 } {{c}}\"\n}\n";
    assert_eq!(format(source), source);
    let long = format!(
        "fn f(n: I64) -> String {{\n    \"{}{{P {{ x: n, y: n }}.x}}\"\n}}\n",
        "x".repeat(120)
    );
    assert_eq!(format(&long), long);
}

#[test]
fn an_edit_inside_a_hole_reparses_like_a_fresh_parse() {
    let source = "fn f(n: I64) -> String { \"a {n} b\" }\nfn g() {}\n";
    let at = |text: &str| TextSize::new(source.find(text).unwrap() as u32);
    for (range, insert) in [
        (TextRange::empty(at("n} b")), "n + "),
        (TextRange::new(at("} b"), at("} b") + TextSize::new(1)), ""),
        (TextRange::empty(at(" b\"")), " {"),
        (TextRange::empty(at("a {")), "\""),
    ] {
        let mut parse = IncrementalParse::new(source, Entry::CompilationUnit);
        let edit = Edit::new(range, insert);
        let edited = edit.apply(source);
        parse.edit(&edit, &edited);
        let fresh = IncrementalParse::new(&edited, Entry::CompilationUnit);
        assert_eq!(parse.tokens(), fresh.tokens(), "{edited:?}");
        assert_eq!(
            format!("{:?}", parse.parse().root),
            format!("{:?}", fresh.parse().root),
            "{edited:?}"
        );
    }
}
