//! Release native lowering of System IR (§108.2): a game's systems lowered
//! from bytecode to Rust, compiled with this test, and run by the same
//! scheduler. The lowered modules are checked in (`native/*.rs`, from
//! `native/*.vs`) and must be what the lowering writes today (`VISO_BLESS=1`
//! rewrites them); the differential tests then run one input tape under
//! bytecode and under the compiled code and compare every tick's snapshot
//! hash and every fault. `benches/game_native.rs` times both.

use std::rc::Rc;

use viso_behavior::aot::lower_systems;
use viso_behavior::game::{InputTape, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};

#[rustfmt::skip]
#[path = "native/colony.rs"]
mod colony;
#[rustfmt::skip]
#[path = "native/crowd.rs"]
mod crowd;

/// A game of seven systems exercising the instruction set: integer and float
/// arithmetic, casts, records and enums with matches, lists, field and
/// element writes, closures, strings, natives, world commands, collisions,
/// a fault every 97 ticks, and a crowd of 32 agents updated every tick.
pub const COLONY: &str = include_str!("native/colony.vs");

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

/// A crowd of 128 agents a single system steps every tick: bytecode-bound.
pub const CROWD: &str = include_str!("native/crowd.vs");

pub fn colony() -> Rc<Module> {
    compiled(COLONY)
}

fn compiled(source: &str) -> Rc<Module> {
    let profile = TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn tape() -> InputTape {
    InputTape::parse(
        "5..200: axis move = (1, 0)\n30: tap jump\n120: tap jump\n200..260: axis move = (-1, 1)",
        9,
        60,
    )
    .expect("a tape")
}

fn game(module: &Rc<Module>, native: bool) -> Scheduler {
    let mut vm = Vm::new(module.clone(), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    if native {
        let chunks = vm
            .install_native(&colony::NATIVE)
            .expect("the lowered build");
        assert!(chunks > 10, "{chunks} chunks");
    }
    let mut game = Scheduler::with_seed(vm, 9).expect("a started game");
    game.play(tape()).expect("plays");
    game
}

#[test]
fn the_checked_in_lowerings_are_current() {
    for (name, source) in [("colony", COLONY), ("crowd", CROWD)] {
        let lowered = lower_systems(&compiled(source), "viso_behavior");
        let path = format!("{}/tests/native/{name}.rs", env!("CARGO_MANIFEST_DIR"));
        if std::env::var_os("VISO_BLESS").is_some() {
            std::fs::write(&path, &lowered).expect("writes the lowering");
            continue;
        }
        let checked_in = std::fs::read_to_string(&path).expect("reads the lowering");
        assert!(
            checked_in == lowered,
            "tests/native/{name}.rs is stale: rerun with VISO_BLESS=1"
        );
        let again = lower_systems(&compiled(source), "viso_behavior");
        assert_eq!(lowered, again, "deterministic");
    }
}

#[test]
fn a_bytecode_bound_crowd_matches_too() {
    let module = compiled(CROWD);
    let start = |native: bool| {
        let mut vm = Vm::new(module.clone(), Budget::default());
        vm.link(&Natives::standard(), &[]).expect("link");
        if native {
            vm.install_native(&crowd::NATIVE)
                .expect("the lowered build");
        }
        Scheduler::with_seed(vm, 1).expect("a started game")
    };
    let (mut bytecode, mut native) = (start(false), start(true));
    for tick in 0..120 {
        bytecode.step(1);
        native.step(1);
        assert_eq!(
            bytecode.snapshot().hash(),
            native.snapshot().hash(),
            "tick {tick}"
        );
    }
    assert!(bytecode.faults().is_empty() && native.faults().is_empty());
}

#[test]
fn bytecode_and_native_code_reach_the_same_snapshot_hashes() {
    let module = colony();
    let mut bytecode = game(&module, false);
    let mut native = game(&module, true);
    let hook = module.systems()[0].hooks[0].1;
    assert!(native.vm().is_native(hook) && !bytecode.vm().is_native(hook));
    let mut faults = 0;
    for tick in 0..400 {
        bytecode.step(1);
        native.step(1);
        assert_eq!(
            bytecode.snapshot().hash(),
            native.snapshot().hash(),
            "tick {tick}"
        );
        let (a, b) = (bytecode.take_faults(), native.take_faults());
        assert_eq!(a.len(), b.len(), "tick {tick}");
        faults += a.len();
        for (a, b) in a.iter().zip(&b) {
            assert_eq!(
                (a.system, a.tick, a.code, a.fault.kind, &a.fault.message),
                (b.system, b.tick, b.code, b.fault.kind, &b.fault.message),
                "tick {tick}"
            );
            assert_eq!(a.fault.at, b.fault.at, "tick {tick}");
        }
    }
    assert_eq!(faults, 4, "the Ledger's fault every 97 ticks");
    assert_eq!(bytecode.delivered_commands(), native.delivered_commands());
    assert!(native.delivered_commands() > 0);
}

#[test]
fn native_code_spends_the_budget_as_bytecode_does() {
    let module = colony();
    for instructions in [50, 400, 3_000] {
        let budget = Budget {
            instructions,
            ..Budget::default()
        };
        let run = |native: bool| {
            let mut game = game(&module, native);
            game.set_budget(budget);
            let mut trace = Vec::new();
            for _ in 0..60 {
                game.step(1);
                let faults = game.take_faults();
                trace.push((
                    game.snapshot().hash(),
                    faults
                        .iter()
                        .map(|f| (f.system, f.code, f.fault.at))
                        .collect::<Vec<_>>(),
                ));
            }
            trace
        };
        assert_eq!(run(false), run(true), "{instructions} instructions a tick");
    }
}

#[test]
fn code_lowered_from_another_build_is_refused() {
    let other = lower_systems(&colony(), "viso_behavior");
    assert!(other.contains("pub static NATIVE"));
    let profile = TargetProfile::default();
    let compiled = compile_file_for(COLONY, &origin(), Natives::standard(), profile);
    let same_binary = Rc::new(compiled.behavior.bytecode().expect("verified"));
    let mut vm = Vm::new(same_binary, Budget::default());
    let error = vm
        .install_native(&colony::NATIVE)
        .expect_err("another build");
    assert_ne!(error.expected, error.found);
}
