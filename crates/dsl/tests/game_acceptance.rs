//! The game acceptance list (Viso_DSL_1.0.md section 155), each item run
//! headless against compiled `.vs` source.

use std::rc::Rc;

use viso_behavior::game::kit::Sfx;
use viso_behavior::game::{InputTape, Key, Rebuild, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Value, Vm};
use viso_dsl::behavior::Program;
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};
use viso_dsl::hotreload::game::{ReloadTier, SwapError, classify, swap};
use viso_dsl::scenario::{self, Scenario};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn profile() -> TargetProfile {
    TargetProfile {
        tick_rate: 60,
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    }
}

fn codes(source: &str) -> Vec<String> {
    compile_file_for(source, &origin(), Natives::standard(), profile())
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

fn program(source: &str) -> Program {
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    compiled.behavior
}

fn game_with(program: &Program, seed: u64) -> Scheduler {
    let module = Rc::new(program.bytecode().expect("verified bytecode"));
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::with_seed(vm, seed).expect("a started game")
}

fn game(source: &str) -> Scheduler {
    game_with(&program(source), 1)
}

/// State `name` of the system named `system`.
fn state(game: &Scheduler, system: &str, name: &str) -> Value {
    let module = game.vm().module();
    let index = module
        .systems()
        .iter()
        .position(|s| module.components()[s.component as usize].name.as_ref() == system)
        .unwrap_or_else(|| panic!("no system {system}"));
    let slot = module
        .layout(module.systems()[index].component)
        .state(name)
        .unwrap_or_else(|| panic!("no state {name}"));
    game.instance(index).states()[slot].clone()
}

/// The tick, the random state, `Tiny`'s states and every body's position:
/// what two builds of one game compare by, their build hashes aside.
fn simulation(game: &Scheduler) -> (u64, u64, Vec<Value>, Vec<[f32; 3]>) {
    let snapshot = game.snapshot();
    let states = ["player", "jumps", "roll"]
        .iter()
        .map(|name| state(game, "Tiny", name))
        .collect();
    let mut world = Vec::new();
    game.world().extract(1.0, &mut world);
    let positions = world.iter().map(|e| e.position.to_array()).collect();
    (snapshot.tick(), snapshot.rng_state(), states, positions)
}

const QUICK: &str = r#"
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{EntityId, SpawnDesc, InputAction, InputAxis};
import viso::math::Vec3F32;

export system Tiny implements QuickGame {
    state player: Option<EntityId> = Option::None;
    state jumps = 0;
    state roll = 0;

    action start(cx: QuickStart) {
        cx.spawn(SpawnDesc::block(Vec3F32::new(40.0f32, 1.0f32, 40.0f32)).at(Vec3F32::new(0.0f32, -0.5f32, 0.0f32)));
        player = Option::Some(cx.spawn(SpawnDesc::player().at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32))));
    }

    action fixed(frame: QuickFrame) {
        roll += frame.world.random_range(0, 100);
        match player {
            Option::Some(id) => {
                frame.world.walk(id, frame.input.axis(InputAxis::move_x) * 6.0, 0.0);
                if frame.input.pressed(InputAction::jump) && frame.world.on_floor(id) {
                    frame.world.jump(id, 5.0);
                    jumps += 1;
                }
            },
            Option::None => {},
        }
    }
}
"#;

const SPLIT: &str = r#"
import viso::game::{Startup, GameStart, FixedUpdate, FixedFrame};
import viso::game::{EntityId, SpawnDesc, InputAction, InputAxis};
import viso::math::Vec3F32;

export system Tiny implements Startup + FixedUpdate {
    state player: Option<EntityId> = Option::None;
    state jumps = 0;
    state roll = 0;

    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::block(Vec3F32::new(40.0f32, 1.0f32, 40.0f32)).at(Vec3F32::new(0.0f32, -0.5f32, 0.0f32)));
        player = Option::Some(cx.spawn(SpawnDesc::player().at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32))));
    }

    action fixed_update(frame: FixedFrame) {
        roll += frame.world.random_range(0, 100);
        match player {
            Option::Some(id) => {
                frame.world.walk(id, frame.input.axis(InputAxis::move_x) * 6.0, 0.0);
                if frame.input.pressed(InputAction::jump) && frame.world.on_floor(id) {
                    frame.world.jump(id, 5.0);
                    jumps += 1;
                }
            },
            Option::None => {},
        }
    }
}
"#;

