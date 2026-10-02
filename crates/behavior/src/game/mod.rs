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

mod clock;
mod scheduler;

use std::cell::Cell;

use crate::native::{
    NativeFunction, NativeHook, NativeId, NativeLibrary, NativeObject, NativeTrait, NativeType,
    Obj, Param, SchemaTy,
};

pub use clock::{Clock, TickOverrun};
pub use scheduler::{Scheduler, SystemFault};

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

static FIXED_FRAME_METHODS: [NativeFunction; 3] = [
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
];

static RENDER_FRAME_METHODS: [NativeFunction; 2] = [
    crate::native!(fn "dt" |_cx, this: Obj<RenderFrame>| -> f64 { Ok(this.dt.get()) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "time" |_cx, this: Obj<RenderFrame>| -> f64 { Ok(this.time.get()) })
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

/// The scheduler traits and the frame handles their hooks receive. The
/// handles are borrowed: a hook reads them during its call and cannot keep
/// them.
pub(crate) static GAME: NativeLibrary = NativeLibrary {
    path: "viso::game",
    version: 1,
    functions: &[],
    types: &[
        NativeType::new("FixedFrame", &FIXED_FRAME_METHODS).borrowed(),
        NativeType::new("RenderFrame", &RENDER_FRAME_METHODS).borrowed(),
        NativeType::new("CollisionEvent", &COLLISION_EVENT_METHODS).borrowed(),
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
            }],
        },
    ],
    widgets: &[],
};

#[cfg(test)]
mod tests;
