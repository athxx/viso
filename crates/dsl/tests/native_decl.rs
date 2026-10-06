//! Handwritten `native` declarations (§47): parsed into typed declarations at
//! the top level and in components, typed and lowered as native imports of
//! the module's path, checked against a registry that has their library
//! (`E6101`), and linked to the Rust library that implements them.

use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::native::{NativeLibrary, Natives, STANDARD};
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::ast::{AstNode, CompilationUnit, Item, Member, NativeDeclKind};
use viso_dsl::frontend::{Origin, compile_file_in};
use viso_dsl::syntax::{Entry, SyntaxNode, parse_entry, tokenize};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn codes(source: &str, natives: Arc<Natives>) -> Vec<(String, String)> {
    compile_file_in(source, &origin(), natives)
        .errors()
        .map(|d| (d.code.to_string(), d.message.clone()))
        .collect()
}

fn module(source: &str, natives: Arc<Natives>) -> Rc<Module> {
    let compiled = compile_file_in(source, &origin(), natives);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

static HOST: NativeLibrary = NativeLibrary {
    path: "app::main",
    version: 1,
    functions: &[
        viso_behavior::native!(fn "hash" |_cx, text: String| -> i64 {
            Ok(text.len() as i64 * 31)
        }),
        viso_behavior::native!(action "beep" |_cx, times: i64| -> () {
            let _ = times;
            Ok(())
        })
        .requires(&["audio.play"]),
    ],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[],
};

fn host() -> Arc<Natives> {
    let mut natives = Natives::new();
    natives.extend(STANDARD).expect("standard");
    natives.register(&HOST).expect("host");
    Arc::new(natives)
}

const DECLARED: &str = r#"
native fn hash(text: String) -> I64;

component Tally {
    state n: I64 = 0;
    native action beep(times: I64) requires { audio::play };
    action go() { n = hash("abc"); beep(2); }
    view { Text { text: "a"; } }
}
"#;

#[test]
fn declarations_parse_to_typed_items_and_members() {
    let source = "native fn f(a: I64) -> I64;\nexport native type Body;\n\
                  component C { native task load(path: String) -> String; view { Text {} } }";
    let parse = parse_entry(&tokenize(source), source, Entry::CompilationUnit);
    assert!(parse.errors.is_empty(), "{:?}", parse.errors);
    let cu = CompilationUnit::cast(SyntaxNode::new_root(parse.root)).expect("unit");
    let items: Vec<Item> = cu.items().collect();
    let Item::Native(f) = &items[0] else {
        panic!("{items:?}")
    };
    assert_eq!(f.kind(), Some(NativeDeclKind::Fn));
    assert_eq!(f.params().len(), 1);
    let Item::Export(body) = &items[1] else {
        panic!("{items:?}")
    };
    let Some(Item::Native(body)) = body.declaration() else {
        panic!("an exported native type")
    };
    assert_eq!(body.kind(), Some(NativeDeclKind::Type));
    let Item::Component(c) = &items[2] else {
        panic!("{items:?}")
    };
    let load = c
        .members()
        .find_map(|m| match m {
            Member::Native(d) => Some(d),
            _ => None,
        })
        .expect("a native member");
    assert_eq!(load.kind(), Some(NativeDeclKind::Task));
    assert_eq!(
        load.name().map(|n| n.text().to_string()),
        Some("load".into())
    );

    let with_body = "native fn f() -> I64 { 1 }\ncomponent C { view { Text {} } }";
    let parse = parse_entry(&tokenize(with_body), with_body, Entry::CompilationUnit);
    assert!(!parse.errors.is_empty(), "a native has no body");
}

#[test]
fn a_declared_native_types_lowers_and_links_to_its_rust_library() {
    let module = module(DECLARED, Natives::standard());
    let imports: Vec<(&str, u64)> = module
        .natives()
        .iter()
        .map(|n| (&*n.path, n.signature))
        .collect();
    let hash = &HOST.functions[0];
    let beep = &HOST.functions[1];
    assert!(
        imports.contains(&("app::main::hash", hash.signature())),
        "{imports:?}"
    );
    assert!(
        imports.contains(&("app::main::Tally::beep", beep.signature())),
        "{imports:?}"
    );

    let mut vm = Vm::new(module.clone(), Budget::default());
    let conflict = vm
        .link(&Natives::standard(), &["audio.play"])
        .expect_err("the library is not linked");
    assert_eq!(conflict.code(), "E6101");
}

#[test]
fn a_declaration_beside_its_registered_library_runs() {
    let source = "native fn hash(text: String) -> I64;\n\
                  component Tally {\n\
                      state n: I64 = 0;\n\
                      action go() { n = hash(\"abc\"); }\n\
                      view { Text { text: \"a\"; } }\n\
                  }";
    let module = module(source, host());
    let component = module.component("Tally").expect("component");
    let go = module.layout(component).member("go").expect("go");
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&host(), &[]).expect("links");
    let mut tally = vm.instantiate(component, []).expect("instantiate");
    vm.call(&mut tally, go, &[]).expect("go");
    assert_eq!(tally.states(), &[Value::Int(93)]);

    // Compiled without the library, it links to it all the same.
    let module = self::module(source, Natives::standard());
    let mut vm = Vm::new(module.clone(), Budget::default());
    vm.link(&host(), &[]).expect("links");
    let mut tally = vm.instantiate(component, []).expect("instantiate");
    vm.call(&mut tally, go, &[]).expect("go");
    assert_eq!(tally.states(), &[Value::Int(93)]);
}

