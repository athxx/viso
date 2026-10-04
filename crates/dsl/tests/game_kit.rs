//! The Quick Game Kit: terrain, prefabs, behaviors and vehicles lower to
//! ordinary world commands and replay bit for bit; camera, particles, sound
//! and debug draw are Presentation commands landing on the stage; a
//! misspelled Kit method or variant suggests the nearest.

use std::rc::Rc;

use std::sync::Arc;
use viso_behavior::game::kit::{Model, Sfx, Sound, Synth};
use viso_behavior::game::{BodyKind, Extracted, Scheduler};

use viso_behavior::native::{NativeLibrary, NativeObject, NativeType, Natives, Obj, STANDARD};
use viso_behavior::{Budget, Module, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn compiled(source: &str, profile: TargetProfile) -> viso_dsl::frontend::Compiled {
    let profile = TargetProfile {
        tick_rate: 30,
        determinism: Determinism::CrossPlatform,
        ..profile
    };
    compile_file_for(source, &origin(), Natives::standard(), profile)
}

fn module_for(source: &str, profile: TargetProfile) -> Rc<Module> {
    let compiled = compiled(source, profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn game_for(source: &str, seed: u64) -> Scheduler {
    let mut vm = Vm::new(
        module_for(source, TargetProfile::default()),
        Budget::default(),
    );
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::with_seed(vm, seed).expect("a started game")
}

fn entities(game: &Scheduler) -> Vec<Extracted> {
    let mut out = Vec::new();
    game.world().extract(1.0, &mut out);
    out
}

fn by_model(game: &Scheduler, model: Model) -> Extracted {
    entities(game)
        .into_iter()
        .find(|e| e.model == model)
        .unwrap_or_else(|| panic!("no {model:?}"))
}

const IMPORTS: &str = "\
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{EntityId, SpawnDesc, GameTag, InputAxis};
import viso::game::kit::{CameraRig, Terrain, Prefab, Particle, Sfx, Wave};
import viso::math::Vec3F32;
";

const ARENA: &str = r#"
export system Arena implements QuickGame {
    state hero: Option<EntityId> = Option::None;
    state ticks = 0;

    action start(cx: QuickStart) {
        cx.kit.terrain(Terrain::hills(64.0, 3.0).seed(5));
        let h = cx.spawn(SpawnDesc::prefab(Prefab::hero).at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32)).tag(GameTag::player));
        hero = Option::Some(h);
        let m = cx.spawn(SpawnDesc::prefab(Prefab::monster).at(Vec3F32::new(6.0f32, 0.8f32, 0.0f32)));
        cx.kit.chase(m, GameTag::player, 20.0, 3.0);
        let v = cx.spawn(SpawnDesc::prefab(Prefab::villager).at(Vec3F32::new(-4.0f32, 0.85f32, 4.0f32)));
        cx.kit.wander(v, 3.0, 1.5, 500ms);
        let g = cx.spawn(SpawnDesc::prefab(Prefab::hero).at(Vec3F32::new(0.0f32, 0.9f32, -6.0f32)).model(Model::robot));
        cx.kit.patrol(g, [Vec3F32::new(3.0f32, 0.0f32, -6.0f32), Vec3F32::new(-3.0f32, 0.0f32, -6.0f32)], 2.0);
        cx.kit.camera(CameraRig::third_person(h).distance(8.0));
        cx.kit.sound(Sfx::powerup);
    }

    action fixed(frame: QuickFrame) {
        ticks += 1;
        if ticks == 10 {
            frame.kit.burst(Particle::confetti, Vec3F32::new(0.0f32, 1.0f32, 0.0f32), 40);
            frame.kit.sound_at(Sfx::coin, Vec3F32::new(4.0f32, 1.0f32, 0.0f32));
            frame.kit.shake(0.3, 0.5);
        }
        frame.kit.debug_line(Vec3F32::new(0.0f32, 0.0f32, 0.0f32), Vec3F32::new(0.0f32, 2.0f32, 0.0f32));
    }
}
"#;

fn arena() -> String {
    format!("{IMPORTS}import viso::game::kit::Model;\n{ARENA}")
}

#[test]
fn the_kit_builds_terrain_spawns_prefabs_and_steers_them() {
    let mut game = game_for(&arena(), 7);
    let all = entities(&game);
    let ground = all.iter().filter(|e| e.model == Model::Ground).count();
    assert!(ground > 1, "hills are more than the ground block");
    assert!(all.iter().all(|e| e.model != Model::Auto));
    let hero = by_model(&game, Model::Hero);
    assert_eq!(hero.kind, BodyKind::Character);
    let monster = by_model(&game, Model::Monster);
    let start = monster.position.x;
    for _ in 0..30 {
        game.frame(1.0 / 30.0);
    }
    assert!(game.take_faults().is_empty());
    let monster = by_model(&game, Model::Monster);
    assert!(
        (start - monster.position.x - 3.0).abs() < 0.05,
        "it chased the hero 3 m in a second: {} -> {}",
        start,
        monster.position.x
    );
    assert!(
        monster.facing[0] < -0.99,
        "it faces the hero: {:?}",
        monster.facing
    );
    let robot = by_model(&game, Model::Robot);
    assert!(
        (robot.position.x - 2.0).abs() < 0.05,
        "{}",
        robot.position.x
    );
    let villager = by_model(&game, Model::Villager);
    let moved = (villager.position.x + 4.0).abs() + (villager.position.z - 4.0).abs();
    assert!(moved > 0.1, "it wandered");
}

#[test]
fn kit_steering_replays_into_the_same_snapshot() {
    let run = |seed| {
        let mut game = game_for(&arena(), seed);
        game.step(90);
        (game.snapshot().hash(), entities(&game))
    };
    let (a, entities_a) = run(11);
    let (b, entities_b) = run(11);
    assert_eq!(a, b);
    assert_eq!(entities_a, entities_b);
    let (c, _) = run(12);
    assert_ne!(a, c, "the wanderer draws from the seed");

    let mut game = game_for(&arena(), 11);
    game.step(40);
    let saved = game.snapshot();
    game.step(50);
    let end = game.snapshot().hash();
    game.restore(&saved);
    game.step(50);
    assert_eq!(game.snapshot().hash(), end, "a restored kit game resumes");
    assert_eq!(game.snapshot().hash(), a);
}

#[test]
fn presentation_commands_land_on_the_stage_once() {
    let mut game = game_for(&arena(), 7);
    let cues: Vec<_> = game.stage().cues().iter().map(|c| c.sound).collect();
    assert_eq!(cues, [Sound::Sfx(Sfx::Powerup)], "the start's sound");
    game.stage_mut().drain_cues().for_each(drop);
    for _ in 0..12 {
        game.frame(1.0 / 30.0);
    }
    let stage = game.stage();
    assert_eq!(stage.cues().len(), 1);
    let coin = stage.cues()[0];
    assert_eq!(coin.sound, Sound::Sfx(Sfx::Coin));
    assert!(
        coin.pan > 0.0,
        "the coin sounds right of the camera: {}",
        coin.pan
    );
    assert!(stage.particles() > 0 && stage.particles() <= 40);
    assert_eq!(stage.debug_shapes().count(), 1, "the last tick's line");
    let view = stage.view();
    let hero = by_model(&game, Model::Hero).position;
    assert!(view.eye.z > hero.z + 5.0, "behind the hero: {view:?}");
    drop(stage);

    // A replayed tick issues nothing twice.
    let saved = game.snapshot();
    game.stage_mut().drain_cues().for_each(drop);
    game.step(3);
    game.restore(&saved);
    game.step(3);
    assert!(game.stage().cues().is_empty());
    assert!(game.replayed_commands() > 0);

    let mut synth = Synth::new(48000.0);
    let mut out = vec![0.0f32; 4800 * 2];
    synth.play(&coin);
    synth.render(&mut out, 2);
    assert!(out.iter().any(|s| s.abs() > 0.01), "the cue renders");
}

#[test]
fn a_release_build_removes_kit_debug_draw() {
    let release = TargetProfile {
        release: true,
        ..TargetProfile::default()
    };
    let mut vm = Vm::new(module_for(&arena(), release), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let mut game = Scheduler::new(vm).expect("a started game");
    game.frame(1.0 / 30.0);
    assert_eq!(game.stage().debug_shapes().count(), 0);
}

const CAR: &str = r#"
export system Driver implements QuickGame {
    state car: Option<EntityId> = Option::None;
    state ticks = 0;

    action start(cx: QuickStart) {
        cx.kit.terrain(Terrain::flat(200.0));
        car = Option::Some(cx.spawn(SpawnDesc::prefab(Prefab::car).at(Vec3F32::new(0.0f32, 0.6f32, 0.0f32))));
    }

    action fixed(frame: QuickFrame) {
        ticks += 1;
        match car {
            Option::Some(id) => {
                let steer = if ticks > 30 { 1.0 } else { 0.0 };
                frame.kit.drive(id, 1.0, steer);
            },
            Option::None => {},
        }
    }
}
"#;

#[test]
fn a_driven_vehicle_accelerates_along_its_facing_and_turns() {
    let mut game = game_for(&format!("{IMPORTS}{CAR}"), 1);
    game.step(30);
    let car = by_model(&game, Model::Car);
    assert!(car.position.z < -5.0, "forward is -z: {:?}", car.position);
    assert!(car.position.x.abs() < 1e-4);
    assert_eq!(car.facing, [0.0, -1.0]);
    game.step(30);
    let car = by_model(&game, Model::Car);
    assert!(car.facing[0] > 0.5, "it turned right: {:?}", car.facing);
    assert!(car.position.x > 0.5);
}

fn errors(source: &str) -> Vec<(String, Vec<String>)> {
    compiled(&format!("{IMPORTS}{source}"), TargetProfile::default())
        .errors()
        .map(|d| {
            let fixes = d.fixes.iter().map(|f| f.title.clone()).collect();
            (d.code.to_string(), fixes)
        })
        .collect()
}

fn quick(body: &str) -> String {
    format!(
        "export system G implements QuickGame {{
    action start(cx: QuickStart) {{ {body} }}
    action fixed(frame: QuickFrame) {{}}
}}"
    )
}

