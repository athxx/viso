use super::*;
use crate::native::Natives;

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
