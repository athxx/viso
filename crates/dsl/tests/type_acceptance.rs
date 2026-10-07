//! Type acceptance (§153), from source through the whole frontend: the safe
//! widening ladders and the forbidden implicit conversions, the removed
//! `Float`, `StableKey` keys, closures typed by the function type they meet,
//! capability propagation, the computed cycle path, `MixedLength` and the
//! `Percent` flow.

use viso_dsl::edit::Document;
use viso_dsl::frontend::Origin;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

/// The diagnostics of `src`, code and message, with a mountable component
/// added when the source declares none.
fn diagnostics(src: &str) -> Vec<(&'static str, String)> {
    let full = if src.contains("component ") {
        src.to_owned()
    } else {
        format!("{src}\ncomponent Host {{ view {{ Text {{}} }} }}\n")
    };
    Document::new(&full, &origin())
        .diagnostics()
        .iter()
        .map(|d| (d.code, d.message.clone()))
        .collect()
}

fn codes(src: &str) -> Vec<&'static str> {
    diagnostics(src).into_iter().map(|(c, _)| c).collect()
}

fn clean(src: &str) {
    let found = diagnostics(src);
    assert!(found.is_empty(), "{src}\n{found:#?}");
}

/// `src` reports exactly one diagnostic, `code`, whose message contains each
/// of `parts`.
fn one(src: &str, code: &str, parts: &[&str]) {
    let found = diagnostics(src);
    assert_eq!(
        found.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
        [code],
        "{src}\n{found:#?}"
    );
    for part in parts {
        assert!(
            found[0].1.contains(part),
            "{src}: {:?} lacks {part:?}",
            found[0].1
        );
    }
}

#[test]
fn every_safe_widening_is_implicit() {
    let ladders: [&[&str]; 3] = [
        &["I8", "I16", "I32", "I64"],
        &["U8", "U16", "U32", "U64"],
        &["F32", "F64"],
    ];
    for ladder in ladders {
        for (i, from) in ladder.iter().enumerate() {
            for to in &ladder[i..] {
                clean(&format!("fn f(x: {from}) -> {to} {{ x }}"));
                clean(&format!("fn f(x: {from}) {{ let y: {to} = x; }}"));
            }
        }
    }
    for unit in ["Dp", "Px", "Sp", "Em", "Percent"] {
        clean(&format!("fn f(x: {unit}) -> MixedLength {{ x }}"));
    }
    // Lengths of different units add up to a `MixedLength`.
    clean("fn f() -> MixedLength { 1dp + 2px - 1sp + 1em + 50% }");
}

#[test]
fn every_forbidden_conversion_is_a_diagnostic() {
    for (from, to) in [
        ("I32", "U32"),
        ("U8", "I8"),
        ("U32", "I64"),
        ("I64", "I32"),
        ("U64", "U8"),
        ("I32", "F64"),
        ("U8", "F32"),
        ("F64", "I64"),
        ("F32", "I32"),
        ("F64", "F32"),
    ] {
        one(
            &format!("fn f(x: {from}) -> {to} {{ x }}"),
            "E2102",
            &[from, to, "explicit cast"],
        );
    }
    for (from, to) in [
        ("Dp", "Px"),
        ("Sp", "Dp"),
        ("Em", "Percent"),
        ("Bool", "I64"),
        ("I64", "Bool"),
        ("String", "I64"),
        ("I64", "String"),
        ("Color", "String"),
    ] {
        one(
            &format!("fn f(x: {from}) -> {to} {{ x }}"),
            "E2103",
            &[from, to],
        );
    }
    for unit in ["Dp", "Px", "Sp", "Em", "Percent"] {
        one(
            &format!("fn f(x: MixedLength) -> {unit} {{ x }}"),
            "E2106",
            &[unit],
        );
    }
    // The explicit cast is the way through.
    clean("fn f(x: I64) -> F64 { x as F64 }");
    clean("fn f(x: F64) -> F32 { x as F32 }");
}

