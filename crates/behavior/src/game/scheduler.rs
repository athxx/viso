//! The fixed-step scheduler over a module's systems.

use std::borrow::Cow;
use std::cell::{Ref, RefCell, RefMut};
use std::mem;
use std::rc::Rc;

use super::input::{Action, InputLatch};
use super::kit::{Kit, Stage};
use super::quick::{QUICK_FIXED, QUICK_START, QuickFrame, QuickStart};
use super::snapshot::{GameSnapshot, Restored, SystemState, fnv};
use super::tape::{InputTape, Playback, Recorder, TapeError};
use super::world::Bodies;
use super::{
    COLLISION, Clock, CollisionEvent, EntityId, FIXED_UPDATE, FRAME_UPDATE, FixedFrame, GameStart,
    GameWorld, InputSchema, InputSnapshot, Key, PadButton, PadStick, RenderFrame, STARTUP,
    TouchButton,
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
/// tick freezes the input, runs every `FixedUpdate`, commits their commands,
/// steps the world, delivers the contacts that began (after those the host
/// queued) to every `CollisionListener` and commits theirs; the start's
/// commands commit before the first tick. A `FrameUpdate` reads the world
/// and cannot write it.
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
        let mut scheduler = Scheduler::assemble(vm, seed)?;
        scheduler.run_start()?;
        scheduler.deliver_queued(0);
        Ok(scheduler)
    }

    /// A scheduler over `vm` with fresh instances and an empty world seeded
    /// with `seed`, before its start.
    fn assemble(mut vm: Vm, seed: u64) -> Result<Scheduler, SystemFault> {
        let module = vm.module().clone();
        let clock = Clock::at_rate(module.tick_rate());
        let instances = instantiate(&mut vm, clock.tick())?;
        let schema = input_schema(&module);
        let world = Obj::new(GameWorld::new(seed));
        let stage = Rc::default();
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
        let (bodies, rng) = (self.world.bodies(), self.world.rng());
        let (tick, delivered) = (self.clock.tick(), self.delivered);
        let collisions = mem::take(&mut self.collisions);
        self.world.restore(Rc::default(), self.seed);
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
                self.world.restore(bodies, rng);
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
    /// As [`rebuild_world`](Self::rebuild_world): this game, its build among
    /// it, is left as it was.
    pub fn rebuild(&mut self, vm: Vm, keep: Rebuild) -> Result<u32, SystemFault> {
        let mut shadow = Scheduler::assemble(vm, self.seed)?;
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
        shadow.deliver_queued(0);
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
        // A tick already delivered drops the commands it issues again.
        self.delivered = u64::MAX;
        self.run_tick();
        (self.playback, self.recorder) = tapes;
        let fault = self.faults.drain(faults..).next();
        self.restore_states(&started);
        self.world.restore(started.world.clone(), started.rng);
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
    /// The fault of a system whose instance could not be created: the game is
    /// left as it was.
    pub fn reload(&mut self, mut vm: Vm) -> Result<Restored, SystemFault> {
        let snapshot = self.snapshot();
        let mut instances = instantiate(&mut vm, self.clock.tick())?;
        let module = vm.module().clone();
        let locals = carry_locals(self.vm.module(), &self.instances, &module, &mut instances);
        let schema = input_schema(&module);
        if module.tick_rate() != self.vm.module().tick_rate() {
            self.clock.set_rate(module.tick_rate());
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
        self.vm = vm;
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
    /// this one.
    pub fn restore(&mut self, snapshot: &GameSnapshot) -> Restored {
        let restored = self.restore_states(snapshot);
        self.world.restore(snapshot.world.clone(), snapshot.rng);
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
        for i in 0..self.hooks.frame.len() {
            let arg = self.cx.render_frame.clone();
            if !self.run_hook(self.hooks.frame[i], arg, &mut left, Phase::Frame) {
                break;
            }
        }
        self.stage.borrow_mut().end_frame(dt, &self.world, alpha);
        ticks
    }

    /// Runs `ticks` ticks now, paused or not, on the path frames run them on.
    pub fn step(&mut self, ticks: u32) {
        for _ in 0..ticks {
            self.run_tick();
        }
    }

    fn run_tick(&mut self) {
        let tick = self.clock.tick();
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
        self.world
            .step(self.clock.fixed_dt() as f32, &mut self.delivering);
        if running && !self.hooks.collision.is_empty() {
            'events: for e in 0..self.delivering.len() {
                let (first, second) = self.delivering[e];
                let event = object::<CollisionEvent>(&self.cx.collision_event);
                event.tick.set(tick);
                event.first.set(first);
                event.second.set(second);
                for i in 0..self.hooks.collision.len() {
                    let arg = self.cx.collision_event.clone();
                    if !self.run_hook(self.hooks.collision[i], arg, &mut left, Phase::Tick) {
                        break 'events;
                    }
                }
            }
            self.world.commit();
        }
        self.vm.defer_presentation(false);
        self.delivering.clear();
        self.deliver_commands(tick);
        self.clock.finish_tick();
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

/// A module's hooks by phase, each in system order.
struct Hooks {
    start: Box<[Hook]>,
    fixed: Box<[Hook]>,
    frame: Box<[Hook]>,
    collision: Box<[Hook]>,
}

impl Hooks {
    fn of(module: &Module) -> Hooks {
        let (mut start, mut fixed) = (Vec::new(), Vec::new());
        let (mut frame, mut collision) = (Vec::new(), Vec::new());
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
            bind(&mut frame, FRAME_UPDATE, false);
            bind(&mut collision, COLLISION, false);
        }
        Hooks {
            start: start.into(),
            fixed: fixed.into(),
            frame: frame.into(),
            collision: collision.into(),
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
