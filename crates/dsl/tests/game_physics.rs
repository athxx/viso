//! The physics integration contract (§106, §106.5, §106.7): the
//! `PrePhysics` and `PostPhysics` phases around the step, the order contacts
//! reach the listeners in, an engine a host installs held to the game's
//! determinism tier (`E9104`), degraded snapshots of an engine that cannot
//! save, and a multi-system game over all of it.

use std::rc::Rc;

use viso_behavior::game::kit::{Sfx, Sound};
use viso_behavior::game::{
    BodyKind, GameError, GameSnapshot, InputTape, Kinematic, Physics, PhysicsError, Scheduler,
    StepBodies,
};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{CollisionDelivery, Determinism, TargetProfile};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn module_for(source: &str, profile: TargetProfile) -> Rc<Module> {
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn module(source: &str) -> Rc<Module> {
    module_for(source, TargetProfile::default())
}

fn vm(module: Rc<Module>) -> Vm {
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    vm
}

fn state(game: &Scheduler, system: &str, name: &str) -> Value {
    let module = game.vm().module();
    let component = module.component(system).expect("system");
    let index = module
        .systems()
        .iter()
        .position(|s| s.component == component)
        .expect("a system");
    let slot = module.layout(component).state(name).expect("state");
    game.instance(index).states()[slot].clone()
}

fn int(value: Value) -> i64 {
    match value {
        Value::Int(n) => n,
        other => panic!("not an integer: {other:?}"),
    }
}

fn float(value: Value) -> f64 {
    match value {
        Value::Float(x) => x,
        other => panic!("not a float: {other:?}"),
    }
}

const PHASES: &str = r#"
import viso::game::{Startup, GameStart, FixedUpdate, PrePhysics, PostPhysics, CollisionListener};
import viso::game::{FixedFrame, CollisionEvent, EntityId, SpawnDesc};
import viso::math::Vec3F32;

export system Order implements Startup + FixedUpdate + PrePhysics + PostPhysics + CollisionListener {
    state log = 0;
    state hero: Option<EntityId> = Option::None;
    state pre_y = 0.0;
    state post_y = 0.0;

    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::sensor(Vec3F32::new(2.0f32, 2.0f32, 2.0f32)).at(Vec3F32::new(0.0f32, 5.0f32, 0.0f32)));
        hero = Option::Some(cx.spawn(SpawnDesc::character(Vec3F32::new(1.0f32, 1.0f32, 1.0f32))));
    }

    action fixed_update(frame: FixedFrame) {
        log = log * 10 + 1;
        match hero {
            Option::Some(id) => frame.world.teleport(id, Vec3F32::new(0.0f32, 5.0f32, 0.0f32)),
            Option::None => {},
        }
    }

    action pre_physics(frame: FixedFrame) {
        log = log * 10 + 2;
        match hero {
            Option::Some(id) => { pre_y = frame.world.position(id).y as F64; },
            Option::None => {},
        }
    }

    action collision(event: CollisionEvent) {
        log = log * 10 + 3;
    }

    action post_physics(frame: FixedFrame) {
        log = log * 10 + 4;
        match hero {
            Option::Some(id) => { post_y = frame.world.position(id).y as F64; },
            Option::None => {},
        }
    }
}
"#;

#[test]
fn pre_and_post_physics_run_around_the_step() {
    let mut game = Scheduler::new(vm(module(PHASES))).expect("a started game");
    game.step(1);
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
    assert_eq!(
        int(state(&game, "Order", "log")),
        1234,
        "fixed, pre, the contact that began, post"
    );
    assert_eq!(
        float(state(&game, "Order", "pre_y")),
        5.0,
        "PrePhysics sees what FixedUpdate committed"
    );
    let post = float(state(&game, "Order", "post_y"));
    assert!(post < 5.0, "PostPhysics sees the stepped world: {post}");
    game.step(1);
    assert_eq!(
        int(state(&game, "Order", "log")),
        1_234_124,
        "a contact begins once"
    );
}

const LISTENERS: &str = r#"
import viso::game::{CollisionListener, CollisionEvent};

export system A implements CollisionListener {
    state seen = 0;
    action collision(event: CollisionEvent) {
        seen = seen * 100 + event.world.random_range(0, 100);
    }
}

export system B implements CollisionListener {
    state seen = 0;
    action collision(event: CollisionEvent) {
        seen = seen * 100 + event.world.random_range(0, 100);
    }
}
"#;

