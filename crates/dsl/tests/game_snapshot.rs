//! Tick timers converted to ticks at compile time, game snapshots that
//! restore tick for tick, and the interpolation weight of a frame.

use std::rc::Rc;

use viso_behavior::game::{GameSnapshot, Restored, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::TargetProfile;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

const IMPORTS: &str = "import viso::game::{FixedUpdate, FrameUpdate, FixedFrame, RenderFrame, Cooldown, TickTimer};\n";

/// A 4 Hz fixed step: a tick is 250ms.
fn profile() -> TargetProfile {
    TargetProfile {
        tick_rate: 4,
        ..TargetProfile::default()
    }
}

fn compile(source: &str) -> viso_dsl::frontend::Compiled {
    compile_file_for(
        &format!("{IMPORTS}{source}"),
        &origin(),
        Natives::standard(),
        profile(),
    )
}

fn codes(source: &str) -> Vec<String> {
    compile(source)
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

fn module(source: &str) -> Rc<Module> {
    let compiled = compile(source);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn scheduler(module: Rc<Module>) -> Scheduler {
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::new(vm).expect("systems instantiate")
}

fn state(game: &Scheduler, name: &str) -> Value {
    let module = game.vm().module();
    let layout = module.layout(module.systems()[0].component);
    game.instance(0).states()[layout.state(name).expect("state")].clone()
}

const GUN: &str = r#"
system Gun implements FixedUpdate {
    state cooldown: Cooldown = Cooldown::new(300ms);
    state wave: TickTimer = TickTimer::every(1s + 250ms);
    state shots = 0;
    state waves = 0;

    action fixed_update(frame: FixedFrame) {
        if cooldown.ready(frame.tick()) {
            cooldown = cooldown.fire(frame.tick());
            shots += 1;
        }
        if wave.due(frame.tick()) {
            wave = wave.rearm(frame.tick());
            waves += 1;
        }
    }
}
"#;

#[test]
fn timers_count_whole_ticks_of_the_compiled_step() {
    let module = module(GUN);
    assert_eq!(module.tick_rate(), 4);
    let mut game = scheduler(module);
    // 300ms is 1.2 ticks, rounded up to 2; 1.25s is 5 ticks.
    game.step(10);
    assert_eq!(state(&game, "shots"), Value::Int(5));
    assert_eq!(state(&game, "waves"), Value::Int(1));
    game.step(1);
    assert_eq!(state(&game, "waves"), Value::Int(2));
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
}

#[test]
fn a_tick_duration_is_a_non_negative_constant() {
    let dynamic = GUN.replace(
        "Cooldown::new(300ms)",
        "Cooldown::new(if true { 1s } else { 2s })",
    );
    assert_eq!(codes(&dynamic), ["E2501"]);
    let negative = GUN.replace("Cooldown::new(300ms)", "Cooldown::new(-1s)");
    assert_eq!(codes(&negative), ["E2112"]);
    let scaled = GUN.replace("Cooldown::new(300ms)", "Cooldown::new((250ms - 50ms) * 2)");
    assert_eq!(codes(&scaled), Vec::<String>::new());
}

const WALKER: &str = r#"
record Body {
    x: I64;
    speed: F64;
}

system Walker implements FixedUpdate + FrameUpdate {
    state body: Body = Body { x: 0, speed: 0.5 };
    state cooldown: Cooldown = Cooldown::new(500ms);
    state jumps = 0;
    @local state shown = 0;
    @local state alpha: F32 = 0.0f32;
    computed doubled: I64 = body.x * 2;

    action fixed_update(frame: FixedFrame) {
        body = Body { x: body.x + frame.tick(), speed: body.speed * 1.5 };
        if cooldown.ready(frame.tick()) {
            cooldown = cooldown.fire(frame.tick());
            jumps += 1;
        }
    }

    action frame_update(frame: RenderFrame) {
        shown = doubled;
        alpha = frame.alpha();
    }
}
"#;

/// The Simulation states after each of `ticks` more ticks.
fn trajectory(game: &mut Scheduler, ticks: u32) -> Vec<Vec<Value>> {
    (0..ticks)
        .map(|_| {
            game.step(1);
            ["body", "cooldown", "jumps"]
                .iter()
                .map(|name| state(game, name))
                .collect()
        })
        .collect()
}

#[test]
fn restoring_a_snapshot_resumes_tick_for_tick() {
    let module = module(WALKER);
    let mut game = scheduler(module.clone());
    game.step(3);
    let snapshot = game.snapshot();
    assert_eq!(snapshot.tick(), 3);
    // `@local` states are not captured.
    assert_eq!(snapshot.states(), 3);
    let bytes = snapshot.encode();
    let uninterrupted = trajectory(&mut game, 5);
    let hash = game.snapshot().hash();

    // A fresh session loads the blob and runs the same ticks.
    let mut loaded = scheduler(module);
    let decoded = GameSnapshot::decode(&bytes).expect("a snapshot blob");
    assert_eq!(decoded, snapshot);
    assert_eq!(
        loaded.restore(&decoded),
        Restored {
            states: 3,
            ..Restored::default()
        }
    );
    assert_eq!(loaded.clock().tick(), 3);
    assert_eq!(trajectory(&mut loaded, 5), uninterrupted);
    assert_eq!(loaded.snapshot().hash(), hash);

    // Rolling the first session back in memory does the same.
    game.restore(&snapshot);
    assert_eq!(trajectory(&mut game, 5), uninterrupted);
    assert_eq!(game.snapshot(), loaded.snapshot());
}

#[test]
fn restore_recomputes_derived_state_and_keeps_local_state() {
    let mut game = scheduler(module(WALKER));
    game.step(2);
    let early = game.snapshot();
    game.step(4);
    game.frame(0.0);
    let shown = state(&game, "shown");
    assert_eq!(shown, Value::Int(30));

    game.restore(&early);
    assert_eq!(
        state(&game, "shown"),
        shown,
        "a local state keeps its value"
    );
    game.frame(0.0);
    assert_eq!(state(&game, "shown"), Value::Int(2), "`doubled` recomputed");
}

#[test]
fn a_snapshot_restores_what_another_build_shares() {
    let snapshot = {
        let mut game = scheduler(module(WALKER));
        game.step(4);
        game.snapshot()
    };
    let edited = WALKER
        .replace("state jumps = 0;", "state jumps = 0.0;")
        .replace("jumps += 1;", "jumps += 1.0;")
        .replace("state body:", "state extra = 7;\n    state body:");
    let mut game = scheduler(module(&edited));
    assert_eq!(
        game.restore(&snapshot),
        Restored {
            states: 2,
            mismatched: 1,
            missing: 1,
            locals: 0,
            degraded: false,
        }
    );
    assert_eq!(state(&game, "extra"), Value::Int(7));
    assert_eq!(state(&game, "jumps"), Value::Float(0.0));
}

#[test]
fn a_corrupt_snapshot_is_an_error() {
    let mut game = scheduler(module(WALKER));
    game.step(1);
    let bytes = game.snapshot().encode();
    for len in 0..bytes.len() {
        assert!(GameSnapshot::decode(&bytes[..len]).is_err(), "{len}");
    }
    assert!(GameSnapshot::decode(&module(WALKER).encode()).is_err());
}

#[test]
fn a_frame_interpolates_by_its_share_of_the_next_tick() {
    let mut game = scheduler(module(WALKER));
    assert_eq!(game.frame(0.375), 1);
    assert_eq!(state(&game, "alpha"), Value::Float(0.5));
    assert_eq!(game.frame(0.125), 1);
    assert_eq!(state(&game, "alpha"), Value::Float(0.0));
}
