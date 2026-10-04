//! The game world: hooks write it through commands merged in system order,
//! entities keep stable generational ids and allocation order, tags come
//! from the package's `@derive(GameTag)` enum, the physics step moves
//! characters and reports contacts, and snapshots carry the world and the
//! seeded random state.

use std::rc::Rc;

use viso_behavior::game::{
    BodyKind, EntityId, Extracted, GameSnapshot, Key, Scheduler, SystemFault,
};
use viso_behavior::native::{NativeValue, Natives, Vec3F32};
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_in};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

fn compiled(source: &str) -> viso_dsl::frontend::Compiled {
    compile_file_in(source, &origin(), Natives::standard())
}

fn codes(source: &str) -> Vec<String> {
    compiled(source)
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

fn module(source: &str) -> Rc<Module> {
    let compiled = compiled(source);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = compiled.behavior.bytecode().expect("verified bytecode");
    Rc::new(module.with_tick_rate(60).expect("a tick rate"))
}

fn seeded(module: Rc<Module>, seed: u64) -> Result<Scheduler, SystemFault> {
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::with_seed(vm, seed)
}

fn start(source: &str) -> Scheduler {
    seeded(module(source), 7).expect("a started game")
}

/// State `name` of system `system`.
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

fn entity(value: &Value) -> EntityId {
    EntityId::from_value(value).expect("an entity id")
}

fn at(game: &Scheduler, id: EntityId) -> [f32; 3] {
    game.world().position(id).expect("alive").to_array()
}

/// The fixed step as the world steps it.
fn dt() -> f32 {
    (1.0f64 / 60.0) as f32
}

const IMPORTS: &str = "
import viso::game::{Startup, GameStart, FixedUpdate, FixedFrame, FrameUpdate, RenderFrame,
    CollisionListener, CollisionEvent, EntityId, SpawnDesc, GameTag, InputAction};
import viso::math::Vec3F32;
";

fn play(body: &str) -> Scheduler {
    start(&format!("{IMPORTS}{body}"))
}

#[test]
fn what_the_start_spawns_exists_at_the_first_tick() {
    let mut game = start(
        r#"
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{EntityId, SpawnDesc};
import viso::math::Vec3F32;

export system Tiny implements QuickGame {
    state player: Option<EntityId> = Option::None;
    state seen = false;
    state floor = false;

    action start(cx: QuickStart) {
        player = Option::Some(cx.spawn(SpawnDesc::player().at(Vec3F32::new(0.0f32, 3.0f32, 0.0f32))));
        cx.world.spawn(SpawnDesc::block(Vec3F32::new(40.0f32, 1.0f32, 40.0f32)));
    }

    action fixed(frame: QuickFrame) {
        match player {
            Option::Some(id) => {
                if frame.tick() == 0 {
                    seen = frame.world.is_alive(id);
                }
                floor = frame.world.on_floor(id);
            },
            Option::None => {},
        }
    }
}
"#,
    );
    let player = entity(&state(&game, "Tiny", "player"));
    assert_eq!(game.world().entities().len(), 2);
    game.step(1);
    assert_eq!(state(&game, "Tiny", "seen"), Value::bool(true));
    assert!(at(&game, player)[1] < 3.0, "gravity pulls the player down");
    game.step(120);
    assert_eq!(
        at(&game, player),
        [0.0, 0.5 + 0.9, 0.0],
        "it rests on the block"
    );
    game.step(1);
    assert_eq!(state(&game, "Tiny", "floor"), Value::bool(true));
}

const COMMANDS: &str = r#"
system Setup implements Startup {
    state player: Option<EntityId> = Option::None;
    action startup(cx: GameStart) {
        player = Option::Some(cx.spawn(SpawnDesc::player().tag(GameTag::player)));
        cx.spawn(SpawnDesc::block(Vec3F32::new(40.0f32, 1.0f32, 40.0f32)).at(Vec3F32::new(0.0f32, -1.4f32, 0.0f32)));
    }
}

@after(Setup)
system Late implements FixedUpdate {
    state mode = 0;
    action fixed_update(frame: FixedFrame) {
        for id in frame.world.query(GameTag::player) {
            if mode == 0 { frame.world.walk(id, 2.0, 0.5f32); }
            if mode == 1 { frame.world.teleport(id, Vec3F32::new(7.0f32, 0.0f32, 0.0f32)); }
            if mode == 2 { frame.world.walk(id, 1.0, 0.0); }
        }
    }
}

@after(Setup)
@before(Late)
system Early implements FixedUpdate {
    state mode = 0;
    action fixed_update(frame: FixedFrame) {
        for id in frame.world.query(GameTag::player) {
            if mode == 0 { frame.world.walk(id, 1.0, 0.0); }
            if mode == 1 {
                frame.world.teleport(id, Vec3F32::new(9.0f32, 0.0f32, 0.0f32));
                frame.world.teleport(id, Vec3F32::new(5.0f32, 0.0f32, 0.0f32));
            }
            if mode == 2 { frame.world.remove(id); }
        }
    }
}
"#;

#[test]
fn walks_add_up_in_system_order() {
    let mut game = play(COMMANDS);
    let player = entity(&state(&game, "Setup", "player"));
    game.step(1);
    let x = 3.0f32 * dt();
    let z = 0.5f32 * dt();
    let [px, _, pz] = at(&game, player);
    assert_eq!((px, pz), (x, z));
    assert_eq!(game.world().skipped_commands(), 0);
}

#[test]
fn the_last_teleport_wins_and_puts_the_body_at_rest() {
    let source = COMMANDS.replace("state mode = 0;", "state mode = 1;");
    let mut game = play(&source);
    let player = entity(&state(&game, "Setup", "player"));
    game.step(1);
    // Late runs after Early, so its teleport commits last; the player then
    // falls from rest onto the block.
    let rest = -1.4f32 + (0.9 + 0.5);
    assert_eq!(at(&game, player), [7.0, rest, 0.0]);
    let mut drawn = Vec::new();
    game.world().extract(0.5, &mut drawn);
    let start = drawn.iter().find(|e| e.id == player).expect("drawn");
    assert_eq!(
        start.position,
        Vec3F32::new(7.0, 0.0, 0.0).lerp(Vec3F32::new(7.0, rest, 0.0), 0.5),
        "the frame interpolates from the teleport, not across it"
    );
}

#[test]
fn a_removal_voids_the_later_commands_naming_the_entity() {
    let source = COMMANDS.replace("state mode = 0;", "state mode = 2;");
    let mut game = play(&source);
    let player = entity(&state(&game, "Setup", "player"));
    game.step(1);
    assert!(!game.world().is_alive(player));
    assert_eq!(game.world().entities().len(), 1, "the block remains");
    assert_eq!(
        game.world().skipped_commands(),
        1,
        "Late's walk found no entity"
    );
}

#[test]
fn a_spawned_entity_exists_from_the_commit_and_slots_reuse_with_the_next_generation() {
    let mut game = play(
        r#"
system Spawner implements FixedUpdate {
    state ids: List<EntityId> = [];
    state seen_at_once = true;
    state first: Option<EntityId> = Option::None;

    action fixed_update(frame: FixedFrame) {
        let w = frame.world;
        if frame.tick() == 0 {
            let a = w.spawn(SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32)).tag(GameTag::coin));
            let b = w.spawn(SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32)));
            let c = w.spawn(SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32)).tag(GameTag::coin));
            seen_at_once = w.is_alive(a);
            first = Option::Some(b);
            ids = [a, b, c];
        }
        if frame.tick() == 1 {
            w.remove(ids[1]);
        }
        if frame.tick() == 2 {
            let d = w.spawn(SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32)).tag(GameTag::coin));
            ids = [ids[0], ids[2], d];
        }
    }
}
"#,
    );
    game.step(1);
    assert_eq!(state(&game, "Spawner", "seen_at_once"), Value::bool(false));
    let b = entity(&state(&game, "Spawner", "first"));
    assert!(game.world().is_alive(b));
    game.step(2);
    let Value::List(ids) = state(&game, "Spawner", "ids") else {
        panic!("a list");
    };
    let ids: Vec<EntityId> = ids.iter().map(entity).collect();
    assert_eq!(game.world().entities(), ids, "allocation order");
    let d = ids[2];
    assert_eq!((d.index(), d.generation()), (b.index(), b.generation() + 1));
    assert!(
        !game.world().is_alive(b),
        "a stale id never names the new entity"
    );
}

