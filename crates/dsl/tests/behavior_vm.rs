//! Behavior execution: `.vs` sources compile to bytecode and run headless in the
//! interpreter, with action transactions, faults and budgets observed through
//! the component instance.

use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::native::{
    Clipboard, NativeLibrary, NativeObject, NativeType, Natives, Obj, STANDARD, ThreadDomain,
};
use viso_behavior::{Aggregate, Budget, Fault, FaultKind, Instance, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file, compile_file_in};

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
        ..Budget::default()
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

/// The error codes of compiling `source` against `natives`.
fn native_errors(source: &str, natives: Arc<Natives>) -> Vec<String> {
    compile_file_in(source, &origin(), natives)
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

/// A VM over `source`, linked against the standard natives with `grants`.
fn linked(source: &str, component: &str, grants: &[&str]) -> (Vm, Instance) {
    let (mut vm, instance) = instance(source, component);
    vm.link(&Natives::standard(), grants).expect("link");
    (vm, instance)
}

const NATIVES: &str = r#"
import viso::text;
import viso::math::{clamp, sqrt};
import viso::time::Stopwatch;
import viso::clipboard;

component Tools {
    state label = "";
    state root = 0.0;
    state elapsed = -1.0;

    computed shout: String = text::upper(label);

    action rename(to: String) {
        label = text::trim(to);
    }

    action measure(x: F64) {
        root = sqrt(clamp(x, 0.0, 100.0));
    }

    action bad_clamp() {
        root = clamp(1.0, 2.0, 1.0);
    }

    action time() {
        let watch = Stopwatch::start();
        elapsed = watch.elapsed_ms();
    }

    action copy() {
        clipboard::write_text(label);
    }

    view { Text { text: shout; } }
}
"#;

#[test]
fn native_functions_run_in_the_vm_after_linking() {
    let (mut vm, mut tools) = linked(NATIVES, "Tools", &[]);
    call(&mut vm, &mut tools, "rename", &[Value::str("  ada ")]).expect("rename");
    assert_eq!(tools.states()[0], Value::str("ada"));
    let shout = call(&mut vm, &mut tools, "shout", &[]).expect("shout");
    assert_eq!(shout, Value::str("ADA"));

    call(&mut vm, &mut tools, "measure", &[Value::Float(400.0)]).expect("measure");
    assert_eq!(tools.states()[1], Value::Float(10.0));
}

#[test]
fn a_native_handle_method_takes_its_receiver_first() {
    let (mut vm, mut tools) = linked(NATIVES, "Tools", &[]);
    call(&mut vm, &mut tools, "time", &[]).expect("time");
    let elapsed = tools.states()[2].as_float().expect("elapsed");
    assert!(elapsed >= 0.0, "{elapsed}");
}

#[test]
fn vectors_are_plain_values_with_f32_arithmetic() {
    let source = r#"
import viso::math::{Vec2F32, Vec3F32};
component Mover {
    state at: Vec3F32 = Vec3F32::new(1.0f32, 2.0f32, 2.0f32);
    state len = 0.0f32;
    state same = false;
    state flat = 0.0f32;
    action step() {
        at = at.add(Vec3F32::new(0.1f32, 0.0f32, 0.0f32)).scale(2.0f32).sub(at);
        len = Vec3F32::new(1.0f32, 2.0f32, 2.0f32).length();
        same = at == Vec3F32::new(1.2f32, 2.0f32, 2.0f32);
        flat = Vec2F32::new(3.0f32, 4.0f32).length() + at.x;
    }
    view { Text { text: "x"; } }
}
"#;
    let module = module(source);
    let index = module.component("Mover").expect("component");
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let mut mover = vm.instantiate(index, []).expect("instantiate");
    call(&mut vm, &mut mover, "step", &[]).expect("step");
    let x = (1.0f32 + 0.1f32) * 2.0f32 - 1.0f32;
    let at = |i: usize| match &mover.states()[0] {
        Value::Agg(agg) => agg.fields[i].clone(),
        other => panic!("{other:?}"),
    };
    assert_eq!(at(0), Value::Float(f64::from(x)));
    assert_eq!(at(1), Value::Float(2.0));
    assert_eq!(mover.states()[1], Value::Float(3.0));
    assert_eq!(mover.states()[2], Value::bool(x == 1.2f32));
    assert_eq!(mover.states()[3], Value::Float(f64::from(5.0f32 + x)));
}

#[test]
fn a_native_error_faults_the_call() {
    let (mut vm, mut tools) = linked(NATIVES, "Tools", &[]);
    let fault = call(&mut vm, &mut tools, "bad_clamp", &[]).expect_err("empty range");
    assert_eq!(fault.kind, FaultKind::NativeFailure);
    assert_eq!(fault.kind.code(), "E7106");
    assert_eq!(tools.states()[1], Value::Float(0.0));
}

#[test]
fn an_unlinked_native_cannot_run() {
    let (mut vm, mut tools) = instance(NATIVES, "Tools");
    let fault = call(&mut vm, &mut tools, "measure", &[Value::Float(4.0)]).expect_err("unlinked");
    assert_eq!(fault.kind, FaultKind::Unsupported);
}

#[test]
fn an_ungranted_capability_faults_at_the_call() {
    struct Memory(Rc<std::cell::RefCell<String>>);
    impl Clipboard for Memory {
        fn read_text(&mut self) -> Option<String> {
            Some(self.0.borrow().clone())
        }
        fn write_text(&mut self, text: &str) {
            *self.0.borrow_mut() = text.to_owned();
        }
    }

    let (mut vm, mut tools) = linked(NATIVES, "Tools", &[]);
    let fault = call(&mut vm, &mut tools, "copy", &[]).expect_err("denied");
    assert_eq!(fault.kind, FaultKind::CapabilityDenied);
    assert_eq!(fault.kind.code(), "E6103");

    let (mut vm, mut tools) = linked(NATIVES, "Tools", &["clipboard.write"]);
    let board = Rc::new(std::cell::RefCell::new(String::new()));
    let service: Box<dyn Clipboard> = Box::new(Memory(board.clone()));
    vm.services_mut().insert(service);
    call(&mut vm, &mut tools, "rename", &[Value::str("hi")]).expect("rename");
    call(&mut vm, &mut tools, "copy", &[]).expect("copy");
    assert_eq!(*board.borrow(), "hi");
}

#[test]
fn native_calls_spend_their_own_budget() {
    let source = r#"
import viso::math;
component Loop {
    state total = 0.0;
    action run(n: I64) {
        for i in 0..n { total = math::abs(total) + 1.0; }
    }
    view { Text { text: "x"; } }
}
"#;
    let module = module(source);
    let index = module.component("Loop").expect("component");
    let budget = Budget {
        native_calls: 3,
        ..Budget::default()
    };
    let mut vm = Vm::new(module, budget);
    vm.link(&Natives::standard(), &[]).expect("link");
    let mut looped = vm.instantiate(index, []).expect("instantiate");
    call(&mut vm, &mut looped, "run", &[Value::Int(3)]).expect("within budget");
    let fault = call(&mut vm, &mut looped, "run", &[Value::Int(4)]).expect_err("over budget");
    assert_eq!(fault.kind, FaultKind::NativeCallBudget);
    assert_eq!(fault.kind.code(), "E7101");
}

#[test]
fn linking_rejects_a_registry_without_the_import() {
    let (mut vm, _) = instance(NATIVES, "Tools");
    let conflict = vm.link(&Natives::new(), &[]).expect_err("unregistered");
    assert_eq!(conflict.code(), "E6101");
}

#[test]
fn a_native_capability_joins_the_callable_capability_set() {
    let compiled = compile_file(NATIVES, &origin());
    let component = compiled.component.expect("component");
    let set = |name: &str| {
        let callable = component.schema.callables.iter().find(|c| c.name == name);
        callable.expect("callable").meta.capability_set.clone()
    };
    assert!(set("copy").contains("clipboard.write"));
    assert!(!set("rename").contains("clipboard.write"));
}

#[test]
fn native_misuse_is_diagnosed() {
    let standard = Natives::standard;
    let unknown = "import viso::text;\ncomponent A { computed x: String = text::shout(\"a\"); view { Text { text: x; } } }";
    assert_eq!(native_errors(unknown, standard()), ["E2001"]);

    let action = "import viso::time::Stopwatch;\ncomponent A { computed x: F64 = Stopwatch::start().elapsed_ms(); view { Text { text: \"a\"; } } }";
    assert!(native_errors(action, standard()).contains(&"E2502".to_owned()));
}

static CUSTOM: NativeLibrary = NativeLibrary {
    path: "app::device",
    version: 1,
    functions: &[
        viso_behavior::native!(fn "scan" |_cx| -> i64 { Ok(1) }).on(ThreadDomain::Worker),
        viso_behavior::native!(fn "lease" |_cx| -> Obj<Lease> { Ok(Obj::new(Lease)) }),
    ],
    types: &[NativeType::new("Lease", &[]).borrowed()],
    traits: &[],
    derives: &[],
    widgets: &[],
};

#[derive(Debug)]
struct Lease;

impl NativeObject for Lease {
    const PATH: &'static str = "app::device::Lease";
}

fn custom() -> Arc<Natives> {
    let mut natives = Natives::new();
    natives.extend(STANDARD).expect("standard");
    natives.register(&CUSTOM).expect("custom");
    Arc::new(natives)
}

#[test]
fn a_worker_native_outside_a_task_is_diagnosed() {
    let source = "import app::device;\ncomponent A { state n = 0; action go() { n = device::scan(); } view { Text { text: \"a\"; } } }";
    assert_eq!(native_errors(source, custom()), ["E6102"]);
}

#[test]
fn a_borrowed_native_handle_cannot_be_stored() {
    let stored = "import app::device;\ncomponent A { state held = device::lease(); view { Text { text: \"a\"; } } }";
    assert_eq!(native_errors(stored, custom()), ["E6102"]);

    let local = "import app::device;\ncomponent A { action go() { let l = device::lease(); } view { Text { text: \"a\"; } } }";
    assert_eq!(native_errors(local, custom()), Vec::<String>::new());
}

const MEMO: &str = r#"
component M {
    state a = 1;
    state b = 10;
    computed doubled: I64 = a * 2;
    computed total: I64 = doubled + offset();
    fn offset() -> I64 { b }

    action bump() { a += 1; }
    action touch_b() { b += 1; }
    action boom(items: List<I64>) {
        a += 1;
        let x = doubled;
        a += items[5] + x;
    }

    view { Text { text: format("{n}", n: total); } }
}
"#;

#[test]
fn a_computed_is_evaluated_once_until_a_read_slot_changes() {
    let (mut vm, mut m) = instance(MEMO, "M");
    let total = member(&vm, &m, "total");
    let doubled = member(&vm, &m, "doubled");
    assert!(!m.is_cached(total));
    assert_eq!(call(&mut vm, &mut m, "total", &[]).unwrap(), Value::Int(12));
    assert!(m.is_cached(total) && m.is_cached(doubled));
    let first = vm.cost().instructions;
    assert_eq!(call(&mut vm, &mut m, "total", &[]).unwrap(), Value::Int(12));
    assert_eq!(vm.cost().instructions, 0, "a cached read runs no code");
    assert!(first > 0);

    // `b` is read by `total` through `offset`, not by `doubled`.
    call(&mut vm, &mut m, "touch_b", &[]).unwrap();
    assert!(!m.is_cached(total), "a write through a call invalidates");
    assert!(m.is_cached(doubled), "an unread slot leaves the cache");
    assert_eq!(call(&mut vm, &mut m, "total", &[]).unwrap(), Value::Int(13));

    call(&mut vm, &mut m, "bump", &[]).unwrap();
    assert!(!m.is_cached(total) && !m.is_cached(doubled));
    assert_eq!(call(&mut vm, &mut m, "total", &[]).unwrap(), Value::Int(15));
}

#[test]
fn a_host_write_invalidates_only_on_change() {
    let (mut vm, mut m) = instance(MEMO, "M");
    let doubled = member(&vm, &m, "doubled");
    call(&mut vm, &mut m, "doubled", &[]).unwrap();
    m.set_state(0, Value::Int(1));
    assert!(m.is_cached(doubled), "an equal value keeps the cache");
    m.set_state(0, Value::Int(4));
    assert!(!m.is_cached(doubled));
    assert_eq!(
        call(&mut vm, &mut m, "doubled", &[]).unwrap(),
        Value::Int(8)
    );
}

#[test]
fn a_faulted_transaction_discards_what_it_computed() {
    let (mut vm, mut m) = instance(MEMO, "M");
    let doubled = member(&vm, &m, "doubled");
    call(&mut vm, &mut m, "doubled", &[]).unwrap();
    // `boom` writes `a`, reads `doubled` at the written value, then faults.
    call(&mut vm, &mut m, "boom", &[ints(&[1])]).unwrap_err();
    assert_eq!(m.states()[0], Value::Int(1));
    assert!(
        !m.is_cached(doubled),
        "a value seen only inside the fault is gone"
    );
    assert_eq!(
        call(&mut vm, &mut m, "doubled", &[]).unwrap(),
        Value::Int(2)
    );
}

#[test]
fn a_computed_reaching_itself_is_a_reactive_cycle() {
    let (mut vm, mut c) = instance(
        r#"
component C {
    state a = 1;
    computed x: I64 = a + again();
    fn again() -> I64 { x }
    view { Text { text: format("{n}", n: x); } }
}
"#,
        "C",
    );
    let fault = call(&mut vm, &mut c, "x", &[]).unwrap_err();
    assert_eq!(fault.kind, FaultKind::ReactiveCycle);
    assert_eq!(fault.kind.code(), "E4202");
    assert!(!c.is_cached(member(&vm, &c, "x")));
}
