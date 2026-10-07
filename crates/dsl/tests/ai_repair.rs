//! The AI generation loop (§157): sources written with the mistakes generated
//! code makes most — the removed `child` and event arrow, `Float`, a misspelled
//! property, event, method, action or variant, a number where text goes, mixed
//! numeric types, `?.` on a value that is not optional — are repaired by
//! applying each first error's first fix until none is left, with no other
//! input, and the repaired program then does what the prompt asked, checked by
//! an event trace or a game tape rather than by compiling alone.

mod support;

use std::ops::Range;
use std::rc::Rc;

use support::{Rt, origin};
use viso_behavior::game::{PadStick, Scheduler};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Value, Vm};
use viso_dsl::Severity;
use viso_dsl::edit::Document;
use viso_dsl::frontend::compile_file;

/// The most fix rounds a case may take.
const MAX_ROUNDS: usize = 8;

/// What the loop did to one source.
#[derive(Debug)]
struct Repair {
    /// The source with every applied fix.
    source: String,
    /// The first pass reported no syntax error (`E1xxx`, or a removed form
    /// the parser rejects).
    parsed: bool,
    /// The first pass reported no error at all.
    checked: bool,
    /// The fixes applied, by the code they repaired.
    applied: Vec<&'static str>,
}

/// The codes the parser itself reports.
fn is_syntax(code: &str) -> bool {
    code.starts_with("E1") || matches!(code, "E3001" | "E3201")
}

/// Applies the first error's first fix until no error is left.
fn repair(broken: &str) -> Repair {
    let mut source = broken.to_owned();
    let mut applied = Vec::new();
    let (mut parsed, mut checked) = (true, true);
    for round in 0..=MAX_ROUNDS {
        let document = Document::new(source.as_str(), &origin());
        let errors: Vec<_> = document
            .diagnostics()
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .collect();
        if round == 0 {
            parsed = !errors.iter().any(|d| is_syntax(d.code));
            checked = errors.is_empty();
        }
        let Some(error) = errors.first() else {
            return Repair {
                source,
                parsed,
                checked,
                applied,
            };
        };
        assert!(round < MAX_ROUNDS, "no fixpoint for:\n{broken}");
        let fix = error
            .fixes
            .first()
            .unwrap_or_else(|| panic!("{} has no fix: {}\n{source}", error.code, error.message));
        let mut edits = fix.edits.clone();
        assert!(edits.iter().all(|e| e.module.is_none()), "{fix:?}");
        edits.sort_by_key(|e| std::cmp::Reverse(e.range.start()));
        for edit in edits {
            source.replace_range(Range::<usize>::from(edit.range), &edit.replacement);
        }
        applied.push(error.code);
    }
    unreachable!()
}

/// Reads a state of the mounted view.
fn value(rt: &Rt, name: &str) -> Value {
    let host = rt.view.as_ref().expect("a view").borrow();
    host.state(host.state_slot(name).expect("a state"))
        .cloned()
        .expect("a value")
}

/// The texts under the root, in pre-order.
fn texts(rt: &Rt) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![rt.root.expect("mounted")];
    while let Some(at) = stack.pop() {
        if let Some(request) = rt.store.text_request(at) {
            out.push(request.text.clone());
        }
        stack.extend(rt.children(at).into_iter().rev());
    }
    out
}

/// A case: what the prompt asked, the generated source, the codes its repair
/// takes in order, and the behavior check.
struct Case {
    prompt: &'static str,
    broken: &'static str,
    repairs: &'static [&'static str],
    check: fn(&str),
}