#[test]
fn queries_list_tagged_entities_in_allocation_order() {
    let mut game = play(
        r#"
system Census implements Startup + FixedUpdate {
    state coins: List<EntityId> = [];
    state all: List<EntityId> = [];
    state tagged = false;

    action startup(cx: GameStart) {
        let size = Vec3F32::new(1.0f32, 1.0f32, 1.0f32);
        cx.spawn(SpawnDesc::sensor(size).tag(GameTag::coin));
        cx.spawn(SpawnDesc::sensor(size).tag(GameTag::hazard));
        cx.spawn(SpawnDesc::sensor(size).tag(GameTag::hazard).tag(GameTag::coin));
    }

    action fixed_update(frame: FixedFrame) {
        coins = frame.world.query(GameTag::coin);
        all = frame.world.entities();
        tagged = frame.world.has_tag(all[2], GameTag::hazard) && !frame.world.has_tag(all[0], GameTag::hazard);
    }
}
"#,
    );
    game.step(1);
    let list = |name| match state(&game, "Census", name) {
        Value::List(ids) => ids.iter().map(entity).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    let all = list("all");
    assert_eq!(list("coins"), vec![all[0], all[2]]);
    assert_eq!(state(&game, "Census", "tagged"), Value::bool(true));
}

#[test]
fn a_package_derives_its_own_tags() {
    let source = r#"
import viso::game::{Startup, GameStart, FixedUpdate, FixedFrame, EntityId, SpawnDesc};
import viso::math::Vec3F32;

@derive(Eq, GameTag)
export enum Kind { Coin; Spike; }

system Level implements Startup + FixedUpdate {
    state spikes = 0;

    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32)).tag(Kind::Spike));
    }

    action fixed_update(frame: FixedFrame) {
        for id in frame.world.query(Kind::Spike) { spikes += 1; }
    }
}
"#;
    let mut game = start(source);
    game.step(1);
    assert_eq!(state(&game, "Level", "spikes"), Value::Int(1));

    let standard = source
        .replace("SpawnDesc};", "SpawnDesc, GameTag};")
        .replace("query(Kind::Spike)", "query(GameTag::hazard)");
    assert!(
        codes(&standard).contains(&"E2103".to_owned()),
        "{:?}",
        codes(&standard)
    );

    let second = source.replace(
        "export enum Kind { Coin; Spike; }",
        "export enum Kind { Coin; Spike; }\n@derive(GameTag)\nexport enum More { Wall; }",
    );
    assert_eq!(codes(&second), ["E2202"]);

    let variants: Vec<String> = (0..65).map(|i| format!("V{i};")).collect();
    let wide = source.replace(
        "Coin; Spike;",
        &format!("Coin; Spike; {}", variants.join(" ")),
    );
    assert!(
        codes(&wide).contains(&"E2201".to_owned()),
        "{:?}",
        codes(&wide)
    );

    let payload = source.replace("Coin; Spike;", "Coin(I64); Spike;");
    assert!(
        codes(&payload).contains(&"E2201".to_owned()),
        "{:?}",
        codes(&payload)
    );
}

