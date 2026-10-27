//! A handler's event methods: `e.stop_propagation()` keeps the event from
//! the nodes further along its route, `e.stop_immediate_propagation()` from
//! the node's handlers after it too, and either compiles only without
//! arguments.

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;

const SOURCE: &str = r#"
export component Nest {
    state outer = 0;
    state inner = 0;
    state first = 0;
    state second = 0;

    view {
        Column {
            width: 100dp;
            height: 40dp;
            on click { outer += 1; }
            Column {
                width: 100dp;
                height: 20dp;
                on click(e) {
                    inner += 1;
                    if inner > 1 { e.stop_propagation(); }
                }
            }
            Column {
                width: 100dp;
                height: 20dp;
                on click(e) { first += 1; e.stop_immediate_propagation(); }
                on click { second += 1; }
            }
        }
    }
}
"#;

fn stops(mut rt: Rt) {
    rt.click(0);
    assert_eq!(rt.ints(["inner", "outer"]), [Some(1), Some(1)], "bubbled");
    rt.click(0);
    assert_eq!(
        rt.ints(["inner", "outer"]),
        [Some(2), Some(1)],
        "stopped at the inner column"
    );

    rt.click(1);
    assert_eq!(
        rt.ints(["first", "second", "outer"]),
        [Some(1), Some(0), Some(1)],
        "neither the node's next handler nor its parent heard it"
    );
}

#[test]
fn a_stopped_event_goes_no_further() {
    stops(Rt::mount(SOURCE));
}

#[test]
fn a_packaged_stopped_event_goes_no_further() {
    stops(Rt::packaged(SOURCE));
}

#[test]
fn an_event_method_takes_no_arguments() {
    let source = SOURCE.replace("e.stop_propagation()", "e.stop_propagation(1)");
    let compiled = compile_file(&source, &origin());
    let codes: Vec<_> = compiled.errors().map(|d| d.code.to_string()).collect();
    assert_eq!(codes, ["E2103"], "{:#?}", compiled.diagnostics);
}
