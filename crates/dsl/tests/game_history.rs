//! The dev history: a ring of snapshots over the last seconds and the input
//! every tick read, so a replay runs that stretch again — with the same code
//! it reaches the same state, after a logic reload what the new code makes
//! of the same play.

use std::rc::Rc;

use viso_behavior::game::{Key, ReplayError, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_in};

fn module(source: &str) -> Rc<Module> {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let compiled = compile_file_in(source, &origin, Natives::standard());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn vm(source: &str) -> Vm {
    let mut vm = Vm::new(module(source), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    vm
}

fn game(source: &str) -> Scheduler {
    Scheduler::new(vm(source)).expect("a started game")
}

fn state(game: &Scheduler, name: &str) -> Value {
    let module = game.vm().module();
    let slot = module
        .layout(module.systems()[0].component)
        .state(name)
        .expect("state");
    game.instance(0).states()[slot].clone()
}

const PAD: &str = r#"
import viso::game::{FixedUpdate, FixedFrame, InputAction, InputAxis};

export system Pad implements FixedUpdate {
    state presses = 0;
    state held = 0;
    state x = 0.0;
    state roll = 0;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) { presses += 1; }
        if frame.input.held(InputAction::jump) { held += 1; }
        x += frame.input.axis(InputAxis::move_x);
        roll += frame.world.random_range(0, 10);
    }
}
"#;

/// Runs ticks `from..to` of `game`, the keys going up and down on a fixed
/// pattern before each.
fn drive(game: &mut Scheduler, from: u64, to: u64) {
    for tick in from..to {
        game.key(Key::Space, (tick / 7) % 2 == 0);
        game.key(Key::D, (tick / 50) % 2 == 0);
        game.step(1);
    }
}

fn values(game: &Scheduler) -> [Value; 4] {
    ["presses", "held", "x", "roll"].map(|name| state(game, name))
}

#[test]
fn a_replay_with_the_same_code_reaches_the_same_state() {
    let mut game = game(PAD);
    game.keep_history(10);
    drive(&mut game, 0, 120);
    let before = game.snapshot().hash();
    let replayed = game.replay_from(50).expect("replays");
    assert_eq!(
        (replayed.from, replayed.ticks),
        (45, 75),
        "a snapshot every 15 ticks"
    );
    assert_eq!(game.clock().tick(), 120);
    assert_eq!(game.snapshot().hash(), before);
    // Device input stays with the devices: the next tick reads it.
    game.key(Key::Space, true);
    game.key(Key::D, false);
    let presses = state(&game, "presses");
    game.replay_from(100).expect("replays");
    game.step(1);
    let Value::Int(n) = presses else {
        panic!("{presses:?}")
    };
    let pressed_now = (119 / 7) % 2 != 0;
    assert_eq!(
        state(&game, "presses"),
        Value::Int(n + i64::from(pressed_now))
    );
}

#[test]
fn after_a_logic_reload_a_replay_runs_the_new_code_on_the_same_play() {
    let doubled = PAD.replace("held += 1;", "held += 2;");
    // The reference reloads at tick 60 and plays on.
    let mut reference = game(PAD);
    drive(&mut reference, 0, 60);
    reference.reload(vm(&doubled)).expect("reloads");
    drive(&mut reference, 60, 120);

    let mut game = game(PAD);
    game.keep_history(10);
    drive(&mut game, 0, 120);
    let old = values(&game);
    game.reload(vm(&doubled)).expect("reloads");
    let replayed = game.replay_from(60).expect("replays");
    assert_eq!((replayed.from, replayed.ticks), (60, 60));
    assert_eq!(replayed.restored.states, 4);
    assert_ne!(values(&game)[1], old[1], "the new code ran");
    assert_eq!(values(&game), values(&reference));
    assert_eq!(game.snapshot().hash(), reference.snapshot().hash());
}

#[test]
fn the_history_keeps_only_its_window() {
    let mut game = game(PAD);
    assert_eq!(game.replay_from(0), Err(ReplayError::NoHistory));
    game.keep_history(1);
    assert_eq!(game.history(), None);
    drive(&mut game, 0, 600);
    let (oldest, kept) = game.history().expect("a history");
    assert_eq!((oldest, kept), (525, 5), "a second at 60 ticks, every 15");
    assert_eq!(
        game.replay_from(oldest - 1),
        Err(ReplayError::Forgotten {
            oldest: Some(oldest)
        })
    );
    assert_eq!(game.replay_from(601), Err(ReplayError::Ahead));
    // `D` is held from tick 500 to 549, across the cut the window made.
    let before = game.snapshot().hash();
    game.replay_from(oldest).expect("replays the window");
    assert_eq!(game.snapshot().hash(), before);
    game.drop_history();
    assert_eq!(game.history(), None);
}
