//! Human usability samples (§156): one source per task of the usability
//! study, each compiling with no diagnostic at all, and the behavior each task
//! asks for checked headless — the counter counts, the list adds, the layout
//! follows the size class, a local scope reads its own width, the Quick Game
//! and its split into systems agree on one tape, the diagnostic's own fix
//! repairs the broken source, and a hot reload keeps the input focus. The
//! formatter's stability over the same files is checked in `viso-lsp`.

mod support;

use std::rc::Rc;

use support::{Rt, origin};
use viso_behavior::game::{Key, PadStick, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Value, Vm};
use viso_dsl::edit::Document;
use viso_dsl::frontend::compile_file;
use viso_dsl::view_behavior::view_behavior;
use viso_view::{EventRoute, Scope};

/// Every sample, by file name.
const SAMPLES: [(&str, &str); 11] = [
    ("01-counter.vs", include_str!("usability/01-counter.vs")),
    ("02-form.vs", include_str!("usability/02-form.vs")),
    ("03-todo-list.vs", include_str!("usability/03-todo-list.vs")),
    ("04-search.vs", include_str!("usability/04-search.vs")),
    (
        "05-slot-component.vs",
        include_str!("usability/05-slot-component.vs"),
    ),
    (
        "06-size-class.vs",
        include_str!("usability/06-size-class.vs"),
    ),
    (
        "07-adaptive-scope.vs",
        include_str!("usability/07-adaptive-scope.vs"),
    ),
    (
        "08-quick-game.vs",
        include_str!("usability/08-quick-game.vs"),
    ),
    (
        "09-split-system.vs",
        include_str!("usability/09-split-system.vs"),
    ),
    (
        "10-fixed-diagnostic.vs",
        include_str!("usability/10-fixed-diagnostic.vs"),
    ),
    (
        "11-hot-reload-focus.vs",
        include_str!("usability/11-hot-reload-focus.vs"),
    ),
];

fn sample(name: &str) -> &'static str {
    SAMPLES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, source)| *source)
        .expect("a sample")
}

#[test]
fn every_sample_compiles_without_a_diagnostic() {
    for (name, source) in SAMPLES {
        let document = Document::new(source, &origin());
        let found: Vec<_> = document
            .diagnostics()
            .iter()
            .map(|d| format!("{} {}", d.code, d.message))
            .collect();
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
}

/// The handler entries of `source`'s `click` routes, in view order.
fn clicks(source: &str) -> Vec<u32> {
    let view = view_behavior(&compile_file(source, &origin()))
        .expect("mounts")
        .expect("has behavior");
    view.nodes()
        .flat_map(|(_, routes)| routes.iter())
        .filter(|route| route.event == EventRoute::Click)
        .map(|route| route.handler)
        .collect()
}

impl Rt {
    /// Runs handler `handler` as its event would, then a frame.
    fn fire(&mut self, handler: u32) {
        let host = self.view.as_ref().expect("a view");
        assert!(
            host.borrow_mut()
                .dispatch(handler, Value::Nil, &Scope::EMPTY, &mut self.states),
            "{:?}",
            self.fault()
        );
        self.frame();
    }

    fn value(&self, name: &str) -> Value {
        let host = self.view.as_ref().expect("a view").borrow();
        host.state(host.state_slot(name).expect("a state"))
            .cloned()
            .expect("a value")
    }

    fn set(&mut self, name: &str, value: Value) {
        let host = self.view.as_ref().expect("a view");
        let slot = host.borrow().state_slot(name).expect("a state");
        host.borrow_mut().set_state(slot, value);
    }

    /// The text the `n`th Text under `node`, in pre-order, asks to show.
    fn texts_under(&self, node: viso_ui::NodeId) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![node];
        while let Some(at) = stack.pop() {
            if let Some(request) = self.store.text_request(at) {
                out.push(request.text.clone());
            }
            stack.extend(self.children(at).into_iter().rev());
        }
        out
    }
}

#[test]
fn the_counter_counts() {
    let source = sample("01-counter.vs");
    let [add] = clicks(source)[..] else {
        panic!("one click handler");
    };
    let mut rt = Rt::mount(source);
    for _ in 0..3 {
        rt.fire(add);
    }
    assert_eq!(rt.int("count"), Some(3));
}

#[test]
fn the_todo_list_adds_keyed_rows() {
    let source = sample("03-todo-list.vs");
    let [add] = clicks(source)[..] else {
        panic!("one click handler");
    };
    let mut rt = Rt::mount(source);
    for title in ["milk", "", "eggs"] {
        rt.set("draft", Value::str(title));
        rt.fire(add);
    }
    let Value::List(todos) = rt.value("todos") else {
        panic!("a list");
    };
    assert_eq!(todos.len(), 2, "an empty draft adds nothing");
    assert_eq!(rt.int("next_id"), Some(3));
    let root = rt.root.expect("mounted");
    let rows = rt.children(root);
    assert_eq!(rt.texts_under(rows[1]), ["milk"]);
    assert_eq!(rt.texts_under(rows[2]), ["eggs"]);
}

