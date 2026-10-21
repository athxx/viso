//! A component's handlers interpreted against the same handlers lowered to
//! Rust (ADR 0041), on the differential test's fixture
//! (`tests/differential/core.vs`, lowered in `tests/differential/core.rs`):
//! a release `view!` runs the lowered code, a debug build the interpreter.
//!
//! - `release_handler_native_vs_vm/{vm,native}/action`: one action that
//!   writes a state and emits an event, then the computed reading it.
//! - `.../fib`: a call-bound function (`fib(15)`).
//! - `.../loop`: an instruction-bound loop over a list.
//!
//! Run release (`cargo bench -p viso-dsl --bench behavior_native`).

use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_behavior::aot::NativeCode;
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Instance, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file};

#[rustfmt::skip]
#[path = "../tests/differential/core.rs"]
mod core_native;

fn side(native: Option<&'static NativeCode>) -> (Vm, Instance) {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    };
    let compiled = compile_file(include_str!("../tests/differential/core.vs"), &origin);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let component = module.component("Core").expect("component");
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    if let Some(code) = native {
        vm.install_native(code).expect("the lowered build");
    }
    let instance = vm.instantiate(component, []).expect("instantiate");
    (vm, instance)
}

fn bench(c: &mut Criterion) {
    let list = Value::List(Rc::new((0..256).map(Value::Int).collect()));
    for (name, native) in [("vm", None), ("native", Some(&core_native::NATIVE))] {
        let (mut vm, mut instance) = side(native);
        let layout = vm.module().layout(instance.component().expect("component"));
        let [add, total, fib, sum] =
            ["add", "total", "fib", "sum"].map(|m| layout.member(m).expect("member"));
        let mut group = c.benchmark_group(format!("release_handler_native_vs_vm/{name}"));
        group.bench_function("action", |b| {
            let mut by = 1;
            b.iter(|| {
                by = -by;
                vm.call(&mut instance, add, &[Value::Int(by)]).expect("add");
                instance.clear_dirty();
                black_box(vm.call(&mut instance, total, &[]).expect("total"));
            });
        });
        group.bench_function("fib", |b| {
            b.iter(|| black_box(vm.call(&mut instance, fib, &[Value::Int(15)]).expect("fib")));
        });
        group.bench_function("loop", |b| {
            b.iter(|| {
                black_box(
                    vm.call(&mut instance, sum, std::slice::from_ref(&list))
                        .expect("sum"),
                )
            });
        });
        group.finish();
    }
}

criterion_group!(benches, bench);
criterion_main!(benches);
