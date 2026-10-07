//! Game state layers: `@local` state, the Simulation domain's determinism and
//! snapshot rules, and Presentation commands deferred and keyed per tick.

use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::game::Scheduler;
use viso_behavior::native::{NativeLibrary, Natives, STANDARD};
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

const IMPORTS: &str = "import viso::game::{FixedUpdate, FrameUpdate, FixedFrame, RenderFrame};\nimport viso::time::Stopwatch;\nimport viso::text;\nimport app::fx;\n";

/// Records what the `app::fx` commands deliver.
#[derive(Default)]
struct Played(Vec<i64>);

static FX: NativeLibrary = NativeLibrary {
    path: "app::fx",
    version: 1,
    functions: &[
        viso_behavior::native!(action "play" |cx, id: i64| -> () {
            cx.service::<Played>()?.0.push(id);
            Ok(())
        })
        .presentation(),
        viso_behavior::native!(action "line" |cx, id: i64| -> () {
            cx.service::<Played>()?.0.push(-id);
            Ok(())
        })
        .debug_draw(),
        viso_behavior::native!(fn "camera" |_cx| -> f64 { Ok(1.0) }).presentation(),
        // The host `sin` reproduces on one build and target only.
        viso_behavior::native!(fn "wobble" |_cx, x: f64| -> f64 { Ok(x.sin()) })
            .deterministic()
            .reproducible(viso_behavior::native::Determinism::SameBinary),
    ],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[],
};

fn natives() -> Arc<Natives> {
    let mut natives = Natives::new();
    natives.extend(STANDARD).expect("standard");
    natives.register(&FX).expect("fx");
    Arc::new(natives)
}

/// Compiles `source` for a 4 Hz fixed step under `profile`.
fn compile(source: &str, profile: TargetProfile) -> viso_dsl::frontend::Compiled {
    let profile = TargetProfile {
        tick_rate: 4,
        ..profile
    };
    compile_file_for(&format!("{IMPORTS}{source}"), &origin(), natives(), profile)
}

fn codes_with(source: &str, profile: TargetProfile) -> Vec<String> {
    compile(source, profile)
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

fn codes(source: &str) -> Vec<String> {
    codes_with(source, TargetProfile::default())
}

fn module(source: &str, profile: TargetProfile) -> Rc<Module> {
    let compiled = compile(source, profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn scheduler(module: Rc<Module>) -> Scheduler {
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&natives(), &[]).expect("link");
    vm.services_mut().insert(Played::default());
    Scheduler::new(vm).expect("systems instantiate")
}

fn played(game: &mut Scheduler) -> Vec<i64> {
    game.services_mut()
        .get_mut::<Played>()
        .expect("the sink")
        .0
        .clone()
}

const LAYERS: &str = r#"
system Hud implements FixedUpdate + FrameUpdate {
    state ticks = 0;
    @local state frames = 0;
    @local state watch: Option<Stopwatch> = Option::None;

    action fixed_update(frame: FixedFrame) {
        ticks += 1;
        let label = text::upper("tick");
    }

    action frame_update(frame: RenderFrame) {
        frames += ticks;
        watch = Option::Some(Stopwatch::start());
    }
}
"#;

#[test]
fn presentation_owns_local_state_and_reads_the_simulation() {
    let mut game = scheduler(module(LAYERS, TargetProfile::default()));
    assert_eq!(game.frame(0.5), 2);
    let module = game.vm().module().clone();
    let layout = module.layout(module.systems()[0].component);
    let frames = layout.state("frames").expect("frames");
    assert_eq!(game.instance(0).states()[frames], Value::Int(2));
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
}

#[test]
fn the_simulation_does_not_touch_local_state() {
    let write = LAYERS.replace("ticks += 1;", "ticks += 1;\n        frames = 0;");
    assert_eq!(codes(&write), ["E9103"]);

    let through = LAYERS
        .replace("ticks += 1;", "ticks += 1;\n        ticks = seen();")
        .replace(
            "action fixed_update",
            "fn seen() -> I64 { return frames; }\n\n    action fixed_update",
        );
    let compiled = compile(&through, TargetProfile::default());
    let errors: Vec<_> = compiled.errors().collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(errors[0].code, "E9103");
    assert!(
        errors[0].notes[0].contains("`Hud.seen` runs in the Simulation domain"),
        "{:?}",
        errors[0].notes
    );
}

#[test]
fn local_marks_a_system_state() {
    assert_eq!(codes("system S { @local action go() {} }"), ["E9103"]);
    assert_eq!(
        codes("component C { @local state n = 0; view { Text { text: \"\"; } } }"),
        ["E9103"]
    );
}

#[test]
fn the_simulation_reaches_no_non_deterministic_source() {
    let clock = LAYERS.replace(
        "let label = text::upper(\"tick\");",
        "let w = Stopwatch::start();",
    );
    assert_eq!(codes(&clock), ["E9104"]);

    let helper = LAYERS
        .replace(
            "let label = text::upper(\"tick\");",
            "let ms = elapsed();",
        )
        .replace(
            "action fixed_update",
            "action elapsed() -> F64 { return Stopwatch::start().elapsed_ms(); }\n\n    action fixed_update",
        );
    let errors: Vec<_> = compile(&helper, TargetProfile::default())
        .errors()
        .map(|d| d.code.to_string())
        .collect();
    assert_eq!(errors, ["E9104", "E9104"]);
}

/// The Simulation domain's E9104 messages for `body` in place of the
/// fixed hook's label line, with the import it needs.
fn simulation_reasons(body: &str) -> Vec<String> {
    let source = format!(
        "import viso::time;\n{}",
        LAYERS.replace("let label = text::upper(\"tick\");", body)
    );
    compile(&source, TargetProfile::default())
        .errors()
        .filter(|d| d.code == "E9104")
        .map(|d| d.message.clone())
        .collect()
}

#[test]
fn the_simulation_reads_no_environment_and_awaits_no_task() {
    let reasons = simulation_reasons("let w = env.size_class;");
    assert_eq!(
        reasons,
        ["the Simulation domain does not read the adaptive environment"]
    );
    let reasons = simulation_reasons("await time::sleep(1s);");
    assert!(
        reasons
            .iter()
            .any(|m| m == "the Simulation domain does not `await`"),
        "{reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|m| m.contains("`sleep` is a native task")),
        "{reasons:?}"
    );
    // A native task named without `await` is still one.
    let reasons = simulation_reasons("let x = time::sleep(1s);");
    assert_eq!(
        reasons,
        ["`sleep` is a native task, which the Simulation domain does not await"]
    );
}

#[test]
fn host_floating_point_meets_same_binary_but_not_cross_platform() {
    let source = r#"
import viso::game::{FixedUpdate, FixedFrame};
import app::fx;
system Walker implements FixedUpdate {
    state x = 0.0;
    action fixed_update(frame: FixedFrame) {
        x = fx::wobble(frame.input.move_axes().relative_to(1.5).x);
    }
}
"#;
    assert_eq!(codes(source), Vec::<String>::new());
    let strict = TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    };
    assert_eq!(codes_with(source, strict.clone()), ["E9104"]);
    let software = source
        .replace("fx::wobble(", "math::sin(")
        .replace("import app::fx;", "import viso::math;");
    assert_eq!(
        codes_with(&software, strict),
        Vec::<String>::new(),
        "`viso::math` and the input's lengths and turns are `cross_platform`"
    );
}

#[test]
fn simulation_state_must_snapshot() {
    let handle = LAYERS.replace(
        "state ticks = 0;",
        "state ticks = 0;\n    state held: Option<Stopwatch> = Option::None;",
    );
    assert_eq!(codes(&handle), ["E9105"]);
    let nested = LAYERS
        .replace(
            "system Hud",
            "record Lap { watch: Option<Stopwatch>; }\n\nsystem Hud",
        )
        .replace(
            "state ticks = 0;",
            "state ticks = 0;\n    state laps: List<Lap> = [];",
        );
    assert_eq!(codes(&nested), ["E9105"]);
    let values = LAYERS.replace(
        "state ticks = 0;",
        "state ticks = 0;\n    state pos: (F64, F64) = (0.0, 0.0);\n    state names: List<String> = [];",
    );
    assert_eq!(codes(&values), Vec::<String>::new());
}

const COMMANDS: &str = r#"
system Fx implements FixedUpdate + FrameUpdate {
    state n = 0;

    action fixed_update(frame: FixedFrame) {
        fx::play(frame.tick());
        fx::line(frame.tick());
        n += 1;
    }

    action frame_update(frame: RenderFrame) {
        fx::play(100);
    }
}

system Late implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {
        fx::play(10 + frame.tick());
    }
}
"#;

