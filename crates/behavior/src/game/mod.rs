//! The Game Profile runtime: the `viso::game` native schema, the fixed-step
//! [`Clock`] and the [`Scheduler`] that drives a module's systems.
//!
//! A `system` is a component without a view that implements scheduler traits.
//! The compiler binds each trait hook to the system action of its name and
//! records the binding by hook identity ([`NativeId`]) in
//! [`Module::systems`](crate::Module::systems); this scheduler maps the hook
//! identities of `viso::game` to its phases. Nothing in the compiler knows a
//! hook by name.
//!
//! One frame runs every fixed tick the clock owes, then every `FrameUpdate`.
//! One tick runs every `FixedUpdate` in system order, then delivers the
//! collision events queued for it to every `CollisionListener`, event by event.
//! Ticks share one instruction and native call budget each; frames share
//! another.
//!
//! Each tick reads a frozen [`InputSnapshot`] through `frame.input`: the
//! actions of the package's `InputMap` (or the default [`InputAction`] set)
//! and the move vector, edges delivered once (see [`input`]).
//!
//! The fixed step is the module's compile-time tick rate. [`Cooldown`] and
//! [`TickTimer`] count its whole ticks. A [`GameSnapshot`] captures the
//! Simulation state of every system, and restoring it resumes tick for tick
//! as if never interrupted. `RenderFrame.alpha()` is how far the frame is
//! into the next tick, for interpolation.

mod clock;
pub mod input;
mod scheduler;
mod snapshot;
mod timer;

use std::cell::Cell;

use crate::native::{
    HookDomain, NativeFunction, NativeHook, NativeId, NativeLibrary, NativeObject, NativeTrait,
    NativeType, Obj, Param, SchemaTy,
};

pub use clock::{Clock, TickOverrun};
pub use input::{
    Action, INPUT_ACTION_DERIVE, InputAction, InputAxis, InputBindings, InputMap, InputSchema,
    InputSnapshot, Key, KeySet, MoveAxes, MoveSource, PadButton, PadStick, TouchButton,
};
pub use scheduler::{CommandKey, Scheduler, SystemFault};
pub use snapshot::{GameSnapshot, Restored};
pub use timer::{Cooldown, TickTimer};

/// The identity of the `FixedUpdate.fixed_update` hook.
pub const FIXED_UPDATE: NativeId = NativeId::of("viso::game::FixedUpdate::fixed_update");
/// The identity of the `FrameUpdate.frame_update` hook.
pub const FRAME_UPDATE: NativeId = NativeId::of("viso::game::FrameUpdate::frame_update");
/// The identity of the `CollisionListener.collision` hook.
pub const COLLISION: NativeId = NativeId::of("viso::game::CollisionListener::collision");

/// The fixed tick a `FixedUpdate` runs in, behind a `viso::game::FixedFrame`
/// handle.
#[derive(Debug)]
pub struct FixedFrame {
    tick: Cell<u64>,
    dt: f64,
    input: Obj<InputSnapshot>,
}

impl NativeObject for FixedFrame {
    const PATH: &'static str = "viso::game::FixedFrame";
}

/// The frame a `FrameUpdate` runs in, behind a `viso::game::RenderFrame`
/// handle.
#[derive(Debug, Default)]
pub struct RenderFrame {
    dt: Cell<f64>,
    time: Cell<f64>,
    alpha: Cell<f32>,
}

impl NativeObject for RenderFrame {
    const PATH: &'static str = "viso::game::RenderFrame";
}

/// A contact between two bodies, behind a `viso::game::CollisionEvent`
/// handle.
#[derive(Debug, Default)]
pub struct CollisionEvent {
    tick: Cell<u64>,
    first: Cell<i64>,
    second: Cell<i64>,
}

impl NativeObject for CollisionEvent {
    const PATH: &'static str = "viso::game::CollisionEvent";
}

/// A tick or body key as an `I64`; ticks past `I64::MAX` saturate.
fn signed(tick: u64) -> i64 {
    i64::try_from(tick).unwrap_or(i64::MAX)
}

