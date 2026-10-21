//! Native lowering against the interpreter (§108.2, ADR 0041): every chunk of
//! a module lowered to Rust (`lower_module`), compiled with this test, and run
//! step for step beside the interpreter. Each scripted step — a call, a task
//! start and every resume, a `done` handler — must give both the same value or
//! fault (kind, location, message), the same states, revision, dirty slots,
//! events, starts and cost. Every script runs twice: the native side on the
//! module the compiler built, and on the module a native release ships
//! ([`Module::compiled`], read in place by [`Module::decode_static`]), whose
//! bodies keep no bytecode, so its faults carry no source span. The lowered
//! modules are checked in
//! (`differential/*.rs`, from `differential/*.vs`) and must be what the
//! lowering writes today (`VISO_BLESS=1` rewrites them).

use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Duration;

use viso_behavior::aot::{NativeCode, lower_module};
use viso_behavior::native::{NativeError, Natives, Timers};
use viso_behavior::{Budget, Fault, Fiber, Instance, Module, Outcome, TaskStep, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file};

#[rustfmt::skip]
#[path = "differential/core.rs"]
mod core_native;
#[rustfmt::skip]
#[path = "differential/tasks.rs"]
mod tasks_native;

const CORE: &str = include_str!("differential/core.vs");
const TASKS: &str = include_str!("differential/tasks.vs");

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
        ..Default::default()
    }
}

