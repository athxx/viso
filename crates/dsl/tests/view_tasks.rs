//! A view's tasks: `start` runs a task call on the UI task protocol after the
//! commit, in a named slot under its policy, and hands its value to the
//! `success` / `error` handler as a new transaction while the instance lives;
//! unmount and hot reload cancel. And the checks: `start` takes a task call
//! (`E4401`), only where a body may mutate; `await` belongs in a task
//! (`E4101`); a task reads no state once it may have suspended (`E4102`); a
//! module-level action starts nothing (`E4501`); policies and handlers
//! (`E4302`).

mod support;

use support::{Rt, origin};
use viso_behavior::FaultKind;
use viso_dsl::frontend::compile_file;

/// No diagnostic.
const CLEAN: [&str; 0] = [];

/// The diagnostic codes compiling `members` into a component reports.
fn codes(members: &str) -> Vec<&'static str> {
    let source = format!(
        "import viso::time;\nexport component C {{\n    state a = 0;\n    state b = 0;\n    \
         computed twice = a * 2;\n    fn peek() -> I64 {{ a }}\n    fn one() -> I64 {{ 1 }}\n    \
         task fetch(x: I64) -> Result<I64, String> {{ await time::sleep(1s); Ok(x) }}\n    \
         task wait() -> I64 {{ await time::sleep(1s); 3 }}\n    {members}\n    \
         view {{ Text {{}} }}\n}}\n"
    );
    module_codes(&source)
}

fn module_codes(source: &str) -> Vec<&'static str> {
    let compiled = compile_file(source, &origin());
    let diagnostics: Vec<_> = compiled.errors().collect();
    eprintln!("{diagnostics:#?}");
    diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn a_start_takes_a_task_call_with_typed_handlers() {
    assert_eq!(codes(""), CLEAN);
    assert_eq!(
        codes(
            "action go() { start fetch(a) as job { success(v) { b = v; } error(e) { b = 0; } \
             cancelled { b = -1; } }; }"
        ),
        CLEAN
    );
    assert_eq!(
        codes("action go() { start wait() { success(v) { b = v; } }; }"),
        CLEAN
    );
    assert_eq!(codes("action go() { start time::sleep(1s); }"), CLEAN);
    assert_eq!(
        codes("action go() { start wait() { error(e) { } }; }"),
        ["E2103"],
        "a task returning no `Result` has no error"
    );
    assert_eq!(
        codes("action go() { start fetch(1) { success(v) { let s: String = v; } }; }"),
        ["E2103"]
    );
    assert_eq!(codes("action go() { start one(); }"), ["E4401"]);
    assert_eq!(
        codes("action go() { start reset_b(); }  action reset_b() { b = 0; }"),
        ["E4401"]
    );
}

#[test]
fn a_start_belongs_where_a_body_may_mutate() {
    assert_eq!(codes("effect e { start wait(); }"), CLEAN);
    assert_eq!(codes("effect e { cleanup { start wait(); } }"), ["E2501"]);
    assert_eq!(codes("fn f() { start wait(); }"), ["E2501"]);
    assert_eq!(codes("task t() -> I64 { start wait(); 1 }"), ["E2501"]);
    assert_eq!(codes("computed c = { start wait(); 1 };"), ["E2502"]);
    assert_eq!(
        module_codes(
            "task wait() -> I64 { 3 }\naction go() { start wait(); }\n\
             export component C { view { Text {} } }\n"
        ),
        ["E4501"]
    );
}

#[test]
fn await_belongs_in_a_task() {
    assert_eq!(codes("action go() { let x = await wait(); }"), ["E4101"]);
    assert_eq!(codes("task t() -> I64 { let x = await wait(); x }"), CLEAN);
}

#[test]
fn a_task_reads_no_state_once_it_may_have_suspended() {
    assert_eq!(
        codes("task t() -> I64 { let x = a; let y = await fetch(b); x }"),
        CLEAN,
        "reads before the first suspension, its arguments included, see the start"
    );
    assert_eq!(
        codes("task t() -> I64 { await time::sleep(1s); a }"),
        ["E4102"]
    );
    assert_eq!(
        codes("task t() -> I64 { let w = wait(); twice }"),
        ["E4102"]
    );
    assert_eq!(
        codes("task t() -> I64 { await time::sleep(1s); peek() }"),
        ["E4102"],
        "a `fn` reading state is a read"
    );
    assert_eq!(
        codes("task t() -> I64 { await time::sleep(1s); one() }"),
        CLEAN
    );
    assert_eq!(
        codes(
            "task t() -> I64 { let mut n = 0; while n < a { await time::sleep(1s); n += 1; } n }"
        ),
        ["E4102"],
        "a loop that suspends reads again after it"
    );
}