#[test]
fn a_misspelled_kit_method_or_variant_suggests_the_nearest() {
    assert_eq!(
        errors(&quick("cx.kit.sond(Sfx::coin);")),
        [("E2001".to_owned(), vec!["replace with `sound`".to_owned()])]
    );
    assert_eq!(
        errors(&quick("cx.kit.sound(Sfx::jmp);")),
        [("E2001".to_owned(), vec!["replace with `jump`".to_owned()])]
    );
    // Only methods returning what the call must produce are suggested.
    assert_eq!(
        errors(&quick(
            "let at: Vec3F32 = cx.kit.facin(cx.spawn(SpawnDesc::player()));"
        )),
        [("E2001".to_owned(), vec!["replace with `facing`".to_owned()])]
    );
}

#[test]
fn the_kit_tags_its_methods_by_layer() {
    // Simulation methods are reproducible on every target; a Presentation
    // method returns nothing and may be called from the Simulation too.
    assert!(
        errors(&quick(
            "let e = cx.spawn(SpawnDesc::player()); cx.kit.wander(e, 2.0, 1.0, 1s); \
         cx.kit.beep(Wave::saw, 220.0, 440.0, 0.2); cx.kit.emit(e, Particle::smoke);"
        ))
        .is_empty()
    );
}

/// A handle with two methods one edit from `spexd`: `speed` returns an
/// `F64`, `spend` nothing.
#[derive(Debug)]
struct Gauge;

