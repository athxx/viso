//! The fixed-step scheduler over a module's systems.

use std::borrow::Cow;
use std::cell::{Ref, RefCell, RefMut};
use std::mem;
use std::rc::Rc;

use super::audio::{AUDIO_EVENT, AudioLink};
use super::history::{History, ReplayError, Replayed};
use super::input::{Action, InputLatch};
use super::kit::{Kit, Stage};
use super::persist::{PERSIST_CAPABILITY, Persist, PersistReport, Persistence, Stored};
use super::physics::{Kinematic, Physics, PhysicsTier};
use super::quick::{QUICK_FIXED, QUICK_START, QuickFrame, QuickStart};
use super::snapshot::{GameSnapshot, Restored, SystemState, fnv};
use super::tape::{InputTape, Playback, Recorder, TapeError};
use super::world::Bodies;
use super::{
    COLLISION, Clock, CollisionDelivery, CollisionEvent, EntityId, FIXED_UPDATE, FRAME_UPDATE,
    FixedFrame, GameStart, GameWorld, InputSchema, InputSnapshot, Key, POST_PHYSICS, PRE_PHYSICS,
    PadButton, PadStick, RenderFrame, STARTUP, TouchButton,
};
use crate::native::{NativeObject, NativeValue, Obj, Services};
use crate::{Budget, Deferred, Fault, FaultKind, Instance, Module, Value, Vm};

/// What a World Rebuild keeps of the running game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rebuild {
    /// Nothing: the world as the start spawns it.
    #[default]
    Fresh,
    /// The place and motion of each character the start spawns under the
    /// stable key of a running one: its tags and its rank among the
    /// characters with those tags, in allocation order.
    KeepCharacters,
}

/// A fault a system hook raised: its state changes were discarded. A fault
/// creating a system or starting the game leaves no scheduler.
#[derive(Debug, Clone)]
pub struct SystemFault {
    /// The system, an index into [`Module::systems`](crate::Module::systems).
    pub system: usize,
    /// The tick it ran in, the tick the frame ended at for a `FrameUpdate`,
    /// or the tick the game starts at for a `Startup`.
    pub tick: u64,
    /// The stable diagnostic code: `E9102` when the tick's shared budget ran
    /// out, otherwise the fault's own.
    pub code: &'static str,
    /// The fault.
    pub fault: Fault,
}

/// Why a game did not start, reload or rebuild.
#[derive(Debug, Clone)]
pub enum GameError {
    /// A system faulted creating its instance or starting the game.
    Fault(SystemFault),
    /// The physics engine does not reach the module's determinism tier.
    Physics(PhysicsTier),
}

impl GameError {
    /// The stable diagnostic code.
    pub fn code(&self) -> &'static str {
        match self {
            GameError::Fault(fault) => fault.code,
            GameError::Physics(tier) => tier.code(),
        }
    }
}

impl std::fmt::Display for GameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GameError::Fault(fault) => write!(f, "{}: {}", fault.code, fault.fault),
            GameError::Physics(tier) => write!(f, "{}: {tier}", tier.code()),
        }
    }
}

impl std::error::Error for GameError {}

impl From<SystemFault> for GameError {
    fn from(fault: SystemFault) -> GameError {
        GameError::Fault(fault)
    }
}

impl From<PhysicsTier> for GameError {
    fn from(tier: PhysicsTier) -> GameError {
        GameError::Physics(tier)
    }
}

/// The identity of a Presentation command a Simulation hook issued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandKey {
    /// The tick it was issued in.
    pub tick: u64,
    /// The issuing system, an index into
    /// [`Module::systems`](crate::Module::systems).
    pub system: usize,
    /// Its place among that system's commands of that tick.
    pub sequence: u32,
}

#[derive(Debug, Clone, Copy)]
struct Hook {
    system: usize,
    chunk: u32,
    /// Whether it is a `QuickGame` hook, which takes the quick context.
    quick: bool,
}

/// The seed [`Scheduler::new`] gives the world's random source.
pub const DEFAULT_SEED: u64 = 0x5eed_5eed_5eed_5eed;

/// Which budget a phase draws on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Start,
    Tick,
    Frame,
}

/// Runs a module's systems on a fixed-step [`Clock`].
///
/// Each system is one [`Instance`] created when the scheduler is, which then
/// runs every `Startup` hook (a `QuickGame.start` among them) once before the
/// first tick, on one shared budget; a fault there fails the whole start, and
/// the commands the start issued are delivered only once it succeeded,
/// before the first tick's. A `QuickGame.fixed` runs as a `FixedUpdate`.
/// Hooks run in [`Module::systems`](crate::Module::systems) order, which the
/// compiler sorts by `@after`/`@before`. Every hook call is its own transaction: a fault
/// discards that call's state writes, is recorded as a [`SystemFault`], and the
/// next system still runs, except when the shared budget ran out, which skips
/// the rest of the tick (`E9102`) or frame.
///
/// The systems share one [`GameWorld`], which their Simulation hooks reach as
/// `frame.world`, `cx.world` or `event.world` and write through commands. One
/// tick freezes the input, runs every `FixedUpdate` and commits their
/// commands, runs every `PrePhysics` and commits theirs, steps the world with
/// its [`Physics`] engine, delivers the contacts that began (after those the
/// host queued) to every `CollisionListener` in the module's
/// [`CollisionDelivery`] order and commits theirs, then runs every
/// `PostPhysics`, which sees the stepped world, and commits theirs; the
/// start's commands commit before the first tick. A `FrameUpdate` reads the
/// world and cannot write it.
///
/// The host reports device input between frames ([`key`](Self::key),
/// [`pad`](Self::pad), [`stick`](Self::stick), [`touch`](Self::touch)); each
/// tick reads it frozen through `frame.input`, mapped by the module's input
/// schema or the default `InputAction` set.
///
/// A Presentation native a Simulation hook calls does not run then: it is a
/// command keyed `(tick, source system, sequence)`, delivered after its tick
/// in key order unless that tick was delivered before, so a replayed or
/// rolled-back tick ([`rewind_to`](Self::rewind_to)) issues nothing twice. A
/// faulting hook's commands are discarded with its state writes.
///
/// The clock steps the module's compile-time tick rate. A
/// [`snapshot`](Self::snapshot) captures the world, its random state and
/// every system's Simulation states at a tick boundary;
/// [`restore`](Self::restore) puts them back, recomputes what derives from
/// them and keeps `@local` states, and the game then runs tick for tick as if
/// it had never left that tick. [`rebuild_world`](Self::rebuild_world) starts
/// the game over; [`reload`](Self::reload) swaps in a new build and keeps
/// the game running.
pub struct Scheduler {
    vm: Vm,
    /// A hash of the module, identifying the build a snapshot came from.
    build: u64,
    clock: Clock,
    budget: Budget,
    instances: Vec<Instance>,
    hooks: Hooks,
    cx: Contexts,
    input: InputLatch,
    world: Obj<GameWorld>,
    /// What the Kit's Presentation commands produce.
    stage: Rc<RefCell<Stage>>,
    /// The seed a World Rebuild reseeds the world with.
    seed: u64,
    collisions: Vec<(EntityId, EntityId)>,
    delivering: Vec<(EntityId, EntityId)>,
    faults: Vec<SystemFault>,
    commands: Vec<(CommandKey, Deferred)>,
    sequences: Box<[u32]>,
    /// The first tick whose commands have not been delivered.
    delivered: u64,
    delivered_commands: u64,
    replayed_commands: u64,
    playback: Option<Playback>,
    recorder: Option<Recorder>,
    /// The recent past a dev host can replay, when kept.
    history: Option<History>,
    /// The store `@persist` states load from and are written to.
    persist: Option<Persistence>,
    persist_reports: Vec<PersistReport>,
}

