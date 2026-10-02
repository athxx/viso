//! The fixed-step scheduler over a module's systems.

use std::mem;

use super::input::InputLatch;
use super::{
    COLLISION, Clock, CollisionEvent, FIXED_UPDATE, FRAME_UPDATE, FixedFrame, InputSchema,
    InputSnapshot, Key, PadButton, PadStick, RenderFrame, TouchButton,
};
use crate::native::{NativeObject, NativeValue, Obj};
use crate::{Budget, Fault, FaultKind, Instance, Value, Vm};

/// A fault a system hook raised: its state changes were discarded.
#[derive(Debug, Clone)]
pub struct SystemFault {
    /// The system, an index into [`Module::systems`](crate::Module::systems).
    pub system: usize,
    /// The tick it ran in, or the tick the frame ended at for a
    /// `FrameUpdate`.
    pub tick: u64,
    /// The stable diagnostic code: `E9102` when the tick's shared budget ran
    /// out, otherwise the fault's own.
    pub code: &'static str,
    /// The fault.
    pub fault: Fault,
}

#[derive(Debug, Clone, Copy)]
struct Hook {
    system: usize,
    chunk: u32,
}

/// Which budget a phase draws on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Tick,
    Frame,
}

/// Runs a module's systems on a fixed-step [`Clock`].
///
/// Each system is one [`Instance`] created when the scheduler is. Hooks run in
/// [`Module::systems`](crate::Module::systems) order, which the compiler sorts
/// by `@after`/`@before`. Every hook call is its own transaction: a fault
/// discards that call's state writes, is recorded as a [`SystemFault`], and the
/// next system still runs, except when the shared budget ran out, which skips
/// the rest of the tick (`E9102`) or frame.
///
/// The host reports device input between frames ([`key`](Self::key),
/// [`pad`](Self::pad), [`stick`](Self::stick), [`touch`](Self::touch)); each
/// tick reads it frozen through `frame.input`, mapped by the module's input
/// schema or the default `InputAction` set.
pub struct Scheduler {
    vm: Vm,
    clock: Clock,
    budget: Budget,
    instances: Vec<Instance>,
    fixed: Box<[Hook]>,
    frame: Box<[Hook]>,
    collision: Box<[Hook]>,
    fixed_frame: Value,
    input: InputLatch,
    render_frame: Value,
    collision_event: Value,
    collisions: Vec<(i64, i64)>,
    delivering: Vec<(i64, i64)>,
    faults: Vec<SystemFault>,
}

impl Scheduler {
    /// A scheduler running `vm`'s module on `clock`, creating one instance of
    /// every system. The `vm`'s budget becomes the budget each tick's hooks
    /// share, and each frame's: memory and depth stay per call.
    ///
    /// # Errors
    ///
    /// The fault of a system whose instance could not be created.
    pub fn new(mut vm: Vm, clock: Clock) -> Result<Scheduler, Fault> {
        let module = vm.module().clone();
        let mut instances = Vec::with_capacity(module.systems().len());
        let (mut fixed, mut frame, mut collision) = (Vec::new(), Vec::new(), Vec::new());
        for (index, system) in module.systems().iter().enumerate() {
            instances.push(vm.instantiate(system.component, [])?);
            let bind = |hooks: &mut Vec<Hook>, id| {
                if let Some(chunk) = system.hook(id) {
                    hooks.push(Hook {
                        system: index,
                        chunk,
                    });
                }
            };
            bind(&mut fixed, FIXED_UPDATE);
            bind(&mut frame, FRAME_UPDATE);
            bind(&mut collision, COLLISION);
        }
        let fixed_dt = clock.fixed_dt();
        let standard;
        let schema = match module.input() {
            Some(schema) => schema,
            None => {
                standard = InputSchema::standard();
                &standard
            }
        };
        Ok(Scheduler {
            budget: vm.budget(),
            vm,
            clock,
            instances,
            fixed: fixed.into(),
            frame: frame.into(),
            collision: collision.into(),
            fixed_frame: handle(FixedFrame {
                tick: 0.into(),
                dt: fixed_dt,
                input: Obj::new(InputSnapshot::new(schema.actions.len())),
            }),
            input: InputLatch::new(schema),
            render_frame: handle(RenderFrame::default()),
            collision_event: handle(CollisionEvent::default()),
            collisions: Vec::new(),
            delivering: Vec::new(),
            faults: Vec::new(),
        })
    }

    /// The clock.
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    /// The clock, to pause, scale or change its overrun policy.
    pub fn clock_mut(&mut self) -> &mut Clock {
        &mut self.clock
    }

    /// The interpreter.
    pub fn vm(&self) -> &Vm {
        &self.vm
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

    /// Queues a contact between bodies `first` and `second` for the
    /// `CollisionListener`s of the next tick.
    pub fn push_collision(&mut self, first: i64, second: i64) {
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
        let frame = object::<RenderFrame>(&self.render_frame);
        frame.dt.set(if wall_dt.is_finite() && wall_dt > 0.0 {
            wall_dt
        } else {
            0.0
        });
        frame.time.set(self.clock.time());
        let mut left = self.budget;
        for i in 0..self.frame.len() {
            let arg = self.render_frame.clone();
            if !self.run_hook(self.frame[i], arg, &mut left, Phase::Frame) {
                break;
            }
        }
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
        let frame = object::<FixedFrame>(&self.fixed_frame);
        frame.tick.set(tick);
        self.input.deliver(&frame.input);
        mem::swap(&mut self.collisions, &mut self.delivering);
        let mut left = self.budget;
        'tick: {
            for i in 0..self.fixed.len() {
                let arg = self.fixed_frame.clone();
                if !self.run_hook(self.fixed[i], arg, &mut left, Phase::Tick) {
                    break 'tick;
                }
            }
            if self.collision.is_empty() {
                break 'tick;
            }
            for e in 0..self.delivering.len() {
                let (first, second) = self.delivering[e];
                let event = object::<CollisionEvent>(&self.collision_event);
                event.tick.set(tick);
                event.first.set(first);
                event.second.set(second);
                for i in 0..self.collision.len() {
                    let arg = self.collision_event.clone();
                    if !self.run_hook(self.collision[i], arg, &mut left, Phase::Tick) {
                        break 'tick;
                    }
                }
            }
        }
        self.delivering.clear();
        self.clock.finish_tick();
    }

    /// Runs one hook on what is left of the shared budget; false when the
    /// budget ran out.
    fn run_hook(&mut self, hook: Hook, arg: Value, left: &mut Budget, phase: Phase) -> bool {
        self.vm.set_budget(*left);
        let result = self
            .vm
            .call(&mut self.instances[hook.system], hook.chunk, &[arg]);
        let cost = self.vm.cost();
        left.instructions = left.instructions.saturating_sub(cost.instructions);
        left.native_calls = left.native_calls.saturating_sub(cost.native_calls);
        let Err(fault) = result else {
            return true;
        };
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

fn handle<T: NativeObject>(object: T) -> Value {
    Obj::new(object).into_value()
}

fn object<T: NativeObject>(value: &Value) -> &T {
    match value {
        Value::Handle(handle) => handle.get().expect("a scheduler-owned handle"),
        _ => unreachable!("a scheduler-owned handle"),
    }
}