fn tape() -> InputTape {
    InputTape::parse(
        "5..40: axis move = (1, 0)\n10: tap jump\n100: tap jump\n70..90: axis move = (-1, 0)",
        1,
        60,
    )
    .expect("a tape")
}

#[test]
fn a_quick_game_and_its_split_system_compute_the_same() {
    let mut quick = game(QUICK);
    let mut split = game(SPLIT);
    for game in [&mut quick, &mut split] {
        game.play(tape()).expect("plays");
        game.step(120);
    }
    assert_eq!(simulation(&quick), simulation(&split));
    assert_eq!(state(&quick, "Tiny", "jumps"), Value::Int(2));
    // A snapshot of one restores into the other.
    let mut other = game(SPLIT);
    other.restore(&quick.snapshot());
    assert_eq!(simulation(&other), simulation(&quick));
}

#[test]
fn a_quick_game_needs_no_frame_callback_or_wall_clock() {
    // Stepping ticks directly and running frames of any wall time agree.
    let mut stepped = game(QUICK);
    stepped.play(tape()).expect("plays");
    stepped.step(90);
    let mut framed = game(QUICK);
    framed.play(tape()).expect("plays");
    let mut ran = 0;
    for dt in [0.0, 0.5, 0.004, 1.0 / 60.0, 0.25, 0.1].iter().cycle() {
        if ran >= 90 {
            break;
        }
        framed.clock_mut().set_max_catch_up_steps(90 - ran);
        ran += framed.frame(*dt);
    }
    assert_eq!(ran, 90);
    assert_eq!(simulation(&stepped), simulation(&framed));
}

#[test]
fn a_fixed_seed_and_input_tape_replay() {
    let run = |seed| {
        let mut game = game_with(&program(QUICK), seed);
        game.play(tape()).expect("plays");
        game.step(120);
        game.snapshot().hash()
    };
    assert_eq!(run(9), run(9));
    assert_ne!(run(9), run(10));
    // A recorded live run replays from its tape.
    let mut live = game(QUICK);
    live.record();
    live.step(3);
    live.key(Key::Space, true);
    live.step(2);
    live.key(Key::Space, false);
    live.key(Key::D, true);
    live.step(30);
    let recorded = live.stop_recording().expect("recording");
    let mut replay = game(QUICK);
    replay.play(recorded).expect("plays");
    replay.step(35);
    assert_eq!(replay.snapshot().hash(), live.snapshot().hash());
}

#[test]
fn the_tick_rate_does_not_follow_the_display() {
    // Two seconds of frames at any refresh rate run 120 ticks, give or
    // take the one the frame boundary splits, and every tick computes the
    // same.
    let run = |hz: f64| {
        let mut game = game(QUICK);
        game.play(tape()).expect("plays");
        let mut ticks = 0;
        for _ in 0..(2.0 * hz) as u32 {
            ticks += game.frame(1.0 / hz);
        }
        assert!((119..=121).contains(&ticks), "{hz} Hz ran {ticks} ticks");
        game.step(125 - ticks);
        game.snapshot().hash()
    };
    let at60 = run(60.0);
    for hz in [30.0, 75.0, 144.0, 240.0] {
        assert_eq!(run(hz), at60, "{hz} Hz");
    }
}

#[test]
fn catch_up_steps_are_bounded() {
    let mut game = game(QUICK);
    let cap = game.clock().max_catch_up_steps();
    assert!(cap > 0);
    assert_eq!(game.frame(10.0), cap, "a stall runs at most the cap");
    assert!(game.clock().dropped_time() > 0.0 || game.clock().overrun_ticks() > 0);
    assert!(game.frame(1.0 / 60.0) <= cap);
}

const ORDERED: &str = r#"
import viso::game::{Startup, GameStart, FixedUpdate, FixedFrame, SpawnDesc, EntityId};
import viso::math::Vec3F32;

@after(First)
export system Second implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {
        for id in frame.world.entities() {
            frame.world.teleport(id, Vec3F32::new(2.0f32, 0.0f32, 0.0f32));
            frame.world.walk(id, 0.25, 0.0);
        }
    }
}

export system First implements Startup + FixedUpdate {
    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::player());
    }
    action fixed_update(frame: FixedFrame) {
        for id in frame.world.entities() {
            frame.world.teleport(id, Vec3F32::new(1.0f32, 0.0f32, 0.0f32));
            frame.world.walk(id, 0.5, 0.0);
        }
    }
}
"#;