const COINS: &str = r#"
system Collector implements Startup + FixedUpdate + CollisionListener {
    state player: Option<EntityId> = Option::None;
    state score = 0;
    state events = 0;

    action startup(cx: GameStart) {
        player = Option::Some(cx.spawn(SpawnDesc::player()));
        cx.spawn(SpawnDesc::block(Vec3F32::new(40.0f32, 1.0f32, 40.0f32)).at(Vec3F32::new(0.0f32, -1.4f32, 0.0f32)));
        cx.spawn(SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32)).at(Vec3F32::new(2.0f32, 0.0f32, 0.0f32)).tag(GameTag::coin));
        cx.spawn(SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32)).at(Vec3F32::new(4.0f32, 0.0f32, 0.0f32)).tag(GameTag::hazard));
    }

    action fixed_update(frame: FixedFrame) {
        match player {
            Option::Some(id) => { frame.world.walk(id, 6.0, 0.0); },
            Option::None => {},
        }
    }

    action collision(event: CollisionEvent) {
        events += 1;
        match player {
            Option::Some(id) => {
                match event.other_of(id) {
                    Option::Some(other) => {
                        if event.world.has_tag(other, GameTag::coin) {
                            event.world.remove(other);
                            score += 1;
                        }
                    },
                    Option::None => {},
                }
            },
            Option::None => {},
        }
    }
}
"#;

#[test]
fn contacts_that_begin_reach_the_listeners_once() {
    let mut game = play(COINS);
    game.step(60);
    assert_eq!(state(&game, "Collector", "score"), Value::Int(1));
    // The coin, then the hazard the player walks through: one event each.
    assert_eq!(state(&game, "Collector", "events"), Value::Int(2));
    assert_eq!(game.world().entities().len(), 3, "the coin was removed");
}

