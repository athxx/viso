//! The methods of `String` from source to the VM: its length in characters,
//! whether it is empty, holds, starts or ends with another, and itself
//! trimmed or in another case; a release package's bytecode carries them;
//! and the checks: an unknown method (`E2001`, with the nearest name) and
//! the arity and argument types (`E2103`).

use std::rc::Rc;

use viso_behavior::{Budget, Instance, Module, Value, Vm};
use viso_dsl::edit::Document;
use viso_dsl::frontend::{Origin, compile_file};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    }
}

const STRINGS: &str = r#"
fn tidy(name: String) -> String {
    name.trim().to_lowercase()
}

export component Strings {
    state text = "  Crème brûlée  "
    state lengths: List<I64> = []
    state flags: List<Bool> = []
    state shown = ""
    state word = "abc"

    computed size: I64 = word.len()

    action read() {
        let t = text.trim()
        lengths = [text.len(), t.len(), "".len(), size]
        flags = [
            t.contains("me br"),
            t.starts_with("Cr"),
            t.ends_with("lée"),
            t.contains("x"),
            "".is_empty(),
            t.is_empty(),
        ]
        shown = "{tidy(text)}|{t.to_uppercase()}"
    }

    view { Text(shown) }
}
"#;

fn strings(module: Module) -> (Vm, Instance) {
    let module = Rc::new(module);
    let index = module.component("Strings").expect("component");
    let mut vm = Vm::new(module, Budget::default());
    let instance = vm.instantiate(index, []).expect("instantiate");
    (vm, instance)
}

fn read(vm: &mut Vm, instance: &mut Instance) {
    let layout = vm.module().layout(instance.component().expect("component"));
    let chunk = layout.member("read").expect("member");
    vm.call(instance, chunk, &[]).expect("reads");
}

fn state(vm: &Vm, instance: &Instance, name: &str) -> Value {
    let layout = vm.module().layout(instance.component().expect("component"));
    instance.states()[layout.state(name).expect("state")].clone()
}

fn assert_read(module: Module) {
    let (mut vm, mut instance) = strings(module);
    read(&mut vm, &mut instance);
    let ints = |v: &[i64]| Value::List(Rc::new(v.iter().map(|&n| Value::Int(n)).collect()));
    let bools = |v: &[bool]| Value::List(Rc::new(v.iter().map(|&b| Value::bool(b)).collect()));
    assert_eq!(
        state(&vm, &instance, "lengths"),
        ints(&[16, 12, 0, 3]),
        "characters, not bytes"
    );
    assert_eq!(
        state(&vm, &instance, "flags"),
        bools(&[true, true, true, false, true, false])
    );
    assert_eq!(
        state(&vm, &instance, "shown"),
        Value::str("crème brûlée|CRÈME BRÛLÉE")
    );
}

fn compiled() -> Module {
    let compiled = compile_file(STRINGS, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    compiled.behavior.bytecode().expect("verified bytecode")
}

#[test]
fn a_string_answers_its_methods() {
    assert_read(compiled());
}

#[test]
fn a_package_carries_them() {
    let bytes = compiled().encode();
    assert_read(Module::decode(&bytes).expect("decodes"));
}

fn codes(members: &str) -> Vec<(&'static str, String)> {
    let source = format!(
        "export component C {{\n    state s: String = \"a\"\n    {members}\n    view {{ Text {{}} }}\n}}\n"
    );
    Document::new(&source, &origin())
        .diagnostics()
        .iter()
        .map(|d| (d.code, d.message.clone()))
        .collect()
}

#[test]
fn string_methods_are_checked() {
    let found = codes("computed c: Bool = s.contians(\"a\")");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].0, "E2001");
    assert!(found[0].1.contains("no method `contians` on `String`"));
    let document = Document::new(
        "export component C { state s: String = \"a\"; computed c: Bool = s.contians(\"a\"); view { Text {} } }",
        &origin(),
    );
    assert_eq!(
        document.diagnostics()[0].fixes[0].edits[0].replacement,
        "contains"
    );
    assert_eq!(codes("computed c: Bool = s.contains(1)")[0].0, "E2103");
    assert_eq!(codes("computed c: String = s.trim(1)")[0].0, "E2103");
    assert!(codes("computed c: I64 = s.len() + s.trim().len()").is_empty());
}
