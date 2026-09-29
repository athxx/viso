//! Behavior execution: `.vs` sources compile to bytecode and run headless in the
//! interpreter, with action transactions, faults and budgets observed through
//! the component instance.

use std::rc::Rc;

use viso_behavior::{Aggregate, Budget, Fault, FaultKind, Instance, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

/// Compiles `source`, which must have no errors, to a module.
fn module(source: &str) -> Rc<Module> {
    let compiled = compile_file(source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

/// A VM over `source` and an instance of its component `component`.
fn instance(source: &str, component: &str) -> (Vm, Instance) {
    let module = module(source);
    let index = module.component(component).expect("component");
    let mut vm = Vm::new(module, Budget::default());
    let instance = vm.instantiate(index, []).expect("instantiate");
    (vm, instance)
}

/// The chunk of member `member` of the instance's component.
fn member(vm: &Vm, instance: &Instance, member: &str) -> u32 {
    let layout = vm.module().layout(instance.component().expect("component"));
    layout.member(member).expect("member")
}

fn call(vm: &mut Vm, instance: &mut Instance, name: &str, args: &[Value]) -> Result<Value, Fault> {
    let chunk = member(vm, instance, name);
    vm.call(instance, chunk, args).map(|o| o.value)
}

fn ints(values: &[i64]) -> Value {
    Value::List(Rc::new(values.iter().map(|&v| Value::Int(v)).collect()))
}

const COUNTER: &str = r#"
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
"#;

#[test]
fn a_counter_increments_through_its_action() {
    let (mut vm, mut counter) = instance(COUNTER, "Counter");
    assert_eq!(counter.states(), &[Value::Int(0)]);
    assert_eq!(counter.revision(), 0);

    let increment = member(&vm, &counter, "increment");
    let outcome = vm.call(&mut counter, increment, &[]).expect("increment");
    assert_eq!(counter.states(), &[Value::Int(1)]);
    assert_eq!(counter.revision(), 1);
    assert_eq!(counter.dirty().collect::<Vec<_>>(), [0]);
    assert_eq!(outcome.events.len(), 1);
    assert_eq!(outcome.events[0].index, 0);
    assert_eq!(&*outcome.events[0].args, &[Value::Int(1)]);

    vm.call(&mut counter, increment, &[]).expect("increment");
    assert_eq!(counter.states(), &[Value::Int(2)]);
    assert_eq!(counter.revision(), 2);
    let doubled = call(&mut vm, &mut counter, "doubled", &[]).expect("doubled");
    assert_eq!(doubled, Value::Int(4));
    // Reading a computed value is a transaction without writes.
    assert_eq!(counter.revision(), 2);
}

#[test]
fn a_faulting_action_rolls_back_its_writes_and_events() {
    let (mut vm, mut c) = instance(
        r#"
component C {
    state count = 0;
    state log = "";
    event hit();

    action boom(items: List<I64>) {
        count += 1;
        emit hit();
        log = "touched";
        count += items[5];
    }

    view { Text { text: log; } }
}
"#,
        "C",
    );
    let fault = call(&mut vm, &mut c, "boom", &[ints(&[1, 2])]).unwrap_err();
    assert_eq!(fault.kind, FaultKind::IndexOutOfBounds);
    assert_eq!(fault.kind.code(), "E7104");
    assert_eq!(c.states(), &[Value::Int(0), Value::str("")]);
    assert_eq!(c.revision(), 0);
    assert_eq!(c.dirty().count(), 0);

    let chunk = member(&vm, &c, "boom");
    let outcome = vm
        .call(&mut c, chunk, &[ints(&[0, 0, 0, 0, 0, 10])])
        .expect("boom");
    assert_eq!(c.states(), &[Value::Int(11), Value::str("touched")]);
    assert_eq!(c.revision(), 1, "one transaction, one revision");
    assert_eq!(c.dirty().collect::<Vec<_>>(), [0, 1]);
    assert_eq!(outcome.events.len(), 1);
}

#[test]
fn a_fault_points_at_its_source() {
    let source = r#"
component C {
    state count = 0;
    fn at(items: List<I64>, i: I64) -> I64 { items[i] }
    view { Text {} }
}
"#;
    let module = module(source);
    let c = module.component("C").unwrap();
    let chunk = module.layout(c).member("at").unwrap();
    let mut vm = Vm::new(module, Budget::default());
    let mut instance = vm.instantiate(c, []).unwrap();
    let fault = vm
        .call(&mut instance, chunk, &[ints(&[1]), Value::Int(-1)])
        .unwrap_err();
    assert_eq!(fault.message, "index -1 is out of bounds of a list of 1");
    let at = fault.at.expect("location");
    assert_eq!(at.chunk, chunk);
    assert_eq!(
        &source[at.span.start as usize..at.span.end as usize],
        "items[i]"
    );
}

#[test]
fn integer_overflow_faults_per_width() {
    let (mut vm, mut c) = instance(
        r#"
component C {
    state count = 0;
    fn add(a: I64, b: I64) -> I64 { a + b }
    fn add8(a: I8, b: I8) -> I8 { a + b }
    fn div(a: I32, b: I32) -> I32 { a / b }
    view { Text {} }
}
"#,
        "C",
    );
    let add =
        |vm: &mut Vm, c: &mut Instance, a, b| call(vm, c, "add", &[Value::Int(a), Value::Int(b)]);
    assert_eq!(add(&mut vm, &mut c, 2, 3), Ok(Value::Int(5)));
    let fault = add(&mut vm, &mut c, i64::MAX, 1).unwrap_err();
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::Overflow, "E7103")
    );
    let args = [Value::Int(100), Value::Int(27)];
    assert_eq!(call(&mut vm, &mut c, "add8", &args), Ok(Value::Int(127)));
    let args = [Value::Int(100), Value::Int(28)];
    assert_eq!(
        call(&mut vm, &mut c, "add8", &args).unwrap_err().kind,
        FaultKind::Overflow
    );
    let args = [Value::Int(1), Value::Int(0)];
    assert_eq!(
        call(&mut vm, &mut c, "div", &args).unwrap_err().kind,
        FaultKind::DivideByZero
    );
}

#[test]
fn budgets_stop_runaway_code() {
    let module = module(
        r#"
component C {
    state count = 0;

    fn spin(n: I64) -> I64 {
        let mut k = 0;
        while k >= 0 {
            k += n;
        }
        k
    }

    fn deep(n: I64) -> I64 { deep(n + 1) }

    fn grow(n: I64) -> String {
        let mut s = "ab";
        for i in 0..n {
            s = s + s;
        }
        s
    }

    view { Text {} }
}
"#,
    );
    let c = module.component("C").unwrap();
    let layout = module.layout(c).clone();
    let budget = Budget {
        instructions: 10_000,
        memory: 1 << 16,
        depth: 64,
    };
    let mut vm = Vm::new(module, budget);
    let mut instance = vm.instantiate(c, []).unwrap();
    let mut run = |name: &str, n: i64| {
        let chunk = layout.member(name).unwrap();
        vm.call(&mut instance, chunk, &[Value::Int(n)])
    };

    let fault = run("spin", 0).unwrap_err();
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::InstructionBudget, "E7101")
    );
    let fault = run("deep", 0).unwrap_err();
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::CallDepth, "E7101")
    );
    let fault = run("grow", 40).unwrap_err();
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::MemoryBudget, "E7102")
    );
    // Each invocation gets a fresh budget.
    let grown = run("grow", 3).expect("small growth fits");
    assert_eq!(grown.value.as_str().map(str::len), Some(16));
}