#[test]
fn system_order_is_stable_and_commands_merge_deterministically() {
    let program = program(ORDERED);
    let names: Vec<_> = program
        .systems
        .iter()
        .map(|s| program.components[s.component as usize].name.clone())
        .collect();
    assert_eq!(
        names,
        ["First", "Second"],
        "`@after` orders, not declaration"
    );
    let run = || {
        let mut game = game_with(&program, 1);
        game.step(1);
        let mut out = Vec::new();
        game.world().extract(1.0, &mut out);
        (out[0].position.to_array(), game.snapshot().hash())
    };
    let (at, hash) = run();
    // The last teleport wins, the walks add up: 2 + (0.5 + 0.25) / 60.
    assert_eq!(at[0], 2.0 + 0.75 / 60.0);
    for _ in 0..4 {
        assert_eq!(run(), (at, hash));
    }
}

const RELOADED: &str = r#"
import viso::game::{FixedUpdate, FixedFrame, Cooldown};

export system Gun implements FixedUpdate {
    state shots = 0;
    state cooldown: Cooldown = Cooldown::new(500ms);
    state left = 0;

    action fixed_update(frame: FixedFrame) {
        if frame.tick() == 2 {
            cooldown = cooldown.fire(frame.tick());
            shots += 1;
        }
        left = cooldown.remaining(frame.tick());
    }
}
"#;

#[test]
fn a_logic_reload_swaps_at_a_tick_boundary_and_keeps_timers() {
    let last_good = program(RELOADED);
    let mut game = game_with(&last_good, 1);
    game.step(10);
    let rng = game.snapshot().rng_state();
    assert_eq!(state(&game, "Gun", "left"), Value::Int(30 + 2 - 9));
    let candidate = program(&RELOADED.replace("shots += 1;", "shots += 10;"));
    let swapped = swap(
        &mut game,
        &last_good,
        &candidate,
        &Natives::standard(),
        &[],
        Rebuild::Fresh,
    )
    .expect("swaps");
    assert_eq!(swapped.reload.tier, ReloadTier::Logic);
    // Between frames, on the tick boundary: the clock, the random state and
    // the cooldown's remaining ticks carry over.
    assert_eq!(game.clock().tick(), 10);
    assert_eq!(game.snapshot().rng_state(), rng);
    game.step(1);
    assert_eq!(state(&game, "Gun", "shots"), Value::Int(1));
    assert_eq!(state(&game, "Gun", "left"), Value::Int(30 + 2 - 10));
}

#[test]
fn a_faulting_or_broken_version_does_not_replace_the_last_good() {
    let last_good = program(QUICK);
    let mut game = game_with(&last_good, 1);
    game.step(20);
    let (build, hash) = (game.build(), game.snapshot().hash());
    // Its start faults: the shadow rebuild is thrown away.
    let faulting = program(&QUICK.replace(
        "cx.spawn(SpawnDesc::block",
        "let _ = cx.world.random_range(3, 3);\n        cx.spawn(SpawnDesc::block",
    ));
    let error = swap(
        &mut game,
        &last_good,
        &faulting,
        &Natives::standard(),
        &[],
        Rebuild::KeepCharacters,
    )
    .expect_err("its start faults");
    assert!(matches!(error, SwapError::Fault(_)), "{error}");
    assert_eq!((game.build(), game.snapshot().hash()), (build, hash));
    // A version that does not compile never reaches the game.
    assert!(!codes(&QUICK.replace("jumps += 1;", "jumps += true;")).is_empty());
    game.step(1);
    assert_eq!(game.clock().tick(), 21);
}

#[test]
fn a_world_rebuild_migrates_entities_by_stable_key() {
    let last_good = program(QUICK);
    let mut game = game_with(&last_good, 1);
    game.play(tape()).expect("plays");
    game.step(40);
    let moved = |game: &Scheduler| {
        let mut out = Vec::new();
        game.world().extract(1.0, &mut out);
        out.iter()
            .find(|e| e.half_extents.y > 0.8)
            .map(|e| e.position.to_array())
            .expect("the player")
    };
    let before = moved(&game);
    assert!(before[0] > 1.0, "the player walked: {before:?}");
    let candidate = program(&QUICK.replace("40.0f32, 1.0f32, 40.0f32", "60.0f32, 1.0f32, 60.0f32"));
    let swapped = swap(
        &mut game,
        &last_good,
        &candidate,
        &Natives::standard(),
        &[],
        Rebuild::KeepCharacters,
    )
    .expect("rebuilds");
    assert_eq!(swapped.reload.tier, ReloadTier::WorldRebuild);
    assert_eq!(swapped.carried, 1);
    assert_eq!(moved(&game), before, "the player keeps its place");
}

