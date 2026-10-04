//! `viso::game::kit`: the Quick Game Kit, a high-level typed surface over the
//! Game Profile. Terrain and basic models, camera rigs, prefabs, behaviors,
//! vehicles, particles and synthesized sound. It is Native Schema like the
//! rest of `viso::game`, not a parser special case.
//!
//! Every hook context reaches it as `.kit`. Each of its methods belongs to
//! one layer:
//!
//! - Simulation: `terrain`, `wander`, `chase`, `patrol`, `idle`, `drive` are
//!   `action`s that buffer ordinary world commands under the calling system
//!   (spawns, and the steering the step lowers to `walk`), committed and
//!   discarded like hand-written ones; `facing` reads the committed world.
//!   All reproduce on every target.
//! - Presentation: `camera`, `shake`, `burst`, `emit`, `stop_emit`, `sound`,
//!   `sound_at`, `beep`, and the debug draws `debug_line` and `debug_box`.
//!   Called from the Simulation they are deferred commands delivered after
//!   their tick; from a `FrameUpdate` they run at once. They land on the
//!   game's [`Stage`].
//!
//! Models, prefabs, particles, sounds and waves are schema enums, never
//! strings: `SpawnDesc::prefab(Prefab::car)`, `kit.sound(Sfx::coin)`.

pub mod camera;
mod particles;
mod stage;
pub(crate) mod steer;
mod synth;
mod terrain;

use std::cell::RefCell;
use std::rc::Rc;

use super::input::schema_enum;
use super::world::{BodyKind, EntityId, GameWorld, SpawnDesc, Tag};
use crate::native::{
    Determinism, NativeError, NativeFunction, NativeLibrary, NativeObject, NativeType, NativeValue,
    Obj, SchemaTy, Ticks, Vec3F32,
};
use crate::value::Value;

pub use camera::{CameraRig, CameraView, RigKind};
pub use particles::{MAX_EMITTERS, MAX_PARTICLES, Sprite};
pub use stage::{DebugShape, MAX_CUES, Stage};
pub use synth::{Cue, MAX_VOICES, Sound, Synth};
pub use terrain::Terrain;

use steer::{Chase, Control, Patrol, Wander};

/// The most waypoints a patrol route has.
pub const MAX_ROUTE: usize = 256;

schema_enum! {
    /// What a body is drawn as; `auto` draws it by its kind.
    Model = "viso::game::kit::Model" {
        Auto = "auto", Cube = "cube", Capsule = "capsule", Sphere = "sphere",
        Cylinder = "cylinder", Cone = "cone", Wedge = "wedge", Ground = "ground",
        Tree = "tree", Rock = "rock", Barrel = "barrel", Coin = "coin", Gem = "gem",
        Flag = "flag", Car = "car", Truck = "truck", Hero = "hero",
        Villager = "villager", Monster = "monster", Robot = "robot",
    }
}

schema_enum! {
    /// A ready-made body: its kind, size and model.
    Prefab = "viso::game::kit::Prefab" {
        Hero = "hero", Villager = "villager", Monster = "monster", Car = "car",
        Truck = "truck", Wall = "wall", Platform = "platform", Tree = "tree",
        Rock = "rock", Barrel = "barrel", Coin = "coin", Gem = "gem", Goal = "goal",
    }
}

schema_enum! {
    /// A kind of particle.
    Particle = "viso::game::kit::Particle" {
        Spark = "spark", Smoke = "smoke", Dust = "dust", Trail = "trail",
        Fire = "fire", Confetti = "confetti",
    }
}

schema_enum! {
    /// A synthesized sound of the bank.
    Sfx = "viso::game::kit::Sfx" {
        Jump = "jump", Shoot = "shoot", Zap = "zap", Grab = "grab", Angry = "angry",
        Calm = "calm", Rescue = "rescue", Shove = "shove", Board = "board",
        Coin = "coin", Hurt = "hurt", Win = "win", Lose = "lose",
        Powerup = "powerup", Explode = "explode", Click = "click", Step = "step",
        Squeak = "squeak", Roar = "roar", Bark = "bark", Moo = "moo",
        Clank = "clank", Whip = "whip",
    }
}

schema_enum! {
    /// A tone's waveform.
    Wave = "viso::game::kit::Wave" {
        Sine = "sine", Square = "square", Saw = "saw", Triangle = "triangle",
        Noise = "noise",
    }
}

