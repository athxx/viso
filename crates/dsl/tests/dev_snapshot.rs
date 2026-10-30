//! A view's development state across a restart: captured from the mount the
//! old process ran, carried as bytes, and restored into the mount of the view
//! the new build embeds through the host's plan between the two — each kept
//! state with its value, converted where its type changed, each new one from
//! its initializer, and the focus, scroll offset and text of each kept node
//! on the node it became.

#[path = "support/reload.rs"]
mod reload;

use std::cell::RefCell;
use std::rc::Rc;

use reload::{LiveRuntime, hot_reload_view};
use viso_behavior::Value;
use viso_dsl::aot::{ShaderLang, emit_view_package, package_shaders};
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, reload_plan};
use viso_ui::state::StateKey;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, Rect, Selection, SemanticProjector, StateStore,
    StateValue, TextEdits, TextOffset, TextPosition, Vec2,
};
use viso_view::ViewHost;
use viso_view::dev::commit::Candidate;
use viso_view::dev::commit::{CommitReport, restore};
use viso_view::dev::snapshot::{
    DevSnapshot, MountRef, Redact, RustCell, ViewSnapshot, WindowCells, capture_view,
};

const SCROLLED: &str = "width: 100dp; height: 50dp;";
const TALL: &str = "width: 100dp; height: 400dp;";

/// A card whose states are `states`, its body `before` its scroll container
/// and field.
fn card(states: &str, before: &str) -> String {
    format!(
        "component Card {{
            {states}
            view {{
                Column {{
                    width: 200dp;
                    height: 300dp;
                    {before}
                    Scroll {{ {SCROLLED} Text(\"{{count}}\") {{ {TALL} }} }}
                    TextInput {{ width: 100dp; height: 20dp; }}
                    Text(name) {{ width: 100dp; height: 20dp; }}
                }}
            }}
        }}"
    )
}

/// A mounted view and the stores it runs over.
#[derive(Default)]
struct Live {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<Option<NodeId>>,
    view: Option<Rc<RefCell<ViewHost>>>,
    last_good: CandidatePlan,
}

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["card".into()],
        language: None,
        ..Default::default()
    }
}

impl Live {
    /// `source` mounted as a fresh process mounts it.
    fn mount(source: &str) -> Live {
        let mut live = Live::default();
        let done = hot_reload_view(
            &mut live.runtime(),
            &CandidatePlan::default(),
            source,
            &origin(),
        )
        .expect("the view compiles");
        live.root = live.runtime_root();
        live.last_good = done.candidate;
        live.lay_out();
        live
    }

    fn runtime(&mut self) -> LiveRuntime<'_> {
        LiveRuntime {
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
        }
    }

    /// The root the first commit built: the first static node.
    fn runtime_root(&self) -> Option<NodeId> {
        self.nodes.first().copied().flatten()
    }

    fn lay_out(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 400.0,
        };
        viso_ui::text_edit::reconcile(&mut self.store, &mut self.text_edits, None);
        if let Some(root) = self.root {
            self.store.layout(root, surface, &mut Vec::new());
        }
    }

    /// The durable key of the state `name`.
    fn key(&self, name: &str) -> StateKey {
        let symbol = self.last_good.symbol_for_name(name).expect("a state");
        StateKey::from_parts(symbol.hi, symbol.lo)
    }

    fn cell(&self, name: &str) -> Option<StateValue> {
        self.states.get(self.states.id_for_key(self.key(name))?)
    }

    fn value(&self, name: &str) -> Option<Value> {
        let host = self.view.as_ref()?.borrow();
        host.current(host.state_slot(name)?, &self.states)
    }

    fn set_value(&mut self, name: &str, value: Value) {
        let mut host = self.view.as_ref().expect("a host").borrow_mut();
        let slot = host.state_slot(name).expect("a state");
        host.set_state(slot, value);
    }

    /// The static node at `index`.
    fn node(&self, index: usize) -> NodeId {
        self.nodes[index].expect("a live static node")
    }

    /// The mount as a snapshot captures it.
    fn capture(&self) -> ViewSnapshot {
        self.capture_with(Redact::None)
    }

    /// The mount as a snapshot captures it, less what `redact` leaves out.
    fn capture_with(&self, redact: Redact<'_>) -> ViewSnapshot {
        let package = emit_view_package(&self.last_good);
        let cells: Vec<_> = package
            .states
            .iter()
            .filter_map(|state| Some((state.key, self.states.id_for_key(state.key)?)))
            .collect();
        let host = self.view.as_ref().map(|host| host.borrow());
        capture_view(
            &self.store,
            &self.states,
            &self.text_edits,
            host.as_deref(),
            &self.nodes,
            &cells,
            MountRef {
                path: "/app/card.vs".into(),
                window: 0,
                mount: 0,
            },
            redact,
        )
    }

    /// Restores `snapshot`, of a mount of `old`, into this mount by the
    /// host's plan from `old` to the view this mount runs.
    fn restore(&mut self, old: &CandidatePlan, snapshot: &ViewSnapshot) -> CommitReport {
        let mut package = emit_view_package(&self.last_good);
        package.shaders = package_shaders(&self.last_good, &[ShaderLang::None]).unwrap();
        let plan = reload_plan(old, &self.last_good, &package);
        let module = self
            .last_good
            .view
            .as_ref()
            .map(|view| Rc::clone(&view.module));
        let candidate = Candidate::verified(package, module).expect("a packed brush loads");
        let report = restore(&mut self.runtime(), &candidate, &plan, snapshot);
        self.root = self.nodes.first().copied().flatten();
        self.lay_out();
        report
    }
}

