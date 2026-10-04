use super::world;
use super::*;
use crate::native::Natives;
use crate::value::Value;

#[test]
fn whole_fixed_steps_come_due_and_the_fraction_carries() {
    let mut clock = Clock::new(0.25);
    assert_eq!(clock.advance(0.625), 2);
    assert_eq!(clock.advance(0.125), 1);
    assert_eq!(clock.advance(0.0), 0);
    for _ in 0..3 {
        clock.finish_tick();
    }
    assert_eq!(clock.tick(), 3);
    assert_eq!(clock.time(), 0.75);
    assert_eq!(clock.overrun_ticks(), 0);
}

#[test]
fn time_scale_and_pause_change_the_rate_not_the_step() {
    let mut clock = Clock::new(0.25);
    clock.set_time_scale(0.5);
    assert_eq!(clock.advance(1.0), 2);
    clock.set_paused(true);
    assert_eq!(clock.advance(10.0), 0);
    clock.set_paused(false);
    for bad in [-1.0, f64::NAN, f64::INFINITY] {
        clock.set_time_scale(bad);
        assert_eq!(clock.time_scale(), 0.0);
        assert_eq!(clock.advance(1.0), 0);
    }
    clock.set_time_scale(1.0);
    assert_eq!(clock.advance(f64::NAN), 0);
    assert_eq!(clock.advance(-1.0), 0);
    assert_eq!(clock.fixed_dt(), 0.25);
}

#[test]
fn drop_time_discards_the_overrun_and_keeps_the_fraction() {
    let mut clock = Clock::new(0.25);
    clock.set_max_catch_up_steps(2);
    assert_eq!(clock.advance(1.375), 2);
    assert_eq!(clock.overrun_ticks(), 3);
    assert_eq!(clock.dropped_time(), 0.75);
    assert_eq!(clock.advance(0.125), 1);
}

#[test]
fn slow_motion_defers_the_overrun_up_to_one_frame_of_catch_up() {
    let mut clock = Clock::new(0.25);
    clock.set_overrun(TickOverrun::SlowMotion);
    clock.set_max_catch_up_steps(2);
    assert_eq!(clock.advance(1.0), 2);
    assert_eq!(clock.overrun_ticks(), 2);
    assert_eq!(clock.dropped_time(), 0.0);
    assert_eq!(clock.advance(0.0), 2);
    assert_eq!(clock.advance(0.0), 0);

    assert_eq!(clock.advance(2.0), 2);
    assert_eq!(clock.overrun_ticks(), 8);
    assert_eq!(clock.dropped_time(), 1.0);
    assert_eq!(clock.advance(0.0), 2);
    assert_eq!(clock.advance(0.0), 0);
}

#[test]
#[should_panic(expected = "fixed_dt")]
fn a_clock_needs_a_positive_step() {
    Clock::new(0.0);
}

#[test]
fn the_standard_registry_holds_the_scheduler_traits() {
    let natives = Natives::standard();
    let fixed = natives
        .native_trait("viso::game::FixedUpdate")
        .expect("FixedUpdate is registered");
    assert_eq!(fixed.hook_id("fixed_update"), FIXED_UPDATE);
    assert_eq!(
        fixed.native_trait.hooks[0].params[0].ty,
        SchemaTy::Handle("viso::game::FixedFrame")
    );
    let frame = natives.native_trait("viso::game::FrameUpdate").unwrap();
    assert_eq!(frame.hook_id("frame_update"), FRAME_UPDATE);
    let collision = natives
        .native_trait("viso::game::CollisionListener")
        .unwrap();
    assert_eq!(collision.hook_id("collision"), COLLISION);
    assert_eq!(
        natives.native_trait_by_id(fixed.id).map(|t| &*t.path),
        Some("viso::game::FixedUpdate")
    );
    assert!(natives.native_trait("viso::game::FixedFrame").is_none());
    assert!(natives.ty("viso::game::FixedFrame").is_some());
    assert!(natives.function("viso::game::FixedFrame::time").is_some());
}

/// A latch over the default action set and the snapshot its ticks read.
fn latch() -> (input::InputLatch, InputSnapshot) {
    let schema = InputSchema::standard();
    (
        input::InputLatch::new(&schema),
        InputSnapshot::new(schema.actions.len()),
    )
}

const JUMP: Action = Action(InputAction::Jump as u32);
const FIRE: Action = Action(InputAction::Fire as u32);

