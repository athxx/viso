use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::{
    Budget, Chunk, ChunkKind, Code, FaultKind, Instance, Module, NativeImport, Op, Span, Vm,
};

static UPPER: NativeFunction =
    crate::native!(fn "upper" |_cx, text: String| -> String { Ok(text.to_uppercase()) })
        .deterministic();

static TEST: NativeLibrary = NativeLibrary {
    path: "test",
    version: 1,
    functions: &[
        crate::native!(action "boom" |_cx| -> () { panic!("kaboom") }),
        crate::native!(fn "fail" |_cx| -> i64 { Err(NativeError::new("no luck")) }),
        crate::native!(task "fetch" |_cx| -> String { Ok(String::new()) }).on(ThreadDomain::Worker),
        crate::native!(fn "sum" |_cx, items: Vec<i64>| -> i64 { Ok(items.iter().sum()) }).cost(10),
    ],
    types: &[NativeType::new("Frame", &[]).borrowed()],
    traits: &[],
    widgets: &[],
};

static TEST_V2: NativeLibrary = NativeLibrary { version: 2, ..TEST };

static OTHER_UPPER: NativeLibrary = NativeLibrary {
    path: "viso::text",
    version: 1,
    functions: &[
        crate::native!(fn "upper" |_cx, text: String, n: i64| -> String {
            Ok(text.repeat(n as usize))
        }),
    ],
    types: &[],
    traits: &[],
    widgets: &[],
};

static TWICE: NativeLibrary = NativeLibrary {
    path: "twice",
    version: 1,
    functions: &[
        crate::native!(fn "f" |_cx| -> i64 { Ok(1) }),
        crate::native!(fn "f" |_cx| -> i64 { Ok(2) }),
    ],
    types: &[],
    traits: &[],
    widgets: &[],
};

#[test]
fn the_macro_derives_the_schema_from_the_rust_signature() {
    assert_eq!(UPPER.kind, NativeKind::Fn);
    assert_eq!(
        UPPER.params,
        &[Param {
            name: "text",
            ty: SchemaTy::String
        }]
    );
    assert_eq!(UPPER.ret, SchemaTy::String);
    assert!(UPPER.deterministic);
    assert_eq!(UPPER.thread, ThreadDomain::Any);
    assert_eq!(
        format!("{UPPER:?}"),
        "native fn upper(text: String) -> String"
    );
    let sum = &TEST.functions[3];
    assert_eq!(sum.params[0].ty, SchemaTy::List(&SchemaTy::I64));
    assert_eq!(sum.cost, 10);
    assert_eq!(TEST.functions[2].kind, NativeKind::Task);
}

#[test]
fn the_standard_registry_resolves_functions_types_and_methods() {
    let natives = Natives::standard();
    assert!(natives.is_library("viso::math"));
    let upper = natives.function("viso::text::upper").expect("upper");
    assert_eq!(upper.id, NativeId::of("viso::text::upper"));
    assert_eq!(upper.function.signature(), UPPER.signature());
    let stopwatch = natives.ty("viso::time::Stopwatch").expect("Stopwatch");
    let elapsed = natives
        .method(stopwatch.id, "elapsed_ms")
        .expect("elapsed_ms");
    assert!(elapsed.is_method(&natives));
    let start = natives.method(stopwatch.id, "start").expect("start");
    assert!(!start.is_method(&natives));
    assert_eq!(
        start.function.ret,
        SchemaTy::Handle("viso::time::Stopwatch")
    );
    let read = natives.function("viso::clipboard::read_text").unwrap();
    assert_eq!(read.function.capabilities, &["clipboard.read"]);
}

#[test]
fn conflicting_schemas_are_rejected() {
    let mut natives = Natives::with(&[&TEST]).expect("registry");
    natives
        .register(&TEST)
        .expect("the same library again is a no-op");
    let e = natives.register(&TEST_V2).unwrap_err();
    assert_eq!(e.code(), "E6101");
    assert_eq!(e.message, "`test` is registered at schema version 1 and 2");
    let e = natives.register(&OTHER_UPPER).unwrap_err();
    assert_eq!(
        e.message,
        "`viso::text::upper` is registered with two different schemas"
    );
    let e = Natives::with(&[&TWICE]).unwrap_err();
    assert_eq!(e.message, "`twice::f` is declared twice");
    assert_eq!(
        natives.ty("test::Frame").unwrap().ty.ownership,
        Ownership::Borrowed
    );
}