#[test]
fn a_slot_runs_under_one_known_policy() {
    for policy in ["keep_latest", "drop_new", "queue", "parallel(2)"] {
        assert_eq!(
            codes(&format!(
                "action go() {{ start wait() as job {{ policy = [TaskPolicy::{policy}]; }}; }}"
            )),
            CLEAN,
            "{policy}"
        );
    }
    for bad in [
        "policy = [TaskPolicy::sometimes];",
        "policy = [TaskPolicy::parallel(0)];",
        "policy = [TaskPolicy::queue, TaskPolicy::drop_new];",
        "policy = TaskPolicy::queue;",
        "policy = [TaskPolicy::queue]; policy = [TaskPolicy::queue];",
        "success(v) { } success(v) { }",
    ] {
        assert_eq!(
            codes(&format!(
                "action go() {{ start wait() as job {{ {bad} }}; }}"
            )),
            ["E4302"],
            "{bad}"
        );
    }
    assert_eq!(
        codes("action go() { start wait() { policy = [TaskPolicy::queue]; }; }"),
        ["E4302"],
        "a policy needs a named slot"
    );
}

const PROBE: &str = r#"
import viso::time;

component Job {
    state got = 0;
    task tick(n: I64) -> I64 { await time::sleep(1s); n + 1 }
    view {
        Text { width: 20dp; height: 20dp; on click { start tick(got) { success(v) { got = v; } }; } }
    }
}

export component Probe {
    state runs = 0;
    state done = 0;
    state failed = 0;
    state cancels = 0;
    state show = true;
    state quick = 0;
    task work(n: I64) -> Result<I64, I64> {
        await time::sleep(1s);
        if n < 0 { return Err(n); }
        Ok(n * 10)
    }
    task now() -> I64 { 7 }
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text {
                width: 20dp; height: 20dp;
                on click {
                    runs += 1;
                    start work(runs) as job {
                        success(v) { done = v; }
                        error(e) { failed = e; }
                        cancelled { cancels += 1; }
                    };
                }
            }
            Text {
                width: 20dp; height: 20dp;
                on click { start work(-1 - runs) { success(v) { done = v; } error(e) { failed = e; } }; }
            }
            Text { width: 20dp; height: 20dp; on click { show = !show; } }
            Text { width: 20dp; height: 20dp; on click { start now() { success(v) { quick = v; } }; } }
            Column { width: 400dp; height: 20dp; if show { Job {} } }
        }
    }
}
"#;

#[test]
fn a_started_task_hands_its_value_to_success_or_error_after_it_awaits() {
    let mut rt = Rt::mount(PROBE);
    rt.click(0);
    assert_eq!(rt.fault(), None);
    assert_eq!(rt.clock.pending(), 1, "the task ran to its sleep");
    assert_eq!(rt.ints(["runs", "done"]), [Some(1), Some(0)]);
    rt.frame();
    assert_eq!(rt.int("done"), Some(0), "it waits for the sleep");
    assert_eq!(rt.ring(), 1);
    assert_eq!(
        rt.int("done"),
        Some(10),
        "the argument was taken at the start"
    );
    assert_eq!(rt.store.task_count(), 0);

    rt.click(1);
    rt.ring();
    assert_eq!(rt.ints(["done", "failed"]), [Some(10), Some(-2)]);
    assert_eq!(rt.fault(), None);
}

#[test]
fn a_task_that_never_awaits_finishes_at_the_next_frame() {
    let mut rt = Rt::mount(PROBE);
    rt.press(3);
    assert_eq!(
        rt.int("quick"),
        Some(0),
        "not inside the starting transaction"
    );
    rt.frame();
    assert_eq!(rt.int("quick"), Some(7));
    assert_eq!(rt.fault(), None);
}

#[test]
fn keep_latest_cancels_the_running_task() {
    let mut rt = Rt::mount(PROBE);
    rt.click(0);
    rt.click(0);
    assert_eq!(
        rt.int("cancels"),
        Some(1),
        "the second start cancelled the first"
    );
    assert_eq!(rt.store.task_count(), 1);
    rt.ring();
    assert_eq!(rt.ints(["done", "cancels"]), [Some(20), Some(1)]);
}