/// `snapshot` as the bytes between the processes, read back, beside a
/// window's Rust cells.
fn carried(snapshot: ViewSnapshot) -> ViewSnapshot {
    let whole = DevSnapshot {
        views: vec![snapshot],
        cells: vec![WindowCells {
            window: 1,
            cells: vec![RustCell {
                key: StateKey::from_parts(3, 4),
                nth: 1,
                value: StateValue::Float(2.5),
            }],
        }],
        ..DevSnapshot::default()
    };
    let bytes = whole.encode();
    let read = DevSnapshot::decode(&bytes).expect("the snapshot decodes");
    assert_eq!(read, whole);
    read.views.into_iter().next().unwrap()
}

#[test]
fn a_snapshot_for_the_disk_leaves_out_sensitive_states_and_secure_text() {
    let source = "component Card {
        state count = 3;
        @sensitive state pin = 1234;
        @sensitive state token = \"\";
        view {
            Column {
                width: 200dp;
                height: 300dp;
                TextInput { width: 100dp; height: 20dp; secure: true; }
                TextInput { width: 100dp; height: 20dp; }
                Text(\"{pin} {token}\") { width: 100dp; height: 20dp }
                Text(\"{count}\") { width: 100dp; height: 20dp }
            }
        }
    }";
    let mut live = Live::mount(source);
    live.states.set(
        live.states.id_for_key(live.key("pin")).unwrap(),
        StateValue::Int(4321),
    );
    live.set_value("token", Value::Str(Rc::new("t0ken".into())));
    let selection = Selection {
        anchor: TextPosition::upstream(TextOffset(0)),
        focus: TextPosition::upstream(TextOffset(0)),
    };
    // Column, the secure field, the plain field, the two texts.
    live.text_edits
        .editing(live.node(1))
        .restore("password".into(), selection);
    live.text_edits
        .editing(live.node(2))
        .restore("plain".into(), selection);

    let plan = &live.last_good;
    let keys: Vec<StateKey> = plan
        .sensitive
        .iter()
        .map(|symbol| StateKey::from_parts(symbol.hi, symbol.lo))
        .collect();
    let nodes = plan.sensitive_nodes();
    assert_eq!(keys.len(), 2);
    assert_eq!(nodes, [3], "the text reading them");
    let text = |snapshot: &ViewSnapshot, index: u32| {
        snapshot
            .nodes
            .iter()
            .find(|n| n.node == viso_view::dev::wire::NodeRef::Static(index))
            .and_then(|n| n.text.as_ref().map(|(text, _)| text.clone()))
    };
    let slot = |name: &str| {
        let host = live.view.as_ref().unwrap().borrow();
        host.state_slot(name).unwrap()
    };

    let whole = live.capture();
    assert_eq!(whole.cell(live.key("pin")), Some(StateValue::Int(4321)));
    assert_eq!(text(&whole, 1).as_deref(), Some("password"), "in memory");
    assert!(whole.states[slot("token")].is_some());

    let disk = carried(live.capture_with(Redact::Listed {
        states: &keys,
        nodes: &nodes,
    }));
    assert_eq!(disk.cell(live.key("pin")), None);
    assert_eq!(disk.cell(live.key("count")), Some(StateValue::Int(3)));
    assert_eq!(disk.states[slot("pin")], None);
    assert_eq!(disk.states[slot("token")], None);
    assert!(disk.states[slot("count")].is_some());
    assert_eq!(text(&disk, 1), None, "a secure field's text");
    assert_eq!(text(&disk, 2).as_deref(), Some("plain"));

    let unknown = live.capture_with(Redact::All);
    assert!(unknown.cells.is_empty());
    assert!(unknown.states.iter().all(Option::is_none));
    assert!(unknown.nodes.iter().all(|n| n.text.is_none()));
}

#[test]
fn a_restored_view_takes_each_state_through_the_new_builds_plan() {
    let states = "state count = 0; state name = \"draft\"; state gone = 1;";
    let mut old = Live::mount(&card(states, ""));
    old.states.set(
        old.states.id_for_key(old.key("count")).unwrap(),
        StateValue::Int(7),
    );
    old.set_value("name", Value::Str(Rc::new("edited".into())));
    let snapshot = carried(old.capture());

    // The new build retypes `count`, drops `gone` and adds `extra`.
    let states = "state count: F64 = 0.0; state name = \"draft\"; state extra = 5;";
    let mut new = Live::mount(&card(states, ""));
    assert_eq!(new.cell("count"), Some(StateValue::Float(0.0)), "fresh");
    let report = new.restore(&old.last_good, &snapshot);

    assert_eq!(new.cell("count"), Some(StateValue::Float(7.0)), "converted");
    assert_eq!(
        new.value("name"),
        Some(Value::Str(Rc::new("edited".into()))),
        "a state no cell holds carries its value"
    );
    assert_eq!(
        new.cell("extra"),
        Some(StateValue::Int(5)),
        "new: its initializer"
    );
    assert!(report.migrated >= 2, "{report:?}");
    assert_eq!(report.reset, 1, "{report:?}");
}

#[test]
fn focus_scroll_and_text_land_on_the_nodes_they_became() {
    let states = "state count = 3; state name = \"n\";";
    let mut old = Live::mount(&card(states, ""));
    // Column, Scroll, Text, TextInput, Text.
    let (scroll, input) = (old.node(1), old.node(3));
    old.store.scroll_by(scroll, Vec2::new(0.0, 40.0));
    old.store.set_focused(Some(input));
    // The buffer typing into the field starts.
    let buffer = old.text_edits.editing(input);
    let selection = Selection {
        anchor: TextPosition::upstream(TextOffset(1)),
        focus: TextPosition::upstream(TextOffset(4)),
    };
    buffer.restore("typed".into(), selection);
    let snapshot = carried(old.capture());
    assert_eq!(snapshot.nodes.len(), 2, "{:#?}", snapshot.nodes);

    // A node inserted before them shifts every static index by one.
    let mut new = Live::mount(&card(states, "Text { width: 10dp; height: 10dp; }"));
    let report = new.restore(&old.last_good, &snapshot);
    let (scroll, input) = (new.node(2), new.node(4));
    assert!(!report.focus_lost, "{report:?}");
    assert_eq!(report.scroll_lost, 0);
    assert_eq!(new.store.focused(), Some(input));
    assert_eq!(new.store.scroll(scroll).y, 40.0);
    let buffer = new
        .text_edits
        .get(input)
        .expect("the rebuilt field's buffer");
    assert_eq!(buffer.text, "typed");
    assert_eq!(buffer.sel, selection);
    assert_eq!(new.cell("count"), Some(StateValue::Int(3)));
}

#[test]
fn a_node_the_new_build_drops_reports_what_it_held_lost() {
    let states = "state count = 0; state name = \"n\";";
    let mut old = Live::mount(&card(states, ""));
    old.store.scroll_by(old.node(1), Vec2::new(0.0, 25.0));
    old.store.set_focused(Some(old.node(3)));
    let snapshot = carried(old.capture());

    let new_source = format!(
        "component Card {{
            {states}
            view {{ Column {{ width: 200dp; height: 300dp; Text(\"{{count}}\") }} }}
        }}"
    );
    let mut new = Live::mount(&new_source);
    let report = new.restore(&old.last_good, &snapshot);
    assert!(report.focus_lost);
    assert_eq!(report.scroll_lost, 1);
    assert_eq!(new.store.focused(), None);
}

#[test]
fn a_closure_a_state_holds_is_not_captured() {
    let source = "component Card {
        state count = 2;
        state pick: Fn(I64) -> I64 = |x| x + 1;
        view { Column { width: 10dp; height: 10dp; Text(\"{count}\") } }
    }";
    let old = Live::mount(source);
    let snapshot = carried(old.capture());
    let host = old.view.as_ref().unwrap().borrow();
    let pick = host.state_slot("pick").unwrap();
    let count = host.state_slot("count").unwrap();
    assert_eq!(snapshot.states[pick], None);
    assert_eq!(snapshot.states[count], Some(Value::Int(2)));
}