#[test]
fn the_shell_follows_the_size_class() {
    let mut rt = Rt::mount(sample("06-size-class.vs"));
    let mut seen = Vec::new();
    for width in [320.0, 700.0, 1000.0] {
        rt.update_env(|e| e.window.width = width);
        rt.frame();
        let root = rt.root.expect("mounted");
        seen.push(rt.texts_under(root));
    }
    assert_eq!(
        seen,
        [vec!["Phone"], vec!["Tablet"], vec!["Sidebar", "Desktop"]]
    );
}

#[test]
fn a_local_scope_reads_its_own_width_not_the_window() {
    let mut rt = Rt::mount(sample("07-adaptive-scope.vs"));
    for width in [320.0, 1200.0] {
        rt.update_env(|e| e.window.width = width);
        rt.frame();
        rt.update_env(|_| {});
        rt.frame();
        let root = rt.root.expect("mounted");
        let [narrow, wide] = rt.children(root)[..] else {
            panic!("two columns");
        };
        assert_eq!(rt.texts_under(narrow), ["Narrow"], "at {width}dp");
        assert_eq!(rt.texts_under(wide), ["Wide"], "at {width}dp");
    }
}

/// A game `source` declares, started.
fn game(source: &str) -> Scheduler {
    let compiled = compile_file(source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let module = compiled.behavior.bytecode().expect("verified bytecode");
    let module = Rc::new(module.with_tick_rate(10).expect("a tick rate"));
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::new(vm).expect("the game starts")
}

/// State `name` of system `system`.
fn system_state(game: &Scheduler, system: &str, name: &str) -> Value {
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

/// One input frame: key edges, the left stick's x, and the frame's seconds.
type Frame = (&'static [(Key, bool)], f64, f64);

/// `x` and `jumps` after each frame of one tape.
fn play(mut game: Scheduler, x: (&str, &str), jumps: (&str, &str)) -> Vec<(Value, Value)> {
    let mut trace = Vec::new();
    let tape: [Frame; 4] = [
        (&[(Key::Space, true)], 1.0, 0.3),
        (&[(Key::Space, false)], 0.5, 0.2),
        (&[(Key::Space, true), (Key::Space, false)], -1.0, 0.1),
        (&[], 0.0, 0.4),
    ];
    for (keys, stick, dt) in tape {
        for &(key, down) in keys {
            game.key(key, down);
        }
        game.stick(PadStick::Left, stick, 0.0);
        game.frame(dt);
        trace.push((
            system_state(&game, x.0, x.1),
            system_state(&game, jumps.0, jumps.1),
        ));
    }
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
    trace
}

#[test]
fn the_quick_game_and_its_split_into_systems_agree_on_one_tape() {
    let quick = play(
        game(sample("08-quick-game.vs")),
        ("Runner", "x"),
        ("Runner", "jumps"),
    );
    let split = play(
        game(sample("09-split-system.vs")),
        ("Movement", "x"),
        ("Jumping", "jumps"),
    );
    assert_eq!(quick, split);
    assert_ne!(quick[0].0, Value::Float(0.0), "the stick moved it");
    assert_eq!(quick.last().map(|(_, j)| j.clone()), Some(Value::Int(2)));
}

#[test]
fn the_diagnostic_fix_repairs_the_broken_source() {
    let fixed = sample("10-fixed-diagnostic.vs");
    let broken = fixed.replace("count as F64 * scale", "count * scale");
    let document = Document::new(&broken, &origin());
    let [diagnostic] = document.diagnostics() else {
        panic!("one diagnostic: {:#?}", document.diagnostics());
    };
    assert_eq!(diagnostic.code, "E2102");
    let fix = diagnostic.fixes.first().expect("a fix");
    let mut repaired = broken.clone();
    let mut edits = fix.edits.clone();
    edits.sort_by_key(|e| std::cmp::Reverse(e.range.start()));
    for edit in edits {
        repaired.replace_range(
            std::ops::Range::<usize>::from(edit.range),
            &edit.replacement,
        );
    }
    assert_eq!(repaired, fixed);
}

#[test]
fn a_hot_reload_keeps_the_input_focus() {
    let source = sample("11-hot-reload-focus.vs");
    let mut rt = Rt::mount(source);
    let root = rt.root.expect("mounted");
    let input = rt.children(root)[1];
    rt.store.set_focused(Some(input));
    rt.reload(&source.replace("\"Notes\"", "\"My notes\""));
    let root = rt.root.expect("mounted");
    let [label, input_now] = rt.children(root)[..] else {
        panic!("two children");
    };
    assert_eq!(
        rt.store.focused(),
        Some(input_now),
        "the input keeps the focus"
    );
    assert_eq!(rt.texts_under(label), ["My notes"]);
}