#[test]
fn a_headless_simulation_outputs_its_entity_snapshot() {
    let program = program(QUICK);
    let mut scenario = Scenario::empty(1, 60);
    scenario.tape = tape();
    let run = scenario::run(&program, &scenario, 60, |_, _| {}).expect("runs");
    assert_eq!(run.ticks, 60);
    assert_eq!(run.entities.len(), 2);
    assert_eq!(
        run.entities.iter().map(|e| e.kind).collect::<Vec<_>>(),
        ["block", "character"]
    );
    let again = scenario::run(&program, &scenario, 60, |_, _| {}).expect("runs");
    assert_eq!(run.snapshot_hash, again.snapshot_hash);
    assert_eq!(run.entities, again.entities);
}

const EDGES: &str = r#"
import viso::game::{FixedUpdate, FixedFrame, InputAction};

export system Edges implements FixedUpdate {
    state pressed = 0;
    state released = 0;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) { pressed += 1; }
        if frame.input.released(InputAction::jump) { released += 1; }
    }
}
"#;

#[test]
fn every_input_edge_is_seen_once_across_zero_and_many_tick_frames() {
    let mut game = game(EDGES);
    // A press and release within a frame that runs no tick, then a frame
    // that runs several: each edge once.
    game.key(Key::Space, true);
    assert_eq!(game.frame(0.001), 0);
    game.key(Key::Space, false);
    assert_eq!(game.frame(0.001), 0);
    assert!(game.frame(4.0 / 60.0) >= 3);
    assert_eq!(state(&game, "Edges", "pressed"), Value::Int(1));
    assert_eq!(state(&game, "Edges", "released"), Value::Int(1));
    // A press held over many ticks is pressed once.
    game.key(Key::Space, true);
    game.frame(5.0 / 60.0);
    game.frame(5.0 / 60.0);
    assert_eq!(state(&game, "Edges", "pressed"), Value::Int(2));
    assert_eq!(state(&game, "Edges", "released"), Value::Int(1));
}

#[test]
fn the_simulation_rejects_local_state_and_nondeterminism_at_compile_time() {
    let base = r#"
import viso::game::{FixedUpdate, FrameUpdate, FixedFrame, RenderFrame};
import viso::time::Stopwatch;

export system S implements FixedUpdate + FrameUpdate {
    state n = 0.0;
    @local state shown = 0.0;
    action fixed_update(frame: FixedFrame) { BODY }
    action frame_update(frame: RenderFrame) { shown = n; }
}
"#;
    let with = |body: &str| codes(&base.replace("BODY", body));
    assert_eq!(with("n += 1.0;"), Vec::<String>::new());
    assert_eq!(with("n = shown;"), ["E9103"]);
    assert_eq!(
        with("let w = Stopwatch::start(); n = w.elapsed_ms();"),
        ["E9104", "E9104"]
    );
}

#[test]
fn a_restored_snapshot_continues_tick_for_tick() {
    let mut straight = game(QUICK);
    straight.play(tape()).expect("plays");
    let mut resumed = game(QUICK);
    resumed.play(tape()).expect("plays");
    resumed.step(30);
    let saved = resumed.snapshot();
    resumed.step(25);
    resumed.restore(&saved);
    straight.step(30);
    for tick in 30..120 {
        straight.step(1);
        resumed.step(1);
        assert_eq!(
            straight.snapshot().hash(),
            resumed.snapshot().hash(),
            "tick {tick}"
        );
    }
}

const NOISY: &str = r#"
import viso::game::{FixedUpdate, FixedFrame};
import viso::game::kit::Sfx;

export system Noisy implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {
        frame.kit.sound(Sfx::click);
    }
}
"#;

#[test]
fn a_rolled_back_rerun_does_not_redeliver_presentation_commands() {
    let mut game = game(NOISY);
    game.step(10);
    let saved = game.snapshot();
    game.step(10);
    let delivered = game.delivered_commands();
    assert_eq!(delivered, 20);
    game.stage_mut().drain_cues().for_each(drop);
    game.restore(&saved);
    game.step(10);
    assert_eq!(game.delivered_commands(), delivered);
    assert_eq!(game.replayed_commands(), 10);
    assert!(game.stage().cues().is_empty());
    game.step(1);
    let cues: Vec<_> = game.stage().cues().iter().map(|c| c.sound).collect();
    assert_eq!(cues, [viso_behavior::game::kit::Sound::Sfx(Sfx::Click)]);
}