impl Scheduler {
    /// A scheduler running `vm`'s module on a clock at its tick rate over an
    /// empty world seeded with [`DEFAULT_SEED`], creating one instance of
    /// every system and starting the game. The `vm`'s budget becomes the
    /// budget the start's hooks share, each tick's, and each frame's: memory
    /// and depth stay per call.
    ///
    /// # Errors
    ///
    /// The fault of a system whose instance could not be created, or of the
    /// first `Startup` hook that faulted: the start's state writes and
    /// commands are discarded with the scheduler.
    pub fn new(vm: Vm) -> Result<Scheduler, SystemFault> {
        Scheduler::with_seed(vm, DEFAULT_SEED)
    }

    /// [`Scheduler::new`] with the world's random source seeded with `seed`.
    ///
    /// # Errors
    ///
    /// As [`Scheduler::new`].
    pub fn with_seed(vm: Vm, seed: u64) -> Result<Scheduler, SystemFault> {
        Scheduler::start(vm, seed, Box::new(Kinematic::default()))
    }

    /// [`Scheduler::with_seed`] with the world stepped by `physics` instead of
    /// the built-in [`Kinematic`] engine.
    ///
    /// # Errors
    ///
    /// [`GameError::Physics`] (`E9104`) when the engine's determinism tier is
    /// below the module's (`[game] determinism`); otherwise as
    /// [`Scheduler::new`].
    pub fn with_physics(
        vm: Vm,
        seed: u64,
        physics: Box<dyn Physics>,
    ) -> Result<Scheduler, GameError> {
        PhysicsTier::check(&*physics, vm.module().determinism())?;
        Ok(Scheduler::start(vm, seed, physics)?)
    }

    fn start(vm: Vm, seed: u64, physics: Box<dyn Physics>) -> Result<Scheduler, SystemFault> {
        let mut scheduler = Scheduler::assemble(vm, seed, physics)?;
        scheduler.load_persisted();
        if let Err(fault) = scheduler.run_start() {
            // A game that never started stores nothing.
            scheduler.persist = None;
            return Err(fault);
        }
        scheduler.deliver_queued(0);
        Ok(scheduler)
    }

    /// A scheduler over `vm` with fresh instances and an empty world seeded
    /// with `seed` and stepped by `physics`, before its start.
    fn assemble(
        mut vm: Vm,
        seed: u64,
        physics: Box<dyn Physics>,
    ) -> Result<Scheduler, SystemFault> {
        let module = vm.module().clone();
        let clock = Clock::at_rate(module.tick_rate());
        let instances = instantiate(&mut vm, clock.tick())?;
        let schema = input_schema(&module);
        let world = Obj::new(GameWorld::with_physics(seed, physics));
        let stage = Rc::default();
        let persist = vm
            .services_mut()
            .remove::<Persist>()
            .map(|p| Persistence::new(p, u64::from(module.tick_rate())));
        Ok(Scheduler {
            budget: vm.budget(),
            build: fnv(&module.encode()),
            vm,
            hooks: Hooks::of(&module),
            cx: Contexts::new(&world, &stage, schema.actions.len(), clock.fixed_dt()),
            clock,
            input: InputLatch::new(&schema),
            world,
            stage,
            seed,
            collisions: Vec::new(),
            delivering: Vec::new(),
            faults: Vec::new(),
            commands: Vec::new(),
            sequences: vec![0; module.systems().len()].into(),
            delivered: 0,
            delivered_commands: 0,
            replayed_commands: 0,
            playback: None,
            recorder: None,
            history: None,
            persist,
            persist_reports: Vec::new(),
            instances,
        })
    }

    /// Rebuilds the world, as a change to the start or the world's
    /// construction needs: every system gets a fresh instance, the world
    /// empties and takes its seed again, the clock returns to tick 0, and the
    /// start runs again; with [`Rebuild::KeepCharacters`] the characters it
    /// spawns take the place and motion of the old ones of their stable key.
    /// One smoke tick then runs on a copy, its commands discarded: the
    /// rebuild commits only when neither it nor the start faulted. Commands of
    /// the new run deliver from tick 0 on. Returns the characters carried.
    ///
    /// # Errors
    ///
    /// The fault of a system whose instance could not be created, of the
    /// first start hook that faulted, or of the smoke tick: the game is left
    /// as it was.
    pub fn rebuild_world(&mut self, keep: Rebuild) -> Result<u32, SystemFault> {
        let instances = instantiate(&mut self.vm, 0)?;
        let instances = mem::replace(&mut self.instances, instances);
        if self.persist.is_some() {
            let module = self.vm.module().clone();
            carry_persisted(
                (&module, &instances),
                &mut self.vm,
                &mut self.instances,
                &mut self.persist_reports,
            );
        }
        let (bodies, rng) = (self.world.bodies(), self.world.rng());
        let physics = self.world.save_physics();
        let (tick, delivered) = (self.clock.tick(), self.delivered);
        let collisions = mem::take(&mut self.collisions);
        // A fresh world: the engine starts afresh too.
        let _ = self.world.restore(Rc::default(), self.seed, None);
        self.clock.rewind(0);
        self.delivered = 0;
        match self.prove_start(&bodies, keep) {
            Ok(carried) => {
                self.restart_tapes();
                self.stage.borrow_mut().reset();
                self.deliver_queued(0);
                Ok(carried)
            }
            Err(fault) => {
                self.instances = instances;
                let _ = self.world.restore(bodies, rng, physics.as_deref());
                self.clock.rewind(tick);
                self.delivered = delivered;
                self.collisions = collisions;
                Err(fault)
            }
        }
    }

