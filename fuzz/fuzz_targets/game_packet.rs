//! A packet from another peer of a rollback session: taking it never panics,
//! whether it decodes or not, and the session steps on after one it took.

#![no_main]

use std::rc::Rc;

use libfuzzer_sys::fuzz_target;
use viso_behavior::game::{RollbackSession, Scheduler, SessionConfig};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};

thread_local! {
    static MODULE: Rc<Module> = compile();
}

fn compile() -> Rc<Module> {
    let origin = Origin {
        package: "fuzz".to_owned(),
        module: vec!["game".to_owned()],
        ..Default::default()
    };
    let profile = TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(
        include_str!("../game.vs"),
        &origin,
        Natives::standard(),
        profile,
    );
    assert!(compiled.errors().next().is_none(), "the fuzz game compiles");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn session(module: &Rc<Module>) -> RollbackSession {
    let mut vm = Vm::new(module.clone(), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let game = Scheduler::with_seed(vm, 1).expect("a started game");
    RollbackSession::new(game, SessionConfig::new(2, 0)).expect("a session")
}

fuzz_target!(|data: &[u8]| {
    MODULE.with(|module| {
        let mut peer = session(module);
        if peer.receive(data).is_ok() {
            peer.advance(2);
            let _ = peer.receive(data);
        }
    });
});