#[test]
fn loops_matches_and_closures_run() {
    let (mut vm, mut c) = instance(
        r#"
enum Shape {
    Circle(F64);
    Square(F64);
    Empty;
}

component C {
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
        hits
    }

    fn apply(k: I64) -> I64 {
        let add = |x: I64| x + k;
        add(3)
    }

    fn classify(n: I64) -> String {
        match n {
            0 => "zero",
            1 | 2 => "few",
            _ => format("many {n}", n: n),
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
        "C",
    );
    let mut run = |name: &str, arg: Value| call(&mut vm, &mut c, name, &[arg]).expect(name);
    assert_eq!(run("sum", ints(&[5, 20, 50, 200])), Value::Int(70));
    assert_eq!(run("count", Value::Int(9)), Value::Int(6));
    assert_eq!(run("apply", Value::Int(4)), Value::Int(7));
    assert_eq!(run("classify", Value::Int(0)), Value::str("zero"));
    assert_eq!(run("classify", Value::Int(2)), Value::str("few"));
    assert_eq!(run("classify", Value::Int(7)), Value::str("many 7"));
    let square = Value::Agg(Rc::new(Aggregate {
        tag: 1,
        fields: Box::new([Value::Float(3.0)]),
    }));
    assert_eq!(run("area", square), Value::Float(9.0));
    assert_eq!(run("area", Value::Int(2)), Value::Float(0.0));
}

#[test]
fn records_update_in_place_and_by_spread() {
    let (mut vm, mut c) = instance(
        r#"
record Point {
    x: F64;
    y: F64 = 1.0;
}

component C {
    state point = Point { x: 2.0 };
    state moved = 0.0;

    action nudge(dx: F64) {
        point.x += dx;
        moved = point.x;
        point = Point { x: point.y, ..point };
    }

    view { Text { text: format("{x}", x: point.x); } }
}
"#,
        "C",
    );
    let point = |x: f64, y: f64| {
        Value::Agg(Rc::new(Aggregate {
            tag: 0,
            fields: Box::new([Value::Float(x), Value::Float(y)]),
        }))
    };
    assert_eq!(c.states(), &[point(2.0, 1.0), Value::Float(0.0)]);
    call(&mut vm, &mut c, "nudge", &[Value::Float(0.5)]).expect("nudge");
    assert_eq!(c.states(), &[point(1.0, 1.0), Value::Float(2.5)]);
}

#[test]
fn inputs_take_defaults_and_feed_state_initializers() {
    let module = module(
        r#"
component Sum {
    input a: I64 = 5;
    input b: I64;
    state total = a + b;
    view { Text {} }
}
"#,
    );
    let sum = module.component("Sum").unwrap();
    let b = module.layout(sum).input("b").unwrap();
    let mut vm = Vm::new(module, Budget::default());
    let instance = vm
        .instantiate(sum, [(b, Value::Int(3))])
        .expect("instantiate");
    assert_eq!(instance.inputs(), &[Value::Int(5), Value::Int(3)]);
    assert_eq!(instance.states(), &[Value::Int(8)]);
    let fault = vm.instantiate(sum, []).unwrap_err();
    assert_eq!(fault.kind, FaultKind::MissingInput);
    assert_eq!(fault.message, "`Sum` requires input `b`");
}

#[test]
fn calling_a_function_that_cannot_run_faults() {
    let compiled = compile_file(
        "component C {\n    state count = 0;\n    fn f() -> I64 { true }\n    view { Text {} }\n}\n",
        &origin(),
    );
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let c = module.component("C").unwrap();
    let f = module.layout(c).member("f").unwrap();
    let mut vm = Vm::new(module, Budget::default());
    let mut instance = vm.instantiate(c, []).unwrap();
    let fault = vm.call(&mut instance, f, &[]).unwrap_err();
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::Unsupported, "E7105")
    );
    assert!(
        fault.message.contains("has type errors"),
        "{}",
        fault.message
    );
}

#[test]
fn a_warmed_up_action_spends_a_fixed_instruction_count() {
    let (mut vm, mut counter) = instance(COUNTER, "Counter");
    let increment = member(&vm, &counter, "increment");
    vm.call(&mut counter, increment, &[]).unwrap();
    let first = vm.cost();
    vm.call(&mut counter, increment, &[]).unwrap();
    assert_eq!(vm.cost(), first);
    assert!(
        first.instructions > 0 && first.instructions < 16,
        "{first:?}"
    );
}
