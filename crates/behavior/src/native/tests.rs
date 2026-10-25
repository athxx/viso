use alloc::borrow::ToOwned;
use alloc::rc::Rc;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::*;
use crate::{
    Body, Budget, Chunk, ChunkKind, Code, FaultKind, Instance, Module, NativeImport, Op, Span, Vm,
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
    derives: &[],
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
    derives: &[],
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
    derives: &[],
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

#[test]
fn a_set_of_libraries_shares_one_registry() {
    assert!(Arc::ptr_eq(
        &Natives::shared(&[]).unwrap(),
        &Natives::standard()
    ));
    let test = Natives::shared(&[&TEST]).expect("registry");
    assert!(Arc::ptr_eq(&test, &Natives::shared(&[&TEST]).unwrap()));
    assert!(test.function("viso::text::upper").is_some());
    assert!(test.function("test::sum").is_some());
    static EXTRA: NativeLibrary = NativeLibrary {
        path: "extra",
        ..TEST
    };
    let both = Natives::shared(&[&TEST, &EXTRA]).expect("registry");
    assert!(both.function("extra::sum").is_some());
    assert!(!Arc::ptr_eq(&test, &both));
    assert_eq!(
        Natives::shared(&[&TEST_V2, &TEST]).unwrap_err().code(),
        "E6101"
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
        body: Body::Code(Code {
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
        run(&mut vm, core::slice::from_ref(&items)),
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
    let ms = (elapsed.function.call)(&mut cx, core::slice::from_ref(&handle)).expect("elapsed");
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
    derives: &[],
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

/// `viso::math`'s results, bit for bit. The software implementations use IEEE
/// basic operations only, so these bits are every Tier-1 target's; a change
/// here breaks `cross_platform` replays and rollback across versions.
const MATH_BITS: &[(&str, &[f64], u64)] = &[
    ("sin", &[0.5], 0x3fdeaee8744b05f0),
    ("sin", &[1e6], 0xbfd6664b2568d867),
    ("sin", &[-3.0], 0xbfc210386db6d55b),
    ("cos", &[0.5], 0x3fec1528065b7d50),
    ("cos", &[1e6], 0x3fedf9df9906d32c),
    ("cos", &[2.5], 0xbfe9a2f7ef858b7d),
    ("tan", &[1.2], 0x400493c43acb164d),
    ("tan", &[-0.7], 0xbfeaf406c2fc78ae),
    ("atan", &[0.3], 0x3fd2a73a661eaf06),
    ("atan", &[-40.0], 0xbff8bb9a63718f45),
    ("atan2", &[1.0, -2.0], 0x40056c6e7397f5ae),
    ("atan2", &[-0.25, 0.75], 0xbfd4978fa3269ee1),
    ("exp", &[1.7], 0x4015e552770df8a6),
    ("exp", &[-20.5], 0x3e157a3afeed00ab),
    ("log", &[2.5], 0x3fed5240f0e0e078),
    ("log", &[1e-9], 0xc034b927f32bffb8),
    ("pow", &[1.5, 2.7], 0x4007e859efb1238c),
    ("pow", &[10.0, -3.3], 0x3f406c4363887513),
    ("hypot", &[3.0, 4.0], 0x4014000000000000),
    ("hypot", &[1e300, 1e300], 0x7e40e4d50f99b211),
    ("hypot", &[0.1, 0.2], 0x3fcc9f25c5bfedd9),
];

#[test]
fn math_transcendentals_are_pinned_bit_for_bit() {
    let natives = Natives::standard();
    let mut services = Services::default();
    let mut cx = NativeCx::new(&mut services);
    for &(name, args, bits) in MATH_BITS {
        let entry = natives
            .function(&format!("viso::math::{name}"))
            .expect("registered");
        assert_eq!(entry.function.determinism, Determinism::CrossPlatform);
        let values: Vec<Value> = args.iter().map(|&x| Value::Float(x)).collect();
        let Ok(Value::Float(r)) = (entry.function.call)(&mut cx, &values) else {
            panic!("{name}{args:?}")
        };
        assert_eq!(r.to_bits(), bits, "{name}{args:?} = {r}");
        // Within one unit in the last place of the host's.
        let host = match (name, args) {
            ("sin", [x]) => x.sin(),
            ("cos", [x]) => x.cos(),
            ("tan", [x]) => x.tan(),
            ("atan", [x]) => x.atan(),
            ("atan2", [y, x]) => y.atan2(*x),
            ("exp", [x]) => x.exp(),
            ("log", [x]) => x.ln(),
            ("pow", [x, y]) => x.powf(*y),
            ("hypot", [x, y]) => x.hypot(*y),
            _ => unreachable!(),
        };
        assert!(
            (r.to_bits() as i64 - host.to_bits() as i64).abs() <= 1,
            "{name}{args:?}: {r} against the host's {host}"
        );
    }
}

#[test]
fn a_nested_path_counts_in_a_signature() {
    fn of(ret: SchemaTy) -> u64 {
        fn call(_: &mut NativeCx<'_>, _: &[Value]) -> Result<Value, NativeError> {
            Ok(Value::Nil)
        }
        NativeFunction::new("f", NativeKind::Fn, &[], ret, call).signature()
    }
    static A: SchemaTy = SchemaTy::Value("a::Item");
    static B: SchemaTy = SchemaTy::Value("b::Item");
    let (a, b) = (&A, &B);
    assert_ne!(of(SchemaTy::List(a)), of(SchemaTy::List(b)));
    assert_ne!(
        of(SchemaTy::Result(a, &SchemaTy::String)),
        of(SchemaTy::Result(b, &SchemaTy::String))
    );
    assert_ne!(
        of(SchemaTy::Result(&SchemaTy::I64, &SchemaTy::String)),
        of(SchemaTy::Option(&SchemaTy::I64))
    );
}

#[test]
fn an_async_task_native_hands_over_its_work_and_the_work_has_the_value() {
    static ASK: NativeFunction = crate::native!(task "ask" async |cx, id: i64| -> Result<i64, String> {
        let base = *cx.service::<i64>()?;
        Ok(async move { Ok(if id > 0 { Ok(base + id) } else { Err(String::from("no")) }) })
    });
    assert_eq!(ASK.kind, NativeKind::Task);
    assert_eq!(ASK.ret, SchemaTy::Result(&SchemaTy::I64, &SchemaTy::String));

    let mut services = Services::default();
    let mut cx = NativeCx::new(&mut services);
    // Without the service it fails before handing over any work.
    assert!((ASK.call)(&mut cx, &[Value::Int(1)]).is_err());
    assert!(cx.take_pending().is_none());

    services.insert(40_i64);
    let mut cx = NativeCx::new(&mut services);
    assert_eq!((ASK.call)(&mut cx, &[Value::Int(2)]), Ok(Value::Nil));
    let mut work = cx.take_pending().expect("the work");
    let mut poll = core::task::Context::from_waker(core::task::Waker::noop());
    let core::task::Poll::Ready(Ok(value)) = work.as_mut().poll(&mut poll) else {
        panic!("the work finishes at once");
    };
    assert_eq!(<Result<i64, String>>::from_value(&value), Some(Ok(42)));
}
