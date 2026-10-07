//! The methods of `List<T>` from source to the VM: the readers on any list,
//! the editors on a writable place — a `state`, a mutable local, a field or
//! element of one — written back as an assignment is, copy on write so a list
//! another value shares is left as it was; out-of-bounds edits fault and roll
//! back; and the checks: an unknown method (`E2001`, with the nearest name),
//! the arity and argument types (`E2103`), an editor on a read-only place
//! (`E2110`) or in a body that may not mutate (`E2501`/`E2502`).

use std::rc::Rc;

use viso_behavior::{Budget, FaultKind, Instance, Value, Vm};
use viso_dsl::edit::Document;
use viso_dsl::frontend::{Origin, compile_file};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

const LISTS: &str = r#"
record Row {
    tags: List<I64>;
}

export component Lists {
    state xs: List<I64> = [3, 1, 2];
    state rows: List<Row> = [Row { tags: [1] }];
    state out: List<I64> = [];
    state saved: List<I64> = [];
    state found = false;
    state n = 0;

    action read() {
        let ys = xs;
        out = [ys.len(), if ys.is_empty() { 1 } else { 0 }];
        match ys.get(1) {
            Option::Some(v) => { out.push(v); },
            Option::None => {},
        }
        match ys.get(9) {
            Option::Some(v) => { out.push(v); },
            Option::None => { out.push(-1); },
        }
        match ys.first() {
            Option::Some(v) => { out.push(v); },
            Option::None => {},
        }
        match ys.last() {
            Option::Some(v) => { out.push(v); },
            Option::None => {},
        }
        found = ys.contains(2) && !ys.contains(7);
    }

    action edit() {
        saved = xs;
        xs.push(4);
        xs.insert(0, 9);
        let gone = xs.remove(2);
        n = gone;
        xs.retain(|x| x != 2);
        match xs.pop() {
            Option::Some(v) => { n = n * 10 + v; },
            Option::None => {},
        }
    }

    action nested() {
        rows[0].tags.push(5);
        let mut local = [1, 2];
        local.push(local.len());
        rows.push(Row { tags: local });
    }

    action empty() {
        xs.clear();
        match xs.pop() {
            Option::Some(v) => { n = v; },
            Option::None => { n = -1; },
        }
    }

    action bad() {
        xs.push(8);
        xs.remove(99);
    }

    view { Text {} }
}
"#;

fn ints(values: &[i64]) -> Value {
    Value::List(Rc::new(values.iter().map(|&v| Value::Int(v)).collect()))
}

fn lists() -> (Vm, Instance) {
    let compiled = compile_file(LISTS, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let index = module.component("Lists").expect("component");
    let mut vm = Vm::new(module, Budget::default());
    let instance = vm.instantiate(index, []).expect("instantiate");
    (vm, instance)
}

fn run(vm: &mut Vm, instance: &mut Instance, name: &str) -> Result<(), FaultKind> {
    let layout = vm.module().layout(instance.component().expect("component"));
    let chunk = layout.member(name).expect("member");
    vm.call(instance, chunk, &[])
        .map(|_| ())
        .map_err(|f| f.kind)
}

fn state(vm: &Vm, instance: &Instance, name: &str) -> Value {
    let layout = vm.module().layout(instance.component().expect("component"));
    instance.states()[layout.state(name).expect("state")].clone()
}

#[test]
fn readers_answer_from_any_list() {
    let (mut vm, mut lists) = lists();
    run(&mut vm, &mut lists, "read").expect("reads");
    assert_eq!(state(&vm, &lists, "out"), ints(&[3, 0, 1, -1, 3, 2]));
    assert_eq!(state(&vm, &lists, "found"), Value::bool(true));
}

#[test]
fn editors_write_the_place_back_and_leave_a_shared_copy_alone() {
    let (mut vm, mut lists) = lists();
    run(&mut vm, &mut lists, "edit").expect("edits");
    // [3, 1, 2] → push 4 → insert 9 at 0 → remove [2] (= 1) → drop 2s → pop 4.
    assert_eq!(state(&vm, &lists, "xs"), ints(&[9, 3]));
    assert_eq!(state(&vm, &lists, "n"), Value::Int(14));
    assert_eq!(
        state(&vm, &lists, "saved"),
        ints(&[3, 1, 2]),
        "copy on write"
    );
}

#[test]
fn editors_reach_fields_elements_and_mutable_locals() {
    let (mut vm, mut lists) = lists();
    run(&mut vm, &mut lists, "nested").expect("edits");
    let Value::List(rows) = state(&vm, &lists, "rows") else {
        panic!("a list");
    };
    let tags = |row: &Value| match row {
        Value::Agg(agg) => agg.fields[0].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(tags(&rows[0]), ints(&[1, 5]));
    assert_eq!(tags(&rows[1]), ints(&[1, 2, 2]));
}

#[test]
fn pop_on_an_empty_list_is_none() {
    let (mut vm, mut lists) = lists();
    run(&mut vm, &mut lists, "empty").expect("runs");
    assert_eq!(state(&vm, &lists, "xs"), ints(&[]));
    assert_eq!(state(&vm, &lists, "n"), Value::Int(-1));
}

#[test]
fn an_out_of_bounds_edit_faults_and_rolls_back() {
    let (mut vm, mut lists) = lists();
    assert_eq!(
        run(&mut vm, &mut lists, "bad"),
        Err(FaultKind::IndexOutOfBounds)
    );
    assert_eq!(state(&vm, &lists, "xs"), ints(&[3, 1, 2]));
}

/// The code and message of each diagnostic `members` in a component gets.
fn diagnostics(members: &str) -> Vec<(&'static str, String)> {
    let source = format!(
        "export component C {{\n    state xs = [1];\n    input ins: List<I64>;\n    {members}\n    view {{ Text {{}} }}\n}}\n"
    );
    Document::new(&source, &origin())
        .diagnostics()
        .iter()
        .map(|d| (d.code, d.message.clone()))
        .collect()
}

fn codes(members: &str) -> Vec<&'static str> {
    diagnostics(members).into_iter().map(|(c, _)| c).collect()
}

#[test]
fn list_methods_are_checked() {
    let found = diagnostics("action a() { xs.psuh(2); }");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].0, "E2001");
    assert!(found[0].1.contains("no method `psuh` on `List<I64>`"));
    let document = Document::new(
        "export component C { state xs = [1]; action a() { xs.psuh(2); } view { Text {} } }",
        &origin(),
    );
    let fix = &document.diagnostics()[0].fixes[0];
    assert_eq!(fix.edits[0].replacement, "push");

    assert_eq!(codes("action a() { xs.push(\"s\"); }"), ["E2103"]);
    assert_eq!(codes("action a() { xs.push(); }"), ["E2103"]);
    assert_eq!(codes("action a() { xs.insert(1); }"), ["E2103"]);
    assert_eq!(codes("action a() { xs.retain(|x| x); }"), ["E2103"]);
    assert_eq!(codes("action a() { ins.push(1); }"), ["E2110"]);
    assert_eq!(codes("action a() { let ys = xs; ys.push(1); }"), ["E2110"]);
    assert_eq!(codes("fn f() { xs.push(1); }"), ["E2501"]);
    assert_eq!(codes("computed c: I64 = { xs.clear(); 1 };"), ["E2502"]);
    // The readers read anywhere, an editor a local a pure body owns.
    assert!(codes("computed c: I64 = xs.len() + ins.len();").is_empty());
    assert!(codes("fn f() -> I64 { let mut ys = xs; ys.push(1); ys.len() }").is_empty());
}
