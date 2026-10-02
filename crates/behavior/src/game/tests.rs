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