    /// Swaps in `vm`'s build with a World Rebuild: the new build starts in a
    /// shadow game, as [`rebuild_world`](Self::rebuild_world) describes, over
    /// this game's clock, input and recorded faults, and replaces this one
    /// only once its start and smoke tick ran clean. Returns the characters
    /// carried.
    ///
    /// # Errors
    ///
    /// As [`rebuild_world`](Self::rebuild_world), and
    /// [`GameError::Physics`] when this game's physics engine does not reach
    /// the new build's determinism tier: this game, its build among it, is
    /// left as it was. The shadow steps with a fresh engine of this game's
    /// kind.
    pub fn rebuild(&mut self, vm: Vm, keep: Rebuild) -> Result<u32, GameError> {
        let physics = self.world.fork_physics();
        PhysicsTier::check(&*physics, vm.module().determinism())?;
        let mut shadow = Scheduler::assemble(vm, self.seed, physics)?;
        // The shadow stores nothing until it replaces this game.
        let incoming = shadow.persist.take();
        if incoming.is_some() || self.persist.is_some() {
            carry_persisted(
                (self.vm.module(), &self.instances),
                &mut shadow.vm,
                &mut shadow.instances,
                &mut self.persist_reports,
            );
        }
        let module = shadow.vm.module().clone();
        shadow.clock = self.clock.clone();
        shadow.clock.set_rate(module.tick_rate());
        shadow.clock.rewind(0);
        shadow.cx = Contexts::new(
            &shadow.world,
            &shadow.stage,
            input_schema(&module).actions.len(),
            shadow.clock.fixed_dt(),
        );
        shadow.input = self.input.clone();
        shadow.input.remap(&input_schema(&module));
        let carried = shadow.prove_start(&self.world.bodies(), keep)?;
        shadow.faults = mem::take(&mut self.faults);
        shadow.playback = self.playback.take();
        shadow.recorder = self.recorder.take();
        shadow.restart_tapes();
        shadow.delivered_commands = self.delivered_commands;
        shadow.replayed_commands = self.replayed_commands;
        shadow.persist = incoming.or_else(|| self.persist.take());
        shadow.persist_reports = mem::take(&mut self.persist_reports);
        shadow.deliver_queued(0);
        if let Some(link) = self.vm.services_mut().remove::<AudioLink>() {
            shadow.vm.services_mut().insert(link);
        }
        *self = shadow;
        Ok(carried)
    }

    /// Runs the start of a rebuilt world at tick 0, carries the characters
    /// of `old` when `keep` says so, and runs one smoke tick on a copy whose
    /// state, input and commands are then put back; the start's commands
    /// stay queued.
    fn prove_start(&mut self, old: &Bodies, keep: Rebuild) -> Result<u32, SystemFault> {
        self.run_start()?;
        let carried = match keep {
            Rebuild::Fresh => 0,
            Rebuild::KeepCharacters => self.world.carry_characters(old),
        };
        let started = self.snapshot();
        let held = mem::take(&mut self.commands);
        let input = self.input.clone();
        let collisions = mem::take(&mut self.collisions);
        let (delivered, replayed) = (self.delivered, self.replayed_commands);
        let faults = self.faults.len();
        let tapes = (self.playback.take(), self.recorder.take());
        let persist = self.persist.take();
        // A tick already delivered drops the commands it issues again.
        self.delivered = u64::MAX;
        self.run_tick();
        (self.playback, self.recorder) = tapes;
        self.persist = persist;
        let fault = self.faults.drain(faults..).next();
        self.restore_states(&started);
        let _ = self.world.restore(
            started.world.clone(),
            started.rng,
            started.physics.as_deref(),
        );
        self.clock.rewind(started.tick);
        self.commands = held;
        self.input = input;
        self.collisions = collisions;
        self.delivered = delivered;
        self.replayed_commands = replayed;
        fault.map_or(Ok(carried), Err)
    }

    /// Reloads the logic: `vm`'s build runs from the next tick on, over the
    /// same world, random state, clock and input. Every Simulation state it
    /// shares with this one by stable identity and schema keeps its value, and
    /// so does every `@local` state whose value holds no closure (a closure
    /// names code of the old build); the rest take their initializers. The
    /// start does not run again. `vm` brings its own budget, hooks, input map
    /// and tick rate. Between frames this is also the swap of a
    /// Presentation-only change, which leaves the Simulation as it was.
    ///
    /// # Errors
    ///
    /// The fault of a system whose instance could not be created, or
    /// [`GameError::Physics`] when the physics engine does not reach the new
    /// build's determinism tier: the game is left as it was.
    pub fn reload(&mut self, mut vm: Vm) -> Result<Restored, GameError> {
        PhysicsTier::check(&*self.world.fork_physics(), vm.module().determinism())?;
        let snapshot = self.snapshot();
        let mut instances = instantiate(&mut vm, self.clock.tick())?;
        let module = vm.module().clone();
        let locals = carry_locals(self.vm.module(), &self.instances, &module, &mut instances);
        if let Some(persist) = vm.services_mut().remove::<Persist>() {
            self.persist = Some(Persistence::new(persist, u64::from(module.tick_rate())));
        }
        if self.persist.is_some() {
            carry_persisted(
                (self.vm.module(), &self.instances),
                &mut vm,
                &mut instances,
                &mut self.persist_reports,
            );
        }
        let schema = input_schema(&module);
        if module.tick_rate() != self.vm.module().tick_rate() {
            self.clock.set_rate(module.tick_rate());
            // The past ran at another rate, so it does not replay.
            if let Some(history) = &mut self.history {
                history.restart(Recorder::new(
                    self.seed,
                    module.tick_rate(),
                    &input_schema(&module).actions,
                ));
            }
        }
        self.cx = Contexts::new(
            &self.world,
            &self.stage,
            schema.actions.len(),
            self.clock.fixed_dt(),
        );
        self.input.remap(&schema);
        self.hooks = Hooks::of(&module);
        self.build = fnv(&module.encode());
        self.budget = vm.budget();
        self.sequences = vec![0; module.systems().len()].into();
        self.instances = instances;
        let audio = self.vm.services_mut().remove::<AudioLink>();
        self.vm = vm;
        if let Some(link) = audio {
            self.vm.services_mut().insert(link);
        }
        self.rebind_tapes();
        Ok(Restored {
            locals,
            ..self.restore_states(&snapshot)
        })
    }