/// A module whose one chunk calls native `path` with `params` arguments
/// passed straight through, and a VM over it.
fn call_module(path: &str, params: u16) -> (Vm, u32) {
    let natives = Natives::with(&[&TEST]).unwrap();
    let signature = natives.function(path).expect("native").function.signature();
    let mut ext = vec![0, u32::from(params)];
    ext.extend(0..u32::from(params));
    let ops = vec![Op::Native { dst: 0, ext: 0 }, Op::Return { src: 0 }];
    let chunk = Chunk {
        name: "f".into(),
        kind: ChunkKind::Action,
        module: 0,
        params,
        regs: params.max(1),
        captures: Box::new([]),
        body: Ok(Code {
            spans: vec![Span::default(); ops.len()].into(),
            ops: ops.into(),
            consts: Box::new([]),
            ext: ext.into(),
        }),
    };
    let import = NativeImport {
        path: path.into(),
        signature,
        params,
    };
    let module = Module::new(vec![chunk], Vec::new(), Vec::new(), vec![import]).expect("verified");
    (Vm::new(Rc::new(module), Budget::default()), 0)
}

fn run(vm: &mut Vm, args: &[Value]) -> Result<Value, crate::Fault> {
    vm.call(&mut Instance::detached(), 0, args).map(|o| o.value)
}

fn linked(path: &str, params: u16, capabilities: &[&str]) -> Vm {
    let (mut vm, _) = call_module(path, params);
    vm.link(&Natives::with(&[&TEST]).unwrap(), capabilities)
        .expect("link");
    vm
}

#[test]
fn a_linked_native_runs_and_is_budgeted() {
    let mut vm = linked("viso::text::upper", 1, &[]);
    assert_eq!(run(&mut vm, &[Value::str("hi")]), Ok(Value::str("HI")));
    let cost = vm.cost();
    assert_eq!((cost.instructions, cost.native_calls), (2, 1));
    assert_eq!(cost.memory, 16 + 2);

    let mut vm = linked("test::sum", 1, &[]);
    let items = Value::List(Rc::new(vec![Value::Int(2), Value::Int(3)]));
    assert_eq!(
        run(&mut vm, std::slice::from_ref(&items)),
        Ok(Value::Int(5))
    );
    assert_eq!(vm.cost().instructions, 11, "a call spends its schema cost");
    vm.set_budget(Budget {
        native_calls: 0,
        ..Budget::default()
    });
    let fault = run(&mut vm, &[items]).unwrap_err();
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::NativeCallBudget, "E7101")
    );
}

#[test]
fn an_unlinked_or_mismatched_native_does_not_run() {
    let (mut vm, _) = call_module("viso::text::upper", 1);
    let fault = run(&mut vm, &[Value::str("x")]).unwrap_err();
    assert_eq!(fault.kind, FaultKind::Unsupported);
    assert_eq!(fault.message, "native `viso::text::upper` is not linked");

    let e = vm
        .link(&Natives::new(), &[])
        .expect_err("an empty registry lacks it");
    assert_eq!(e.message, "`viso::text::upper` is not registered");
    let mut other = Natives::new();
    other.register(&OTHER_UPPER).unwrap();
    let e = vm.link(&other, &[]).unwrap_err();
    assert_eq!(e.code(), "E6101");
    assert!(e.message.contains("another schema"), "{}", e.message);
}

#[test]
fn a_native_error_or_panic_is_a_fault() {
    let mut vm = linked("test::fail", 0, &[]);
    let fault = run(&mut vm, &[]).unwrap_err();
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::NativeFailure, "E7106")
    );
    assert_eq!(fault.message, "native `test::fail` failed: no luck");

    let mut vm = linked("test::boom", 0, &[]);
    let fault = run(&mut vm, &[]).unwrap_err();
    assert_eq!(fault.kind, FaultKind::NativeFailure);
    assert_eq!(fault.message, "native `test::boom` panicked: kaboom");
    // The interpreter stays usable after a panic.
    assert!(run(&mut vm, &[]).is_err());

    let mut vm = linked("test::fetch", 0, &[]);
    let fault = run(&mut vm, &[]).unwrap_err();
    assert_eq!(fault.kind, FaultKind::Unsupported);
    assert!(fault.message.contains("worker thread"), "{}", fault.message);
}