/// The two draws of each listener, oldest first, under `delivery`.
fn draws(delivery: CollisionDelivery) -> [(i64, i64); 2] {
    let profile = TargetProfile {
        collision_delivery: delivery,
        ..TargetProfile::default()
    };
    let module = module_for(LISTENERS, profile);
    assert_eq!(module.collision_delivery(), delivery);
    let mut game = Scheduler::with_seed(vm(module), 7).expect("a started game");
    let id = viso_behavior::game::EntityId::new;
    game.push_collision(id(1, 0), id(2, 0));
    game.push_collision(id(3, 0), id(4, 0));
    game.step(1);
    let split = |n: i64| (n / 100, n % 100);
    [
        split(int(state(&game, "A", "seen"))),
        split(int(state(&game, "B", "seen"))),
    ]
}

#[test]
fn contacts_reach_the_listeners_in_the_profiles_order() {
    // Draws r1..r4 in delivery order: event-major hands A r1 and r3 and B
    // r2 and r4; listener-major hands A r1 and r2 and B r3 and r4.
    let [(e_a1, e_a2), (e_b1, e_b2)] = draws(CollisionDelivery::EventMajor);
    let [(l_a1, l_a2), (l_b1, l_b2)] = draws(CollisionDelivery::ListenerMajor);
    assert_eq!((e_a1, e_b1, e_a2, e_b2), (l_a1, l_a2, l_b1, l_b2));
    assert_ne!((e_a2, e_b1), (l_a2, l_b1), "the orders differ");
}

/// A test engine: each step moves every character one unit along x per
/// step it has taken, so what it keeps between steps shapes the motion.
#[derive(Debug)]
struct Drift {
    steps: u32,
    saves: bool,
    tier: Determinism,
}

impl Drift {
    fn boxed(saves: bool, tier: Determinism) -> Box<dyn Physics> {
        Box::new(Drift {
            steps: 0,
            saves,
            tier,
        })
    }
}

impl Physics for Drift {
    fn name(&self) -> &str {
        "test::Drift"
    }

    fn determinism(&self) -> Determinism {
        self.tier
    }

    fn step(&mut self, bodies: &mut StepBodies<'_>, _dt: f32, _contacts: &mut Vec<(u32, u32)>) {
        self.steps += 1;
        for &slot in bodies.order() {
            let slot = slot as usize;
            if bodies.kind(slot) == BodyKind::Character {
                let [x, y, z] = bodies.position(slot);
                bodies.set_position(slot, [x + self.steps as f32, y, z]);
            }
        }
    }

    fn save(&self) -> Option<Vec<u8>> {
        self.saves.then(|| self.steps.to_le_bytes().to_vec())
    }

    fn load(&mut self, state: &[u8]) -> Result<(), PhysicsError> {
        let bytes = state
            .try_into()
            .map_err(|_| PhysicsError::new("a step count is four bytes"))?;
        self.steps = u32::from_le_bytes(bytes);
        Ok(())
    }

    fn fork(&self) -> Box<dyn Physics> {
        Drift::boxed(self.saves, self.tier)
    }
}

const DRIFTING: &str = r#"
import viso::game::{Startup, GameStart, SpawnDesc};
import viso::math::Vec3F32;

export system Spawner implements Startup {
    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::character(Vec3F32::new(1.0f32, 1.0f32, 1.0f32)));
    }
}
"#;

fn cross_platform() -> TargetProfile {
    TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    }
}

fn x_of(game: &Scheduler) -> f32 {
    let mut out = Vec::new();
    game.world().extract(1.0, &mut out);
    out[0].position.to_array()[0]
}

