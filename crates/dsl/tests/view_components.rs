//! User components inlined into the view that mounts them, under hot reload
//! and the release package: a stateless component repeated by a keyed `for`
//! and switched by an `if` hands its events to its caller's handlers, a
//! stateful one keeps its state per instance, and a `bind` writes a
//! component's change back into the caller's state. An instance an `if`, a
//! `for` or a `match` mounts keeps its state per mount.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, LiveAnchors, LiveRuntime, hot_reload_view};
use viso_ui::Rect;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, SemanticProjector, StateStore, TextEdits, run_structure_hooks,
};
use viso_view::{Value, ViewHost, load_view};

const SOURCE: &str = r#"
component TodoItem {
    input id: I64;
    event toggled(id: I64);
    view {
        Row {
            width: 400dp;
            height: 20dp;
            Text { width: 20dp; height: 20dp; on click { emit toggled(id); } }
        }
    }
}

component Stepper {
    @bindable(changed)
    input value: I64;
    input step: I64 = 1;
    event changed(value: I64);
    view {
        Row {
            width: 400dp;
            height: 20dp;
            Text { width: 20dp; height: 20dp; on click { emit changed(value - step); } }
            Text { width: 20dp; height: 20dp; on click { emit changed(value + step); } }
        }
    }
}

component Counter {
    state count = 0;
    view {
        Row {
            width: 400dp;
            height: 20dp;
            Text { width: 20dp; height: 20dp; on click { count += 1; } }
        }
    }
}

export component TodoApp {
    state items = [1, 2, 3];
    state show = true;
    state picked = 0;
    state amount = 5;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: 400dp;
                height: 20dp;
                Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2]; } }
                Text { width: 20dp; height: 20dp; on click { show = !show; } }
            }
            Column {
                width: 400dp;
                height: 60dp;
                for item in items key item {
                    TodoItem { id: item; on toggled(event) { picked = event.id; } }
                }
            }
            Column {
                width: 400dp;
                height: 20dp;
                if show {
                    TodoItem { id: 9; on toggled(event) { picked = event.id * 10; } }
                }
            }
            Stepper { bind value <=> amount; step: 2; }
            Counter {}
            Counter {}
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["todo".into()],
        language: None,
    }
}

/// A mounted view and the stores it runs over.
#[derive(Default)]
struct Rt {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<(viso_dsl::ir::binding_ir::NodeKey, NodeId)>,
    view: Option<Rc<RefCell<ViewHost>>>,
    last_good: CandidatePlan,
}

impl Rt {
    /// The view mounted by a hot reload.
    fn reloaded(source: &str) -> Self {
        let mut rt = Rt::default();
        rt.reload(source);
        rt
    }

    /// The view loaded from its release package.
    fn packaged(source: &str) -> Self {
        let blob = build_view_package(source, &origin()).expect("packages");
        let mut rt = Rt::default();
        let view = load_view(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        )
        .expect("loads");
        rt.root = view.root;
        rt.view = view.host;
        rt.layout();
        rt
    }

    fn reload(&mut self, source: &str) {
        let mut live = LiveRuntime {
            store: &mut self.store,
            states: &mut self.states,
            bindings: &mut self.bindings,
            effects: &mut self.effects,
            lists: &mut self.lists,
            text_edits: &mut self.text_edits,
            projectors: &mut self.projectors,
            root: self.root,
            scratch: &mut self.scratch,
            nodes: &mut self.nodes,
            view: &mut self.view,
        };
        let done = hot_reload_view(
            &mut live,
            &self.last_good,
            source,
            &origin(),
            &LiveAnchors::default(),
        )
        .expect("reloads");
        self.root = live.root;
        self.last_good = done.candidate;
        self.layout();
    }

    fn layout(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        if let Some(root) = self.root {
            self.store.layout(root, surface, &mut Vec::new());
        }
    }

    /// A primary click at `(x, y)`, then the frame's flush and structure hooks.
    fn click(&mut self, x: f32, y: f32) {
        let root = self.root.expect("mounted");
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x,
                y,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                root,
                event,
                &mut chain,
            );
        }
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &changed,
        );
        self.layout();
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        let arena = self.store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(node).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// The children of the root's `index`th child.
    fn region(&self, index: usize) -> Vec<NodeId> {
        let root = self.root.expect("mounted");
        self.children(self.children(root)[index])
    }

    fn state(&self, name: &str) -> Option<Value> {
        let host = self.view.as_ref()?.borrow();
        let slot = host.state_slot(name)?;
        host.state(slot).cloned()
    }

    fn faulted(&self) -> bool {
        let host = self.view.as_ref().expect("a host");
        host.borrow().last_fault().is_some()
    }
}

fn targets() -> [Rt; 2] {
    [Rt::reloaded(SOURCE), Rt::packaged(SOURCE)]
}