    /// Runs every start hook in system order on one shared budget, then
    /// commits their world commands, leaving the Presentation commands they
    /// issued queued; the first fault ends the start and discards both.
    fn run_start(&mut self) -> Result<(), SystemFault> {
        if self.hooks.start.is_empty() {
            return Ok(());
        }
        let tick = self.clock.tick();
        let mark = self.world.mark();
        self.sequences.fill(0);
        object::<GameStart>(&self.cx.game_start).tick.set(tick);
        let mut left = self.budget;
        self.vm.defer_presentation(true);
        let faults = self.faults.len();
        for i in 0..self.hooks.start.len() {
            let hook = self.hooks.start[i];
            let arg = if hook.quick {
                self.cx.quick_start.clone()
            } else {
                self.cx.game_start.clone()
            };
            if !self.run_hook(hook, arg, &mut left, Phase::Start) || self.faults.len() > faults {
                break;
            }
        }
        self.vm.defer_presentation(false);
        if let Some(fault) = self.faults.drain(faults..).next() {
            self.commands.clear();
            self.world.rollback(mark);
            return Err(fault);
        }
        self.world.commit();
        Ok(())
    }

    /// Replays `tape` from the next tick on: each tick reads the input the
    /// tape gives it, its actions mapped to the build's by name, and device
    /// input moves nothing. Changes of ticks already run are skipped, so a
    /// tape replays exactly from tick 0 on a game started with its seed.
    ///
    /// # Errors
    ///
    /// When the tape names an action the build lacks, or steps another tick
    /// rate: the game reads its devices as before.
    pub fn play(&mut self, tape: InputTape) -> Result<(), TapeError> {
        let module = self.vm.module().clone();
        if tape.tick_rate != module.tick_rate() {
            return Err(TapeError {
                line: None,
                message: format!(
                    "the tape steps {} ticks a second, the build {}",
                    tape.tick_rate,
                    module.tick_rate()
                ),
            });
        }
        let mut playback = Playback::new(tape, &input_schema(&module).actions)?;
        if let Some(last) = self.clock.tick().checked_sub(1) {
            playback.changes(last);
        }
        for change in playback.current() {
            self.input.apply(change);
        }
        self.playback = Some(playback);
        Ok(())
    }

    /// The ticks the replayed tape covers, if one plays.
    pub fn tape_ticks(&self) -> Option<u64> {
        self.playback.as_ref().map(Playback::ticks)
    }

    /// Starts recording the input every tick reads into a tape, replacing
    /// one being recorded. A tape recorded from tick 0 replays the run.
    pub fn record(&mut self) {
        let module = self.vm.module();
        self.recorder = Some(Recorder::new(
            self.seed,
            module.tick_rate(),
            &input_schema(module).actions,
        ));
    }

    /// Stops recording and returns the tape, of this build and seed, through
    /// the last tick run; `None` when not recording.
    pub fn stop_recording(&mut self) -> Option<InputTape> {
        Some(self.recorder.take()?.finish(self.build))
    }

    /// The hash identifying the running build, as a snapshot and a tape
    /// record it.
    pub fn build(&self) -> u64 {
        self.build
    }

    /// Maps a replayed or recorded tape onto the running build's actions,
    /// and puts the latch where a replayed tape has the input.
    fn rebind_tapes(&mut self) {
        let module = self.vm.module().clone();
        let actions = &input_schema(&module).actions;
        if let Some(recorder) = &mut self.recorder {
            recorder.rebind(actions);
        }
        if let Some(history) = &mut self.history {
            history.input_mut().rebind(actions);
        }
        if let Some(playback) = &mut self.playback {
            playback.rebind(actions);
            for change in playback.current() {
                self.input.apply(change);
            }
        }
    }

    /// Starts a replayed tape over from tick 0 and a recording afresh, as a
    /// rebuilt world starts a new run.
    fn restart_tapes(&mut self) {
        let module = self.vm.module().clone();
        let schema = input_schema(&module);
        if self.recorder.is_some() {
            self.recorder = Some(Recorder::new(
                self.seed,
                module.tick_rate(),
                &schema.actions,
            ));
        }
        if let Some(history) = &mut self.history {
            history.restart(Recorder::new(
                self.seed,
                module.tick_rate(),
                &schema.actions,
            ));
        }
        if let Some(playback) = &mut self.playback {
            playback.restart(&schema.actions);
            self.input.remap(&schema);
            for change in playback.current() {
                self.input.apply(change);
            }
        }
    }

    /// The clock.
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// The clock, to pause, scale, cap catch-up or change its overrun policy.
    pub fn clock_mut(&mut self) -> &mut Clock {
        &mut self.clock
    }

    /// The world, to extract what the frame draws.
    pub fn world(&self) -> &GameWorld {
        &self.world
    }