#[derive(Default)]
struct Board(Rc<RefCell<Option<String>>>);

impl Clipboard for Board {
    fn read_text(&mut self) -> Option<String> {
        self.0.borrow().clone()
    }

    fn write_text(&mut self, text: &str) {
        *self.0.borrow_mut() = Some(text.to_owned());
    }
}

#[test]
fn capabilities_gate_native_calls() {
    let mut vm = linked("viso::clipboard::write_text", 1, &[]);
    let fault = run(&mut vm, &[Value::str("x")]).unwrap_err();
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::CapabilityDenied, "E6103")
    );
    assert_eq!(
        fault.message,
        "native `viso::clipboard::write_text` requires capability `clipboard.write`, which is not granted"
    );

    let mut vm = linked("viso::clipboard::write_text", 1, &["clipboard.write"]);
    let fault = run(&mut vm, &[Value::str("x")]).unwrap_err();
    assert!(
        fault.message.contains("provides no"),
        "a missing service is a native failure: {}",
        fault.message
    );
    let board = Board::default();
    let contents = Rc::clone(&board.0);
    vm.services_mut()
        .insert::<Box<dyn Clipboard>>(Box::new(board));
    assert_eq!(run(&mut vm, &[Value::str("copied")]), Ok(Value::Int(0)));
    assert_eq!(contents.borrow().as_deref(), Some("copied"));
}

#[test]
fn handles_carry_native_objects() {
    let natives = Natives::standard();
    let start = natives.function("viso::time::Stopwatch::start").unwrap();
    let elapsed = natives
        .function("viso::time::Stopwatch::elapsed_ms")
        .unwrap();
    let mut services = Services::default();
    let mut cx = NativeCx::new(&mut services);
    let handle = (start.function.call)(&mut cx, &[]).expect("start");
    assert!(matches!(&handle, Value::Handle(h) if h.path() == "viso::time::Stopwatch"));
    assert_eq!(handle, handle.clone(), "handles compare by identity");
    let ms = (elapsed.function.call)(&mut cx, std::slice::from_ref(&handle)).expect("elapsed");
    assert!(ms.as_float().is_some_and(|ms| ms >= 0.0));
    let e = (elapsed.function.call)(&mut cx, &[Value::Int(1)]).unwrap_err();
    assert_eq!(e.message, "argument `this` does not match its schema type");
}

static ROW_AGAIN: NativeLibrary = NativeLibrary {
    path: "other",
    version: 1,
    functions: &[],
    types: &[],
    traits: &[],
    widgets: &[NativeWidget::new("Row", WidgetNode::Leaf)],
};

#[test]
fn the_standard_widgets_are_indexed_by_name() {
    let natives = Natives::standard();
    let row = natives.widget("Row").expect("Row is a standard widget");
    assert_eq!(row.node, WidgetNode::Flex(FlexAxis::Row));
    assert_eq!(
        row.default_slot().map(|s| s.cardinality),
        Some(SlotCardinality::Many)
    );
    assert!(row.property("width").is_some_and(|p| p.percent_basis));
    assert!(row.event("click").is_some_and(|e| e.bubbles));
    let slider = natives
        .widget("Slider")
        .expect("Slider is a standard widget");
    assert!(slider.default_slot().is_none());
    assert_eq!(
        slider.write_back("value").and_then(|e| e.payload),
        Some("SliderChanged")
    );
    assert!(slider.write_back("min").is_none());
    let tabs = natives.widget("Tabs").expect("Tabs is a standard widget");
    assert_eq!(
        tabs.write_back("selected").map(|e| e.name),
        Some("selected_changed")
    );
    assert!(natives.widget("Leaf").is_none());
}

#[test]
fn a_widget_declared_twice_differently_conflicts() {
    let error = Natives::with(&[&ROW_AGAIN]).expect_err("conflicts");
    assert_eq!(error.code(), "E6101");
    assert_eq!(error.path, "other::Row");
}