#[test]
fn an_edge_is_delivered_to_exactly_one_tick() {
    let (mut latch, snapshot) = latch();
    latch.key(Key::Space, true);
    // A frame that runs no tick keeps the edge; the first tick after sees it.
    latch.deliver(&snapshot);
    assert!(snapshot.pressed(JUMP) && snapshot.held(JUMP));
    // A second tick of the same frame sees the same `held`, no edge.
    latch.deliver(&snapshot);
    assert!(!snapshot.pressed(JUMP) && snapshot.held(JUMP));

    latch.key(Key::Space, false);
    latch.deliver(&snapshot);
    assert!(snapshot.released(JUMP) && !snapshot.held(JUMP));
    latch.deliver(&snapshot);
    assert!(!snapshot.released(JUMP));
}

#[test]
fn a_press_and_release_between_ticks_is_both_edges() {
    let (mut latch, snapshot) = latch();
    latch.pad(PadButton::West, true);
    latch.pad(PadButton::West, false);
    latch.deliver(&snapshot);
    assert!(snapshot.pressed(FIRE) && snapshot.released(FIRE) && !snapshot.held(FIRE));
}

#[test]
fn an_action_is_held_while_any_of_its_inputs_is() {
    let (mut latch, snapshot) = latch();
    latch.key(Key::Space, true);
    latch.pad(PadButton::South, true);
    latch.key(Key::Space, false);
    latch.deliver(&snapshot);
    assert!(snapshot.pressed(JUMP) && !snapshot.released(JUMP) && snapshot.held(JUMP));
    latch.touch(TouchButton::Primary, true);
    latch.pad(PadButton::South, false);
    latch.deliver(&snapshot);
    assert!(!snapshot.pressed(JUMP) && !snapshot.released(JUMP) && snapshot.held(JUMP));
    latch.release_all();
    latch.deliver(&snapshot);
    assert!(snapshot.released(JUMP) && !snapshot.held(JUMP));
}

#[test]
fn the_move_vector_has_a_dead_zone_and_is_at_most_one_long() {
    let (mut latch, snapshot) = latch();
    latch.stick(PadStick::Left, 0.1, 0.1);
    latch.deliver(&snapshot);
    assert_eq!(snapshot.move_axes(), (0.0, 0.0));

    latch.stick(PadStick::Left, 0.6, 0.0);
    latch.deliver(&snapshot);
    let (x, y) = snapshot.move_axes();
    assert!((x - 0.5).abs() < 1e-12 && y == 0.0, "{x} {y}");

    latch.stick(PadStick::Left, 0.0, 0.0);
    latch.key(Key::W, true);
    latch.key(Key::D, true);
    latch.deliver(&snapshot);
    let (x, y) = snapshot.move_axes();
    assert!((x - y).abs() < 1e-12 && (x.hypot(y) - 1.0).abs() < 1e-12);

    latch.stick(PadStick::Left, f64::NAN, 4.0);
    latch.key(Key::W, false);
    latch.key(Key::D, false);
    latch.deliver(&snapshot);
    assert_eq!(snapshot.move_axes(), (0.0, 1.0));
}

#[test]
fn an_input_schema_survives_the_wire_and_is_verified() {
    use crate::module::Module;
    let module = |schema: InputSchema| {
        Module::new(Vec::new(), Vec::new(), Vec::new(), Vec::new())
            .unwrap()
            .with_input(schema)
    };
    let mut schema = InputSchema::standard();
    schema.bindings.dead_zone = 0.35;
    let encoded = module(schema.clone()).unwrap().encode();
    let decoded = Module::decode(&encoded).unwrap();
    assert_eq!(decoded.input(), Some(&schema));

    let mut bad = InputSchema::standard();
    bad.bindings.keys.push((Key::Q, Action(9)));
    assert!(module(bad).is_err());
    let mut bad = InputSchema::standard();
    bad.bindings.dead_zone = 1.0;
    assert!(module(bad).is_err());
    let mut bad = InputSchema::standard();
    bad.actions = Box::new([]);
    bad.bindings = InputBindings::new();
    assert!(module(bad).is_err());
}