    /// What the Kit's Presentation commands produced: the camera's view,
    /// the particles, the sound cues and the debug shapes.
    pub fn stage(&self) -> Ref<'_, Stage> {
        self.stage.borrow()
    }

    /// The stage, to drain its sound cues.
    pub fn stage_mut(&mut self) -> RefMut<'_, Stage> {
        self.stage.borrow_mut()
    }

    /// The interpreter.
    pub fn vm(&self) -> &Vm {
        &self.vm
    }

    /// The host services the systems' natives use.
    pub fn services_mut(&mut self) -> &mut Services {
        self.vm.services_mut()
    }

    /// The budget each tick's hooks share, and each frame's.
    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// Replaces the shared budget.
    pub fn set_budget(&mut self, budget: Budget) {
        self.budget = budget;
    }

    /// The instance of system `system`.
    ///
    /// # Panics
    ///
    /// If `system` is out of range.
    pub fn instance(&self, system: usize) -> &Instance {
        &self.instances[system]
    }

    /// The faults recorded since the last [`take_faults`](Self::take_faults).
    pub fn faults(&self) -> &[SystemFault] {
        &self.faults
    }

    /// Takes the recorded faults.
    pub fn take_faults(&mut self) -> Vec<SystemFault> {
        mem::take(&mut self.faults)
    }

    /// Reports key `key` going down or up.
    pub fn key(&mut self, key: Key, down: bool) {
        self.input.key(key, down);
    }

    /// Reports gamepad button `button` going down or up.
    pub fn pad(&mut self, button: PadButton, down: bool) {
        self.input.pad(button, down);
    }

    /// Reports the position of gamepad stick `stick`, each axis in `[-1, 1]`
    /// with `y` up.
    pub fn stick(&mut self, stick: PadStick, x: f64, y: f64) {
        self.input.stick(stick, x, y);
    }

    /// Reports touch button `button` going down or up.
    pub fn touch(&mut self, button: TouchButton, down: bool) {
        self.input.touch(button, down);
    }

    /// Releases every key, button and stick, as when the game view loses
    /// input focus: each held action is released on the next tick.
    pub fn release_input(&mut self) {
        self.input.release_all();
    }

    /// Keeps the last `seconds` of the game, as a dev host does
    /// ([`DEFAULT_HISTORY_SECONDS`]): a snapshot at every quarter second's
    /// tick boundary and the input every tick reads, from the next tick on,
    /// for [`replay_from`](Self::replay_from). Replaces a history kept
    /// before.
    pub fn keep_history(&mut self, seconds: u32) {
        let module = self.vm.module();
        let rate = u64::from(module.tick_rate());
        let input = Recorder::new(self.seed, module.tick_rate(), &input_schema(module).actions);
        self.history = Some(History::new(u64::from(seconds) * rate, rate / 4, input));
    }

    /// Stops keeping a history and frees it.
    pub fn drop_history(&mut self) {
        self.history = None;
    }

    /// The earliest tick [`replay_from`](Self::replay_from) can go back to
    /// and the snapshots kept; `None` without a history or before its first
    /// snapshot.
    pub fn history(&self) -> Option<(u64, usize)> {
        let history = self.history.as_ref()?;
        Some((history.oldest()?, history.len()))
    }

    /// Goes back to the latest snapshot kept at or before `tick` and runs the
    /// ticks since again, each reading the input it read before, up to the
    /// tick the game was at: after a logic reload this shows what the new
    /// code makes of the last seconds of play. A replayed tape keeps feeding
    /// the ticks; otherwise the recorded input does, and device input
    /// reported meanwhile is kept for the next tick. Presentation commands of
    /// ticks already delivered are not delivered again, and `@local` states
    /// keep their values.
    ///
    /// # Errors
    ///
    /// Without a history, for a tick before its oldest snapshot, or for a
    /// tick not run yet: the game is left as it was.
    pub fn replay_from(&mut self, tick: u64) -> Result<Replayed, ReplayError> {
        let now = self.clock.tick();
        let history = self.history.as_ref().ok_or(ReplayError::NoHistory)?;
        if tick > now {
            return Err(ReplayError::Ahead);
        }
        let snapshot = history
            .before(tick)
            .ok_or(ReplayError::Forgotten {
                oldest: history.oldest(),
            })?
            .clone();
        let devices = self.input.clone();
        let fed = self.playback.is_none();
        if fed {
            let module = self.vm.module().clone();
            let actions = &input_schema(&module).actions;
            self.playback = Some(Playback::lenient(history.tape(self.build), actions));
        }
        let restored = self.restore(&snapshot);
        let ticks = now - snapshot.tick;
        for _ in 0..ticks {
            self.run_tick();
        }
        if fed {
            self.playback = None;
            self.input = devices;
        }
        Ok(Replayed {
            from: snapshot.tick,
            ticks,
            restored,
        })
    }

    /// Rewinds the clock to `tick`, to replay or roll back from there; the
    /// caller restores the systems' state. Commands of ticks already
    /// delivered are not delivered again; a replayed tape resumes at `tick`
    /// and a recording forgets the ticks from it on.
    pub fn rewind_to(&mut self, tick: u64) {
        self.clock.rewind(tick);
        self.seek_tapes(tick);
    }

    /// Puts a replayed tape and the input it feeds where they are before
    /// tick `tick`, and cuts a recording back to it.
    fn seek_tapes(&mut self, tick: u64) {
        if let Some(recorder) = &mut self.recorder {
            recorder.rewind(tick);
        }
        if let Some(history) = &mut self.history {
            history.rewind(tick);
        }
        if let Some(playback) = &mut self.playback {
            let module = self.vm.module().clone();
            let schema = input_schema(&module);
            playback.seek(tick, &schema.actions);
            self.input.remap(&schema);
            for change in playback.current() {
                self.input.apply(change);
            }
        }
    }

    /// The world, its random state and the Simulation state of every system
    /// now, at a tick boundary.
    pub fn snapshot(&self) -> GameSnapshot {
        let module = self.vm.module();
        let mut systems: Vec<SystemState> = module
            .systems()
            .iter()
            .zip(&self.instances)
            .map(|(system, instance)| SystemState {
                id: system.id,
                states: system
                    .snapshot
                    .iter()
                    .map(|s| (s.id, s.schema, instance.states()[s.slot as usize].clone()))
                    .collect(),
            })
            .collect();
        systems.sort_by_key(|s| s.id);
        GameSnapshot {
            build: self.build,
            tick: self.clock.tick(),
            rng: self.world.rng(),
            world: self.world.bodies(),
            physics: self.world.save_physics(),
            systems: systems.into(),
        }
    }

    /// Restores `snapshot`: the clock goes to its tick, a replayed tape and a
    /// recording with it, the world and its
    /// random state become the snapshot's, and every Simulation state it
    /// holds under the same identity and schema takes its value;
    /// computeds reading them recompute and `@local` states keep theirs.
    /// Commands of ticks already delivered are not delivered again when they
    /// rerun. A snapshot of another build restores the states it shares with
    /// this one. A degraded snapshot, or engine state that does not load,
    /// starts the physics engine afresh over the restored bodies and says so
    /// in [`Restored::degraded`].
    pub fn restore(&mut self, snapshot: &GameSnapshot) -> Restored {
        let mut restored = self.restore_states(snapshot);
        let loaded = self.world.restore(
            snapshot.world.clone(),
            snapshot.rng,
            snapshot.physics.as_deref(),
        );
        restored.degraded = snapshot.physics.is_none() || loaded.is_err();
        self.rewind_to(snapshot.tick);
        restored
    }

    /// Sets every Simulation state `snapshot` holds under the same identity
    /// and schema.
    fn restore_states(&mut self, snapshot: &GameSnapshot) -> Restored {
        let mut restored = Restored::default();
        let module = self.vm.module().clone();
        for (system, instance) in module.systems().iter().zip(&mut self.instances) {
            let saved = snapshot
                .systems
                .binary_search_by_key(&system.id, |s| s.id)
                .ok()
                .map(|i| &snapshot.systems[i].states[..]);
            for state in system.snapshot.iter() {
                let found = saved.and_then(|states| {
                    let i = states.binary_search_by_key(&state.id, |s| s.0).ok()?;
                    Some(&states[i])
                });
                match found {
                    Some((_, schema, value)) if *schema == state.schema => {
                        instance.set_state(state.slot as usize, value.clone());
                        restored.states += 1;
                    }
                    Some(_) => restored.mismatched += 1,
                    None => restored.missing += 1,
                }
            }
        }
        restored
    }

    /// Presentation commands delivered so far.
    pub fn delivered_commands(&self) -> u64 {
        self.delivered_commands
    }

    /// Presentation commands a replayed tick issued again and that were not
    /// delivered.
    pub fn replayed_commands(&self) -> u64 {
        self.replayed_commands
    }

    /// Queues a contact between `first` and `second` that a host-side
    /// simulation found, for the `CollisionListener`s of the next tick, ahead
    /// of the world's own.
    pub fn push_collision(&mut self, first: EntityId, second: EntityId) {
        self.collisions.push((first, second));
    }

    /// Runs one frame of `wall_dt` seconds: every tick the clock owes, then
    /// every `FrameUpdate`, which runs while paused too. Returns the ticks
    /// run.
    pub fn frame(&mut self, wall_dt: f64) -> u32 {
        let ticks = self.clock.advance(wall_dt);
        for _ in 0..ticks {
            self.run_tick();
        }
        let frame = object::<RenderFrame>(&self.cx.render_frame);
        frame.dt.set(if wall_dt.is_finite() && wall_dt > 0.0 {
            wall_dt
        } else {
            0.0
        });
        frame.time.set(self.clock.time());
        frame.alpha.set(self.clock.alpha());
        let (dt, alpha) = (frame.dt.get() as f32, frame.alpha.get());
        self.stage.borrow_mut().begin_frame();
        let mut left = self.budget;
        if !self.deliver_audio_events(&mut left) {
            self.stage.borrow_mut().end_frame(dt, &self.world, alpha);
            return ticks;
        }
        for i in 0..self.hooks.frame.len() {
            let arg = self.cx.render_frame.clone();
            if !self.run_hook(self.hooks.frame[i], arg, &mut left, Phase::Frame) {
                break;
            }
        }
        self.stage.borrow_mut().end_frame(dt, &self.world, alpha);
        ticks
    }

    /// Lets the game's systems talk to the audio systems `link` reaches:
    /// `send_audio` queues for them, and each frame hands the events they
    /// sent to every `AudioListener`, before the `FrameUpdate`s. Replaces
    /// the link attached before; a World Rebuild and a Logic Reload keep it.
    pub fn attach_audio(&mut self, link: AudioLink) {
        self.vm.services_mut().insert(link);
    }

    /// The link [`attach_audio`](Self::attach_audio) attached.
    pub fn audio(&mut self) -> Option<&AudioLink> {
        self.vm
            .services_mut()
            .get_mut::<AudioLink>()
            .map(|link| &*link)
    }

    /// Hands every event the audio thread sent to the `AudioListener`s, in
    /// the order sent and then system order; false when the frame's budget
    /// ran out.
    fn deliver_audio_events(&mut self, left: &mut Budget) -> bool {
        if self.hooks.listen.is_empty() {
            return true;
        }
        loop {
            let event = match self.vm.services_mut().get_mut::<AudioLink>() {
                Some(link) => link.receive(),
                None => None,
            };
            let Some(event) = event else {
                return true;
            };
            for i in 0..self.hooks.listen.len() {
                if !self.run_hook(self.hooks.listen[i], Value::Int(event), left, Phase::Frame) {
                    return false;
                }
            }
        }
    }

    /// Runs `ticks` ticks now, paused or not, on the path frames run them on.
    pub fn step(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.run_tick();
        }
    }

    fn run_tick(&mut self) {
        let tick = self.clock.tick();
        if self.history.as_ref().is_some_and(|h| h.due(tick)) {
            let snapshot = self.snapshot();
            if let Some(history) = &mut self.history {
                history.push(snapshot);
            }
        }
        let frame = object::<FixedFrame>(&self.cx.fixed_frame);
        frame.tick.set(tick);
        if let Some(playback) = &mut self.playback {
            for change in playback.changes(tick) {
                self.input.apply(change);
            }
        }
        self.input.deliver(&frame.input);
        if let Some(recorder) = &mut self.recorder {
            let input = &frame.input;
            let bits = |a| {
                let a = Action(a);
                (input.held(a), input.pressed(a), input.released(a))
            };
            recorder.observe(tick, bits, input.move_axes());
        }
        if let Some(history) = &mut self.history {
            let input = &frame.input;
            let bits = |a| {
                let a = Action(a);
                (input.held(a), input.pressed(a), input.released(a))
            };
            history.input_mut().observe(tick, bits, input.move_axes());
        }
        self.world.begin_tick();
        mem::swap(&mut self.collisions, &mut self.delivering);
        let mut left = self.budget;
        self.sequences.fill(0);
        self.vm.defer_presentation(true);
        let mut running = true;
        for i in 0..self.hooks.fixed.len() {
            let hook = self.hooks.fixed[i];
            let arg = if hook.quick {
                self.cx.quick_frame.clone()
            } else {
                self.cx.fixed_frame.clone()
            };
            if !self.run_hook(hook, arg, &mut left, Phase::Tick) {
                running = false;
                break;
            }
        }
        self.world.commit();
        if running && !self.hooks.pre.is_empty() {
            running = self.run_phase(Around::Pre, &mut left);
            self.world.commit();
        }
        self.world
            .step(self.clock.fixed_dt() as f32, &mut self.delivering);
        if running && !self.hooks.collision.is_empty() {
            running = self.deliver_collisions(tick, &mut left);
            self.world.commit();
        }
        if running && !self.hooks.post.is_empty() {
            self.run_phase(Around::Post, &mut left);
            self.world.commit();
        }
        self.vm.defer_presentation(false);
        self.delivering.clear();
        self.deliver_commands(tick);
        self.clock.finish_tick();
        self.write_persisted(false);
    }

    /// Runs the `PrePhysics` or `PostPhysics` hooks in system order; false
    /// when the tick's budget ran out.
    fn run_phase(&mut self, around: Around, left: &mut Budget) -> bool {
        let count = match around {
            Around::Pre => self.hooks.pre.len(),
            Around::Post => self.hooks.post.len(),
        };
        for i in 0..count {
            let hook = match around {
                Around::Pre => self.hooks.pre[i],
                Around::Post => self.hooks.post[i],
            };
            let arg = self.cx.fixed_frame.clone();
            if !self.run_hook(hook, arg, left, Phase::Tick) {
                return false;
            }
        }
        true
    }

    /// Hands each contact of the tick to every `CollisionListener`, in the
    /// module's [`CollisionDelivery`] order; false when the tick's budget ran
    /// out.
    fn deliver_collisions(&mut self, tick: u64, left: &mut Budget) -> bool {
        let (events, listeners) = (self.delivering.len(), self.hooks.collision.len());
        let major = self.vm.module().collision_delivery();
        let (outer, inner) = match major {
            CollisionDelivery::EventMajor => (events, listeners),
            CollisionDelivery::ListenerMajor => (listeners, events),
        };
        for o in 0..outer {
            for i in 0..inner {
                let (e, l) = match major {
                    CollisionDelivery::EventMajor => (o, i),
                    CollisionDelivery::ListenerMajor => (i, o),
                };
                let (first, second) = self.delivering[e];
                let event = object::<CollisionEvent>(&self.cx.collision_event);
                event.tick.set(tick);
                event.first.set(first);
                event.second.set(second);
                let arg = self.cx.collision_event.clone();
                if !self.run_hook(self.hooks.collision[l], arg, left, Phase::Tick) {
                    return false;
                }
            }
        }
        true
    }

    /// Loads every `@persist` state from the store before the start: a
    /// stored value of another type converts into the state's, and one that
    /// does not load leaves the initializer's value and a report.
    fn load_persisted(&mut self) {
        let module = self.vm.module().clone();
        let Some(persist) = &mut self.persist else {
            return;
        };
        let slots = || module.systems().iter().flat_map(|s| s.persist.iter());
        if slots().next().is_none() {
            return;
        }
        if !self.vm.granted(PERSIST_CAPABILITY) {
            self.persist_reports
                .extend(slots().map(|slot| PersistReport {
                    key: slot.key.clone(),
                    code: "E6103",
                    message: format!(
                        "the game is not granted `{PERSIST_CAPABILITY}`: `{}` starts from its \
                     initializer and is not stored",
                        slot.key
                    ),
                }));
            self.persist = None;
            return;
        }
        self.vm.set_budget(self.budget);
        for (system, instance) in module.systems().iter().zip(&mut self.instances) {
            for slot in system.persist.iter() {
                let loaded = persist.load(slot, &module, &mut self.vm, instance);
                let at = slot.slot as usize;
                match loaded {
                    Ok(Some(value)) => instance.set_state(at, value),
                    Ok(None) => {}
                    Err(message) => self.persist_reports.push(PersistReport {
                        key: slot.key.clone(),
                        code: "E9111",
                        message: format!(
                            "`{}` did not load, so it starts from its initializer: {message}",
                            slot.key
                        ),
                    }),
                }
                persist.loaded(slot, instance.states()[at].clone());
            }
        }
    }

    /// Stores each `@persist` state whose value changed since it was last
    /// stored, once the interval since the last write is over or `now`.
    fn write_persisted(&mut self, now: bool) {
        let Some(persist) = &mut self.persist else {
            return;
        };
        if !now && persist.wait > 0 {
            persist.wait -= 1;
            return;
        }
        persist.wait = persist.interval.saturating_sub(1);
        if !self.vm.granted(PERSIST_CAPABILITY) {
            return;
        }
        for (system, instance) in self.vm.module().systems().iter().zip(&self.instances) {
            for slot in system.persist.iter() {
                persist.store_changed(slot, &instance.states()[slot.slot as usize]);
            }
        }
    }

    /// Stores every `@persist` state that changed and blocks until the
    /// store has made them durable, as an app going to the background
    /// must; dropping the scheduler does too.
    pub fn suspend(&mut self) {
        self.write_persisted(true);
        if let Some(persist) = &mut self.persist
            && let Err(message) = persist.store.flush()
        {
            self.persist_reports.push(PersistReport {
                key: "".into(),
                code: "E9111",
                message: format!("persisted state was not stored: {message}"),
            });
        }
    }

    /// Writes changed `@persist` states every `ticks` ticks at most; one
    /// second's worth by default.
    pub fn set_persist_interval(&mut self, ticks: u64) {
        if let Some(persist) = &mut self.persist {
            persist.interval = ticks.max(1);
            persist.wait = persist.wait.min(persist.interval - 1);
        }
    }

    /// The persisted states that did not load or carry, and the writes that
    /// failed, since the last call.
    pub fn take_persist_reports(&mut self) -> Vec<PersistReport> {
        mem::take(&mut self.persist_reports)
    }

    /// Delivers the commands `tick` issued in key order, unless an earlier
    /// run of `tick` delivered them.
    fn deliver_commands(&mut self, tick: u64) {
        if tick < self.delivered {
            self.replayed_commands += self.commands.len() as u64;
            self.commands.clear();
            return;
        }
        self.delivered = tick + 1;
        self.stage.borrow_mut().begin_tick();
        self.deliver_queued(tick);
    }

    /// Delivers the queued commands, issued in `tick` or the start before
    /// it, in key order.
    fn deliver_queued(&mut self, tick: u64) {
        let mut commands = mem::take(&mut self.commands);
        // Hooks issue in run order, which the key order follows except across
        // collision events; a stable sort keeps each system's sequence.
        commands.sort_by_key(|(key, _)| *key);
        for (key, command) in commands.drain(..) {
            self.delivered_commands += 1;
            if let Err(fault) = self.vm.deliver(&command) {
                self.faults.push(SystemFault {
                    system: key.system,
                    tick,
                    code: fault.kind.code(),
                    fault,
                });
            }
        }
        self.commands = commands;
    }

    /// Runs one hook on what is left of the shared budget; false when the
    /// budget ran out.
    fn run_hook(&mut self, hook: Hook, arg: Value, left: &mut Budget, phase: Phase) -> bool {
        self.vm.set_budget(*left);
        let mark = self.world.mark();
        if phase != Phase::Frame {
            self.world.open(hook.system);
        }
        let result = self
            .vm
            .call(&mut self.instances[hook.system], hook.chunk, &[arg]);
        self.world.close();
        let cost = self.vm.cost();
        left.instructions = left.instructions.saturating_sub(cost.instructions);
        left.native_calls = left.native_calls.saturating_sub(cost.native_calls);
        if phase != Phase::Frame {
            let tick = self.clock.tick();
            let sequence = &mut self.sequences[hook.system];
            for command in self.vm.deferred_mut().drain(..) {
                let key = CommandKey {
                    tick,
                    system: hook.system,
                    sequence: *sequence,
                };
                *sequence += 1;
                self.commands.push((key, command));
            }
        }
        let Err(fault) = result else {
            return true;
        };
        self.world.rollback(mark);
        let exhausted = matches!(
            fault.kind,
            FaultKind::InstructionBudget | FaultKind::NativeCallBudget
        );
        let code = match phase {
            Phase::Tick if exhausted => "E9102",
            _ => fault.kind.code(),
        };
        self.faults.push(SystemFault {
            system: hook.system,
            tick: self.clock.tick(),
            code,
            fault,
        });
        !exhausted
    }
}