impl NativeObject for Gauge {
    const PATH: &'static str = "app::probe::Gauge";
}

static GAUGE: NativeLibrary = NativeLibrary {
    path: "app::probe",
    version: 1,
    functions: &[
        viso_behavior::native!(action "gauge" |_cx| -> Obj<Gauge> { Ok(Obj::new(Gauge)) })
            .reproducible(Determinism::CrossPlatform),
    ],
    types: &[NativeType::new(
        "Gauge",
        &[
            viso_behavior::native!(fn "speed" |_cx, this: Obj<Gauge>| -> f64 { Ok(1.0) }),
            viso_behavior::native!(action "spend" |_cx, this: Obj<Gauge>| -> () { Ok(()) }),
        ],
    )],
    traits: &[],
    derives: &[],
    widgets: &[],
};

#[test]
fn a_misspelled_method_suggests_those_returning_the_expected_type() {
    let mut natives = Natives::new();
    natives.extend(STANDARD).expect("standard");
    natives.register(&GAUGE).expect("gauge");
    let natives = Arc::new(natives);
    let fixes = |body: &str| -> Vec<String> {
        let source = format!(
            "import app::probe;\n{}",
            quick(&format!("let g = probe::gauge(); {body}"))
        );
        let compiled = compile_file_for(
            &format!("{IMPORTS}{source}"),
            &origin(),
            natives.clone(),
            TargetProfile::default(),
        );
        let errors: Vec<_> = compiled.errors().collect();
        assert_eq!(errors.len(), 1, "{errors:#?}");
        assert_eq!(errors[0].code, "E2001");
        errors[0].fixes.iter().map(|f| f.title.clone()).collect()
    };
    assert_eq!(fixes("let s: F64 = g.spexd();"), ["replace with `speed`"]);
    assert_eq!(
        fixes("g.spexd();"),
        ["replace with `speed`", "replace with `spend`"]
    );
}