#[test]
fn an_engine_below_the_games_tier_is_e9104() {
    let strict = module_for(DRIFTING, cross_platform());
    let Err(GameError::Physics(tier)) = Scheduler::with_physics(
        vm(strict.clone()),
        1,
        Drift::boxed(true, Determinism::SameBinary),
    ) else {
        panic!("a same_binary engine in a cross_platform game")
    };
    assert_eq!(tier.code(), "E9104");
    assert!(tier.to_string().contains("test::Drift"), "{tier}");

    let mut game = Scheduler::with_physics(
        vm(module(DRIFTING)),
        1,
        Drift::boxed(true, Determinism::SameBinary),
    )
    .expect("a same_binary game takes it");
    assert_eq!(
        game.world().physics(),
        ("test::Drift".to_owned(), Determinism::SameBinary)
    );
    game.step(2);
    assert_eq!(x_of(&game), 3.0, "the engine stepped the world");
    let Err(error) = game.reload(vm(strict.clone())) else {
        panic!("a reload into a cross_platform build")
    };
    assert_eq!(error.code(), "E9104");
    let Err(error) = game.rebuild(vm(strict), Default::default()) else {
        panic!("a rebuild into a cross_platform build")
    };
    assert_eq!(error.code(), "E9104");
    game.step(1);
    assert_eq!(x_of(&game), 6.0, "the game ran on as it was");

    // The built-in engine reaches every tier and is what `with_seed` installs.
    let mut built_in =
        Scheduler::with_physics(vm(module(PHASES)), 3, Box::new(Kinematic::default()))
            .expect("the built-in engine");
    let mut default = Scheduler::with_seed(vm(module(PHASES)), 3).expect("a started game");
    built_in.step(5);
    default.step(5);
    assert_eq!(built_in.snapshot().hash(), default.snapshot().hash());
}

#[test]
fn an_engines_state_snapshots_and_restores() {
    let start = || {
        Scheduler::with_physics(
            vm(module(DRIFTING)),
            1,
            Drift::boxed(true, Determinism::SameBinary),
        )
        .expect("a started game")
    };
    let mut straight = start();
    straight.step(5);
    let mut resumed = start();
    resumed.step(3);
    let saved = resumed.snapshot();
    assert!(!saved.degraded());
    let saved = GameSnapshot::decode(&saved.encode()).expect("decodes");
    resumed.step(4);
    let restored = resumed.restore(&saved);
    assert!(!restored.degraded);
    resumed.step(2);
    assert_eq!(x_of(&resumed), x_of(&straight));
    assert_eq!(resumed.snapshot().hash(), straight.snapshot().hash());
}

#[test]
fn an_engine_that_cannot_save_makes_snapshots_degraded() {
    let start = || {
        Scheduler::with_physics(
            vm(module(DRIFTING)),
            1,
            Drift::boxed(false, Determinism::SameBinary),
        )
        .expect("a started game")
    };
    let mut straight = start();
    straight.step(5);
    let mut resumed = start();
    resumed.step(3);
    let saved = resumed.snapshot();
    assert!(saved.degraded());
    let saved = GameSnapshot::decode(&saved.encode()).expect("decodes");
    assert!(saved.degraded(), "the blob keeps it degraded");
    resumed.step(4);
    let restored = resumed.restore(&saved);
    assert!(restored.degraded, "reported");
    resumed.step(2);
    // The engine started afresh at the restored bodies: 6 + 1 + 2.
    assert_eq!(x_of(&straight), 15.0);
    assert_eq!(x_of(&resumed), 9.0);
}

/// A game of six systems over one world: input moves the hero, a brain
/// steers the monsters (the Kit's `chase` and `wander`), the built-in
/// physics steps them, combat scores the contacts and plays their sounds,
/// and a referee after the step drops what fell and ends the round.
const ARENA: &str = r#"
import viso::game::{Startup, GameStart, FixedUpdate, PrePhysics, PostPhysics, CollisionListener};
import viso::game::{FixedFrame, CollisionEvent, EntityId, SpawnDesc, GameTag, InputAction, InputAxis};
import viso::game::kit::{Prefab, Sfx};
import viso::math::Vec3F32;

export system Level implements Startup {
    state hero: Option<EntityId> = Option::None;

    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::block(Vec3F32::new(60.0f32, 1.0f32, 60.0f32)).at(Vec3F32::new(0.0f32, -0.5f32, 0.0f32)));
        hero = Option::Some(cx.spawn(SpawnDesc::prefab(Prefab::hero).at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32)).tag(GameTag::player)));
        for i in 0..8 {
            let x = 2.0f32 + 1.5f32 * (i as F32);
            cx.spawn(SpawnDesc::prefab(Prefab::coin).at(Vec3F32::new(x, 0.6f32, 0.0f32)).tag(GameTag::coin));
        }
        for i in 0..4 {
            let x = 6.0f32 + 3.0f32 * (i as F32);
            let m = cx.spawn(SpawnDesc::prefab(Prefab::monster).at(Vec3F32::new(x, 0.8f32, 3.0f32)).tag(GameTag::enemy));
            if i % 2 == 0 {
                cx.kit.chase(m, GameTag::player, 20.0, 3.0);
            } else {
                cx.kit.wander(m, 3.0, 1.0, 400ms);
            }
        }
    }
}

