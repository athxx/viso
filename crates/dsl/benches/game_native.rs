//! Bytecode against native lowering of System IR (§108.2): one fixed tick
//! of the differential test's games (`tests/native/*.vs`), each run by the
//! scheduler interpreting its bytecode and running its systems lowered to
//! Rust (`tests/native/*.rs`). `crowd` is bytecode-bound; `colony` spends
//! much of a tick in natives, the world and physics.
//!
//! Run release (`cargo bench -p viso-dsl --bench game_native`); criterion
//! defaults to a release profile. Debug timing is not a perf result.

use std::rc::Rc;

use criterion::{Criterion, criterion_group, criterion_main};
use viso_behavior::aot::NativeCode;
use viso_behavior::game::Scheduler;
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};

#[rustfmt::skip]
#[path = "../tests/native/colony.rs"]
mod colony;
#[rustfmt::skip]
#[path = "../tests/native/crowd.rs"]
mod crowd;

fn game(source: &str, native: Option<&'static NativeCode>) -> Scheduler {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let profile = TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(source, &origin, Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = Rc::new(compiled.behavior.bytecode().expect("verified bytecode"));
    let budget = Budget {
        instructions: 1 << 40,
        memory: 1 << 32,
        depth: 256,
        native_calls: u32::MAX,
    };
    let mut vm = Vm::new(module, budget);
    vm.link(&Natives::standard(), &[]).expect("link");
    if let Some(code) = native {
        vm.install_native(code).expect("the lowered build");
    }
    let mut game = Scheduler::with_seed(vm, 9).expect("a started game");
    game.step(30);
    game
}

fn step(c: &mut Criterion) {
    let games: [(&str, &str, &'static NativeCode); 2] = [
        (
            "crowd",
            include_str!("../tests/native/crowd.vs"),
            &crowd::NATIVE,
        ),
        (
            "colony",
            include_str!("../tests/native/colony.vs"),
            &colony::NATIVE,
        ),
    ];
    let mut group = c.benchmark_group("game_native");
    for (name, source, code) in games {
        // Both run to the same hash, so the timing compares equal work.
        let (mut a, mut b) = (game(source, None), game(source, Some(code)));
        a.step(100);
        b.step(100);
        assert_eq!(a.snapshot().hash(), b.snapshot().hash(), "{name}");
        for (mode, native) in [("bytecode", None), ("native", Some(code))] {
            let mut game = game(source, native);
            group.bench_function(format!("{name}/{mode}"), |b| b.iter(|| game.step(1)));
        }
    }
    group.finish();
}

criterion_group!(benches, step);
criterion_main!(benches);
