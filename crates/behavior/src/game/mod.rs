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
//! One tick runs, each phase in system order and committing its world
//! commands before the next: every `FixedUpdate`, every `PrePhysics`, the
//! physics step, the delivery of the contacts that began to every
//! `CollisionListener` (event by event, or listener by listener as the
//! module's `collision_delivery` says), and every `PostPhysics`. Ticks share
//! one instruction and native call budget each; frames share another.
//!
//! The physics step is a [`Physics`] engine's: the built-in [`Kinematic`]
//! unless the host installs another ([`Scheduler::with_physics`]), which must
//! reach the module's determinism tier (`E9104`). An engine that cannot save
//! its state makes every snapshot degraded.
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
//! The systems share one [`GameWorld`]: hooks read its committed state and
//! write commands the scheduler commits in system order, steps it between a
//! tick's `FixedUpdate`s and its `CollisionListener`s, and its seeded random
//! source is the only randomness a Simulation hook has. A snapshot holds it
//! too. A logic-only [`Scheduler::reload`] keeps it running under a new
//! build; [`Scheduler::rebuild_world`] starts the game over and
//! [`Scheduler::rebuild`] starts a new build over in a shadow game, either
//! keeping the running characters by stable key.
//!
//! A dev host keeps the recent past ([`Scheduler::keep_history`]): a ring of
//! snapshots and the input each tick read, so after a logic reload
//! [`Scheduler::replay_from`] runs the last seconds again on the new code.
//!
//! A `@persist` state loads from the host's [`Persist`] store before the
//! start and is written back at tick boundaries, converted across a change
//! of its type; [`Scheduler::suspend`] makes the writes durable.
//!
//! [`quick`] is the low-ceremony surface: one `QuickGame` system whose
//! `start` and `fixed` the scheduler runs as a `Startup` and a `FixedUpdate`.
//!
//! An `AudioProcess` hook fills an [`AudioBlock`] on the audio thread, held
//! by the compiler to realtime rules; an [`AudioHost`] runs it there, not the
//! scheduler. The audio systems trade typed messages with the rest through
//! bounded lock-free queues: [`Scheduler::attach_audio`] lets `send_audio`
//! reach the `AudioCommands` hooks and delivers the audio thread's events to
//! the `AudioListener` hooks each frame.

mod audio;
mod clock;
mod grid;
mod history;
pub mod input;
pub mod kit;
mod net;
mod persist;
mod physics;
pub mod quick;
mod scheduler;
mod snapshot;
mod tape;
mod timer;
mod world;

use std::cell::Cell;

use kit::Kit;

use crate::native::{
    HookDomain, NativeError, NativeFunction, NativeHook, NativeId, NativeLibrary, NativeObject,
    NativeTrait, NativeType, Obj, Param, SchemaTy, Vec3F32,
};

pub use crate::module::CollisionDelivery;
pub use audio::{
    AUDIO_COMMAND, AUDIO_COMMAND_DERIVE, AUDIO_EVENT, AUDIO_EVENT_DERIVE, AUDIO_PROCESS,
    AudioBlock, AudioCommandValue, AudioEventValue, AudioFault, AudioHost, AudioLink, AudioMessage,
    AudioStatus, COMMAND_CAPACITY, EVENT_CAPACITY, MESSAGE_TOKENS, MessageSlot, RealtimeReceiver,
    RealtimeSender, realtime_queue,
};
pub use clock::{Clock, TickOverrun};
pub use history::{DEFAULT_HISTORY_SECONDS, ReplayError, Replayed};
pub use input::{
    Action, INPUT_ACTION_DERIVE, InputAction, InputAxis, InputBindings, InputMap, InputSchema,
    InputSnapshot, Key, KeySet, MoveAxes, MoveSource, PadButton, PadStick, TickInput, TouchButton,
};
pub use net::{Desync, PacketError, RollbackSession, SessionConfig, SessionError, SessionStats};
#[cfg(not(target_family = "wasm"))]
pub use persist::DirStore;
pub use persist::{
    LazyStore, MemoryStore, PERSIST_CAPABILITY, Persist, PersistReport, PersistStore, Persistence,
    SharedStore,
};
pub use physics::{Kinematic, Physics, PhysicsError, PhysicsTier, StepBodies};
pub use scheduler::{CommandKey, DEFAULT_SEED, GameError, Rebuild, Scheduler, SystemFault};
pub use snapshot::{GameSnapshot, Restored};
pub use tape::{InputTape, TapeChange, TapeError, TapeEvent};
pub use timer::{Cooldown, TickTimer};
pub use world::{
    BodyKind, EntityId, Extracted, GAME_TAG_DERIVE, GRAVITY, GameTag, GameWorld, SpawnDesc, Tag,
};

/// The identity of the `FixedUpdate.fixed_update` hook.
pub const FIXED_UPDATE: NativeId = NativeId::of("viso::game::FixedUpdate::fixed_update");
/// The identity of the `FrameUpdate.frame_update` hook.
pub const FRAME_UPDATE: NativeId = NativeId::of("viso::game::FrameUpdate::frame_update");
/// The identity of the `PrePhysics.pre_physics` hook.
pub const PRE_PHYSICS: NativeId = NativeId::of("viso::game::PrePhysics::pre_physics");
/// The identity of the `PostPhysics.post_physics` hook.
pub const POST_PHYSICS: NativeId = NativeId::of("viso::game::PostPhysics::post_physics");
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
    kit: Obj<Kit>,
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
    /// Player 0's input, `players[0]`.
    input: Obj<InputSnapshot>,
    /// Every player's input, [`MAX_PLAYERS`] of them, the first `count` in
    /// play.
    players: Box<[Obj<InputSnapshot>]>,
    count: Cell<u32>,
    world: Obj<GameWorld>,
    kit: Obj<Kit>,
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
    kit: Obj<Kit>,
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
    kit: Obj<Kit>,
}