static FIXED_FRAME_METHODS: [NativeFunction; 4] = [
    crate::native!(fn "tick" |_cx, this: Obj<FixedFrame>| -> i64 { Ok(signed(this.tick.get())) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "dt" |_cx, this: Obj<FixedFrame>| -> f64 { Ok(this.dt) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "time" |_cx, this: Obj<FixedFrame>| -> f64 {
        Ok(this.tick.get() as f64 * this.dt)
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "input" |_cx, this: Obj<FixedFrame>| -> Obj<InputSnapshot> {
        Ok(this.input.clone())
    })
    .deterministic()
    .realtime_safe()
    .property(),
];

static RENDER_FRAME_METHODS: [NativeFunction; 3] = [
    crate::native!(fn "dt" |_cx, this: Obj<RenderFrame>| -> f64 { Ok(this.dt.get()) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "time" |_cx, this: Obj<RenderFrame>| -> f64 { Ok(this.time.get()) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "alpha" |_cx, this: Obj<RenderFrame>| -> f32 { Ok(this.alpha.get()) })
        .deterministic()
        .realtime_safe(),
];

static COLLISION_EVENT_METHODS: [NativeFunction; 3] = [
    crate::native!(fn "tick" |_cx, this: Obj<CollisionEvent>| -> i64 {
        Ok(signed(this.tick.get()))
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "first" |_cx, this: Obj<CollisionEvent>| -> i64 { Ok(this.first.get()) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "second" |_cx, this: Obj<CollisionEvent>| -> i64 {
        Ok(this.second.get())
    })
    .deterministic()
    .realtime_safe(),
];

/// The scheduler traits, the frame handles their hooks receive and the typed
/// input surface. The frame and input handles are borrowed: a hook reads them
/// during its call and cannot keep them.
pub(crate) static GAME: NativeLibrary = NativeLibrary {
    path: "viso::game",
    version: 1,
    functions: &[],
    types: &[
        NativeType::new("FixedFrame", &FIXED_FRAME_METHODS).borrowed(),
        NativeType::new("RenderFrame", &RENDER_FRAME_METHODS).borrowed(),
        NativeType::new("CollisionEvent", &COLLISION_EVENT_METHODS).borrowed(),
        NativeType::new("InputSnapshot", &input::INPUT_SNAPSHOT_METHODS).borrowed(),
        NativeType::new("MoveAxes", &input::MOVE_AXES_METHODS).borrowed(),
        NativeType::new("InputMap", &input::INPUT_MAP_METHODS),
        NativeType::new("KeySet", &input::KEY_SET_METHODS),
        NativeType::value("Cooldown", &timer::COOLDOWN_METHODS),
        NativeType::value("TickTimer", &timer::TICK_TIMER_METHODS),
        NativeType::enumeration("Key", Key::VARIANTS),
        NativeType::enumeration("PadButton", PadButton::VARIANTS),
        NativeType::enumeration("PadStick", PadStick::VARIANTS),
        NativeType::enumeration("TouchButton", TouchButton::VARIANTS),
        NativeType::enumeration("InputAction", InputAction::VARIANTS),
        NativeType::enumeration("InputAxis", InputAxis::VARIANTS),
    ],
    traits: &[
        NativeTrait {
            name: "FixedUpdate",
            hooks: &[NativeHook {
                name: "fixed_update",
                params: &[Param {
                    name: "frame",
                    ty: SchemaTy::Handle(FixedFrame::PATH),
                }],
                domain: HookDomain::Simulation,
            }],
        },
        NativeTrait {
            name: "FrameUpdate",
            hooks: &[NativeHook {
                name: "frame_update",
                params: &[Param {
                    name: "frame",
                    ty: SchemaTy::Handle(RenderFrame::PATH),
                }],
                domain: HookDomain::Presentation,
            }],
        },
        NativeTrait {
            name: "CollisionListener",
            hooks: &[NativeHook {
                name: "collision",
                params: &[Param {
                    name: "event",
                    ty: SchemaTy::Handle(CollisionEvent::PATH),
                }],
                domain: HookDomain::Simulation,
            }],
        },
    ],
    derives: &[input::INPUT_ACTION_DERIVE],
    widgets: &[],
};

#[cfg(test)]
mod tests;
