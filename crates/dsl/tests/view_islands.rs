//! An `Island`: one component of the view's file whose inputs and events are
//! all that cross into it, recorded for a target that draws it apart from
//! the rest of the view.

mod support;

use support::origin;
use viso_dsl::frontend::compile_file;
use viso_dsl::view_behavior::{ViewBehavior, view_behavior};

const CHART: &str = r#"
component Chart {
    input points: List<F64>;
    input title: String = "Chart";
    event picked(at: I64);
    state hovered = 0;
    view {
        Column {
            Text(title)
            Button("pick") { on click { hovered += 1; emit picked(hovered); } }
        }
    }
}
"#;

fn behavior(view: &str) -> Result<Option<ViewBehavior>, Vec<String>> {
    let source = format!("{CHART}\n{view}");
    let compiled = compile_file(&source, &origin());
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    view_behavior(&compiled).map_err(|errors| errors.into_iter().map(|e| e.message).collect())
}

#[test]
fn an_island_records_its_component_and_what_crosses_into_it() {
    let vb = behavior(
        r#"
export component App {
    state data = [1.0, 2.0];
    state last = 0;
    view {
        Column {
            Text("shell")
            Island {
                width: 200dp;
                height: 100dp;
                Chart { points: data; on picked(event) { last = event.at; } }
            }
        }
    }
}
"#,
    )
    .unwrap()
    .unwrap();
    let [island] = &vb.islands[..] else {
        panic!("{:#?}", vb.islands);
    };
    assert_eq!(island.component, "Chart");
    // `points` is bound; `title` takes its default.
    assert_eq!(island.inputs.len(), 1);
    assert_eq!(island.inputs[0].0, 0);
    assert_eq!(island.events.len(), 1);
    assert_eq!(island.events[0].0, "picked");
    assert_ne!(island.inputs[0].1, island.events[0].1);
}

#[test]
fn an_island_holds_one_component_and_is_in_no_other() {
    let two = behavior(
        r#"
export component App {
    view {
        Island { Text("not a component") }
    }
}
"#,
    )
    .unwrap_err();
    assert!(two[0].contains("exactly one component"), "{two:?}");
    let nested = behavior(
        r#"
component Outer {
    view { Island { Chart { points: []; } } }
}
export component App {
    view {
        Island { Outer {} }
    }
}
"#,
    )
    .unwrap_err();
    assert!(nested[0].contains("not in another"), "{nested:?}");
}

#[test]
fn an_island_mounts_as_a_stack_of_its_component_where_nothing_splits_the_view() {
    let source = format!(
        "{CHART}\nexport component App {{ state data = [1.0]; view {{ Island {{ width: 200dp; height: 100dp; Chart {{ points: data; }} }} }} }}"
    );
    let blob = viso_dsl::aot::build_view_package(&source, &origin()).expect("packages");
    let mut store = viso_ui::NodeStore::new();
    let mut states = viso_ui::StateStore::new();
    let mut bindings = viso_ui::BindingTable::new();
    let mut lists = viso_ui::virtual_list::VirtualLists::new();
    let view = viso_view::load_view(&blob, &mut store, &mut states, &mut bindings, &mut lists)
        .expect("loads");
    let root = view.root.expect("a root");
    // The island, its component's column, the title and the button.
    let mut count = 0;
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        count += 1;
        stack.extend(store.arena().children(id));
    }
    assert_eq!(count, 4);
}