#[test]
fn the_standard_registry_holds_the_input_schema() {
    let natives = Natives::standard();
    let key = natives.ty("viso::game::Key").expect("Key is registered");
    assert!(key.ty.is_enum());
    let space = natives
        .variant("viso::game::Key::Space")
        .expect("a variant");
    assert_eq!((space.ty, space.index), (key.id, Key::Space as u32));
    assert!(natives.variant("viso::game::Key::Nope").is_none());
    assert!(natives.variant("viso::game::PadButton::Space").is_none());
    let jump = natives.variant("viso::game::InputAction::jump").unwrap();
    assert_eq!(jump.index, InputAction::Jump as u32);
    assert!(natives.derive("InputAction").is_some());
    assert!(natives.derive("Nope").is_none());
    let input = natives.function("viso::game::FixedFrame::input").unwrap();
    assert!(input.function.property);
    let new = natives.function("viso::game::InputMap::new").unwrap();
    assert!(new.function.constant && new.function.deterministic);
}

#[test]
fn alpha_is_the_carried_fraction_of_a_tick_below_one() {
    let mut clock = Clock::at_rate(4);
    assert_eq!(clock.fixed_dt(), 0.25);
    assert_eq!(clock.alpha(), 0.0);
    clock.advance(0.3125);
    assert_eq!(clock.alpha(), 0.25);
    // A slow-motion backlog keeps more than a tick, which still weighs below 1.
    clock.set_overrun(TickOverrun::SlowMotion);
    clock.set_max_catch_up_steps(1);
    clock.advance(1.0);
    assert!(clock.alpha() < 1.0 && clock.alpha() > 0.99);
}

#[test]
fn a_cooldown_blocks_for_its_period_after_firing() {
    let ready = Cooldown::new(3);
    assert!(ready.ready(0));
    let fired = ready.fire(10);
    assert!(!fired.ready(12));
    assert_eq!(fired.remaining(11), 2);
    assert!(fired.ready(13));
    assert_eq!(fired.remaining(20), 0);
    let free = Cooldown::new(0).fire(5);
    assert!(free.ready(5));
}

#[test]
fn a_tick_timer_rearms_on_its_phase_without_drift() {
    let timer = TickTimer::every(4);
    assert!(!timer.due(3));
    assert!(timer.due(4));
    assert_eq!(timer.rearm(3), timer, "not due yet");
    let next = timer.rearm(4);
    assert!(!next.due(7) && next.due(8));
    // Checked late, it skips the missed periods and stays on phase.
    let late = timer.rearm(13);
    assert_eq!(late.remaining(13), 3);
    assert!(TickTimer::every(0).rearm(1).due(2), "at least one tick");
}

#[test]
fn timer_values_round_trip_and_are_schema_value_types() {
    use crate::native::NativeValue;
    let cooldown = Cooldown::new(5).fire(2);
    assert_eq!(Cooldown::from_value(&cooldown.into_value()), Some(cooldown));
    let timer = TickTimer::every(3).rearm(7);
    assert_eq!(TickTimer::from_value(&timer.into_value()), Some(timer));
    assert_eq!(TickTimer::from_value(&Value::Int(1)), None);
    let natives = Natives::standard();
    let ty = natives.ty(Cooldown::PATH).expect("Cooldown is registered");
    assert!(ty.ty.value && ty.ty.snapshots());
    let fire = natives.function("viso::game::Cooldown::fire").unwrap();
    assert!(fire.is_method(&natives));
    let new = natives.function("viso::game::TickTimer::every").unwrap();
    assert_eq!(new.function.params[0].ty, crate::native::SchemaTy::Ticks);
}

fn spawn_block(world: &GameWorld, system: usize, at: [f32; 3]) -> EntityId {
    world.open(system);
    let id = world
        .spawn(
            SpawnDesc::new(BodyKind::Block, Vec3F32::new(1.0, 1.0, 1.0))
                .at(Vec3F32::from_array(at)),
        )
        .expect("open");
    world.close();
    id
}

#[test]
fn spawns_reserve_ids_in_call_order_and_commit_in_system_order() {
    let world = GameWorld::new(1);
    let late = spawn_block(&world, 1, [1.0, 0.0, 0.0]);
    let early = spawn_block(&world, 0, [2.0, 0.0, 0.0]);
    assert_eq!((late.index(), early.index()), (0, 1));
    assert!(!world.is_alive(late));
    world.commit();
    assert_eq!(world.entities(), [early, late], "system 0 commits first");
    assert!(world.bodies().is_consistent());

    world.open(0);
    world.push(world::Command::Remove(early)).expect("open");
    world.close();
    world.commit();
    let reused = spawn_block(&world, 0, [0.0; 3]);
    world.commit();
    assert_eq!((reused.index(), reused.generation()), (1, 1));
    assert_eq!(world.entities(), [late, reused]);
    assert!(world.bodies().is_consistent());
}

