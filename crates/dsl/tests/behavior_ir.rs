//! Behavior IR lowering: every `fn`/`action` body and member value of a unit
//! lowers to register code, compared against its `behavior-ir 1` dump under
//! `tests/golden/behavior/`. Set `BLESS=1` to (re)generate the dumps.

use std::path::PathBuf;

use viso_dsl::frontend::{Origin, compile_file};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

/// Compiles `source`, which must have no errors, and compares its dump with the
/// golden `name`.
fn check(name: &str, source: &str) {
    let compiled = compile_file(source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let dump = compiled.behavior.dump();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/behavior")
        .join(format!("{name}.txt"));
    if std::env::var("BLESS").is_ok() {
        std::fs::create_dir_all(path.parent().expect("golden directory")).expect("mkdir");
        std::fs::write(&path, &dump).expect("write golden");
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with BLESS=1 to generate it",
            path.display()
        )
    });
    assert_eq!(dump, want, "{name} changed; run with BLESS=1 to accept");
}

/// The body of `name` in `source`'s dump.
fn function<'d>(dump: &'d str, name: &str) -> &'d str {
    let start = dump
        .find(&format!(" {name} "))
        .unwrap_or_else(|| panic!("no function {name} in\n{dump}"));
    let rest = &dump[start..];
    &rest[..rest.find("\n\n").unwrap_or(rest.len())]
}

#[test]
fn counter_action_reads_writes_and_emits() {
    check(
        "counter",
        r#"
component Counter {
    state count = 0;
    computed doubled: I64 = count * 2;
    event changed(value: I64);

    action increment() {
        count += 1;
        emit changed(count);
    }

    view { Text { text: format("{n}", n: doubled); } }
}
"#,
    );
}

#[test]
fn match_lowers_to_a_decision_tree() {
    check(
        "match",
        r#"
enum Shape {
    Circle(F64);
    Square(F64);
    Empty;
}

component Shapes {
    state label = "";

    fn classify(n: I64) -> String {
        match n {
            0 => "zero",
            1 | 2 => "few",
            _ => format("many {n}", n: n),
        }
    }

    fn digit(n: I64) -> I64 {
        match n {
            1 => 10,
            2 => 20,
            3 => 30,
            4 => 40,
            _ => 0,
        }
    }

    fn area(s: Shape) -> F64 {
        match s {
            Shape::Circle(r) => 3.0 * r * r,
            Shape::Square(w) => w * w,
            Shape::Empty => 0.0,
        }
    }

    view { Text { text: label; } }
}
"#,
    );
}

#[test]
fn loops_and_closures_lower() {
    check(
        "loops",
        r#"
component Loops {
    state label = "";

    fn sum(items: List<I64>) -> I64 {
        let mut total = 0;
        for item in items {
            if item > 10 && item < 100 {
                total += item;
            }
        }
        total
    }

    fn count(n: I64) -> I64 {
        let mut hits = 0;
        for i in 0..=n {
            if i % 3 == 0 {
                continue;
            }
            hits += 1;
        }
        let mut k = 0;
        while true {
            k += 1;
            if k > n {
                break;
            }
        }
        hits + k
    }

    fn apply(k: I64) -> I64 {
        let add = |x: I64| x + k;
        add(3)
    }

    view { Text { text: label; } }
}
"#,
    );
}

#[test]
fn records_and_optionals_lower() {
    check(
        "records",
        r#"
record Point {
    x: F64;
    y: F64 = 1.0;
}

component Records {
    state point = Point { x: 2.0 };
    state picked: Option<I64> = None;

    action nudge(dx: F64) {
        point.x += dx;
        point = Point { x: point.y, ..point };
    }

    fn first(items: List<I64>) -> Option<I64> {
        let n = items[0];
        Some(n)
    }

    view { Text { text: format("{x}", x: point.x); } }
}
"#,
    );
}

#[test]
fn an_action_body_writes_state_in_source_order() {
    let compiled = compile_file(
        "component C {\n    state count = 0;\n    action increment() { count += 1; }\n    view { Text {} }\n}\n",
        &origin(),
    );
    assert!(!compiled.has_errors());
    let dump = compiled.behavior.dump();
    let body = function(&dump, "C.increment");
    assert!(body.contains("= state[0]"), "{body}");
    assert!(body.contains("state[0] = "), "{body}");
    assert!(body.contains("add.i64"), "{body}");
}

#[test]
fn a_body_with_type_errors_cannot_run() {
    let compiled = compile_file(
        "component C {\n    state count = 0;\n    fn f() -> I64 { true }\n    view { Text {} }\n}\n",
        &origin(),
    );
    assert!(compiled.has_errors());
    let dump = compiled.behavior.dump();
    assert!(function(&dump, "C.f").contains("unsupported: has type errors"));
}

#[test]
fn a_caller_of_a_function_that_cannot_run_cannot_run() {
    let compiled = compile_file(
        "component C {\n    state count = 0;\n    fn f() -> I64 { true }\n    fn g() -> I64 { f() }\n    view { Text {} }\n}\n",
        &origin(),
    );
    let dump = compiled.behavior.dump();
    assert!(
        function(&dump, "C.g").contains("calls `C.f`, which cannot run"),
        "{dump}"
    );
}

#[test]
fn every_instruction_has_a_source_span() {
    let source = "component C {\n    state count = 0;\n    action step(by: I64) {\n        if by > 0 { count += by; } else { count -= 1; }\n    }\n    view { Text {} }\n}\n";
    let compiled = compile_file(source, &origin());
    assert!(!compiled.has_errors());
    for function in &compiled.behavior.functions {
        let Ok(body) = &function.body else {
            continue;
        };
        assert_eq!(body.insts.len(), body.spans.len());
        for at in &body.spans {
            assert!(at.end().to_u32() as usize <= source.len());
        }
    }
}

#[test]
fn a_record_spread_supplies_the_omitted_fields() {
    let source = "record P { x: F64; y: F64; }\ncomponent C {\n    state count = 0;\n    fn f(p: P) -> P { P { y: 1.0, ..p } }\n    view { Text {} }\n}\n";
    let compiled = compile_file(source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let dump = compiled.behavior.dump();
    let body = function(&dump, "C.f");
    assert!(body.contains("= r0.0"), "{body}");
}

#[test]
fn a_record_spread_of_another_type_is_an_error() {
    let source = "record P { x: F64; y: F64; }\ncomponent C {\n    state count = 0;\n    fn f() -> P { P { y: 1.0, ..3 } }\n    view { Text {} }\n}\n";
    let compiled = compile_file(source, &origin());
    assert!(
        compiled.errors().any(|d| d.code == "E2103"),
        "{:#?}",
        compiled.diagnostics
    );
}

#[test]
fn an_input_default_is_checked_against_its_type() {
    let source = "component C {\n    input n: I64 = \"five\";\n    view { Text {} }\n}\n";
    let compiled = compile_file(source, &origin());
    assert!(
        compiled.errors().any(|d| d.code == "E2103"),
        "{:#?}",
        compiled.diagnostics
    );
}
