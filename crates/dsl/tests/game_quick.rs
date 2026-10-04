//! Quick Game: a `system ... implements QuickGame` runs its `start` once before
//! the first tick as the startup transaction and its `fixed` every tick, and
//! computes on an input tape exactly what the same game split into `Startup`
//! and `FixedUpdate` hooks does.

use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::game::{GameSnapshot, Key, PadStick, Scheduler};
use viso_behavior::native::{NativeLibrary, Natives, STANDARD};
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_in};

fn origin() -> Origin {
    Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    }
}

/// Records what `app::fx::play` delivers.
#[derive(Default)]
struct Played(Vec<i64>);

static FX: NativeLibrary = NativeLibrary {
    path: "app::fx",
    version: 1,
    functions: &[viso_behavior::native!(action "play" |cx, id: i64| -> () {
        cx.service::<Played>()?.0.push(id);
        Ok(())
    })
    .presentation()],
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

fn compiled(source: &str) -> viso_dsl::frontend::Compiled {
    compile_file_in(source, &origin(), natives())
}

fn codes(source: &str) -> Vec<String> {
    compiled(source)
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

fn module(source: &str) -> Rc<Module> {
    let compiled = compiled(source);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = compiled.behavior.bytecode().expect("verified bytecode");
    Rc::new(module.with_tick_rate(4).expect("a tick rate"))
}

fn start(module: Rc<Module>) -> Result<Scheduler, viso_behavior::game::SystemFault> {
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&natives(), &[]).expect("link");
    vm.services_mut().insert(Played::default());
    Scheduler::new(vm)
}

/// State `name` of system `system`.
fn state(game: &Scheduler, system: &str, name: &str) -> Value {
    let module = game.vm().module();
    let component = module.component(system).expect("system");
    let index = module
        .systems()
        .iter()
        .position(|s| s.component == component)
        .expect("a system");
    let slot = module.layout(component).state(name).expect("state");
    game.instance(index).states()[slot].clone()
}

fn played(game: &mut Scheduler) -> Vec<i64> {
    let sink = game.services_mut().get_mut::<Played>().expect("the sink");
    std::mem::take(&mut sink.0)
}

const QUICK: &str = r#"
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{InputAction, InputAxis};
import app::fx;

export system Tiny implements QuickGame {
    state lives = 0;
    state x = 0.0;
    state jumps = 0;
    state ticks = 0;

    action start(cx: QuickStart) {
        lives = 2;
        x = 1.5;
        fx::play(cx.tick() + 50);
    }

    action fixed(frame: QuickFrame) {
        ticks += 1;
        x = x + frame.input.axis(InputAxis::move_x) * frame.dt();
        if frame.input.pressed(InputAction::jump) && lives > 0 {
            jumps += 1;
            lives -= 1;
        }
        fx::play(frame.tick());
    }
}
"#;

/// [`QUICK`] as the full Game Profile writes it.
fn split() -> String {
    QUICK
        .replace(
            "import viso::game::quick::{QuickGame, QuickStart, QuickFrame};\n",
            "",
        )
        .replace(
            "import viso::game::{InputAction, InputAxis};",
            "import viso::game::{Startup, FixedUpdate, GameStart, FixedFrame, InputAction, InputAxis};",
        )
        .replace("implements QuickGame", "implements Startup + FixedUpdate")
        .replace("action start(cx: QuickStart)", "action startup(cx: GameStart)")
        .replace("action fixed(frame: QuickFrame)", "action fixed_update(frame: FixedFrame)")
}

/// One frame of an input tape: what the host reports, then the wall time.
enum Input {
    Key(Key, bool),
    Stick(f64, f64),
}

fn tape() -> Vec<(Vec<Input>, f64)> {
    vec![
        (vec![], 0.25),
        (
            vec![Input::Key(Key::Space, true), Input::Stick(1.0, 0.0)],
            0.25,
        ),
        (vec![Input::Key(Key::Space, false)], 0.5),
        (
            vec![Input::Key(Key::Space, true), Input::Key(Key::Space, false)],
            0.1,
        ),
        (vec![Input::Stick(-0.5, 0.0)], 0.4),
        (vec![Input::Key(Key::Space, true)], 0.25),
        (vec![Input::Key(Key::D, true)], 0.75),
    ]
}

/// The states and the delivered commands after each frame.
type Trace = Vec<(Vec<Value>, Vec<i64>)>;

/// Plays [`tape`] on the game `source` compiles to: its trace and the final
/// snapshot.
fn play(source: &str) -> (Trace, GameSnapshot) {
    let mut game = start(module(source)).expect("the game starts");
    let read = |game: &Scheduler| -> Vec<Value> {
        ["lives", "x", "jumps", "ticks"]
            .iter()
            .map(|name| state(game, "Tiny", name))
            .collect()
    };
    let mut trace = vec![(read(&game), played(&mut game))];
    for (inputs, dt) in tape() {
        for input in inputs {
            match input {
                Input::Key(key, down) => game.key(key, down),
                Input::Stick(x, y) => game.stick(PadStick::Left, x, y),
            }
        }
        game.frame(dt);
        trace.push((read(&game), played(&mut game)));
    }
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
    (trace, game.snapshot())
}

#[test]
fn start_runs_once_before_the_first_tick() {
    let mut game = start(module(QUICK)).expect("the game starts");
    assert_eq!(state(&game, "Tiny", "lives"), Value::Int(2));
    assert_eq!(state(&game, "Tiny", "ticks"), Value::Int(0));
    assert_eq!(
        played(&mut game),
        [50],
        "the start's command lands before tick 0"
    );

    game.key(Key::Space, true);
    assert_eq!(game.frame(0.5), 2);
    assert_eq!(state(&game, "Tiny", "jumps"), Value::Int(1));
    assert_eq!(state(&game, "Tiny", "lives"), Value::Int(1));
    assert_eq!(played(&mut game), [0, 1], "start does not run again");
}

#[test]
fn a_quick_game_computes_what_its_split_form_does_on_one_tape() {
    let (quick, quick_snapshot) = play(QUICK);
    let (full, full_snapshot) = play(&split());
    assert_eq!(quick, full);
    assert_eq!(quick_snapshot.tick(), full_snapshot.tick());
    assert_eq!(
        quick_snapshot.states(),
        full_snapshot.states(),
        "the same Simulation states"
    );
    let last = &quick.last().expect("frames").0;
    assert_ne!(last[3], Value::Int(0), "the tape ran ticks");

    // Splitting keeps every state's identity and schema: a quick game's
    // snapshot restores into its split form whole.
    let mut split = start(module(&split())).expect("the game starts");
    let restored = split.restore(&quick_snapshot);
    assert_eq!(
        (restored.states, restored.mismatched, restored.missing),
        (4, 0, 0)
    );
    assert_eq!(state(&split, "Tiny", "x"), last[1]);
}

#[test]
fn start_is_simulation() {
    let source = QUICK
        .replace(
            "import app::fx;",
            "import app::fx;\nimport viso::time::Stopwatch;",
        )
        .replace(
            "lives = 2;",
            "lives = 2;\n        let watch = Stopwatch::start();",
        );
    assert!(
        codes(&source).contains(&"E9104".to_owned()),
        "{:?}",
        codes(&source)
    );
}

const TWO: &str = r#"
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{Startup, GameStart};
import app::fx;

system First implements Startup {
    state ready = false;
    action startup(cx: GameStart) {
        ready = true;
        fx::play(1);
    }
}

@after(First)
export system Second implements QuickGame {
    state at = 0;
    action start(cx: QuickStart) {
        let xs = [1];
        at = xs[DIVISOR];
    }
    action fixed(frame: QuickFrame) {}
}
"#;

#[test]
fn a_faulting_start_starts_no_game() {
    let fault = start(module(&TWO.replace("DIVISOR", "3")))
        .err()
        .expect("the start faults");
    assert_eq!(fault.tick, 0);
    let module = module(&TWO.replace("DIVISOR", "0"));
    let second = module.component("Second").expect("Second");
    let index = module
        .systems()
        .iter()
        .position(|s| s.component == second)
        .expect("a system");
    assert_eq!(fault.system, index, "the second system's start faulted");

    let mut game = start(module).expect("the game starts");
    assert_eq!(state(&game, "First", "ready"), Value::bool(true));
    assert_eq!(played(&mut game), [1]);
}

#[test]
fn quick_hooks_are_checked_like_any_trait_hook() {
    let wrong = QUICK.replace(
        "action fixed(frame: QuickFrame)",
        "action fixed(frame: QuickStart)",
    );
    assert!(
        codes(&wrong).contains(&"E2201".to_owned()),
        "{:?}",
        codes(&wrong)
    );
    let missing = QUICK.replace(
        "action start(cx: QuickStart)",
        "action begin(cx: QuickStart)",
    );
    assert_eq!(codes(&missing), ["E2201"]);
}