#[test]
fn a_rollback_returns_reservations_and_draws() {
    let world = GameWorld::new(9);
    let mark = world.mark();
    let first = spawn_block(&world, 0, [0.0; 3]);
    world.open(0);
    let draw = world.random().expect("open");
    world.close();
    world.rollback(mark);
    let again = spawn_block(&world, 0, [0.0; 3]);
    world.open(0);
    assert_eq!(world.random().expect("open"), draw);
    world.close();
    assert_eq!(first, again);
    world.commit();
    assert_eq!(world.entities(), [again]);
}

#[test]
fn writes_outside_a_hook_fail() {
    let world = GameWorld::new(0);
    assert!(world.spawn(SpawnDesc::player()).is_err());
    assert!(world.random().is_err());
    world.commit();
    assert!(world.entities().is_empty());
}

#[test]
fn random_ranges_cover_their_bounds_without_overflow() {
    let world = GameWorld::new(3);
    world.open(0);
    for _ in 0..64 {
        let x = world.random_range(i64::MIN, i64::MAX).expect("range");
        assert!(x < i64::MAX);
        assert_eq!(world.random_range(5, 6).expect("one value"), 5);
        let unit = world.random().expect("draw");
        assert!((0.0..1.0).contains(&unit));
    }
    assert!(world.random_range(2, 2).is_err());
    world.close();
}

#[test]
fn bodies_round_trip_canonically_and_reject_inconsistency() {
    let world = GameWorld::new(0);
    spawn_block(&world, 0, [1.5, -2.0, 0.25]);
    world.open(0);
    world.spawn(SpawnDesc::player().tag(Tag(3))).expect("open");
    world.close();
    world.commit();
    world.step(1.0 / 60.0, &mut Vec::new());
    let bodies = world.bodies();
    let mut enc = viso_ende::Encoder::new();
    bodies.encode(&mut enc);
    let bytes = enc.into_bytes();
    let mut dec = viso_ende::Decoder::new(&bytes);
    assert_eq!(world::Bodies::decode(&mut dec).expect("decodes"), *bodies);

    let mut broken = (*bodies).clone();
    broken.order.push(0);
    let mut enc = viso_ende::Encoder::new();
    broken.encode(&mut enc);
    let bytes = enc.into_bytes();
    assert!(world::Bodies::decode(&mut viso_ende::Decoder::new(&bytes)).is_err());
}

#[test]
fn a_character_lands_on_a_block_and_stands() {
    let world = GameWorld::new(0);
    world.open(0);
    let floor = world
        .spawn(SpawnDesc::new(
            BodyKind::Block,
            Vec3F32::new(10.0, 1.0, 10.0),
        ))
        .expect("open");
    let player = world
        .spawn(SpawnDesc::player().at(Vec3F32::new(0.0, 2.0, 0.0)))
        .expect("open");
    world.close();
    world.commit();
    let mut began = Vec::new();
    for _ in 0..240 {
        world.begin_tick();
        world.step(1.0 / 240.0, &mut began);
    }
    assert_eq!(
        world.position(player),
        Some(Vec3F32::new(0.0, 0.5 + 0.9, 0.0))
    );
    assert!(world.bodies().floor[player.index() as usize]);
    assert!(began.is_empty(), "blocks report no contacts: {floor}");
}

/// The physics step without a broadphase: every character against every
/// block in allocation order, every pair tested for contact.
fn reference_step(b: &mut world::Bodies, dt: f32, began: &mut Vec<(EntityId, EntityId)>) {
    let order = b.order.clone();
    for &slot in &order {
        let slot = slot as usize;
        if b.kind[slot] != BodyKind::Character {
            continue;
        }
        let push = std::mem::take(&mut b.push[slot]);
        b.vel[slot][0] = push[0];
        b.vel[slot][2] = push[2];
        b.vel[slot][1] += push[1] - GRAVITY * dt;
        b.floor[slot] = false;
        for axis in [1, 0, 2] {
            let speed = b.vel[slot][axis];
            if speed == 0.0 {
                continue;
            }
            b.pos[slot][axis] += speed * dt;
            for &other in &order {
                let other = other as usize;
                if b.kind[other] == BodyKind::Block && b.overlap(slot, other, axis) {
                    b.resolve(slot, other, axis, speed);
                }
            }
        }
    }
    super::kit::steer::settle(b, &order);
    let mut now = Vec::new();
    for (i, &a) in order.iter().enumerate() {
        if b.kind[a as usize] != BodyKind::Character {
            continue;
        }
        for (j, &o) in order.iter().enumerate() {
            let pairs = match b.kind[o as usize] {
                BodyKind::Sensor => true,
                BodyKind::Character => j > i,
                BodyKind::Block => false,
            };
            if pairs && b.touch(a as usize, o as usize) {
                now.push((i.min(j), i.max(j)));
            }
        }
    }
    now.sort_unstable();
    let mut contacts: Vec<_> = now
        .iter()
        .map(|&(i, j)| (b.id(order[i] as usize), b.id(order[j] as usize)))
        .collect();
    for &pair in &contacts {
        if b.contacts.binary_search(&pair).is_err() {
            began.push(pair);
        }
    }
    contacts.sort_unstable();
    b.contacts = contacts;
}