#[test]
fn presentation_commands_are_delivered_once_per_tick_in_key_order() {
    let mut game = scheduler(module(COMMANDS, TargetProfile::default()));
    assert_eq!(game.frame(0.5), 2);
    assert_eq!(played(&mut game), [0, 0, 10, 1, -1, 11, 100]);
    assert_eq!(game.delivered_commands(), 6);

    // Replaying ticks 0 and 1 issues their commands again, delivering none.
    game.rewind_to(0);
    assert_eq!(game.frame(0.5), 2);
    assert_eq!(played(&mut game), [0, 0, 10, 1, -1, 11, 100, 100]);
    assert_eq!(game.replayed_commands(), 6);
    assert_eq!(game.frame(0.25), 1);
    assert_eq!(played(&mut game)[8..], [2, -2, 12, 100]);
}

#[test]
fn a_faulting_hook_issues_no_command() {
    let source = COMMANDS.replace(
        "        n += 1;",
        "        n += 1;\n        let xs = [1];\n        n = xs[frame.tick() + 5];",
    );
    let mut game = scheduler(module(&source, TargetProfile::default()));
    game.frame(0.25);
    assert_eq!(played(&mut game), [10, 100]);
    assert_eq!(game.take_faults().len(), 1);
}

#[test]
fn a_release_build_removes_debug_draw() {
    let release = TargetProfile {
        release: true,
        ..TargetProfile::default()
    };
    let mut game = scheduler(module(COMMANDS, release));
    game.frame(0.25);
    assert_eq!(played(&mut game), [0, 10, 100]);
}

#[test]
fn the_simulation_does_not_use_a_presentation_value() {
    let source = COMMANDS.replace("n += 1;", "n += 1;\n        let zoom = fx::camera();");
    assert_eq!(codes(&source), ["E9103"]);
    let presentation = COMMANDS.replace(
        "fx::play(100);",
        "fx::play(100);\n        let zoom = fx::camera();",
    );
    assert_eq!(codes(&presentation), Vec::<String>::new());
}

#[test]
fn the_simulation_starts_no_task() {
    let source = LAYERS
        .replace("let label = text::upper(\"tick\");", "load();")
        .replace(
            "action fixed_update",
            "task load() {}\n\n    action fixed_update",
        );
    let codes = codes(&source);
    assert!(codes.contains(&"E9104".to_owned()), "{codes:?}");
}
