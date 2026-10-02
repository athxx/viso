//! Typed game input: `@derive(InputAction)` enums, the `@const` `InputMap` a
//! package evaluates at compile time, and the frozen per-tick snapshot systems
//! read through `frame.input`.

use std::rc::Rc;

use viso_behavior::game::{Clock, InputAction, Key, PadButton, PadStick, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::Severity;
use viso_dsl::frontend::{Origin, compile_file, compile_file_for};
use viso_dsl::hir::InputDevices;

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

const IMPORTS: &str = "import viso::game::{FixedUpdate, FixedFrame, InputAction, InputAxis, InputMap, Key, KeySet, PadButton, PadStick, TouchButton};\n";

fn module(source: &str) -> Rc<Module> {
    let compiled = compile_file(&format!("{IMPORTS}{source}"), &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

/// The error codes of `source`, which a system makes a source that needs no
/// mountable component.
fn codes(source: &str) -> Vec<String> {
    compile_file(&format!("{IMPORTS}{source}\nsystem Host {{}}"), &origin())
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

/// A scheduler over `module` on a quarter-second clock.
fn scheduler(module: Rc<Module>) -> Scheduler {
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::new(vm, Clock::new(0.25)).expect("systems instantiate")
}

fn state(scheduler: &Scheduler, name: &str) -> Value {
    let module = scheduler.vm().module();
    let component = module.systems()[0].component;
    let slot = module.layout(component).state(name).expect("state");
    scheduler.instance(0).states()[slot].clone()
}

const DEFAULT: &str = r#"
system Player implements FixedUpdate {
    state jumps = 0;
    state held = 0;
    state x = 0.0;
    state z = 0.0;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) {
            jumps += 1;
        }
        if frame.input.held(InputAction::jump) {
            held += 1;
        }
        x = frame.input.axis(InputAxis::move_x);
        z = frame.input.move_axes().y;
    }
}
"#;

#[test]
fn a_frame_delivers_an_edge_to_one_tick_and_held_to_every_tick() {
    let module = module(DEFAULT);
    assert_eq!(module.input(), None);
    let mut game = scheduler(module);
    game.key(Key::Space, true);
    game.key(Key::D, true);
    game.key(Key::W, true);
    assert_eq!(game.frame(0.5), 2);
    assert_eq!(state(&game, "jumps"), Value::Int(1));
    assert_eq!(state(&game, "held"), Value::Int(2));
    let Value::Float(x) = state(&game, "x") else {
        panic!("a float");
    };
    assert!((x - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-12);
    assert_eq!(state(&game, "z"), Value::Float(x));
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
}

#[test]
fn a_frame_without_a_tick_keeps_the_edge_for_the_next() {
    let mut game = scheduler(module(DEFAULT));
    game.pad(PadButton::South, true);
    game.pad(PadButton::South, false);
    assert_eq!(game.frame(0.125), 0);
    assert_eq!(game.frame(0.125), 1);
    assert_eq!(state(&game, "jumps"), Value::Int(1));
    assert_eq!(state(&game, "held"), Value::Int(0));
    assert_eq!(game.frame(0.25), 1);
    assert_eq!(state(&game, "jumps"), Value::Int(1));
}

const MAPPED: &str = r#"
@derive(Eq, Hash, InputAction)
export enum Act {
    Jump;
    Fire;
}

export const CONTROLS: InputMap<Act> = InputMap::new()
    .key(Key::Space, Act::Jump)
    .pad(PadButton::South, Act::Jump)
    .key(Key::J, Act::Fire)
    .pad(PadButton::West, Act::Fire)
    .move_axes(KeySet::arrows(), PadStick::Right)
    .dead_zone(0.25);

system Shooter implements FixedUpdate {
    state shots = 0;
    state x = 0.0;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(Act::Fire) {
            shots += 1;
        }
        x = frame.input.move_axes().relative_to(0.0).x;
    }
}
"#;

#[test]
fn an_input_map_is_evaluated_into_the_module() {
    let module = module(MAPPED);
    let input = module.input().expect("an input schema");
    assert_eq!(&*input.name, "Act");
    assert_eq!(
        input.actions.iter().map(|a| &**a).collect::<Vec<_>>(),
        ["Jump", "Fire"]
    );
    assert_eq!(input.bindings.keys.len(), 2);
    assert_eq!(input.bindings.pads.len(), 2);
    assert_eq!(input.bindings.dead_zone, 0.25);
    let source = input.bindings.move_axes.expect("move axes");
    assert_eq!(
        (source.keys.up, source.stick),
        (Key::ArrowUp, PadStick::Right)
    );
    let decoded = Module::decode(&module.encode()).expect("decode");
    assert_eq!(decoded.input(), module.input());

    let mut game = scheduler(module);
    game.key(Key::J, true);
    // The default set's bindings are gone: Space is `Act::Jump`, not `Fire`.
    game.key(Key::Space, true);
    game.stick(PadStick::Right, 1.0, 0.0);
    assert_eq!(game.frame(0.25), 1);
    assert_eq!(state(&game, "shots"), Value::Int(1));
    assert_eq!(state(&game, "x"), Value::Float(1.0));
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
}

#[test]
fn the_default_set_can_be_remapped() {
    let module = module(
        "const C: InputMap<InputAction> = InputMap::new().key(Key::K, InputAction::fire);\n\
         system S implements FixedUpdate { state n = 0; action fixed_update(frame: FixedFrame) { if frame.input.pressed(InputAction::fire) { n += 1; } } }",
    );
    let input = module.input().expect("an input schema");
    assert_eq!(input.actions.len(), InputAction::VARIANTS.len());
    let mut game = scheduler(module);
    game.key(Key::J, true);
    game.key(Key::K, true);
    game.frame(0.25);
    assert_eq!(state(&game, "n"), Value::Int(1));
}

#[test]
fn an_action_has_the_type_of_the_mapped_enum() {
    let source = MAPPED.replace(
        "frame.input.pressed(Act::Fire)",
        "frame.input.pressed(InputAction::fire)",
    );
    assert_eq!(codes(&source), ["E2103"]);
    let source = MAPPED.replace(".key(Key::J, Act::Fire)", ".key(Key::J, 1)");
    assert_eq!(codes(&source), ["E2103"]);
}

#[test]
fn a_derive_is_checked() {
    assert_eq!(codes("@derive(Nope) enum E { A; }"), ["E2001"]);
    assert_eq!(
        codes("@derive(Eq, Hash, StableKey) enum E { A; }"),
        Vec::<String>::new()
    );
    assert_eq!(
        codes("@derive(InputAction) enum E { A; B(I64); }"),
        ["E2201"]
    );
    assert_eq!(codes("@derive(x: Eq) enum E { A; }"), ["E2001"]);
}

#[test]
fn an_input_map_maps_an_input_action_enum() {
    assert_eq!(
        codes("enum E { A; }\nconst C: InputMap<E> = InputMap::new();"),
        ["E2201"]
    );
    assert_eq!(codes("const C: InputMap = InputMap::new();"), ["E2201"]);
    let twice =
        format!("{MAPPED}\nconst MORE: InputMap<Act> = InputMap::new().key(Key::K, Act::Fire);");
    assert_eq!(codes(&twice), ["E2202"]);
}

#[test]
fn an_input_map_is_a_compile_time_constant() {
    let source = "@derive(InputAction) enum A { Go; }\nfn pick() -> A { return A::Go; }\n\
         const C: InputMap<A> = InputMap::new().key(Key::Space, pick());";
    assert_eq!(codes(source), ["E2501"]);
    assert_eq!(
        codes(
            "@derive(InputAction) enum A { Go; }\nconst C: InputMap<A> = InputMap::new().dead_zone(1.5);"
        ),
        ["E2112"]
    );
    let negated = module(
        "@derive(InputAction) enum A { Go; }\nconst C: InputMap<A> = InputMap::new().key(Key::G, A::Go).dead_zone(-(-0.5));\nsystem Host {}",
    );
    let input = negated.input().expect("an input schema");
    assert_eq!(input.bindings.dead_zone, 0.5);
}

#[test]
fn a_property_is_read_without_a_call() {
    let call = "system S implements FixedUpdate { action fixed_update(frame: FixedFrame) { let i = frame.input(); } }";
    assert_eq!(codes(call), ["E2103"]);
    let unknown = "system S implements FixedUpdate { action fixed_update(frame: FixedFrame) { let i = frame.inputs; } }";
    assert_eq!(codes(unknown), ["E2001"]);
}

#[test]
fn an_action_without_a_binding_for_a_target_device_is_warned() {
    let source = format!(
        "{IMPORTS}@derive(InputAction) enum A {{ Jump; Fire; }}\nsystem Host {{}}\n\
         const C: InputMap<A> = InputMap::new()\n\
             .key(Key::Space, A::Jump).pad(PadButton::South, A::Jump)\n\
             .key(Key::J, A::Fire).touch(TouchButton::Primary, A::Fire);"
    );
    let devices = InputDevices {
        gamepad: true,
        touch: true,
    };
    let compiled = compile_file_for(&source, &origin(), Natives::standard(), devices);
    let warnings: Vec<_> = compiled
        .diagnostics
        .iter()
        .filter(|d| d.code == "E9107")
        .collect();
    assert!(warnings.iter().all(|d| d.severity == Severity::Warning));
    let messages: Vec<&str> = warnings.iter().map(|d| &*d.message).collect();
    assert_eq!(messages.len(), 2, "{messages:#?}");
    assert!(
        messages[0].contains("`A::Jump` has no touch binding"),
        "{messages:#?}"
    );
    assert!(
        messages[1].contains("`A::Fire` has no gamepad binding"),
        "{messages:#?}"
    );
    assert!(compiled.errors().next().is_none());

    let quiet = compile_file(&source, &origin());
    assert!(quiet.diagnostics.iter().all(|d| d.code != "E9107"));
}