/// The physics phase a tick runs hooks around the step in.
#[derive(Debug, Clone, Copy)]
enum Around {
    Pre,
    Post,
}

/// A module's hooks by phase, each in system order.
struct Hooks {
    start: Box<[Hook]>,
    fixed: Box<[Hook]>,
    pre: Box<[Hook]>,
    post: Box<[Hook]>,
    frame: Box<[Hook]>,
    collision: Box<[Hook]>,
    /// The `AudioListener` hooks, run each frame for each audio event.
    listen: Box<[Hook]>,
}

impl Hooks {
    fn of(module: &Module) -> Hooks {
        let (mut start, mut fixed) = (Vec::new(), Vec::new());
        let (mut frame, mut collision) = (Vec::new(), Vec::new());
        let mut listen = Vec::new();
        let (mut pre, mut post) = (Vec::new(), Vec::new());
        for (index, system) in module.systems().iter().enumerate() {
            let bind = |hooks: &mut Vec<Hook>, id, quick| {
                if let Some(chunk) = system.hook(id) {
                    hooks.push(Hook {
                        system: index,
                        chunk,
                        quick,
                    });
                }
            };
            bind(&mut start, STARTUP, false);
            bind(&mut start, QUICK_START, true);
            bind(&mut fixed, FIXED_UPDATE, false);
            bind(&mut fixed, QUICK_FIXED, true);
            bind(&mut pre, PRE_PHYSICS, false);
            bind(&mut post, POST_PHYSICS, false);
            bind(&mut frame, FRAME_UPDATE, false);
            bind(&mut collision, COLLISION, false);
            bind(&mut listen, AUDIO_EVENT, false);
        }
        Hooks {
            start: start.into(),
            fixed: fixed.into(),
            pre: pre.into(),
            post: post.into(),
            frame: frame.into(),
            collision: collision.into(),
            listen: listen.into(),
        }
    }
}