fn compiled(source: &str) -> Rc<Module> {
    let compiled = compile_file(source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

#[test]
fn the_checked_in_lowerings_are_current() {
    for (name, source) in [("core", CORE), ("tasks", TASKS)] {
        let lowered = lower_module(&compiled(source), "viso_behavior");
        let path = format!(
            "{}/tests/differential/{name}.rs",
            env!("CARGO_MANIFEST_DIR")
        );
        if std::env::var_os("VISO_BLESS").is_some() {
            std::fs::write(&path, &lowered).expect("writes the lowering");
            continue;
        }
        let checked_in = std::fs::read_to_string(&path).expect("reads the lowering");
        assert!(
            checked_in == lowered,
            "tests/differential/{name}.rs is stale: rerun with VISO_BLESS=1"
        );
        assert_eq!(lowered, lower_module(&compiled(source), "viso_behavior"));
    }
}

/// Timers whose sleeps never finish: the script resumes every task itself.
struct Frozen;

impl Timers for Frozen {
    fn sleep(&mut self, _: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        Box::pin(std::future::pending())
    }
}

/// What the work a task awaits hands back: a value, or an error.
type Work = Result<Value, &'static str>;

fn work(result: &Work) -> Result<Value, NativeError> {
    result.clone().map_err(NativeError::new)
}

fn show_call(result: &Result<Outcome, Fault>) -> String {
    format!("{result:?}")
}

fn show_task(result: &Result<TaskStep, Fault>) -> String {
    match result {
        Ok(TaskStep::Done(value)) => format!("done {value:?}"),
        Ok(TaskStep::Awaiting(..)) => "awaiting".to_owned(),
        Err(fault) => format!("{fault:?}"),
    }
}

/// The module the native side runs on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Form {
    /// The module the compiler built.
    Bytecode,
    /// [`Module::compiled`], decoded in place from its `'static` bytes.
    Compiled,
}

const FORMS: [Form; 2] = [Form::Bytecode, Form::Compiled];

/// `seen` with every source span blanked, as a module without bytecode
/// reports its faults.
fn without_spans(seen: &str) -> String {
    let mut out = String::with_capacity(seen.len());
    let mut rest = seen;
    while let Some(at) = rest.find("span: Span {") {
        out.push_str(&rest[..at]);
        out.push_str("span: _");
        let end = rest[at..].find('}').expect("a closed span");
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    out
}

/// The interpreter (side 0) and the native code (side 1), each with its own
/// instance of one component.
struct Pair {
    sides: [(Vm, Instance); 2],
    form: Form,
    steps: u32,
}

impl Pair {
    fn new(source: &str, component: &str, native: &'static NativeCode, form: Form) -> Pair {
        let module = compiled(source);
        let index = module.component(component).expect("component");
        let shipped = match form {
            Form::Bytecode => module.clone(),
            Form::Compiled => {
                let bytes: &'static [u8] = Vec::leak(module.compiled().encode());
                Rc::new(Module::decode_static(bytes).expect("the compiled module loads"))
            }
        };
        let side = |install: bool| {
            let module = if install { &shipped } else { &module };
            let mut vm = Vm::new(module.clone(), Budget::default());
            vm.link(&Natives::standard(), &[]).expect("link");
            vm.services_mut()
                .insert::<Box<dyn Timers>>(Box::new(Frozen));
            if install {
                let chunks = vm.install_native(native).expect("the lowered build");
                assert!(chunks > 0);
            }
            let instance = vm.instantiate(index, []).expect("instantiate");
            (vm, instance)
        };
        let pair = Pair {
            sides: [side(false), side(true)],
            form,
            steps: 0,
        };
        let [(a, ai), (b, bi)] = &pair.sides;
        assert_eq!(ai.states(), bi.states(), "state initializers");
        assert_eq!(a.cost(), b.cost(), "state initializers");
        pair
    }

    /// The chunk of member `member`, or of the component's task `member`.
    fn chunk(&self, member: &str) -> u32 {
        let (vm, instance) = &self.sides[0];
        let module = vm.module();
        let component = instance.component().expect("component");
        let task = format!("{}.{member}", module.components()[component as usize].name);
        let chunk = module
            .layout(component)
            .member(member)
            .or_else(|| {
                (module.chunks().iter())
                    .position(|c| *c.name == *task)
                    .map(|c| c as u32)
            })
            .unwrap_or_else(|| panic!("no member {member}"));
        assert!(
            self.sides[1].0.is_native(chunk) && !vm.is_native(chunk),
            "{member} runs native on the right only"
        );
        chunk
    }

    /// Runs `run` on each side and requires the same observation of both.
    fn step<T>(
        &mut self,
        what: &str,
        mut run: impl FnMut(usize, &mut Vm, &mut Instance) -> T,
        show: impl Fn(&T) -> String,
    ) -> [T; 2] {
        self.steps += 1;
        let mut side = 0;
        let form = self.form;
        let out = self.sides.each_mut().map(|(vm, instance)| {
            let value = run(side, vm, instance);
            side += 1;
            let seen = format!(
                "{}\nstates {:?}\nrevision {}\ndirty {:?}\ncost {:?}",
                show(&value),
                instance.states(),
                instance.revision(),
                instance.dirty().collect::<Vec<_>>(),
                vm.cost(),
            );
            instance.clear_dirty();
            match form {
                Form::Bytecode => (value, seen),
                Form::Compiled => (value, without_spans(&seen)),
            }
        });
        assert_eq!(
            out[0].1, out[1].1,
            "{what}: the native code (right) differs from the interpreter (left)"
        );
        out.map(|(value, _)| value)
    }

    /// Calls `member` with `args`, then runs the tasks it started, each
    /// resumed with `()` until it returns, and their `done` handlers.
    fn call(&mut self, member: &str, args: &[Value]) -> Result<Value, Fault> {
        let chunk = self.chunk(member);
        let what = format!("{member}{args:?}");
        let [result, _] = self.step(
            &what,
            |_, vm, instance| vm.call(instance, chunk, args),
            show_call,
        );
        let outcome = result?;
        self.run_starts(&what, outcome.starts);
        Ok(outcome.value)
    }

    fn run_starts(&mut self, what: &str, starts: Vec<viso_behavior::Start>) {
        for start in starts {
            let what = format!("{what} > start {}{:?}", start.task, start.args);
            let Some(value) = self.drive(&what, start.task, &start.args, &[]) else {
                continue;
            };
            if let Some(done) = &start.done {
                let [result, _] = self.step(
                    &format!("{what} > done({value:?})"),
                    |_, vm, instance| vm.call_value(instance, done, std::slice::from_ref(&value)),
                    show_call,
                );
                if let Ok(outcome) = result {
                    self.run_starts(&what, outcome.starts);
                }
            }
        }
    }

    /// Starts task `member` with `args` and resumes it with `script`'s
    /// results in turn, then with `()`, until it returns or faults.
    fn task(&mut self, member: &str, args: &[Value], script: &[Work]) -> Option<Value> {
        let chunk = self.chunk(member);
        self.drive(&format!("task {member}{args:?}"), chunk, args, script)
    }

    fn drive(&mut self, what: &str, chunk: u32, args: &[Value], script: &[Work]) -> Option<Value> {
        let mut steps = self.step(
            what,
            |_, vm, instance| vm.start_task(instance, chunk, args),
            show_task,
        );
        for turn in 0.. {
            assert!(turn < 1000, "{what}: the task never returns");
            let mut fibers: [Option<Fiber>; 2] = [None, None];
            for (side, step) in steps.into_iter().enumerate() {
                match step {
                    Ok(TaskStep::Awaiting(fiber, _work)) => fibers[side] = Some(fiber),
                    Ok(TaskStep::Done(value)) => return Some(value),
                    Err(_) => return None,
                }
            }
            let result = script.get(turn).cloned().unwrap_or(Ok(Value::Nil));
            steps = self.step(
                &format!("{what} > resume #{turn} with {result:?}"),
                |side, vm, instance| {
                    let fiber = fibers[side].take().expect("a suspended side");
                    vm.resume_task(instance, fiber, work(&result))
                },
                show_task,
            );
        }
        unreachable!()
    }

    fn set_state(&mut self, slot: usize, value: Value) {
        for (_, instance) in &mut self.sides {
            instance.set_state(slot, value.clone());
        }
    }

    fn set_budget(&mut self, budget: Budget) {
        for (vm, _) in &mut self.sides {
            vm.set_budget(budget);
        }
    }
}

fn int(v: i64) -> Value {
    Value::Int(v)
}

fn float(v: f64) -> Value {
    Value::Float(v)
}

fn ints(values: &[i64]) -> Value {
    Value::List(Rc::new(values.iter().map(|&v| Value::Int(v)).collect()))
}

const INTS: [i64; 10] = [0, 1, -1, 2, -7, 13, 1 << 32, -(1 << 40), i64::MAX, i64::MIN];

const FLOATS: [f64; 9] = [
    0.0,
    -0.0,
    1.5,
    -2.25,
    1e300,
    f64::MIN_POSITIVE,
    f64::INFINITY,
    f64::NAN,
    9.3e18,
];

#[test]
fn actions_and_computeds_match_with_their_faults_and_rollbacks() {
    for form in FORMS {
        let mut p = Pair::new(CORE, "Core", &core_native::NATIVE, form);
        p.call("doubled", &[]).unwrap();
        p.call("total", &[]).unwrap();
        for by in [1, 5, -3] {
            p.call("add", &[int(by)]).unwrap();
        }
        p.call("doubled", &[]).unwrap();
        p.call("total", &[]).unwrap();
        p.call("total", &[]).unwrap();
        p.call("scale", &[int(3)]).unwrap();
        assert!(p.call("add", &[int(i64::MAX)]).is_err(), "overflow");
        assert!(p.call("scale", &[int(i64::MAX / 2)]).is_err(), "overflow");
        assert!(p.call("divide", &[int(0)]).is_err(), "division by zero");
        assert!(p.call("divide", &[int(-1)]).is_err(), "remainder by zero");
        p.call("divide", &[int(2)]).unwrap();
        for v in [7, 50, -2] {
            p.call("push", &[int(v)]).unwrap();
        }
        p.call("total", &[]).unwrap();
        p.call("poke", &[int(1), int(9)]).unwrap();
        for i in [-1, 6, i64::MAX] {
            assert!(p.call("poke", &[int(i), int(1)]).is_err(), "index {i}");
        }
        p.call("rename", &[Value::str("left")]).unwrap();
        p.call("rename", &[Value::str("")]).unwrap();
        for dx in [0.5, -1e9, f64::NAN] {
            p.call("nudge", &[float(dx)]).unwrap();
        }
        for i in [1, -1, 99, 0] {
            p.call("pick", &[int(i)]).unwrap();
        }
        // A host write invalidates the computeds that read the slot.
        p.set_state(0, int(100));
        p.call("doubled", &[]).unwrap();
        p.call("total", &[]).unwrap();
        assert!(p.steps > 30);
    }
}

#[test]
fn functions_match_over_a_table_of_edge_values() {
    for form in FORMS {
        let mut p = Pair::new(CORE, "Core", &core_native::NATIVE, form);
        for a in INTS {
            for b in INTS {
                let _ = p.call("mul", &[int(a), int(b)]);
                let _ = p.call("sub", &[int(a), int(b)]);
            }
            let _ = p.call("neg", &[int(a)]);
            let _ = p.call("widen", &[int(a)]);
            let _ = p.call("apply", &[int(a)]);
            let _ = p.call("classify", &[int(a)]);
        }
        for a in FLOATS {
            for b in FLOATS {
                let _ = p.call("ratio", &[float(a), float(b)]);
            }
            let _ = p.call("narrow", &[float(a)]);
            let _ = p.call("shapes", &[float(a)]);
        }
        for n in [-1, 0, 1, 2, 3, 4, 5, 9] {
            p.call("digit", &[int(n)]).unwrap();
            p.call("count_to", &[int(n)]).unwrap();
        }
        let list = ints(&[1, 20, 300, -4, 50]);
        p.call("sum", std::slice::from_ref(&list)).unwrap();
        p.call("sum", &[ints(&[])]).unwrap();
        for i in [-1, 0, 4, 5] {
            let _ = p.call("at", &[list.clone(), int(i)]);
        }
        p.call("fib", &[int(15)]).unwrap();
    }
}

#[test]
fn budgets_stop_both_at_the_same_place() {
    for form in FORMS {
        let mut p = Pair::new(CORE, "Core", &core_native::NATIVE, form);
        assert!(p.call("fib", &[int(400)]).is_err(), "too deep");
        p.set_budget(Budget {
            instructions: 500,
            ..Budget::default()
        });
        p.call("spin", &[int(10)]).unwrap();
        assert!(p.call("spin", &[int(10_000)]).is_err(), "out of fuel");
        assert!(p.call("fib", &[int(20)]).is_err(), "out of fuel");
        p.set_budget(Budget {
            memory: 64,
            ..Budget::default()
        });
        let _ = p.call("push", &[int(1)]);
        let _ = p.call(
            "rename",
            &[Value::str(
                "a longer name than sixty-four bytes of heap may hold",
            )],
        );
        p.set_budget(Budget::default());
        p.call("spin", &[int(10_000)]).unwrap();
    }
}

#[test]
fn tasks_suspend_and_resume_alike() {
    for form in FORMS {
        let mut p = Pair::new(TASKS, "Jobs", &tasks_native::NATIVE, form);
        assert_eq!(p.task("wait", &[], &[]), Some(int(3)));
        assert_eq!(p.task("immediate", &[int(4)], &[]), Some(int(5)));
        p.task("fetch", &[int(5)], &[]).unwrap();
        p.task("fetch", &[int(-1)], &[]).unwrap();
        assert_eq!(p.task("count", &[int(0)], &[]), Some(int(0)));
        assert_eq!(p.task("count", &[int(4)], &[]), Some(int(14)));
        assert_eq!(p.task("nested", &[int(3)], &[]), Some(int(8)));
        assert_eq!(p.task("strict", &[int(1)], &[]), None, "overflow");
        assert_eq!(p.task("strict", &[int(5)], &[]), None, "index");
        assert_eq!(p.task("wait", &[], &[Err("lost")]), None, "failed work");
        assert_eq!(
            p.task("count", &[int(3)], &[Ok(Value::Nil), Err("lost")]),
            None
        );
        assert_eq!(
            p.task("nested", &[int(2)], &[Ok(Value::Nil), Err("lost")]),
            None
        );
        // A task called as a function: the interpreter runs it up to the
        // await it cannot make outside a task. A compiled module keeps no
        // bytecode to run, so there the call faults as a build without the
        // interpreter does.
        if form == Form::Bytecode {
            assert!(p.call("wait", &[]).is_err());
        }
    }
}

#[test]
fn started_tasks_and_their_handlers_match() {
    for form in FORMS {
        let mut p = Pair::new(TASKS, "Jobs", &tasks_native::NATIVE, form);
        for _ in 0..3 {
            p.call("go", &[]).unwrap();
        }
        p.set_state(0, int(-4));
        p.call("go", &[]).unwrap();
        p.call("chain", &[int(2)]).unwrap();
        p.call("chain", &[int(0)]).unwrap();
        let (_, instance) = &p.sides[0];
        assert_ne!(instance.states()[1], int(0), "a handler ran");
    }
}

#[test]
fn a_view_host_runs_the_lowered_module_and_refuses_another_build() {
    use viso_view::{HostError, ViewHost};
    let host = ViewHost::with_native(compiled(CORE), "Core", &core_native::NATIVE).expect("mounts");
    assert!(host.runs_native());
    assert!(
        !ViewHost::new(compiled(CORE), "Core")
            .expect("mounts")
            .runs_native()
    );
    let stale = ViewHost::with_native(compiled(CORE), "Core", &tasks_native::NATIVE);
    assert!(matches!(stale, Err(HostError::Native(_))), "{stale:?}");
}