#[test]
fn float_is_no_type_wherever_one_is_written() {
    for site in [
        "fn f(x: Float) { }",
        "fn f() -> Float { 1.0 }",
        "fn f() { let x: Float = 1.0; }",
        "fn f() { let x = 1 as Float; }",
        "record R { a: Float; }",
        "enum E { A(Float); }",
        "const C: Float = 1.0;",
        "fn f(xs: List<Float>) { }",
        "fn f(f: Fn(Float) -> I64) { }",
        "component C { state s: Float = 1.0; view { Text {} } }",
        "component C { input s: Option<Float>; view { Text {} } }",
    ] {
        one(site, "E2101", &["`Float`", "`F32` or `F64`"]);
    }
    let src = "fn f(x: Float) { }\ncomponent Host { view { Text {} } }\n";
    let document = Document::new(src, &origin());
    let replacements: Vec<&str> = document.diagnostics()[0]
        .fixes
        .iter()
        .map(|fix| fix.edits[0].replacement.as_str())
        .collect();
    assert_eq!(replacements, ["F64", "F32"]);
}

/// A component keying a resource load by `key`, whose type is `ty`.
fn keyed(decls: &str, ty: &str, init: &str) -> String {
    format!(
        "{decls}\ntask fetch() -> Result<I64, String> {{ Ok(1) }}\n\
         component C {{\n    state k: {ty} = {init};\n    \
         resource r: Resource<I64, String> {{ load = fetch(); key = k; }}\n    \
         view {{ Text {{}} }}\n}}\n"
    )
}

#[test]
fn stable_keys_derive_through_records_enums_tuples_and_options() {
    let decls = "record Id { n: U64; tag: String; }\n\
                 enum Kind { Plain; Tagged(I32, Char); Named { id: Id; }; }\n\
                 record Wide { a: I64; b: F64; }\n\
                 enum Shape { Dot; Circle(F32); }";
    for (ty, init) in [
        ("I64", "1"),
        ("U8", "1"),
        ("Bool", "true"),
        ("Char", "'c'"),
        ("String", "\"s\""),
        ("Id", "Id { n: 1, tag: \"a\" }"),
        ("Kind", "Kind::Plain"),
        ("(I64, String, Bool)", "(1, \"a\", true)"),
        ("Option<Id>", "None"),
        ("Option<(Kind, I8)>", "None"),
    ] {
        clean(&keyed(decls, ty, init));
    }
    for (ty, init, member) in [
        ("F64", "1.0", "it is a float"),
        ("F32", "1.0", "it is a float"),
        ("Wide", "Wide { a: 1, b: 1.0 }", "`Wide.b` is a float"),
        ("Shape", "Shape::Dot", "`Shape::Circle.0` is a float"),
        ("(I64, F64)", "(1, 1.0)", "`.1` is a float"),
        ("Option<F32>", "None", "it is a float"),
        ("List<I64>", "[]", "is no integer"),
    ] {
        one(&keyed(decls, ty, init), "E2701", &[member]);
    }
    // A keyed view row takes the same keys.
    one(
        "component C { state xs = [1.5, 2.5]; view { Column { for x in xs key x { Text {} } } } }",
        "E2701",
        &["`for` key", "float"],
    );
    clean(
        "record Row { id: I64; }\n\
         component C { state xs = [Row { id: 1 }]; view { Column { for x in xs key x { Text {} } } } }",
    );
}

#[test]
fn a_closure_takes_its_parameters_and_result_from_the_expected_function_type() {
    clean(
        "fn apply(f: Fn(I64) -> I64, x: I64) -> I64 { f(x) }\n\
         fn g() -> I64 { apply(|x| x + 1, 2) }",
    );
    clean("fn g() -> I64 { let f: Fn(I64, I64) -> I64 = |a, b| a * b; f(3, 4) }");
    clean("fn g() -> String { let f: FnMut(String) -> String = |s| s; f(\"a\") }");
    // The parameter's expected type checks its uses, the result its body.
    one(
        "fn g() -> String { let f: Fn(I64) -> String = |x| x; f(3) }",
        "E2103",
        &["String", "I64"],
    );
    one(
        "fn g() -> I64 { let f: Fn(I64) -> I64 = |x: I64| \"s\"; f(3) }",
        "E2103",
        &["I64", "String"],
    );
    // Without an expected type an unannotated parameter cannot be typed.
    one("fn g() { let f = |x| x; }", "E2401", &["`x`"]);
    // An arity that does not match is that mismatch alone.
    one(
        "fn g() { let f: Fn(I64) -> I64 = |a, b| a; }",
        "E2103",
        &["2 parameters", "`Fn(I64) -> I64` takes 1"],
    );
}