/// The handles hooks receive, over one world.
struct Contexts {
    game_start: Value,
    quick_start: Value,
    fixed_frame: Value,
    quick_frame: Value,
    render_frame: Value,
    collision_event: Value,
}

impl Contexts {
    /// Contexts over `world` for an input map of `actions` actions and a
    /// fixed step of `fixed_dt` seconds.
    fn new(
        world: &Obj<GameWorld>,
        stage: &Rc<RefCell<Stage>>,
        actions: usize,
        fixed_dt: f64,
    ) -> Contexts {
        let kit = Kit::new(world, stage);
        let game_start = Obj::new(GameStart {
            tick: 0.into(),
            world: world.clone(),
            kit: kit.clone(),
        });
        let fixed_frame = Obj::new(FixedFrame {
            tick: 0.into(),
            dt: fixed_dt,
            input: Obj::new(InputSnapshot::new(actions)),
            world: world.clone(),
            kit: kit.clone(),
        });
        Contexts {
            quick_start: handle(QuickStart {
                start: game_start.clone(),
            }),
            game_start: game_start.into_value(),
            quick_frame: handle(QuickFrame {
                frame: fixed_frame.clone(),
            }),
            fixed_frame: fixed_frame.into_value(),
            render_frame: handle(RenderFrame {
                dt: 0.0.into(),
                time: 0.0.into(),
                alpha: 0.0.into(),
                world: world.clone(),
                kit: kit.clone(),
            }),
            collision_event: handle(CollisionEvent {
                tick: 0.into(),
                first: Default::default(),
                second: Default::default(),
                world: world.clone(),
                kit,
            }),
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        self.suspend();
    }
}

/// Sets each `@persist` state of the instances `to` of `vm`'s build to the
/// value of the state the build `old` persisted under the same key in
/// `from`, converted into its type, reporting each that does not convert.
fn carry_persisted(
    (old, from): (&Module, &[Instance]),
    vm: &mut Vm,
    to: &mut [Instance],
    reports: &mut Vec<PersistReport>,
) {
    let new = vm.module().clone();
    for (system, instance) in new.systems().iter().zip(to) {
        for slot in system.persist.iter() {
            let found = old.systems().iter().zip(from).find_map(|(s, instance)| {
                let before = s.persist.iter().find(|p| p.key == slot.key)?;
                Some((before, instance.states()[before.slot as usize].clone()))
            });
            let Some((before, value)) = found else {
                continue;
            };
            match Stored::of(before, value).into_slot(slot, &new, vm, instance) {
                Ok(value) => instance.set_state(slot.slot as usize, value),
                Err(message) => reports.push(PersistReport {
                    key: slot.key.clone(),
                    code: "E9111",
                    message: format!(
                        "`{}` did not carry into the new build, so it starts from its \
                         initializer: {message}",
                        slot.key
                    ),
                }),
            }
        }
    }
}

/// One instance of every system of `vm`'s module, at `tick`.
fn instantiate(vm: &mut Vm, tick: u64) -> Result<Vec<Instance>, SystemFault> {
    let module = vm.module().clone();
    module
        .systems()
        .iter()
        .enumerate()
        .map(|(index, system)| {
            vm.instantiate(system.component, [])
                .map_err(|fault| SystemFault {
                    system: index,
                    tick,
                    code: fault.kind.code(),
                    fault,
                })
        })
        .collect()
}

/// Sets each `@local` state of the instances `to` of build `new` that build
/// `old` declared under the same identity and schema to its value in
/// `from`, unless that value holds a closure. Returns the states carried.
fn carry_locals(old: &Module, from: &[Instance], new: &Module, to: &mut [Instance]) -> u32 {
    let mut carried = 0;
    for (system, instance) in new.systems().iter().zip(to) {
        let Some(i) = old.systems().iter().position(|s| s.id == system.id) else {
            continue;
        };
        let before = &old.systems()[i].locals;
        for local in system.locals.iter() {
            let Ok(at) = before.binary_search_by_key(&local.id, |s| s.id) else {
                continue;
            };
            let value = &from[i].states()[before[at].slot as usize];
            if before[at].schema == local.schema && !holds_closure(value) {
                instance.set_state(local.slot as usize, value.clone());
                carried += 1;
            }
        }
    }
    carried
}

fn holds_closure(value: &Value) -> bool {
    match value {
        Value::Closure(_) => true,
        Value::List(items) => items.iter().any(holds_closure),
        Value::Agg(aggregate) => aggregate.fields.iter().any(holds_closure),
        _ => false,
    }
}

/// The module's input schema, or the default `InputAction` set's.
fn input_schema(module: &Module) -> Cow<'_, InputSchema> {
    module
        .input()
        .map_or_else(|| Cow::Owned(InputSchema::standard()), Cow::Borrowed)
}

fn handle<T: NativeObject>(object: T) -> Value {
    Obj::new(object).into_value()
}

fn object<T: NativeObject>(value: &Value) -> &T {
    match value {
        Value::Handle(handle) => handle.get().expect("a scheduler-owned handle"),
        _ => unreachable!("a scheduler-owned handle"),
    }
}