#[test]
fn a_keyed_for_of_a_component_moves_its_nodes_and_routes_its_events() {
    for mut rt in targets() {
        let before = rt.region(1);
        assert_eq!(before.len(), 3, "one row per item");
        rt.click(5.0, 45.0);
        assert_eq!(rt.state("picked"), Some(Value::Int(2)));
        rt.click(5.0, 5.0);
        assert_eq!(rt.region(1), vec![before[2], before[0], before[1]]);
        rt.click(5.0, 25.0);
        assert_eq!(rt.state("picked"), Some(Value::Int(3)));
        assert!(!rt.faulted());
    }
}

#[test]
fn an_if_switches_a_component_in_and_out() {
    for mut rt in targets() {
        assert_eq!(rt.region(2).len(), 1);
        rt.click(5.0, 85.0);
        assert_eq!(rt.state("picked"), Some(Value::Int(90)));
        rt.click(25.0, 5.0);
        assert!(rt.region(2).is_empty());
        rt.click(25.0, 5.0);
        assert_eq!(rt.region(2).len(), 1);
    }
}

#[test]
fn a_bind_writes_the_components_change_back() {
    for mut rt in targets() {
        rt.click(25.0, 105.0);
        assert_eq!(rt.state("amount"), Some(Value::Int(7)));
        rt.click(5.0, 105.0);
        rt.click(5.0, 105.0);
        assert_eq!(rt.state("amount"), Some(Value::Int(3)));
    }
}

#[test]
fn each_stateful_instance_keeps_its_own_state() {
    for mut rt in targets() {
        rt.click(5.0, 125.0);
        rt.click(5.0, 145.0);
        rt.click(5.0, 145.0);
        assert_eq!(rt.state("Counter#0.count"), Some(Value::Int(1)));
        assert_eq!(rt.state("Counter#1.count"), Some(Value::Int(2)));
    }
}

#[test]
fn a_reload_keeps_an_instances_state() {
    let mut rt = Rt::reloaded(SOURCE);
    rt.click(5.0, 145.0);
    rt.reload(&SOURCE.replace("height: 300dp", "height: 290dp"));
    assert_eq!(rt.state("Counter#1.count"), Some(Value::Int(1)));
    rt.click(5.0, 145.0);
    assert_eq!(rt.state("Counter#1.count"), Some(Value::Int(2)));
}

/// The diagnostic codes `source` fails to package with.
fn codes(source: &str) -> Vec<String> {
    build_view_package(source, &origin())
        .expect_err("does not package")
        .into_iter()
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn a_component_mounting_itself_is_e3711() {
    let codes = codes(
        "export component Loop {
            view { Column { Loop {} } }
        }",
    );
    assert!(codes.contains(&"E3711".to_string()), "{codes:?}");
}

const REGIONAL: &str = r#"
component Tally {
    input seed: I64;
    event counted(value: I64);
    state count = seed * 10;
    view {
        Row {
            width: 400dp;
            height: 20dp;
            Text { width: 20dp; height: 20dp; on click { count += 1; emit counted(count); } }
            if count > seed * 10 + 1 {
                Text { width: 20dp; height: 20dp; }
            }
        }
    }
}

export component App {
    state items = [1, 2, 3];
    state show = true;
    state keep = true;
    state mode = 0;
    state last = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: 400dp;
                height: 20dp;
                Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2]; } }
                Text { width: 20dp; height: 20dp; on click { items = [1, 3]; } }
                Text { width: 20dp; height: 20dp; on click { items = [1, 2, 3]; } }
                Text { width: 20dp; height: 20dp; on click { show = !show; } }
                Text { width: 20dp; height: 20dp; on click { keep = !keep; } }
                Text { width: 20dp; height: 20dp; on click { mode = 1 - mode; } }
            }
            Column {
                width: 400dp;
                height: 60dp;
                for item in items key item {
                    Tally { seed: item; on counted(event) { last = event.value; } }
                }
            }
            Column {
                width: 400dp;
                height: 20dp;
                if show {
                    Tally { seed: 7; on counted(event) { last = event.value; } }
                }
            }
            Column {
                width: 400dp;
                height: 20dp;
                if keep preserve "kept" {
                    Tally { seed: 8; on counted(event) { last = event.value; } }
                }
            }
            Column {
                width: 400dp;
                height: 20dp;
                match mode {
                    0 => { Tally { seed: 4; on counted(event) { last = event.value; } } },
                    _ => { Tally { seed: 5; on counted(event) { last = event.value; } } },
                }
            }
        }
    }
}
"#;

fn regional() -> [Rt; 2] {
    [Rt::reloaded(REGIONAL), Rt::packaged(REGIONAL)]
}

