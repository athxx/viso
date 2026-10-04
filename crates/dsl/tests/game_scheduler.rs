//! Game systems: `system` declarations implementing `viso::game` scheduler
//! traits compile to bound hooks in `@after`/`@before` order and run under the
//! fixed-step scheduler.

use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::game::{
    COLLISION, EntityId, FIXED_UPDATE, FRAME_UPDATE, Scheduler, TickOverrun,
};
use viso_behavior::native::{
    HookDomain, NativeHook, NativeLibrary, NativeTrait, NativeValue, Natives, Param, STANDARD,
    SchemaTy,
};
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file, compile_file_in};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

const IMPORTS: &str = "import viso::game::{FixedUpdate, FrameUpdate, CollisionListener, FixedFrame, RenderFrame, CollisionEvent, EntityId};\n";

/// Compiles `source` after the scheduler imports; it must have no errors.
fn module(source: &str) -> Rc<Module> {
    let compiled = compile_file(&format!("{IMPORTS}{source}"), &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = compiled.behavior.bytecode().expect("verified bytecode");
    Rc::new(module.with_tick_rate(4).expect("a tick rate"))
}

/// The error codes of `source` after the scheduler imports.
fn codes(source: &str) -> Vec<String> {
    compile_file(&format!("{IMPORTS}{source}"), &origin())
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

/// A scheduler over `module` on a quarter-second clock.
fn scheduler(module: Rc<Module>) -> Scheduler {
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::new(vm).expect("systems instantiate")
}

/// State `name` of the instance of system `system`.
fn state(scheduler: &Scheduler, system: &str, name: &str) -> Value {
    let module = scheduler.vm().module();
    let component = module.component(system).expect("system");
    let index = module
        .systems()
        .iter()
        .position(|s| s.component == component)
        .expect("a system");
    let slot = module.layout(component).state(name).expect("state");
    scheduler.instance(index).states()[slot].clone()
}

/// The names of the module's systems, in run order.
fn run_order(module: &Module) -> Vec<&str> {
    module
        .systems()
        .iter()
        .map(|s| &*module.layout(s.component).name)
        .collect()
}

const GAME: &str = r#"
@after(Physics)
system Scoring implements FixedUpdate + CollisionListener {
    state ticks = 0;
    state last_time = 0.0;
    state hits = 0;
    state pair: Option<EntityId> = Option::None;

    action fixed_update(frame: FixedFrame) {
        ticks += 1;
        last_time = frame.time();
    }

    action collision(event: CollisionEvent) {
        hits += 1;
        pair = event.other_of(event.first);
    }
}

system Physics implements FixedUpdate {
    state tick = -1;

    action fixed_update(frame: FixedFrame) {
        tick = frame.tick();
    }
}

export system Hud implements FrameUpdate {
    state frames = 0;
    state time = 0.0;

    action frame_update(frame: RenderFrame) {
        frames += 1;
        time = frame.time();
    }
}
"#;

#[test]
fn systems_bind_their_hooks_in_declared_order() {
    let module = module(GAME);
    assert_eq!(run_order(&module), ["Physics", "Scoring", "Hud"]);
    let [physics, scoring, hud] = module.systems() else {
        panic!("three systems");
    };
    let layout = |s: &viso_behavior::System| module.layout(s.component);
    assert_eq!(
        physics.hook(FIXED_UPDATE),
        layout(physics).member("fixed_update")
    );
    assert_eq!(physics.hook(FRAME_UPDATE), None);
    assert_eq!(scoring.hook(COLLISION), layout(scoring).member("collision"));
    assert_eq!(hud.hook(FRAME_UPDATE), layout(hud).member("frame_update"));
    assert_eq!(hud.hook(FIXED_UPDATE), None);

    let decoded = Module::decode(&module.encode()).expect("decode");
    assert_eq!(decoded.systems(), module.systems());
}

#[test]
fn a_frame_runs_the_ticks_it_owes_then_the_frame_hooks() {
    let mut game = scheduler(module(GAME));
    assert_eq!(game.frame(0.5), 2);
    assert_eq!(state(&game, "Physics", "tick"), Value::Int(1));
    assert_eq!(state(&game, "Scoring", "ticks"), Value::Int(2));
    assert_eq!(state(&game, "Scoring", "last_time"), Value::Float(0.25));
    assert_eq!(state(&game, "Hud", "frames"), Value::Int(1));
    assert_eq!(state(&game, "Hud", "time"), Value::Float(0.5));

    game.clock_mut().set_paused(true);
    assert_eq!(game.frame(1.0), 0);
    assert_eq!(state(&game, "Scoring", "ticks"), Value::Int(2));
    assert_eq!(state(&game, "Hud", "frames"), Value::Int(2));
    game.step(1);
    assert_eq!(state(&game, "Physics", "tick"), Value::Int(2));
    assert_eq!(game.clock().tick(), 3);

    game.clock_mut().set_paused(false);
    game.clock_mut().set_time_scale(0.5);
    assert_eq!(game.frame(1.0), 2);
    assert_eq!(state(&game, "Scoring", "ticks"), Value::Int(5));
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
}

#[test]
fn an_overrun_caps_the_frame_and_is_counted() {
    let mut game = scheduler(module(GAME));
    game.clock_mut().set_max_catch_up_steps(2);
    assert_eq!(game.frame(1.25), 2);
    assert_eq!(game.clock().overrun_ticks(), 3);
    assert_eq!(game.clock().dropped_time(), 0.75);
    assert_eq!(state(&game, "Scoring", "ticks"), Value::Int(2));

    game.clock_mut().set_overrun(TickOverrun::SlowMotion);
    assert_eq!(game.frame(1.0), 2);
    assert_eq!(game.frame(0.0), 2);
    assert_eq!(state(&game, "Scoring", "ticks"), Value::Int(6));
}

#[test]
fn collisions_reach_the_listeners_of_the_next_tick() {
    let mut game = scheduler(module(GAME));
    let id = EntityId::new;
    game.push_collision(id(1, 0), id(2, 0));
    game.push_collision(id(3, 0), id(4, 1));
    assert_eq!(game.frame(0.25), 1);
    assert_eq!(state(&game, "Scoring", "hits"), Value::Int(2));
    assert_eq!(state(&game, "Scoring", "pair"), id(4, 1).into_value());
    assert_eq!(game.frame(0.25), 1);
    assert_eq!(state(&game, "Scoring", "hits"), Value::Int(2));
}

const FAULTING: &str = r#"
system B implements FixedUpdate {
    state n = 0;
    action fixed_update(frame: FixedFrame) {
        n += 1;
        let xs = [1];
        n = xs[frame.tick() + 5];
    }
}

@before(B)
system A implements FixedUpdate {
    state n = 0;
    action fixed_update(frame: FixedFrame) {
        n += 1;
        let xs = [1];
        n = xs[frame.tick() + 5];
    }
}

system C implements FixedUpdate {
    state n = 0;
    action fixed_update(frame: FixedFrame) {
        n += 1;
    }
}
"#;

#[test]
fn a_faulting_hook_is_rolled_back_and_the_next_system_runs() {
    let module = module(FAULTING);
    assert_eq!(run_order(&module), ["A", "B", "C"]);
    let mut game = scheduler(module);
    assert_eq!(game.frame(0.25), 1);
    let faults = game.take_faults();
    assert_eq!(
        faults
            .iter()
            .map(|f| (f.system, f.tick))
            .collect::<Vec<_>>(),
        [(0, 0), (1, 0)]
    );
    assert!(faults.iter().all(|f| f.code != "E9102"));
    assert_eq!(state(&game, "A", "n"), Value::Int(0));
    assert_eq!(state(&game, "B", "n"), Value::Int(0));
    assert_eq!(state(&game, "C", "n"), Value::Int(1));
}

#[test]
fn an_exhausted_tick_budget_skips_the_rest_of_the_tick() {
    let mut game = scheduler(module(GAME));
    game.set_budget(Budget {
        instructions: 3,
        ..Budget::default()
    });
    assert_eq!(game.frame(0.25), 1);
    let faults = game.take_faults();
    assert_eq!(
        faults.first().map(|f| (f.system, f.code)),
        Some((0, "E9102"))
    );
    assert_eq!(state(&game, "Scoring", "ticks"), Value::Int(0));
    assert_eq!(game.clock().tick(), 1);
}

#[test]
fn a_cyclic_order_is_reported() {
    let cycle = "@after(B) system A implements FixedUpdate { action fixed_update(frame: FixedFrame) {} }\n@after(A) system B implements FixedUpdate { action fixed_update(frame: FixedFrame) {} }";
    assert_eq!(codes(cycle), ["E9101"]);
    let message = compile_file(&format!("{IMPORTS}{cycle}"), &origin())
        .errors()
        .next()
        .map(|d| d.message.clone())
        .unwrap_or_default();
    assert!(
        message.contains("`A` → `B` → `A`") || message.contains("`B` → `A` → `B`"),
        "{message}"
    );

    let itself =
        "@after(A) system A implements FixedUpdate { action fixed_update(frame: FixedFrame) {} }";
    assert_eq!(codes(itself), ["E9101"]);

    let chain =
        "@before(B) system A {}\n@before(C) system B {}\n@before(A) system C {}\nsystem D {}";
    assert_eq!(codes(chain), ["E9101"]);
}

#[test]
fn a_hook_must_be_an_action_of_its_signature() {
    assert_eq!(
        codes("system A implements FixedUpdate { state n = 0; }"),
        ["E2201"]
    );
    assert_eq!(
        codes("system A implements FixedUpdate { action fixed_update(frame: RenderFrame) {} }"),
        ["E2201"]
    );
    assert_eq!(
        codes("system A implements FixedUpdate { action fixed_update() {} }"),
        ["E2201"]
    );
    assert_eq!(
        codes(
            "system A implements FixedUpdate + FixedUpdate { action fixed_update(frame: FixedFrame) {} }"
        ),
        ["E2201"]
    );
}

#[test]
fn a_bound_must_name_a_scheduler_trait() {
    assert_eq!(codes("system A implements FixedFrame {}"), ["E2201"]);
    assert_eq!(
        codes("record R { x: I64; }\nsystem A implements R {}"),
        ["E2201"]
    );
    assert_eq!(codes("system A implements Nope {}"), ["E2001"]);
}

#[test]
fn an_order_argument_must_name_a_system() {
    assert_eq!(codes("@after(Nope) system A {}"), ["E2001"]);
    assert_eq!(
        codes("record R { x: I64; }\n@after(R) system A {}"),
        ["E2001"]
    );
    assert_eq!(codes("system B {}\n@after(x: B) system A {}"), ["E2001"]);
    assert_eq!(codes("@before system A {}"), ["E2001"]);
}

#[test]
fn a_system_declares_no_view_event_or_slot() {
    assert_eq!(
        codes("system A { view { Text { text: \"a\"; } } }"),
        ["E9109"]
    );
    assert_eq!(codes("system A { event done(); }"), ["E9109"]);
}

static TWIN: NativeLibrary = NativeLibrary {
    path: "app::twin",
    version: 1,
    functions: &[],
    types: &[],
    traits: &[
        NativeTrait {
            name: "Left",
            hooks: &[NativeHook {
                name: "update",
                params: &[Param {
                    name: "dt",
                    ty: SchemaTy::F64,
                }],
                domain: HookDomain::Simulation,
            }],
        },
        NativeTrait {
            name: "Right",
            hooks: &[NativeHook {
                name: "update",
                params: &[Param {
                    name: "dt",
                    ty: SchemaTy::F64,
                }],
                domain: HookDomain::Simulation,
            }],
        },
    ],
    derives: &[],
    widgets: &[],
};

#[test]
fn two_traits_declaring_one_hook_are_ambiguous() {
    let mut natives = Natives::new();
    natives.extend(STANDARD).expect("standard");
    natives.register(&TWIN).expect("twin");
    let source = "import app::twin::{Left, Right};\nsystem A implements Left + Right { action update(dt: F64) {} }";
    let codes: Vec<_> = compile_file_in(source, &origin(), Arc::new(natives))
        .errors()
        .map(|d| d.code.to_string())
        .collect();
    assert_eq!(codes, ["E2202"]);
}