#[test]
fn a_faulting_hook_discards_its_world_commands_and_draws() {
    let faulting = r#"
system Gambler implements FixedUpdate {
    state drawn = 0.0;
    state n = 0;
    action fixed_update(frame: FixedFrame) {
        let r = frame.world.random();
        if frame.tick() == 0 {
            frame.world.spawn(SpawnDesc::player());
            frame.world.random();
            let xs = [1];
            n = xs[5];
        }
        if frame.tick() == 1 { drawn = r; }
    }
}
"#;
    let mut game = play(faulting);
    game.step(1);
    assert_eq!(game.take_faults().len(), 1);
    assert!(
        game.world().entities().is_empty(),
        "the spawn was discarded"
    );
    game.step(1);
    // The faulting tick's draws were rolled back: tick 1 drew the first
    // number of the sequence.
    let mut reference = play(
        &faulting
            .replace("frame.tick() == 0", "false")
            .replace("frame.tick() == 1", "frame.tick() == 0"),
    );
    reference.step(1);
    assert_eq!(
        state(&game, "Gambler", "drawn"),
        state(&reference, "Gambler", "drawn")
    );
    assert_ne!(state(&game, "Gambler", "drawn"), Value::Float(0.0));
}

#[test]
fn the_seed_fixes_the_random_sequence() {
    let source = format!(
        "{IMPORTS}{}",
        r#"
system Dice implements FixedUpdate {
    state roll = 0;
    state unit = true;
    action fixed_update(frame: FixedFrame) {
        roll = frame.world.random_range(-3, 4);
        let r = frame.world.random();
        unit = unit && r >= 0.0 && r < 1.0;
    }
}
"#
    );
    let rolls = |seed| {
        let mut game = seeded(module(&source), seed).expect("game");
        let rolls: Vec<i64> = (0..64)
            .map(|_| {
                game.step(1);
                state(&game, "Dice", "roll").as_int().expect("an int")
            })
            .collect();
        assert_eq!(state(&game, "Dice", "unit"), Value::bool(true));
        rolls
    };
    let a = rolls(1);
    assert_eq!(a, rolls(1));
    assert_ne!(a, rolls(2));
    assert!(a.iter().all(|r| (-3..4).contains(r)), "{a:?}");
    assert!(
        (-3..4).all(|v| a.contains(&v)),
        "every value comes up: {a:?}"
    );
}

#[test]
fn a_snapshot_carries_the_world_and_the_random_state() {
    let source = COINS.replace(
        "events += 1;",
        "events += 1; score = score + event.world.random_range(0, 100) * 1000;",
    );
    let mut game = play(&source);
    game.step(10);
    let snapshot = game.snapshot();
    assert_eq!(snapshot.entities(), 4);
    let blob = snapshot.encode();
    assert_eq!(GameSnapshot::decode(&blob).expect("decodes"), snapshot);
    game.step(40);
    let after = game.snapshot();
    let mut drawn = Vec::new();
    game.world().extract(0.25, &mut drawn);

    game.restore(&snapshot);
    assert_eq!(game.world().entities().len(), 4);
    game.step(40);
    assert_eq!(game.snapshot().hash(), after.hash());
    let mut again: Vec<Extracted> = Vec::new();
    game.world().extract(0.25, &mut again);
    assert_eq!(again, drawn);

    let mut fresh = start(&format!("{IMPORTS}{source}"));
    fresh.restore(&GameSnapshot::decode(&blob).expect("decodes"));
    fresh.step(40);
    assert_eq!(
        fresh.snapshot().hash(),
        after.hash(),
        "a saved game resumes tick for tick"
    );
    assert_ne!(snapshot.hash(), after.hash());
    assert_ne!(snapshot.rng_state(), after.rng_state());

    let mut truncated = blob.clone();
    truncated.truncate(blob.len() / 2);
    assert!(GameSnapshot::decode(&truncated).is_err());
}