impl Model {
    /// The model `auto` stands for on a body of `kind`.
    pub fn resolve(self, kind: BodyKind) -> Model {
        match (self, kind) {
            (Model::Auto, BodyKind::Character) => Model::Capsule,
            (Model::Auto, _) => Model::Cube,
            (model, _) => model,
        }
    }
}

impl Prefab {
    /// What it spawns, at the origin and without tags.
    pub fn desc(self) -> SpawnDesc {
        let (kind, size, model) = match self {
            Prefab::Hero => (BodyKind::Character, [0.8, 1.8, 0.8], Model::Hero),
            Prefab::Villager => (BodyKind::Character, [0.8, 1.7, 0.8], Model::Villager),
            Prefab::Monster => (BodyKind::Character, [1.2, 1.6, 1.2], Model::Monster),
            Prefab::Car => (BodyKind::Character, [2.0, 1.2, 2.0], Model::Car),
            Prefab::Truck => (BodyKind::Character, [2.6, 2.0, 2.6], Model::Truck),
            Prefab::Wall => (BodyKind::Block, [4.0, 3.0, 0.5], Model::Cube),
            Prefab::Platform => (BodyKind::Block, [4.0, 0.5, 4.0], Model::Cube),
            Prefab::Tree => (BodyKind::Block, [0.6, 4.0, 0.6], Model::Tree),
            Prefab::Rock => (BodyKind::Block, [1.4, 1.0, 1.4], Model::Rock),
            Prefab::Barrel => (BodyKind::Block, [0.8, 1.0, 0.8], Model::Barrel),
            Prefab::Coin => (BodyKind::Sensor, [0.6, 0.6, 0.6], Model::Coin),
            Prefab::Gem => (BodyKind::Sensor, [0.5, 0.5, 0.5], Model::Gem),
            Prefab::Goal => (BodyKind::Sensor, [2.0, 3.0, 2.0], Model::Flag),
        };
        SpawnDesc::new(kind, Vec3F32::from_array(size)).model(model)
    }
}

/// The Kit of a game, behind a `viso::game::kit::Kit` handle: its world and
/// its stage.
#[derive(Debug)]
pub struct Kit {
    world: Obj<GameWorld>,
    stage: Rc<RefCell<Stage>>,
}

impl NativeObject for Kit {
    const PATH: &'static str = "viso::game::kit::Kit";
}

impl Kit {
    /// The Kit over `world` and `stage`.
    pub(crate) fn new(world: &Obj<GameWorld>, stage: &Rc<RefCell<Stage>>) -> Obj<Kit> {
        Obj::new(Kit {
            world: world.clone(),
            stage: stage.clone(),
        })
    }

    fn stage(&self) -> std::cell::RefMut<'_, Stage> {
        self.stage.borrow_mut()
    }
}

/// `v` as a finite, non-negative `F32`, or an error naming `what`.
fn amount(what: &str, v: f64) -> Result<f32, NativeError> {
    let f = v as f32;
    if f.is_finite() && f >= 0.0 {
        Ok(f)
    } else {
        Err(NativeError::new(format!(
            "a {what} is finite and not negative, not {v}"
        )))
    }
}

/// A Simulation method: it reads or writes the world and reproduces on every
/// target.
const fn simulation(f: NativeFunction) -> NativeFunction {
    f.reproducible(Determinism::CrossPlatform)
}