const CASES: &[Case] = &[
    Case {
        prompt: "a counter a click increments",
        broken: r#"
export component Counter {
    state count = 0;
    view {
        Column {
            child Text { width: 20dp; height: 20dp; text: "Add"; on click => count += 1; }
        }
    }
}
"#,
        repairs: &["E3001", "E3201"],
        check: |source| {
            let mut rt = Rt::mount(source);
            rt.click(0);
            rt.click(0);
            assert_eq!(rt.int("count"), Some(2));
        },
    },
    Case {
        prompt: "a switch a click toggles",
        broken: r#"
export component Switch {
    state on = false;
    view {
        Column {
            Text { width: 20dp; heigth: 20dp; text: "Toggle"; on clik { on = !on; } }
        }
    }
}
"#,
        repairs: &["E3101", "E3202"],
        check: |source| {
            let mut rt = Rt::mount(source);
            rt.click(0);
            assert_eq!(value(&rt, "on"), Value::bool(true));
        },
    },
    Case {
        prompt: "a label showing how many clicks there were",
        broken: r#"
export component Clicks {
    state count = 0;
    view {
        Column {
            Text { width: 20dp; height: 20dp; text: count; on click { count += 1; } }
        }
    }
}
"#,
        repairs: &["E2103"],
        check: |source| {
            let mut rt = Rt::mount(source);
            assert_eq!(texts(&rt), ["0"]);
            rt.click(0);
            assert_eq!(texts(&rt), ["1"]);
        },
    },
    Case {
        prompt: "a total that grows by count times a scale",
        broken: r#"
export component Scaled {
    state count = 3;
    state scale: Float = 1.5;
    state total = 0.0;
    view {
        Column {
            Text { width: 20dp; height: 20dp; text: "Add"; on click { total = total + count * scale; } }
        }
    }
}
"#,
        repairs: &["E2101", "E2102"],
        check: |source| {
            let mut rt = Rt::mount(source);
            rt.click(0);
            rt.click(0);
            assert_eq!(value(&rt, "total"), Value::Float(9.0));
        },
    },
    Case {
        prompt: "a list a click appends to",
        broken: r#"
export component Names {
    state names: List<String> = [];
    action ad() {
        names.psh("n");
    }
    view {
        Column {
            Text { width: 20dp; height: 20dp; text: "Add"; on click { add(); } }
            for name in names key name {
                Text { text: name; }
            }
        }
    }
}
"#,
        repairs: &["E2001", "E2001"],
        check: |source| {
            let mut rt = Rt::mount(source);
            rt.click(0);
            assert_eq!(texts(&rt), ["Add", "n"]);
        },
    },
    Case {
        prompt: "a click moves a point right",
        broken: r#"
record Point { x: I64; y: I64; }

export component Mover {
    state at = Point { x: 0, y: 0 };
    state seen = 0;
    view {
        Column {
            Text { width: 20dp; height: 20dp; text: "Move"; on click { at = Point { x: at?.x + 1, y: at.y }; seen = at.x; } }
        }
    }
}
"#,
        repairs: &["E2103"],
        check: |source| {
            let mut rt = Rt::mount(source);
            rt.click(0);
            rt.click(0);
            assert_eq!(rt.int("seen"), Some(2));
        },
    },
];

#[test]
fn each_generated_ui_is_repaired_by_its_own_fixes_and_behaves() {
    for case in CASES {
        let repaired = repair(case.broken);
        assert_eq!(
            repaired.applied, case.repairs,
            "{}:\n{}",
            case.prompt, repaired.source
        );
        (case.check)(&repaired.source);
    }
}

const GAME: &str = r#"
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{InputAxis};

export system Runner implements QuickGame {
    state x: Float = 0.0;

    action start(cx: QuickStart) {
        x = 0.0;
    }

    action fixed(frame: QuickFrame) {
        x = x + frame.input.axis(InputAxis::mov_x) * frame.dt();
    }
}
"#;

/// `x` of `Runner` after each frame of a stick tape.
fn tape(source: &str) -> Vec<Value> {
    let compiled = compile_file(source, &origin());
    let module = compiled.behavior.bytecode().expect("verified bytecode");
    let module = Rc::new(module.with_tick_rate(10).expect("a tick rate"));
    let mut vm = Vm::new(module, Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    let mut game = Scheduler::new(vm).expect("the game starts");
    let component = game.vm().module().component("Runner").expect("system");
    let slot = game
        .vm()
        .module()
        .layout(component)
        .state("x")
        .expect("state");
    let mut trace = Vec::new();
    for (stick, dt) in [(1.0, 0.3), (-1.0, 0.1), (0.0, 0.2)] {
        game.stick(PadStick::Left, stick, 0.0);
        game.frame(dt);
        trace.push(game.instance(0).states()[slot].clone());
    }
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
    trace
}

#[test]
fn a_generated_game_is_repaired_and_plays_its_tape() {
    let repaired = repair(GAME);
    assert_eq!(repaired.applied, ["E2101", "E2001"], "{}", repaired.source);
    let trace = tape(&repaired.source);
    let [Value::Float(right), Value::Float(back), Value::Float(still)] = trace[..] else {
        panic!("{trace:?}");
    };
    assert!(right > 0.0 && back < right, "the stick moves it: {trace:?}");
    assert_eq!(back, still, "a still stick holds the point");
}

/// The loop's metrics over the whole set (§157): the first-pass parse and
/// check rates, the mean rounds, and the diagnostic-guided repair rate, which
/// must be total — every generated mistake carries its own fix.
#[test]
fn the_loop_metrics_over_the_set() {
    let sources: Vec<&str> = CASES.iter().map(|c| c.broken).chain([GAME]).collect();
    let repairs: Vec<Repair> = sources.iter().map(|s| repair(s)).collect();
    let n = repairs.len() as f64;
    let parse_rate = repairs.iter().filter(|r| r.parsed).count() as f64 / n;
    let check_rate = repairs.iter().filter(|r| r.checked).count() as f64 / n;
    let rounds = repairs.iter().map(|r| r.applied.len()).sum::<usize>() as f64 / n;
    assert_eq!(
        (parse_rate * n) as usize,
        sources.len() - 1,
        "one case has syntax errors"
    );
    assert_eq!(check_rate, 0.0, "every case starts broken");
    assert!(rounds <= 2.0, "mean rounds {rounds}");
    for repair in &repairs {
        let document = Document::new(repair.source.as_str(), &origin());
        assert!(
            document
                .diagnostics()
                .iter()
                .all(|d| d.severity != Severity::Error),
            "{}",
            repair.source
        );
    }
}
