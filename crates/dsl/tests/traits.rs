//! User traits, impls and generics (§26, §30, §31, §78, §79): inherent
//! methods and associated items, trait methods with defaults, bounds and
//! `where`, generic callables monomorphized per instantiation, generic records
//! and enums, associated types, const generics and trait objects — and the
//! diagnostics of each (`E2201` unmet bound, `E2202` overlap or ambiguity,
//! `E2203` unbounded instantiation).

use std::rc::Rc;

use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn codes(source: &str) -> Vec<(String, String)> {
    compile_file(source, &origin())
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
}

fn module(source: &str) -> Rc<Module> {
    let compiled = compile_file(source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

/// Runs the action `go` of the component `C` in `decls`, whose `members`
/// declare its states, and returns them.
fn run(decls: &str, members: &str) -> Vec<Value> {
    let source = format!("{decls}\nexport component C {{\n{members}\nview {{ Text {{}} }}\n}}\n");
    let module = module(&source);
    let component = module.component("C").expect("component");
    let go = module.layout(component).member("go").expect("go");
    let mut vm = Vm::new(module, Budget::default());
    let mut c = vm.instantiate(component, []).expect("instantiate");
    vm.call(&mut c, go, &[]).expect("go");
    c.states().to_vec()
}

fn has(found: &[(String, String)], code: &str, needle: &str) -> bool {
    found.iter().any(|(c, m)| c == code && m.contains(needle))
}

const SHAPES: &str = r#"
trait Shape {
    fn area(self) -> I64;
    fn twice(self) -> I64 { return self.area() * 2; }
}
record Sq { s: I64; }
record Rect { w: I64; h: I64; }
impl Shape for Sq {
    fn area(self) -> I64 { return self.s * self.s; }
}
impl Shape for Rect {
    fn area(self) -> I64 { return self.w * self.h; }
    fn twice(self) -> I64 { return 0; }
}
"#;

#[test]
fn inherent_methods_associated_functions_and_consts_run() {
    let decls = r#"
record Point { x: I64; y: I64; }
impl Point {
    const BASE: I64 = 100;
    fn new(x: I64, y: I64) -> Point { return Point { x: x, y: y }; }
    fn add(a: I64, b: I64) -> I64 { return a + b; }
    fn sum(self) -> I64 { return Self::add(self.x, self.y); }
    fn scaled(self, k: I64) -> Point { return Point { x: self.x * k, y: self.y * k }; }
}
"#;
    let states = run(
        decls,
        "state n = 0;\naction go() { let p = Point::new(2, 3); n = p.scaled(10).sum() + Point::BASE; }",
    );
    assert_eq!(states, [Value::Int(150)]);
}

#[test]
fn trait_methods_defaults_and_bounded_generics_run() {
    let decls = format!(
        "{SHAPES}
fn total<T: Shape>(xs: List<T>) -> I64 {{
    let mut t = 0;
    for x in xs {{ t += x.twice(); }}
    return t;
}}
fn area_of<T>(x: T) -> I64 where T: Shape {{ return x.area(); }}
"
    );
    let states = run(
        &decls,
        "state a = 0; state b = 0; state c = 0;\n\
         action go() { a = total([Sq { s: 3 }, Sq { s: 1 }]); b = total::<Rect>([Rect { w: 2, h: 5 }]); \
         c = area_of(Rect { w: 4, h: 4 }) + Shape::area(Sq { s: 2 }); }",
    );
    assert_eq!(states, [Value::Int(20), Value::Int(0), Value::Int(20)]);
}

#[test]
fn generic_records_and_enums_construct_match_and_project() {
    let decls = r#"
record Pair<A, B> { a: A; b: B; }
enum Maybe<T> { none; some(T); }
fn swap<A, B>(p: Pair<A, B>) -> Pair<B, A> { return Pair { a: p.b, b: p.a }; }
fn or<T>(m: Maybe<T>, d: T) -> T {
    return match m { Maybe::some(v) => v, Maybe::none => d };
}
"#;
    let states = run(
        decls,
        "state s = \"\"; state k = 0;\n\
         action go() { let p = swap(Pair { a: 1, b: \"x\" }); s = p.a; \
         k = or(Maybe::some(p.b + 1), 0) + or(Maybe::none, 5); }",
    );
    assert_eq!(states, [Value::str("x"), Value::Int(7)]);
}

#[test]
fn associated_types_resolve_through_their_impl() {
    let decls = r#"
trait Source {
    type Item;
    fn get(self) -> Self::Item;
}
record Num { v: I64; }
record Word { w: String; }
impl Source for Num { type Item = I64; fn get(self) -> I64 { return self.v; } }
impl Source for Word { type Item = String; fn get(self) -> Self::Item { return self.w; } }
fn take<S: Source>(s: S) -> S::Item { return s.get(); }
"#;
    let states = run(
        decls,
        "state n = 0; state w = \"\";\naction go() { n = take(Num { v: 9 }) + 1; w = take(Word { w: \"hi\" }); }",
    );
    assert_eq!(states, [Value::Int(10), Value::str("hi")]);
}

#[test]
fn const_generic_arguments_are_constants_in_the_body() {
    let decls = r#"
enum Mode { slow; fast; }
fn times<const N: I64>(x: I64) -> I64 { return x * N; }
fn pick<const M: Mode>() -> I64 { return match M { Mode::slow => 1, Mode::fast => 2 }; }
fn flag<const B: Bool>() -> Bool { return B; }
"#;
    let states = run(
        decls,
        "state n = 0; state f = false;\n\
         action go() { n = times::<const 3>(5) + times::<const -1>(2) + pick::<const Mode::fast>(); \
         f = flag::<const true>(); }",
    );
    assert_eq!(states, [Value::Int(15), Value::bool(true)]);
}

#[test]
fn trait_objects_dispatch_through_their_vtable() {
    let decls = format!(
        "{SHAPES}
fn sum_areas(xs: List<dyn Shape>) -> I64 {{
    let mut t = 0;
    for x in xs {{ t += x.area() + x.twice(); }}
    return t;
}}
"
    );
    let states = run(
        &decls,
        "state a = 0;\n\
         action go() { let xs: List<dyn Shape> = [Sq { s: 2 }, Rect { w: 1, h: 3 }]; a = sum_areas(xs); }",
    );
    // Sq: 4 + 8; Rect: 3 + 0.
    assert_eq!(states, [Value::Int(15)]);
}

#[test]
fn an_unmet_bound_is_e2201() {
    let found = codes(&format!(
        "{SHAPES}\nfn f<T: Shape>(x: T) -> I64 {{ return x.area(); }}\n\
         component C {{ state n = 0; action go() {{ n = f(1); }} view {{ Text {{}} }} }}"
    ));
    assert!(
        has(&found, "E2201", "`I64` does not implement `Shape`"),
        "{found:?}"
    );

    let found = codes(&format!(
        "{SHAPES}\nrecord Tri {{ b: I64; }}\nimpl Shape for Tri {{ fn perimeter(self) -> I64 {{ return 0; }} }}\n\
         component C {{ view {{ Text {{}} }} }}"
    ));
    assert!(has(&found, "E2201", "lacks `area`"), "{found:?}");
    assert!(
        has(&found, "E2201", "`perimeter` is not a member of `Shape`"),
        "{found:?}"
    );

    let found = codes(&format!(
        "{SHAPES}\nrecord Tri {{ b: I64; }}\nimpl Shape for Tri {{ fn area(self) -> String {{ return \"\"; }} }}\n\
         component C {{ view {{ Text {{}} }} }}"
    ));
    assert!(has(&found, "E2201", "another signature"), "{found:?}");

    let found = codes(
        "trait Grow { fn grow(self) -> Self; }\ntrait Map { fn map<U>(self, u: U) -> I64; }\n\
         record P { v: I64; }\n\
         impl Grow for P { fn grow(self) -> P { return self; } }\n\
         impl Map for P { fn map<U>(self, u: U) -> I64 { return 0; } }\n\
         component C { state n = 0; action go() { let a: dyn Grow = P { v: 1 }; let b: dyn Map = P { v: 1 }; } \
         view { Text {} } }",
    );
    assert!(has(&found, "E2201", "`grow` names `Self`"), "{found:?}");
    assert!(
        has(&found, "E2201", "`map` has type parameters"),
        "{found:?}"
    );

    let found = codes(
        "fn same<T>(a: T, b: T) -> Bool { return a == b; }\n\
         fn same_eq<T: Eq>(a: T, b: T) -> Bool { return a == b; }\n\
         component C { view { Text {} } }",
    );
    assert_eq!(
        found.iter().filter(|(c, _)| c == "E2201").count(),
        1,
        "only the unbounded comparison: {found:?}"
    );

    let found =
        codes("fn f<T>(x: T) -> I64 { return x.area(); }\ncomponent C { view { Text {} } }");
    assert!(has(&found, "E2001", "no method `area`"), "{found:?}");
}

#[test]
fn overlapping_and_ambiguous_impls_are_e2202() {
    let found = codes(&format!(
        "{SHAPES}\nimpl Shape for Sq {{ fn area(self) -> I64 {{ return 0; }} }}\n\
         component C {{ view {{ Text {{}} }} }}"
    ));
    assert!(has(&found, "E2202", "two impls of `Shape`"), "{found:?}");

    let found = codes(
        "record P { v: I64; }\n\
         trait A { fn name(self) -> I64; }\ntrait B { fn name(self) -> I64; }\n\
         impl A for P { fn name(self) -> I64 { return 1; } }\n\
         impl B for P { fn name(self) -> I64 { return 2; } }\n\
         component C { state n = 0; action go() { n = P { v: 0 }.name() + A::name(P { v: 0 }); } view { Text {} } }",
    );
    assert_eq!(
        found.iter().filter(|(c, _)| c == "E2202").count(),
        1,
        "the method call, not the trait-qualified one: {found:?}"
    );

    // An inherent method wins over a trait's.
    let states = run(
        "record P { v: I64; }\ntrait A { fn name(self) -> I64; }\n\
         impl A for P { fn name(self) -> I64 { return 1; } }\n\
         impl P { fn name(self) -> I64 { return 2; } }",
        "state n = 0;\naction go() { n = P { v: 0 }.name() * 10 + A::name(P { v: 0 }); }",
    );
    assert_eq!(states, [Value::Int(21)]);
}

#[test]
fn an_unbounded_instantiation_is_e2203() {
    let found = codes(
        "record Pair<A, B> { a: A; b: B; }\n\
         fn grow<T>(x: T, n: I64) -> I64 { if n == 0 { return 0; } return grow(Pair { a: x, b: x }, n - 1); }\n\
         component C { state k = 0; action go() { k = grow(1, 3); } view { Text {} } }",
    );
    assert!(has(&found, "E2203", "recurses without bound"), "{found:?}");
}

#[test]
fn a_method_is_effect_checked_as_its_callable() {
    let found = codes(
        "record P { v: I64; }\nimpl P { action poke(self) {} fn peek(self) -> I64 { return self.v; } }\n\
         component C { state p = P { v: 1 }; computed a: I64 = p.peek(); computed b: Bool = { p.poke(); true }; \
         view { Text {} } }",
    );
    assert_eq!(
        found.iter().filter(|(c, _)| c == "E2502").count(),
        1,
        "{found:?}"
    );
}

#[test]
fn generic_aliases_and_components_take_their_arguments() {
    let decls = r#"
record Pair<A, B> { a: A; b: B; }
type Twin<T> = Pair<T, T>;
fn sum(p: Twin<I64>) -> I64 { return p.a + p.b; }
component Show<T> {
    input value: T;
    view { Text {} }
}
"#;
    let states = run(
        decls,
        "state n = 0;\naction go() { let p: Twin<I64> = Pair { a: 4, b: 5 }; n = sum(p); }",
    );
    assert_eq!(states, [Value::Int(9)]);

    let found = codes(&format!(
        "{decls}\nexport component D {{ state n = 0; action go() {{ let p: Twin<I64> = Pair {{ a: 1, b: \"x\" }}; }} \
         view {{ Column {{ Show {{ value: 1; }} Show {{ value: \"s\"; }} }} }} }}"
    ));
    assert_eq!(found.len(), 1, "only the mismatched twin: {found:?}");
}