static KIT_METHODS: [NativeFunction; 17] = [
    simulation(
        crate::native!(action "terrain" |_cx, this: Obj<Kit>, terrain: Terrain| -> Vec<EntityId> {
            terrain
                .blocks()?
                .into_iter()
                .map(|desc| this.world.spawn(desc))
                .collect()
        }),
    ),
    simulation(
        crate::native!(action "wander" |_cx, this: Obj<Kit>, id: EntityId, range: f64, speed: f64, pause: Ticks| -> () {
            let wander = Wander {
                home: [0.0; 3],
                range: amount("wander range", range)?,
                speed: amount("speed", speed)?,
                pause: u32::try_from(pause.0).unwrap_or(u32::MAX),
                goal: [0.0; 3],
                wait: 0,
            };
            this.world.control(id, Control::Wander(wander))
        }),
    ),
    simulation(
        crate::native!(action "chase" |_cx, this: Obj<Kit>, id: EntityId, tag: Tag, range: f64, speed: f64| -> () {
            let chase = Chase {
                tags: tag.bit(),
                range: amount("chase range", range)?,
                speed: amount("speed", speed)?,
            };
            this.world.control(id, Control::Chase(chase))
        }),
    ),
    simulation(
        crate::native!(action "patrol" |_cx, this: Obj<Kit>, id: EntityId, route: Vec<Vec3F32>, speed: f64| -> () {
            if route.len() > MAX_ROUTE {
                return Err(NativeError::new(format!(
                    "a patrol route has at most {MAX_ROUTE} waypoints, not {}",
                    route.len()
                )));
            }
            let route: Rc<[[f32; 3]]> = route.iter().map(|p| p.to_array()).collect();
            if route.iter().flatten().any(|c| !c.is_finite()) {
                return Err(NativeError::new("a patrol waypoint is finite"));
            }
            let patrol = Patrol {
                route,
                speed: amount("speed", speed)?,
                next: 0,
            };
            this.world.control(id, Control::Patrol(patrol))
        }),
    ),
    simulation(crate::native!(action "idle" |_cx, this: Obj<Kit>, id: EntityId| -> () {
        this.world.control(id, Control::None)
    })),
    simulation(
        crate::native!(action "drive" |_cx, this: Obj<Kit>, id: EntityId, throttle: f64, steer: f64| -> () {
            this.world.drive(id, throttle as f32, steer as f32)
        }),
    ),
    simulation(crate::native!(fn "facing" |_cx, this: Obj<Kit>, id: EntityId| -> Vec3F32 {
        let [x, z] = this
            .world
            .facing(id)
            .ok_or_else(|| NativeError::new(format!("entity {id} is not alive")))?;
        Ok(Vec3F32::new(x, 0.0, z))
    })),
    crate::native!(action "camera" |_cx, this: Obj<Kit>, rig: CameraRig| -> () {
        this.stage().camera(rig);
        Ok(())
    })
    .presentation(),
    crate::native!(action "shake" |_cx, this: Obj<Kit>, strength: f64, seconds: f64| -> () {
        this.stage().shake(strength as f32, seconds as f32);
        Ok(())
    })
    .presentation(),
    crate::native!(action "burst" |_cx, this: Obj<Kit>, kind: Particle, at: Vec3F32, count: i64| -> () {
        this.stage().burst(kind, at, count);
        Ok(())
    })
    .presentation(),
    crate::native!(action "emit" |_cx, this: Obj<Kit>, id: EntityId, kind: Particle| -> () {
        this.stage().emit(id, kind);
        Ok(())
    })
    .presentation(),
    crate::native!(action "stop_emit" |_cx, this: Obj<Kit>, id: EntityId| -> () {
        this.stage().stop_emit(id);
        Ok(())
    })
    .presentation(),
    crate::native!(action "sound" |_cx, this: Obj<Kit>, sfx: Sfx| -> () {
        this.stage().sound(sfx, None);
        Ok(())
    })
    .presentation(),
    crate::native!(action "sound_at" |_cx, this: Obj<Kit>, sfx: Sfx, at: Vec3F32| -> () {
        this.stage().sound(sfx, Some(at));
        Ok(())
    })
    .presentation(),
    crate::native!(action "beep" |_cx, this: Obj<Kit>, wave: Wave, from_hz: f64, to_hz: f64, seconds: f64| -> () {
        this.stage().tone(wave, from_hz as f32, to_hz as f32, seconds as f32);
        Ok(())
    })
    .presentation(),
    crate::native!(action "debug_line" |_cx, this: Obj<Kit>, from: Vec3F32, to: Vec3F32| -> () {
        this.stage().debug(DebugShape::Line { from, to });
        Ok(())
    })
    .debug_draw(),
    crate::native!(action "debug_box" |_cx, this: Obj<Kit>, at: Vec3F32, size: Vec3F32| -> () {
        let half = |v: f32| v.abs() * 0.5;
        let half_extents = Vec3F32::new(half(size.x), half(size.y), half(size.z));
        this.stage().debug(DebugShape::Box {
            center: at,
            half_extents,
        });
        Ok(())
    })
    .debug_draw(),
];