#[test]
fn unmounting_an_instance_drops_its_tasks_without_handlers() {
    let mut rt = Rt::mount(PROBE);
    rt.click(4);
    assert_eq!(rt.store.task_count(), 1, "the instance's own task");
    rt.click(2);
    assert_eq!(rt.store.task_count(), 0, "freeing its root dropped it");
    rt.ring();
    rt.click(2);
    rt.click(4);
    rt.ring();
    assert_eq!(rt.fault(), None);
    assert_eq!(rt.view.as_ref().unwrap().borrow().task_count(), 0);
}

#[test]
fn a_hot_reload_cancels_with_the_code_that_started() {
    let mut rt = Rt::mount(PROBE);
    rt.click(0);
    let edited = PROBE.replace("Ok(n * 10)", "Ok(n * 100)");
    rt.reload(&edited);
    assert_eq!(rt.int("cancels"), Some(1));
    assert_eq!(rt.store.task_count(), 0);
    rt.ring();
    assert_eq!(rt.int("done"), Some(0), "nothing finishes after the swap");
    rt.click(0);
    rt.ring();
    assert_eq!(
        rt.int("done"),
        Some(200),
        "the new code runs the next start"
    );
}

/// A view whose button starts `work` in slot `job` under `policy`.
fn slotted(policy: &str) -> String {
    format!(
        r#"
import viso::time;

export component Slots {{
    state finished = 0;
    state cancels = 0;
    task work() -> I64 {{ await time::sleep(1s); 1 }}
    view {{
        Text {{
            width: 20dp; height: 20dp;
            on click {{
                start work() as job {{
                    policy = [TaskPolicy::{policy}];
                    success(v) {{ finished += v; }}
                    cancelled {{ cancels += 1; }}
                }};
            }}
        }}
    }}
}}
"#
    )
}

/// Clicks three times, then rings until nothing sleeps; the finished count
/// after each ring, and the cancellations.
fn run_slot(policy: &str) -> (Vec<i64>, i64) {
    let mut rt = Rt::mount(&slotted(policy));
    for _ in 0..3 {
        rt.click(0);
    }
    let mut finished = Vec::new();
    while rt.ring() > 0 {
        finished.push(rt.int("finished").unwrap());
        assert!(finished.len() < 5, "{policy} keeps sleeping");
    }
    assert_eq!(rt.fault(), None);
    (finished, rt.int("cancels").unwrap())
}

#[test]
fn a_slot_policy_decides_what_a_start_does_while_one_runs() {
    assert_eq!(run_slot("keep_latest"), (vec![1], 2));
    assert_eq!(run_slot("drop_new"), (vec![1], 0));
    assert_eq!(run_slot("queue"), (vec![1, 2, 3], 0));
    assert_eq!(run_slot("parallel(2)"), (vec![2, 3], 0));
}

#[test]
fn a_task_reading_state_after_it_suspended_faults() {
    let source = r#"
import viso::time;

export component Peek {
    state count = 5;
    state seen = 0;
    task look() -> I64 {
        let read = || count;
        await time::sleep(1s);
        read()
    }
    view { Text { width: 20dp; height: 20dp; on click { start look() { success(v) { seen = v; } }; } } }
}
"#;
    let mut rt = Rt::mount(source);
    rt.click(0);
    rt.ring();
    let fault = rt.fault().expect("a fault");
    assert_eq!(
        (fault.kind, fault.kind.code()),
        (FaultKind::SuspendedRead, "E4102")
    );
    assert_eq!(rt.int("seen"), Some(0));
}

#[test]
fn an_effect_starts_a_task_and_the_release_package_runs_the_same() {
    let source = r#"
import viso::time;

export component Boot {
    state ready = 0;
    task load() -> I64 { await time::sleep(1s); 42 }
    effect boot { start load() { success(v) { ready = v; } }; }
    view { Text { width: 20dp; height: 20dp; } }
}
"#;
    let mut rt = Rt::mount(source);
    assert_eq!(
        rt.clock.pending(),
        1,
        "the mount ran the effect, which started the task"
    );
    rt.ring();
    assert_eq!(rt.int("ready"), Some(42));
    assert_eq!(rt.fault(), None);

    let mut rt = Rt::packaged(PROBE);
    rt.click(0);
    rt.ring();
    assert_eq!(rt.int("done"), Some(10));
    rt.click(4);
    rt.ring();
    assert_eq!(rt.fault(), None);
    assert_eq!(rt.store.task_count(), 0);

    let mut rt = Rt::packaged(source);
    rt.ring();
    assert_eq!(rt.int("ready"), Some(42));
}

#[test]
fn a_system_starts_no_task() {
    let source = "import viso::time;\ntask nap() -> I64 { 1 }\nsystem S {\n    action go() { start nap(); }\n}\n";
    assert_eq!(module_codes(source), ["E9109"]);
}