#[test]
fn a_declaration_differing_from_the_registry_is_e6101() {
    let e6101 = |source: &str, needle: &str| {
        let found = codes(source, host());
        assert!(
            found
                .iter()
                .any(|(c, m)| c == "E6101" && m.contains(needle)),
            "{source}: {found:?}"
        );
    };
    let tail = "\ncomponent A { view { Text {} } }";
    e6101(
        &format!("native fn hash(text: String) -> String;{tail}"),
        "registered with another signature",
    );
    e6101(
        &format!("native action hash(text: String) -> I64;{tail}"),
        "registered with another signature",
    );
    e6101(
        &format!("native fn hush(text: String) -> I64;{tail}"),
        "declares no function `hush`",
    );
    e6101(
        &format!("native type Body;{tail}"),
        "declares no type `Body`",
    );
    e6101(
        &format!("native action beep(times: I64) requires {{ net::http }};{tail}"),
        "requires `audio.play`",
    );
    let ok = codes(
        &format!("native fn hash(text: String) -> I64;\nnative action beep(n: I64);{tail}"),
        host(),
    );
    assert!(
        ok.is_empty(),
        "parameter names are not the signature: {ok:?}"
    );
}

#[test]
fn a_declaration_is_written_in_schema_types() {
    let e6101 = |decl: &str, needle: &str| {
        let source =
            format!("record P {{ x: I64 = 0; }}\n{decl}\ncomponent A {{ view {{ Text {{}} }} }}");
        let found = codes(&source, Natives::standard());
        assert!(
            found
                .iter()
                .any(|(c, m)| c == "E6101" && m.contains(needle)),
            "{decl}: {found:?}"
        );
    };
    e6101("native fn f(p: P) -> I64;", "`P` is no native schema type");
    e6101(
        "native fn f() -> Color;",
        "`Color` is no native schema type",
    );
    e6101("native fn f<T>(x: I64) -> I64;", "not generic");
    e6101("native fn f(x: I64 = 1) -> I64;", "no default");
    e6101("native type T: Clone;", "without bounds");

    let ok = codes(
        "import viso::time::Stopwatch;\n\
         native type Body;\n\
         native fn spawn(mass: F64, tags: List<String>) -> Body;\n\
         native fn mass(body: Body) -> Option<F64>;\n\
         native fn watch(body: Body) -> Stopwatch;\n\
         native action reset();\n\
         component A {\n\
             state body = spawn(1.0, [\"a\"]);\n\
             computed m: Option<F64> = mass(body);\n\
             view { Text {} }\n\
         }",
        Natives::standard(),
    );
    assert!(ok.is_empty(), "{ok:?}");
}

#[test]
fn a_declared_native_is_typed_and_effect_checked_as_any_native() {
    let tail = "\nview { Text { text: \"a\"; } } }";
    let found = codes(
        &format!(
            "native fn hash(text: String) -> I64;\ncomponent A {{ state n: String = \"\"; action go() {{ n = hash(\"a\"); }}{tail}"
        ),
        Natives::standard(),
    );
    assert!(found.iter().any(|(c, _)| c == "E2103"), "{found:?}");
    let found = codes(
        &format!("native action ping() -> I64;\ncomponent A {{ computed n: I64 = ping();{tail}"),
        Natives::standard(),
    );
    assert!(found.iter().any(|(c, _)| c == "E2502"), "{found:?}");
    let found = codes(
        &format!(
            "native fn hash(text: String) -> I64;\ncomponent A {{ computed n: I64 = hash(1);{tail}"
        ),
        Natives::standard(),
    );
    assert!(!found.is_empty(), "an argument of the wrong type");
}

#[test]
fn a_member_declaration_is_seen_by_its_owner_only() {
    // An action in a computed is `E2502`: only where `ping` names the action.
    let found = codes(
        "export component A { native action ping() -> I64; computed n: I64 = ping(); view { Text {} } }\n\
         component B { computed n: I64 = ping(); view { Text {} } }",
        Natives::standard(),
    );
    assert_eq!(
        found.iter().filter(|(c, _)| c == "E2502").count(),
        1,
        "{found:?}"
    );
    let found = codes(
        "native fn f() -> I64;\nnative fn f() -> I64;\nfn g() -> I64 { 1 }\nnative fn g() -> I64;\n\
         component A { view { Text {} } }",
        Natives::standard(),
    );
    assert_eq!(
        found.iter().filter(|(c, _)| c == "E2002").count(),
        2,
        "{found:?}"
    );
}
