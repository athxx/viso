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
//! Before its first tick the scheduler runs every `Startup` hook once, in
//! system order, as one transaction: a fault in any of them leaves no
//! scheduler, so no tick runs on a half-started game.
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
//!
//! [`quick`] is the low-ceremony surface: one `QuickGame` system whose
//! `start` and `fixed` the scheduler runs as a `Startup` and a `FixedUpdate`.

mod clock;
pub mod input;
pub mod quick;
mod scheduler;
mod snapshot;
mod timer;
mod world;

use std::cell::Cell;

use crate::native::{
    HookDomain, NativeError, NativeFunction, NativeHook, NativeId, NativeLibrary, NativeObject,
    NativeTrait, NativeType, Obj, Param, SchemaTy, Vec3F32,
};

pub use clock::{Clock, TickOverrun};
pub use input::{
    Action, INPUT_ACTION_DERIVE, InputAction, InputAxis, InputBindings, InputMap, InputSchema,
    InputSnapshot, Key, KeySet, MoveAxes, MoveSource, PadButton, PadStick, TouchButton,
};
pub use scheduler::{CommandKey, DEFAULT_SEED, Scheduler, SystemFault};
pub use snapshot::{GameSnapshot, Restored};
pub use timer::{Cooldown, TickTimer};
pub use world::{
    BodyKind, EntityId, Extracted, GAME_TAG_DERIVE, GRAVITY, GameTag, GameWorld, SpawnDesc, Tag,
};

/// The identity of the `FixedUpdate.fixed_update` hook.
pub const FIXED_UPDATE: NativeId = NativeId::of("viso::game::FixedUpdate::fixed_update");
/// The identity of the `FrameUpdate.frame_update` hook.
pub const FRAME_UPDATE: NativeId = NativeId::of("viso::game::FrameUpdate::frame_update");
/// The identity of the `CollisionListener.collision` hook.
pub const COLLISION: NativeId = NativeId::of("viso::game::CollisionListener::collision");
/// The identity of the `Startup.startup` hook.
pub const STARTUP: NativeId = NativeId::of("viso::game::Startup::startup");

/// The start of a game a `Startup` hook runs in, behind a
/// `viso::game::GameStart` handle.
#[derive(Debug)]
pub struct GameStart {
    tick: Cell<u64>,
    world: Obj<GameWorld>,
}

impl NativeObject for GameStart {
    const PATH: &'static str = "viso::game::GameStart";
}

/// The fixed tick a `FixedUpdate` runs in, behind a `viso::game::FixedFrame`
/// handle.
#[derive(Debug)]
pub struct FixedFrame {
    tick: Cell<u64>,
    dt: f64,
    input: Obj<InputSnapshot>,
    world: Obj<GameWorld>,
}

impl NativeObject for FixedFrame {
    const PATH: &'static str = "viso::game::FixedFrame";
}

/// The frame a `FrameUpdate` runs in, behind a `viso::game::RenderFrame`
/// handle. Its world is read-only.
#[derive(Debug)]
pub struct RenderFrame {
    dt: Cell<f64>,
    time: Cell<f64>,
    alpha: Cell<f32>,
    world: Obj<GameWorld>,
}

impl NativeObject for RenderFrame {
    const PATH: &'static str = "viso::game::RenderFrame";
}

/// A contact that began between two entities, behind a
/// `viso::game::CollisionEvent` handle: `first` was allocated before
/// `second`.
#[derive(Debug)]
pub struct CollisionEvent {
    tick: Cell<u64>,
    first: Cell<EntityId>,
    second: Cell<EntityId>,
    world: Obj<GameWorld>,
}

impl NativeObject for CollisionEvent {
    const PATH: &'static str = "viso::game::CollisionEvent";
}

/// A tick as an `I64`; ticks past `I64::MAX` saturate.
fn signed(tick: u64) -> i64 {
    i64::try_from(tick).unwrap_or(i64::MAX)
}

static FIXED_FRAME_METHODS: [NativeFunction; 5] = [
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
    crate::native!(fn "world" |_cx, this: Obj<FixedFrame>| -> Obj<GameWorld> {
        Ok(this.world.clone())
    })
    .deterministic()
    .realtime_safe()
    .property(),
];