@after(Level)
export system Controls implements FixedUpdate {
    state jumps = 0;

    action fixed_update(frame: FixedFrame) {
        for id in frame.world.query(GameTag::player) {
            frame.world.walk(id, frame.input.axis(InputAxis::move_x) * 5.0, frame.input.axis(InputAxis::move_z) * 5.0);
            if frame.input.pressed(InputAction::jump) && frame.world.on_floor(id) {
                frame.world.jump(id, 5.0);
                jumps += 1;
            }
        }
    }
}

export system Brain implements PrePhysics {
    state alarms = 0;

    action pre_physics(frame: FixedFrame) {
        for hero in frame.world.query(GameTag::player) {
            let at = frame.world.position(hero);
            for m in frame.world.query(GameTag::enemy) {
                let p = frame.world.position(m);
                let (dx, dz) = (p.x - at.x, p.z - at.z);
                if dx * dx + dz * dz < 4.0f32 {
                    alarms += 1;
                }
            }
        }
    }
}

export system Combat implements CollisionListener {
    state score = 0;
    state hits = 0;

    action collision(event: CollisionEvent) {
        for hero in event.world.query(GameTag::player) {
            match event.other_of(hero) {
                Option::Some(other) => {
                    if event.world.has_tag(other, GameTag::coin) {
                        score += 1;
                        event.world.remove(other);
                        event.kit.sound(Sfx::coin);
                    } else if event.world.has_tag(other, GameTag::enemy) {
                        hits += 1;
                        event.kit.sound(Sfx::hurt);
                    }
                },
                Option::None => {},
            }
        }
    }
}

export system Referee implements PostPhysics {
    state over = false;
    state ticks = 0;

    action post_physics(frame: FixedFrame) {
        ticks += 1;
        for id in frame.world.entities() {
            if frame.world.position(id).y < -10.0f32 {
                frame.world.remove(id);
            }
        }
        let mut left = 0;
        for coin in frame.world.query(GameTag::coin) {
            left += 1;
        }
        over = left == 0;
    }
}
"#;

fn arena_tape() -> InputTape {
    InputTape::parse(
        "5..200: axis move = (1, 0)\n30: tap jump\n120: tap jump\n200..260: axis move = (-1, 1)",
        9,
        60,
    )
    .expect("a tape")
}

fn arena() -> Scheduler {
    let module = module_for(ARENA, cross_platform());
    let mut game = Scheduler::with_seed(vm(module), 9).expect("a started game");
    game.play(arena_tape()).expect("plays");
    game
}

#[test]
fn a_multi_system_game_runs_deterministically_over_every_phase() {
    let mut game = arena();
    let mut hashes = Vec::new();
    for _ in 0..300 {
        game.step(1);
        hashes.push(game.snapshot().hash());
    }
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
    let score = int(state(&game, "Combat", "score"));
    let hits = int(state(&game, "Combat", "hits"));
    assert!(score > 0, "the hero picked up coins");
    assert!(hits > 0, "the chasing monsters reached the hero");
    assert!(int(state(&game, "Brain", "alarms")) > 0);
    assert!(int(state(&game, "Controls", "jumps")) > 0);
    assert_eq!(int(state(&game, "Referee", "ticks")), 300);
    let cues: Vec<_> = game.stage().cues().iter().map(|c| c.sound).collect();
    let count = |sfx| cues.iter().filter(|&&s| s == Sound::Sfx(sfx)).count() as i64;
    assert_eq!((count(Sfx::Coin), count(Sfx::Hurt)), (score, hits));
    assert_eq!(game.delivered_commands(), (score + hits) as u64);

    // A second run reaches every tick's hash.
    let mut again = arena();
    for (tick, hash) in hashes.iter().enumerate() {
        again.step(1);
        assert_eq!(again.snapshot().hash(), *hash, "tick {tick}");
    }

    // Restoring mid-run resumes tick for tick.
    let mut resumed = arena();
    resumed.step(150);
    let saved = GameSnapshot::decode(&resumed.snapshot().encode()).expect("decodes");
    resumed.step(40);
    assert!(!resumed.restore(&saved).degraded);
    for (tick, hash) in hashes.iter().enumerate().skip(150) {
        resumed.step(1);
        assert_eq!(resumed.snapshot().hash(), *hash, "tick {tick}");
    }
}