impl Rt {
    /// Clicks the tally at `y` and returns the count it reports.
    fn tally(&mut self, y: f32) -> Option<Value> {
        self.click(5.0, y);
        assert!(!self.faulted());
        self.state("last")
    }
}

#[test]
fn each_instance_a_for_mounts_keeps_its_own_state_from_its_item() {
    for mut rt in regional() {
        assert_eq!(rt.tally(25.0), Some(Value::Int(11)));
        assert_eq!(rt.tally(25.0), Some(Value::Int(12)));
        assert_eq!(rt.tally(45.0), Some(Value::Int(21)));
        assert_eq!(rt.tally(65.0), Some(Value::Int(31)));
        assert_eq!(rt.tally(25.0), Some(Value::Int(13)));
    }
}

#[test]
fn a_region_inside_an_instance_reads_that_instances_state() {
    for mut rt in regional() {
        let rows = rt.region(1);
        rt.tally(25.0);
        assert_eq!(rt.children(rows[0]).len(), 1);
        rt.tally(25.0);
        assert_eq!(rt.children(rows[0]).len(), 2);
        assert_eq!(rt.children(rows[1]).len(), 1);
    }
}

#[test]
fn an_instances_state_follows_its_key() {
    for mut rt in regional() {
        rt.tally(25.0);
        rt.tally(65.0);
        rt.click(5.0, 5.0);
        assert_eq!(rt.tally(25.0), Some(Value::Int(32)));
        assert_eq!(rt.tally(45.0), Some(Value::Int(12)));
        assert_eq!(rt.tally(65.0), Some(Value::Int(21)));
    }
}

#[test]
fn a_removed_instance_starts_over() {
    for mut rt in regional() {
        rt.tally(45.0);
        rt.click(25.0, 5.0);
        assert_eq!(rt.region(1).len(), 2);
        assert_eq!(rt.tally(45.0), Some(Value::Int(31)));
        rt.click(45.0, 5.0);
        assert_eq!(rt.tally(45.0), Some(Value::Int(21)));
        assert_eq!(rt.tally(65.0), Some(Value::Int(32)));
    }
}

#[test]
fn an_if_restarts_its_instance_and_a_preserved_one_keeps_it() {
    for mut rt in regional() {
        assert_eq!(rt.tally(85.0), Some(Value::Int(71)));
        assert_eq!(rt.tally(105.0), Some(Value::Int(81)));
        rt.click(65.0, 5.0);
        rt.click(85.0, 5.0);
        assert!(rt.region(2).is_empty());
        assert!(rt.region(3).is_empty());
        rt.click(65.0, 5.0);
        rt.click(85.0, 5.0);
        assert_eq!(rt.tally(85.0), Some(Value::Int(71)));
        assert_eq!(rt.tally(105.0), Some(Value::Int(82)));
    }
}

#[test]
fn a_match_arm_mounts_its_own_instance() {
    for mut rt in regional() {
        assert_eq!(rt.tally(125.0), Some(Value::Int(41)));
        rt.click(105.0, 5.0);
        assert_eq!(rt.tally(125.0), Some(Value::Int(51)));
        rt.click(105.0, 5.0);
        assert_eq!(rt.tally(125.0), Some(Value::Int(41)));
    }
}

#[test]
fn a_converted_bind_to_a_component_input_is_e3711() {
    let codes = codes(
        "component Field {
            @bindable(changed)
            input value: I64;
            event changed(value: I64);
            view { Text {} }
        }
        export component App {
            state amount = 5;
            view { Column { Field { bind value <=> amount using I64; } } }
        }",
    );
    assert!(codes.contains(&"E3711".to_string()), "{codes:?}");
}

#[test]
fn a_forwarded_property_on_a_multi_root_view_is_e3711() {
    let codes = codes(
        "component Pair {
            view { Text {} Text {} }
        }
        export component App {
            view { Column { Pair { width: 10dp; } } }
        }",
    );
    assert_eq!(codes, ["E3711"]);
}

#[test]
fn an_inlined_view_binds_the_mounted_sources_it_reads() {
    let compiled = viso_dsl::frontend::compile_file(
        r#"component Label {
            input value: I64;
            state hovered = false;
            view { Text { text: format("{} {}", value, hovered); } }
        }
        export component App {
            state amount = 5;
            view { Column { Label { value: amount + 1; } } }
        }"#,
        &origin(),
    );
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let symbol = |name: &str| {
        compiled
            .sources
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.symbol)
            .unwrap_or_else(|| panic!("no source `{name}`"))
    };
    let mut bound: Vec<_> = compiled.bindings.static_edges().map(|e| e.source).collect();
    bound.sort();
    let mut expected = vec![symbol("amount"), symbol("Label#0.hovered")];
    expected.sort();
    assert_eq!(bound, expected);
}