static CAMERA_RIG_METHODS: [NativeFunction; 10] = [
    crate::native!(fn "third_person" |_cx, target: EntityId| -> CameraRig {
        Ok(CameraRig::third_person(target))
    })
    .deterministic(),
    crate::native!(fn "follow" |_cx, target: EntityId| -> CameraRig {
        Ok(CameraRig::follow(target))
    })
    .deterministic(),
    crate::native!(fn "top_down" |_cx, target: EntityId| -> CameraRig {
        Ok(CameraRig::top_down(target))
    })
    .deterministic(),
    crate::native!(fn "fixed" |_cx, eye: Vec3F32, look: Vec3F32| -> CameraRig {
        Ok(CameraRig::fixed(eye, look))
    })
    .deterministic(),
    crate::native!(fn "distance" |_cx, this: CameraRig, metres: f64| -> CameraRig {
        Ok(CameraRig { distance: amount("camera distance", metres)?, ..this })
    })
    .deterministic(),
    crate::native!(fn "height" |_cx, this: CameraRig, metres: f64| -> CameraRig {
        Ok(CameraRig { height: metres as f32, ..this })
    })
    .deterministic(),
    crate::native!(fn "pitch" |_cx, this: CameraRig, degrees: f64| -> CameraRig {
        Ok(CameraRig { pitch: (degrees as f32).clamp(-90.0, 90.0), ..this })
    })
    .deterministic(),
    crate::native!(fn "yaw" |_cx, this: CameraRig, degrees: f64| -> CameraRig {
        Ok(CameraRig { yaw: degrees as f32, ..this })
    })
    .deterministic(),
    crate::native!(fn "fov" |_cx, this: CameraRig, degrees: f64| -> CameraRig {
        Ok(CameraRig { fov: (degrees as f32).clamp(20.0, 120.0), ..this })
    })
    .deterministic(),
    crate::native!(fn "lag" |_cx, this: CameraRig, seconds: f64| -> CameraRig {
        Ok(CameraRig { lag: amount("camera lag", seconds)?, ..this })
    })
    .deterministic(),
];

static TERRAIN_METHODS: [NativeFunction; 6] = [
    crate::native!(fn "flat" |_cx, size: f64| -> Terrain { Ok(Terrain::flat(size as f32)) })
        .constant(),
    crate::native!(fn "hills" |_cx, size: f64, height: f64| -> Terrain {
        Ok(Terrain::hills(size as f32, height as f32))
    })
    .constant(),
    crate::native!(fn "seed" |_cx, this: Terrain, seed: i64| -> Terrain {
        Ok(this.seed(seed as u64))
    })
    .deterministic(),
    crate::native!(fn "cell" |_cx, this: Terrain, metres: f64| -> Terrain {
        Ok(this.cell(metres as f32))
    })
    .deterministic(),
    crate::native!(fn "step" |_cx, this: Terrain, metres: f64| -> Terrain {
        Ok(this.step(metres as f32))
    })
    .deterministic(),
    crate::native!(fn "feature" |_cx, this: Terrain, metres: f64| -> Terrain {
        Ok(this.feature(metres as f32))
    })
    .deterministic(),
];

/// The Kit handle, its value types and its enums.
pub(crate) static KIT: NativeLibrary = NativeLibrary {
    path: "viso::game::kit",
    version: 1,
    functions: &[],
    types: &[
        NativeType::new("Kit", &KIT_METHODS).borrowed(),
        NativeType::value("CameraRig", &CAMERA_RIG_METHODS),
        NativeType::value("Terrain", &TERRAIN_METHODS),
        NativeType::enumeration("Model", Model::VARIANTS),
        NativeType::enumeration("Prefab", Prefab::VARIANTS),
        NativeType::enumeration("Particle", Particle::VARIANTS),
        NativeType::enumeration("Sfx", Sfx::VARIANTS),
        NativeType::enumeration("Wave", Wave::VARIANTS),
    ],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// The Kit methods of the Simulation layer; every other is Presentation.
pub const SIMULATION_METHODS: &[&str] = &[
    "terrain", "wander", "chase", "patrol", "idle", "drive", "facing",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kit_method_declares_its_layer() {
        for f in &KIT_METHODS {
            let simulation = SIMULATION_METHODS.contains(&f.name);
            assert_eq!(f.presentation, !simulation, "{}", f.name);
            if simulation {
                assert_eq!(f.determinism, Determinism::CrossPlatform, "{}", f.name);
            } else {
                assert_eq!(f.ret, SchemaTy::Unit, "{}", f.name);
            }
            assert!(f.capabilities.is_empty(), "{}", f.name);
        }
        let draws: Vec<_> = KIT_METHODS
            .iter()
            .filter(|f| f.debug_draw)
            .map(|f| f.name)
            .collect();
        assert_eq!(draws, ["debug_line", "debug_box"]);
    }

    #[test]
    fn every_prefab_resolves_a_model() {
        for i in 0..Prefab::VARIANTS.len() {
            let prefab = Prefab::from_index(i as i64).expect("a variant");
            let Value::Agg(desc) = prefab.desc().into_value() else {
                panic!("a spawn description is an aggregate");
            };
            assert_ne!(desc.fields[8], Value::Int(0), "{prefab:?} draws as `auto`");
        }
    }
}