#[test]
fn a_capability_propagates_through_the_call_graph() {
    let native = "native fn fetch(url: String) -> String requires { network };\n";
    // A private callable infers what it calls; a declared clause bounds it.
    clean(&format!(
        "{native}fn get() -> String {{ fetch(\"x\") }}\n\
         export fn load() -> String requires {{ network }} {{ get() }}"
    ));
    one(
        &format!("{native}fn get() -> String requires {{ timer }} {{ fetch(\"x\") }}"),
        "E2601",
        &["`network`"],
    );
    one(
        &format!(
            "{native}fn a() -> String {{ fetch(\"x\") }}\nfn b() -> String {{ a() }}\n\
             export fn c() -> String requires {{ timer }} {{ b() }}"
        ),
        "E2601",
        &["`network`"],
    );
    // A clause wider than the inferred set is an upper bound, not an error.
    clean(&format!(
        "{native}export fn d() -> String requires {{ network, timer }} {{ fetch(\"x\") }}"
    ));
}

#[test]
fn a_computed_cycle_names_its_path() {
    let found = diagnostics(
        "component C {\n    computed a: I64 = b + 1;\n    computed b: I64 = c;\n    \
         computed c: I64 = a;\n    computed d: I64 = a;\n    view { Text {} }\n}\n",
    );
    let cycles: Vec<_> = found.iter().filter(|(c, _)| *c == "E2105").collect();
    assert_eq!(cycles.len(), 1, "{found:#?}");
    assert!(
        cycles[0].1.contains("`a` -> `b` -> `c` -> `a`"),
        "{}",
        cycles[0].1
    );
    // A forward dependency without a cycle is fine.
    clean(
        "component C {\n    computed a: I64 = b + 1;\n    computed b: I64 = 2;\n    view { Text {} }\n}\n",
    );
}

#[test]
fn mixed_lengths_scale_but_neither_compare_nor_multiply_nor_narrow() {
    let mixed = "let m = 1dp + 1px;";
    clean(&format!("fn f() -> MixedLength {{ {mixed} m * 2.0 }}"));
    clean(&format!("fn f() -> MixedLength {{ {mixed} m / 2.0 + m }}"));
    one(
        &format!("fn f() {{ {mixed} let n = m * m; }}"),
        "E2107",
        &[],
    );
    one(
        &format!("fn f() -> Bool {{ {mixed} m < m }}"),
        "E2107",
        &["compared"],
    );
    one(
        &format!("fn f() -> Bool {{ {mixed} m == m }}"),
        "E2107",
        &["compared"],
    );
    one(
        &format!("fn f() {{ {mixed} let d: Dp = m; }}"),
        "E2106",
        &["`Dp`"],
    );
    one(
        &format!("fn f() {{ {mixed} let mut d = 1dp; d = m; }}"),
        "E2106",
        &["`Dp`"],
    );
    one(&format!("fn f() -> Px {{ {mixed} m }}"), "E2106", &["`Px`"]);
}

/// A view binding `translate`, which has no percent basis, to `x`.
fn translate(decls: &str, x: &str) -> String {
    format!(
        "{decls}\ncomponent C {{\n    view {{ Text {{ translate: Offset {{ x: {x}, y: 0dp }}; }} }}\n}}\n"
    )
}

#[test]
fn a_percent_reaching_a_property_without_a_basis_is_rejected() {
    for (decls, x) in [
        ("", "50%"),
        ("const P: Percent = 50%;", "P"),
        ("const P: MixedLength = 10dp + 50%;", "P"),
        (
            "const P: MixedLength = 10dp + 50%;\nconst Q: MixedLength = P;",
            "Q",
        ),
        ("record Pad { x: MixedLength = 50%; }", "Pad {}.x"),
    ] {
        one(
            &translate(decls, x),
            "E3104",
            &["`translate`", "percent basis"],
        );
    }
    // A call's result carries only what its type says.
    clean(&translate("fn pct() -> MixedLength { 50% }", "pct()"));
    clean(&translate("const D: MixedLength = 10dp + 2px;", "D"));
    // A property with a basis takes it.
    clean("const P: Percent = 50%;\ncomponent C { view { Column { width: P; } } }");
}

#[test]
fn an_event_parameter_names_a_type_like_any_other() {
    one(
        "component C { event got(x: Nope); view { Text {} } }",
        "E2001",
        &["`Nope`"],
    );
    one(
        "component C { event got(x: Float); view { Text {} } }",
        "E2101",
        &[],
    );
}

#[test]
fn codes_found_are_reported_once() {
    // A failed annotation does not cascade into a mismatch.
    assert_eq!(codes("fn f() { let x: Float = 1.0; }"), ["E2101"]);
}