#[test]
fn timers_count_whole_ticks() {
    let mut game = game(RELOADED);
    // 500 ms at 60 Hz is 30 ticks, fired at tick 2: ready at tick 32.
    game.step(3);
    assert_eq!(state(&game, "Gun", "left"), Value::Int(30));
    game.step(29);
    assert_eq!(state(&game, "Gun", "left"), Value::Int(1));
    game.step(1);
    assert_eq!(state(&game, "Gun", "left"), Value::Int(0));
}

#[test]
fn the_reload_tier_follows_the_section_110_table() {
    let base = program(RELOADED);
    let tier = |from: &str, to: &str| {
        assert!(RELOADED.contains(from), "{from}");
        classify(&base, &program(&RELOADED.replace(from, to))).tier
    };
    assert_eq!(tier("shots += 1;", "shots += 2;"), ReloadTier::Logic);
    assert_eq!(
        tier("state shots = 0;", "state shots = 5;"),
        ReloadTier::LogicMigration
    );
    assert_eq!(
        tier(
            "state left = 0;",
            "state left = 0;\n    @local state shown = 0;"
        ),
        ReloadTier::Presentation
    );
    assert_eq!(
        classify(&base, &program(RELOADED)).tier,
        ReloadTier::Unchanged
    );
    let started =
        |at: &str| program(&QUICK.replace("0.9f32, 0.0f32))));", &format!("{at}f32, 0.0f32))));")));
    assert_eq!(
        classify(&started("0.9"), &started("1.9")).tier,
        ReloadTier::WorldRebuild
    );
    // An input map change is a logic reload.
    let mapped = |key: &str| {
        program(&format!(
            "{EDGES}\nimport viso::game::{{InputMap, Key}};\n\
             export const INPUT: InputMap<InputAction> = InputMap::new().key(Key::{key}, InputAction::jump);\n"
        ))
    };
    assert_eq!(
        classify(&mapped("Space"), &mapped("W")).tier,
        ReloadTier::Logic
    );
}

const KITTED: &str = r#"
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{EntityId, SpawnDesc, GameTag, InputAxis};
import viso::game::kit::{Terrain, Prefab};
import viso::math::Vec3F32;

export system Kitted implements QuickGame {
    state car: Option<EntityId> = Option::None;

    action start(cx: QuickStart) {
        cx.kit.terrain(Terrain::hills(48.0, 2.0).seed(3));
        let hero = cx.spawn(SpawnDesc::prefab(Prefab::hero).at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32)).tag(GameTag::player));
        for i in 0..6 {
            let x = 3.0f32 + 2.0f32 * (i as F32);
            let m = cx.spawn(SpawnDesc::prefab(Prefab::monster).at(Vec3F32::new(x, 0.8f32, 2.0f32)));
            if i % 2 == 0 {
                cx.kit.chase(m, GameTag::player, 30.0, 2.5);
            } else {
                cx.kit.wander(m, 4.0, 1.5, 250ms);
            }
        }
        car = Option::Some(cx.spawn(SpawnDesc::prefab(Prefab::car).at(Vec3F32::new(-6.0f32, 0.6f32, 0.0f32))));
    }

    action fixed(frame: QuickFrame) {
        match car {
            Option::Some(id) => frame.kit.drive(id, 1.0, frame.input.axis(InputAxis::move_x)),
            Option::None => {},
        }
    }
}
"#;

/// The snapshot hashes `cross_platform` runs of `QUICK` and `KITTED` on
/// `tape()` reach: every Tier-1 target must reproduce them bit for bit.
const CROSS_PLATFORM_HASHES: [u64; 2] = [0x3889_e487_2429_5d39, 0xaa29_d390_70a9_eb46];

#[test]
fn cross_platform_runs_reach_the_pinned_snapshot_hashes() {
    for (source, pinned) in [QUICK, KITTED].into_iter().zip(CROSS_PLATFORM_HASHES) {
        let mut game = game_with(&program(source), 0x5eed);
        game.play(tape()).expect("plays");
        game.step(240);
        assert!(game.take_faults().is_empty());
        let hash = game.snapshot().hash();
        assert_eq!(hash, pinned, "{hash:#018x}");
    }
}
