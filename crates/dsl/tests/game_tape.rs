//! Input tapes: the text and binary forms, recording what a run's ticks read,
//! and replaying it into the same snapshot hash.

use std::rc::Rc;

use viso_behavior::game::{InputTape, Key, Rebuild, Scheduler, TapeChange, TapeEvent};
use viso_behavior::native::{Determinism, Natives};
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

fn game(seed: u64) -> Scheduler {
    let mut vm = Vm::new(module(PAD), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::with_seed(vm, seed).expect("a started game")
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
    state releases = 0;
    state held = 0;
    state x = 0.0;
    state roll = 0;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) { presses += 1; }
        if frame.input.released(InputAction::jump) { releases += 1; }
        if frame.input.held(InputAction::jump) { held += 1; }
        x += frame.input.axis(InputAxis::move_x);
        roll += frame.world.random_range(0, 10);
    }
}
"#;

#[test]
fn the_text_form_reads_ticks_ranges_and_headers() {
    let tape = InputTape::parse(
        "# a jump and a run\n\
         seed 0x10\n\
         tick_rate 60\n\
         30: press jump\n\
         31..90: axis move = (1, 0)\n\
         40: release jump\n\
         95: tap fire\n\
         100..=101: press jump\n",
        7,
        30,
    )
    .expect("a tape");
    assert_eq!((tape.seed, tape.tick_rate, tape.ticks), (16, 60, 103));
    assert_eq!(tape.actions, ["jump", "fire"]);
    let changes: Vec<_> = tape.events.iter().map(|e| (e.tick, e.change)).collect();
    assert_eq!(
        changes,
        [
            (30, TapeChange::Press(0)),
            (31, TapeChange::Move(1.0, 0.0)),
            (40, TapeChange::Release(0)),
            (90, TapeChange::Move(0.0, 0.0)),
            (95, TapeChange::Press(1)),
            (95, TapeChange::Release(1)),
            (100, TapeChange::Press(0)),
            (102, TapeChange::Release(0)),
        ]
    );
    assert_eq!(InputTape::parse(&tape.to_text(), 0, 1), Ok(tape));
}

#[test]
fn a_malformed_text_tape_names_its_line() {
    for (text, line, message) in [
        (
            "seed 1\nx: press jump",
            2,
            "`x` is not a tick or tick range",
        ),
        ("5..5: press jump", 1, "`5..5` is not a tick or tick range"),
        ("\n\n3: wiggle jump", 3, "unknown change `wiggle`"),
        ("3..9: tap jump", 1, "`tap` takes a single tick"),
        (
            "3: axis turn = (1, 0)",
            1,
            "expected `axis move = (x, y)`, found `axis turn = (1, 0)`",
        ),
        ("speed 3", 1, "unknown header `speed`"),
        ("tick_rate 0", 1, "`0` is not a tick rate"),
    ] {
        let error = InputTape::parse(text, 0, 60).expect_err(text);
        assert_eq!(
            (error.line, error.message.as_str()),
            (Some(line), message),
            "{text}"
        );
    }
}

#[test]
fn the_binary_form_round_trips_and_rejects_truncations() {
    let mut tape = InputTape::new(42, 60);
    tape.build = 0xfeed;
    tape.determinism = Determinism::CrossPlatform;
    tape.ticks = 9;
    let jump = tape.action("jump");
    tape.events = vec![
        TapeEvent {
            tick: 3,
            change: TapeChange::Press(jump),
        },
        TapeEvent {
            tick: 3,
            change: TapeChange::Move(0.25, -1.0),
        },
        TapeEvent {
            tick: 8,
            change: TapeChange::Set(jump, false),
        },
    ];
    let bytes = tape.encode();
    assert!(InputTape::is_binary(&bytes));
    assert!(!InputTape::is_binary(b"seed 1\n"));
    assert_eq!(InputTape::decode(&bytes), Ok(tape.clone()));
    for len in 0..bytes.len() {
        assert!(InputTape::decode(&bytes[..len]).is_err(), "prefix {len}");
    }
    assert_eq!(InputTape::parse(&tape.to_text(), 0, 1), Ok(tape));
}

#[test]
fn a_recorded_run_replays_into_the_same_snapshot_hash() {
    let mut live = game(99);
    live.record();
    live.step(2);
    live.key(Key::Space, true);
    live.key(Key::D, true);
    live.step(3);
    live.key(Key::Space, false);
    live.key(Key::Space, true);
    live.key(Key::Space, false);
    live.step(1);
    live.key(Key::D, false);
    live.step(4);
    let tape = live.stop_recording().expect("recording");
    assert_eq!((tape.seed, tape.ticks, tape.build), (99, 10, live.build()));
    assert_eq!(state(&live, "presses"), Value::Int(2));
    assert_eq!(state(&live, "releases"), Value::Int(1));

    let tape = InputTape::decode(&tape.encode()).expect("a tape");
    let mut replay = game(tape.seed);
    replay.play(tape.clone()).expect("plays");
    // Device input moves nothing while a tape plays.
    replay.key(Key::Space, true);
    replay.step(tape.ticks as u32);
    assert_eq!(replay.snapshot().hash(), live.snapshot().hash());
    for name in ["presses", "releases", "held", "x", "roll"] {
        assert_eq!(state(&replay, name), state(&live, name), "{name}");
    }

    let mut from_text = game(tape.seed);
    let text = InputTape::parse(&tape.to_text(), 0, 1).expect("a tape");
    from_text.play(text).expect("plays");
    from_text.step(10);
    assert_eq!(from_text.snapshot().hash(), live.snapshot().hash());
}

#[test]
fn a_hand_written_tape_drives_the_game() {
    let tape = InputTape::parse(
        "2: press jump\n4: release jump\n6: tap jump\n1..3: axis move = (0.5, 0)",
        5,
        60,
    )
    .expect("a tape");
    let mut game = game(5);
    game.play(tape).expect("plays");
    assert_eq!(game.tape_ticks(), Some(7));
    game.step(8);
    assert_eq!(state(&game, "presses"), Value::Int(2));
    assert_eq!(state(&game, "releases"), Value::Int(2));
    assert_eq!(state(&game, "held"), Value::Int(2));
    assert_eq!(state(&game, "x"), Value::Float(1.0));
}

#[test]
fn a_tape_must_fit_the_build() {
    let mut game = game(1);
    let unknown = InputTape::parse("1: press dash", 1, 60).expect("a tape");
    let error = game.play(unknown).expect_err("an unknown action");
    assert_eq!(
        error.message,
        "the tape's action `dash` is not an action of the build"
    );
    let slow = InputTape::parse("tick_rate 30", 1, 60).expect("a tape");
    let error = game.play(slow).expect_err("another tick rate");
    assert_eq!(
        error.message,
        "the tape steps 30 ticks a second, the build 60"
    );
}

#[test]
fn a_rebuilt_world_replays_its_tape_from_tick_zero() {
    let tape = InputTape::parse("2: press jump\n4: release jump", 3, 60).expect("a tape");
    let mut rebuilt = game(3);
    rebuilt.play(tape.clone()).expect("plays");
    rebuilt.record();
    rebuilt.step(6);
    rebuilt.rebuild_world(Rebuild::Fresh).expect("rebuilds");
    rebuilt.step(6);
    let mut fresh = game(3);
    fresh.play(tape).expect("plays");
    fresh.step(6);
    assert_eq!(rebuilt.snapshot().hash(), fresh.snapshot().hash());
    let recorded = rebuilt.stop_recording().expect("recording");
    assert_eq!(recorded.ticks, 6, "the recording starts over with the run");
    assert_eq!(recorded.events.len(), 2);
}