#[test]
fn the_frame_reads_the_world_interpolated_and_cannot_write_it() {
    let source = r#"
system Setup implements Startup {
    state player: Option<EntityId> = Option::None;
    action startup(cx: GameStart) {
        player = Option::Some(cx.spawn(SpawnDesc::player()));
    }
}

export system Camera implements FrameUpdate {
    @local state y = 0.0f32;
    @local state cheat = false;
    action frame_update(frame: RenderFrame) {
        for id in frame.world.entities() {
            y = frame.position(id).y;
            if cheat { frame.world.jump(id, 5.0); }
        }
    }
}
"#;
    let mut game = play(source);
    let player = entity(&state(&game, "Setup", "player"));
    game.frame(dt() as f64 * 2.5);
    let alpha = game.clock().alpha();
    let mut drawn = Vec::new();
    game.world().extract(alpha, &mut drawn);
    assert_eq!(drawn[0].kind, BodyKind::Character);
    assert_eq!(
        state(&game, "Camera", "y"),
        Value::Float(f64::from(drawn[0].position.y))
    );
    assert!(
        drawn[0].position.y > at(&game, player)[1],
        "drawn between the ticks"
    );

    let mut game = start(&format!("{IMPORTS}{source}").replace("cheat = false", "cheat = true"));
    game.frame(dt() as f64);
    let faults = game.take_faults();
    assert_eq!(faults.first().map(|f| f.code), Some("E7106"), "{faults:?}");
}

/// §105.1 and §107, as the specification writes them.
#[test]
fn the_specification_games_run() {
    let tiny = r#"
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{EntityId, SpawnDesc, InputAction, InputAxis};

export system TinyGame implements QuickGame {
    state player: Option<EntityId> = Option::None;
    state score: I64 = 0;

    action start(cx: QuickStart) {
        player = Option::Some(cx.spawn(SpawnDesc::player()));
    }

    action fixed(frame: QuickFrame) {
        match player {
            Option::Some(id) => {
                let move = frame.input.axis(InputAxis::move_x);
                frame.world.walk(id, move * 6.0f32, 0.0f32);

                if frame.input.pressed(InputAction::jump)
                    && frame.world.on_floor(id) {
                    frame.world.jump(id, 10.0f32);
                }
            },
            Option::None => {},
        }
    }
}
"#;
    let mut game = start(tiny);
    game.key(Key::D, true);
    game.step(2);
    let player = entity(&state(&game, "TinyGame", "player"));
    assert!(at(&game, player)[0] > 0.0);

    let full = r#"
import viso::game::{
    Startup,
    GameStart,
    FixedUpdate,
    FixedFrame,
    CollisionListener,
    CollisionEvent,
    EntityId,
    SpawnDesc,
    InputAction,
    GameTag,
};
import viso::math::Vec3F32;

export system PlayerController implements Startup + FixedUpdate + CollisionListener {
    state player: Option<EntityId> = Option::None;
    state move_speed: F32 = 6.0f32;
    state jump_speed: F32 = 10.0f32;
    state score: I64 = 0;
    state respawn_point: Vec3F32 = Vec3F32::new(0.0f32, 4.0f32, 0.0f32);

    action startup(cx: GameStart) {
        player = Option::Some(cx.spawn(SpawnDesc::player().at(respawn_point).tag(GameTag::player)));
        cx.spawn(SpawnDesc::block(Vec3F32::new(20.0f32, 1.0f32, 20.0f32)));
        cx.spawn(
            SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32))
                .at(Vec3F32::new(3.0f32, 1.4f32, 0.0f32))
                .tag(GameTag::coin),
        );
    }

    action fixed_update(frame: FixedFrame) {
        match player {
            Option::Some(id) => {
                let world = frame.world;
                let movement = frame.input.move_axes();
                world.walk(id, movement.x * move_speed, movement.y * move_speed);

                if frame.input.pressed(InputAction::jump) && world.on_floor(id) {
                    world.jump(id, jump_speed);
                }

                if world.position(id).y < -20.0f32 {
                    world.teleport(id, respawn_point);
                }
            },
            Option::None => {},
        }
    }

    action collision(event: CollisionEvent) {
        match player {
            Option::Some(id) => {
                match event.other_of(id) {
                    Option::Some(other) => {
                        if event.world.has_tag(other, GameTag::coin) {
                            event.world.remove(other);
                            score += 1;
                        }
                    },
                    Option::None => {},
                }
            },
            Option::None => {},
        }
    }
}
"#;
    let mut game = start(full);
    game.key(Key::D, true);
    game.step(120);
    assert_eq!(state(&game, "PlayerController", "score"), Value::Int(1));
    let player = entity(&state(&game, "PlayerController", "player"));
    let respawned = (0..360).any(|_| {
        let before = at(&game, player);
        game.step(1);
        let after = at(&game, player);
        // The teleport commits before the tick's step moves the body on.
        before[1] < -19.0 && after[0] < 1.0 && after[1] > 3.9
    });
    assert!(respawned, "walked off the block, fell and respawned");
}