static GAME_START_METHODS: [NativeFunction; 3] = [
    crate::native!(fn "tick" |_cx, this: Obj<GameStart>| -> i64 { Ok(signed(this.tick.get())) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "world" |_cx, this: Obj<GameStart>| -> Obj<GameWorld> {
        Ok(this.world.clone())
    })
    .deterministic()
    .realtime_safe()
    .property(),
    crate::native!(action "spawn" |_cx, this: Obj<GameStart>, desc: SpawnDesc| -> EntityId {
        this.world.spawn(desc)
    })
    .reproducible(crate::native::Determinism::CrossPlatform),
];

static RENDER_FRAME_METHODS: [NativeFunction; 5] = [
    crate::native!(fn "dt" |_cx, this: Obj<RenderFrame>| -> f64 { Ok(this.dt.get()) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "time" |_cx, this: Obj<RenderFrame>| -> f64 { Ok(this.time.get()) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "alpha" |_cx, this: Obj<RenderFrame>| -> f32 { Ok(this.alpha.get()) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "world" |_cx, this: Obj<RenderFrame>| -> Obj<GameWorld> {
        Ok(this.world.clone())
    })
    .deterministic()
    .realtime_safe()
    .property(),
    crate::native!(fn "position" |_cx, this: Obj<RenderFrame>, id: EntityId| -> Vec3F32 {
        this.world
            .interpolated(id, this.alpha.get())
            .ok_or_else(|| NativeError::new(format!("entity {id} is not alive")))
    })
    .reproducible(crate::native::Determinism::CrossPlatform),
];

static COLLISION_EVENT_METHODS: [NativeFunction; 5] = [
    crate::native!(fn "tick" |_cx, this: Obj<CollisionEvent>| -> i64 {
        Ok(signed(this.tick.get()))
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "first" |_cx, this: Obj<CollisionEvent>| -> EntityId {
        Ok(this.first.get())
    })
    .deterministic()
    .realtime_safe()
    .property(),
    crate::native!(fn "second" |_cx, this: Obj<CollisionEvent>| -> EntityId {
        Ok(this.second.get())
    })
    .deterministic()
    .realtime_safe()
    .property(),
    crate::native!(fn "other_of" |_cx, this: Obj<CollisionEvent>, id: EntityId| -> Option<EntityId> {
        let (first, second) = (this.first.get(), this.second.get());
        Ok(if id == first {
            Some(second)
        } else if id == second {
            Some(first)
        } else {
            None
        })
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "world" |_cx, this: Obj<CollisionEvent>| -> Obj<GameWorld> {
        Ok(this.world.clone())
    })
    .deterministic()
    .realtime_safe()
    .property(),
];

/// The scheduler traits, the frame handles their hooks receive, the world and
/// the typed input surface. The frame, world and input handles are borrowed:
/// a hook reads them during its call and cannot keep them.
pub(crate) static GAME: NativeLibrary = NativeLibrary {
    path: "viso::game",
    version: 1,
    functions: &[],
    types: &[
        NativeType::new("GameStart", &GAME_START_METHODS).borrowed(),
        NativeType::new("FixedFrame", &FIXED_FRAME_METHODS).borrowed(),
        NativeType::new("RenderFrame", &RENDER_FRAME_METHODS).borrowed(),
        NativeType::new("CollisionEvent", &COLLISION_EVENT_METHODS).borrowed(),
        NativeType::new("GameWorld", &world::GAME_WORLD_METHODS).borrowed(),
        NativeType::value("EntityId", &[]),
        NativeType::value("SpawnDesc", &world::SPAWN_DESC_METHODS),
        NativeType::enumeration("GameTag", GameTag::VARIANTS),
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
            name: "Startup",
            hooks: &[NativeHook {
                name: "startup",
                params: &[Param {
                    name: "cx",
                    ty: SchemaTy::Handle(GameStart::PATH),
                }],
                domain: HookDomain::Simulation,
            }],
        },
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
    derives: &[input::INPUT_ACTION_DERIVE, world::GAME_TAG_DERIVE],
    widgets: &[],
};

#[cfg(test)]
mod tests;