impl NativeObject for CollisionEvent {
    const PATH: &'static str = "viso::game::CollisionEvent";
}

/// A tick as an `I64`; ticks past `I64::MAX` saturate.
fn signed(tick: u64) -> i64 {
    i64::try_from(tick).unwrap_or(i64::MAX)
}

/// The most players a game takes.
pub const MAX_PLAYERS: u32 = 8;

/// Player `player`'s input of the tick.
fn input_of(frame: &FixedFrame, player: i64) -> Result<Obj<InputSnapshot>, NativeError> {
    let count = frame.count.get();
    match u32::try_from(player).ok().filter(|&p| p < count) {
        Some(p) => Ok(frame.players[p as usize].clone()),
        None => Err(NativeError::new(format!(
            "player {player} is not in the game: its players are 0 to {}",
            count - 1
        ))),
    }
}

static FIXED_FRAME_METHODS: [NativeFunction; 8] = [
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
    crate::native!(fn "players" |_cx, this: Obj<FixedFrame>| -> i64 {
        Ok(i64::from(this.count.get()))
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "input_of" |_cx, this: Obj<FixedFrame>, player: i64| -> Obj<InputSnapshot> {
        input_of(&this, player)
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "world" |_cx, this: Obj<FixedFrame>| -> Obj<GameWorld> {
        Ok(this.world.clone())
    })
    .deterministic()
    .realtime_safe()
    .property(),
    crate::native!(fn "kit" |_cx, this: Obj<FixedFrame>| -> Obj<Kit> { Ok(this.kit.clone()) })
        .deterministic()
        .realtime_safe()
        .property(),
];

static GAME_START_METHODS: [NativeFunction; 4] = [
    crate::native!(fn "tick" |_cx, this: Obj<GameStart>| -> i64 { Ok(signed(this.tick.get())) })
        .deterministic()
        .realtime_safe(),
    crate::native!(fn "world" |_cx, this: Obj<GameStart>| -> Obj<GameWorld> {
        Ok(this.world.clone())
    })
    .deterministic()
    .realtime_safe()
    .property(),
    crate::native!(fn "kit" |_cx, this: Obj<GameStart>| -> Obj<Kit> { Ok(this.kit.clone()) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(action "spawn" |_cx, this: Obj<GameStart>, desc: SpawnDesc| -> EntityId {
        this.world.spawn(desc)
    })
    .reproducible(crate::native::Determinism::CrossPlatform),
];

static RENDER_FRAME_METHODS: [NativeFunction; 6] = [
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
    crate::native!(fn "kit" |_cx, this: Obj<RenderFrame>| -> Obj<Kit> { Ok(this.kit.clone()) })
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

static COLLISION_EVENT_METHODS: [NativeFunction; 6] = [
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
    crate::native!(fn "kit" |_cx, this: Obj<CollisionEvent>| -> Obj<Kit> { Ok(this.kit.clone()) })
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
    functions: &audio::AUDIO_FUNCTIONS,
    types: &[
        NativeType::new("GameStart", &GAME_START_METHODS).borrowed(),
        NativeType::new("FixedFrame", &FIXED_FRAME_METHODS).borrowed(),
        NativeType::new("RenderFrame", &RENDER_FRAME_METHODS).borrowed(),
        NativeType::new("CollisionEvent", &COLLISION_EVENT_METHODS).borrowed(),
        NativeType::new("AudioBlock", &audio::AUDIO_BLOCK_METHODS).borrowed(),
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
            name: "PrePhysics",
            hooks: &[NativeHook {
                name: "pre_physics",
                params: &[Param {
                    name: "frame",
                    ty: SchemaTy::Handle(FixedFrame::PATH),
                }],
                domain: HookDomain::Simulation,
            }],
        },
        NativeTrait {
            name: "PostPhysics",
            hooks: &[NativeHook {
                name: "post_physics",
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
        NativeTrait {
            name: "AudioProcess",
            hooks: &[NativeHook {
                name: "audio_process",
                params: &[Param {
                    name: "block",
                    ty: SchemaTy::Handle(AudioBlock::PATH),
                }],
                domain: HookDomain::Realtime,
            }],
        },
        NativeTrait {
            name: "AudioCommands",
            hooks: &[NativeHook {
                name: "audio_command",
                params: &[Param {
                    name: "command",
                    ty: SchemaTy::AudioCommand,
                }],
                domain: HookDomain::Realtime,
            }],
        },
        NativeTrait {
            name: "AudioListener",
            hooks: &[NativeHook {
                name: "audio_event",
                params: &[Param {
                    name: "event",
                    ty: SchemaTy::AudioEvent,
                }],
                domain: HookDomain::Presentation,
            }],
        },
    ],
    derives: &[
        input::INPUT_ACTION_DERIVE,
        world::GAME_TAG_DERIVE,
        audio::AUDIO_COMMAND_DERIVE,
        audio::AUDIO_EVENT_DERIVE,
    ],
    widgets: &[],
};

#[cfg(test)]
mod tests;
