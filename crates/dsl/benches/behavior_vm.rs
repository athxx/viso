//! Behavior dispatch cost: one counter `action` transaction (load, add, store,
//! emit, commit) and one arithmetic loop, run through the bytecode interpreter.
//!
//! Run release (`cargo bench -p viso-dsl --bench behavior_vm`); criterion
//! defaults to a release profile. Debug timing is not a perf result.

use std::hint::black_box;
use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_behavior::{Budget, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file};

const SOURCE: &str = r#"
component Counter {
    state count = 0;
    event changed(value: I64);

    action increment() {
        count += 1;
        emit changed(count);
    }

    fn sum(n: I64) -> I64 {
        let mut total = 0;
        for i in 0..n {
            total += i * 3;
        }
        total
    }

    view { Text { text: format("{n}", n: count); } }
}
"#;

fn behavior(c: &mut Criterion) {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let compiled = compile_file(SOURCE, &origin);
    assert!(!compiled.has_errors());
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let counter = module.component("Counter").expect("component");
    let layout = module.layout(counter).clone();
    let increment = layout.member("increment").expect("increment");
    let sum = layout.member("sum").expect("sum");
    let mut vm = Vm::new(module, Budget::default());
    let mut instance = vm.instantiate(counter, []).expect("instantiate");

    c.bench_function("behavior/counter_increment", |b| {
        b.iter(|| {
            let outcome = vm.call(&mut instance, increment, &[]).expect("increment");
            black_box(outcome);
        });
    });
    c.bench_function("behavior/sum_loop_1000", |b| {
        b.iter(|| {
            let outcome = vm
                .call(&mut instance, sum, &[Value::Int(black_box(1000))])
                .expect("sum");
            black_box(outcome.value);
        });
    });
}

criterion_group!(benches, behavior);
criterion_main!(benches);