/// A deterministic test source of numbers.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.0 >> 33
    }

    fn range(&mut self, low: f32, high: f32) -> f32 {
        low + (high - low) * (self.next() % 10_000) as f32 / 10_000.0
    }

    fn pick(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_desc(rng: &mut Lcg) -> SpawnDesc {
    let kind = [
        BodyKind::Character,
        BodyKind::Block,
        BodyKind::Block,
        BodyKind::Sensor,
    ][rng.pick(4) as usize];
    let size = if rng.pick(10) == 0 {
        Vec3F32::new(
            rng.range(10.0, 60.0),
            rng.range(0.5, 2.0),
            rng.range(10.0, 60.0),
        )
    } else {
        Vec3F32::new(
            rng.range(0.2, 3.0),
            rng.range(0.2, 3.0),
            rng.range(0.2, 3.0),
        )
    };
    let at = Vec3F32::new(
        rng.range(-20.0, 20.0),
        rng.range(-3.0, 6.0),
        rng.range(-20.0, 20.0),
    );
    SpawnDesc::new(kind, size).at(at)
}

#[test]
fn the_grid_step_equals_testing_every_pair() {
    for seed in 0..12 {
        let mut rng = Lcg(seed);
        let fast = GameWorld::new(0);
        let slow = GameWorld::new(0);
        let mut began_fast = Vec::new();
        let mut began_slow = Vec::new();
        for tick in 0..240 {
            // The same commands to both worlds: spawns early, then walks,
            // jumps, teleports into the level and removals.
            let ids = fast.entities();
            let mut commands = Vec::new();
            let spawns = if tick < 3 {
                40
            } else {
                u64::from(rng.pick(8) == 0)
            };
            for _ in 0..spawns {
                commands.push(None);
            }
            for &id in &ids {
                match rng.pick(40) {
                    0..=19 => commands.push(Some(world::Command::Walk(
                        id,
                        rng.range(-8.0, 8.0),
                        rng.range(-8.0, 8.0),
                    ))),
                    20 => commands.push(Some(world::Command::Jump(id, rng.range(2.0, 12.0)))),
                    21 => commands.push(Some(world::Command::Teleport(
                        id,
                        [
                            rng.range(-20.0, 20.0),
                            rng.range(-2.0, 5.0),
                            rng.range(-20.0, 20.0),
                        ],
                    ))),
                    22 => commands.push(Some(world::Command::Remove(id))),
                    _ => {}
                }
            }
            let descs: Vec<_> = commands
                .iter()
                .map(|c| {
                    c.as_ref()
                        .map_or_else(|| Some(random_desc(&mut rng)), |_| None)
                })
                .collect();
            for world in [&fast, &slow] {
                world.open(0);
                for (command, desc) in commands.iter().zip(&descs) {
                    match (command, desc) {
                        (Some(command), _) => world.push(command.clone()).expect("open"),
                        (None, Some(desc)) => drop(world.spawn(*desc).expect("open")),
                        (None, None) => unreachable!(),
                    }
                }
                world.close();
                world.commit();
                world.begin_tick();
            }
            fast.step(1.0 / 60.0, &mut began_fast);
            slow.with_bodies(|b| reference_step(b, 1.0 / 60.0, &mut began_slow));
            assert_eq!(fast.bodies(), slow.bodies(), "seed {seed}, tick {tick}");
            assert_eq!(began_fast, began_slow, "seed {seed}, tick {tick}");
        }
        assert!(!began_fast.is_empty(), "seed {seed} met no contact");
    }
}
