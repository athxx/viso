//! Native controls under hot reload and the release package: a toggle, a
//! slider, a tab strip and a text field respond to pointer, key and IME samples
//! and write their change back through a `bind` before the author's handler
//! for the same event runs, and a handler on an ancestor
//! runs once per sample.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, LiveAnchors, LiveRuntime, hot_reload_view};
use viso_ui::text_edit::reconcile;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, ImeEvent, Key, KeyEvent, KeyRouter, Modifiers, NodeId, NodeStore,
    PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rect, SemanticProjector, StateStore,
    TextEdits, run_structure_hooks,
};
use viso_view::{Value, ViewHost, load_view};

const SOURCE: &str = r#"
export component Controls {
    state on = false;
    state level: F32 = 0.0;
    state tab: U32 = 0;
    state name = "";
    state changes = 0;
    state seen: F32 = 0.0;
    state submits = 0;
    state outer = 0;
    state local: Dp = 0dp;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            on click { outer += 1; }
            Toggle {
                width: 40dp;
                height: 20dp;
                on changed(event) { if on { changes += 1; } }
                bind checked <=> on;
            }
            Slider {
                width: 200dp;
                height: 20dp;
                min: 0.0;
                max: 10.0;
                step: 1.0;
                bind value <=> level;
                on changed(event) { seen = event.value; }
            }
            Row {
                width: 400dp;
                height: 20dp;
                Tabs {
                    width: 300dp;
                    height: 20dp;
                    bind selected <=> tab;
                    Text { width: 100dp; height: 20dp; }
                    Text { width: 100dp; height: 20dp; }
                    Text { width: 100dp; height: 20dp; }
                }
            }
            TextInput {
                width: 200dp;
                height: 20dp;
                bind value <=> name;
                on submitted { submits += 1; }
            }
            Row {
                width: 400dp;
                height: 20dp;
                Text {
                    width: 100dp;
                    height: 20dp;
                    on pointer_down(event) { local = event.position.x; }
                }
            }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["controls".into()],
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
    /// The view of `source` mounted by a hot reload.
    fn reloaded(source: &str) -> Self {
        let mut rt = Rt::default();
        let mut live = LiveRuntime {
            store: &mut rt.store,
            states: &mut rt.states,
            bindings: &mut rt.bindings,
            effects: &mut rt.effects,
            lists: &mut rt.lists,
            text_edits: &mut rt.text_edits,
            projectors: &mut rt.projectors,
            root: rt.root,
            scratch: &mut rt.scratch,
            nodes: &mut rt.nodes,
            view: &mut rt.view,
        };
        let done = hot_reload_view(
            &mut live,
            &rt.last_good,
            source,
            &origin(),
            &LiveAnchors::default(),
        )
        .expect("reloads");
        rt.root = live.root;
        rt.last_good = done.candidate;
        rt.layout();
        rt
    }

    /// The view of `source` loaded from its release package.
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

    /// One pointer sample at `(x, y)`.
    fn pointer(&mut self, x: f32, y: f32, phase: PointerPhase) {
        let event = PointerEvent {
            x,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        };
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.root.expect("mounted"),
            event,
            &mut self.scratch,
        );
        self.settle();
    }

    /// A primary click at `(x, y)`.
    fn click(&mut self, x: f32, y: f32) {
        self.pointer(x, y, PointerPhase::Down);
        self.pointer(x, y, PointerPhase::Up);
    }

    /// A press of `key` to the focused node.
    fn key(&mut self, key: Key) {
        let event = KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        };
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            self.root.expect("mounted"),
            event,
            &mut self.scratch,
        );
        self.settle();
    }

    /// An IME sample to the focused node.
    fn ime(&mut self, event: ImeEvent) {
        KeyRouter::route_ime(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            self.root.expect("mounted"),
            event,
            &mut self.scratch,
        );
        self.settle();
    }

    /// The frame's edit reconcile, change delivery, flush and structure hooks.
    fn settle(&mut self) {
        reconcile(&mut self.store, &mut self.text_edits, None);
        let mut edited = Vec::new();
        self.text_edits.take_changed(&mut edited);
        for node in edited {
            let Some(text) = self.text_edits.get(node).map(|b| b.text.clone()) else {
                continue;
            };
            KeyRouter::route_text_change(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.text_edits,
                node,
                &text,
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

    fn state(&self, name: &str) -> Option<Value> {
        let host = self.view.as_ref()?.borrow();
        let slot = host.state_slot(name)?;
        host.state(slot).cloned()
    }

    fn int(&self, name: &str) -> i64 {
        self.state(name)
            .and_then(|v| v.as_int())
            .unwrap_or_else(|| panic!("`{name}` is no integer"))
    }

    fn float(&self, name: &str) -> f64 {
        self.state(name)
            .and_then(|v| v.as_float())
            .unwrap_or_else(|| panic!("`{name}` is no float"))
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
fn a_toggle_flips_its_bound_state_on_a_click_and_space() {
    for mut rt in targets() {
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("on"), 1);
        assert_eq!(rt.int("changes"), 1);
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("on"), 0);
        assert_eq!(rt.int("changes"), 1);
        rt.store.set_focused(rt.children_of_root()[0].into());
        rt.key(Key::Space);
        assert_eq!(rt.int("on"), 1);
        assert_eq!(rt.int("changes"), 2);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_slider_takes_its_value_from_the_pointer_and_steps_with_keys() {
    for mut rt in targets() {
        rt.pointer(60.0, 30.0, PointerPhase::Down);
        assert_eq!(rt.float("level"), 3.0);
        assert_eq!(rt.float("seen"), 3.0);
        rt.pointer(200.0, 30.0, PointerPhase::Move);
        assert_eq!(rt.float("level"), 10.0);
        rt.pointer(200.0, 30.0, PointerPhase::Up);
        rt.key(Key::Left);
        assert_eq!(rt.float("level"), 9.0);
        rt.key(Key::Home);
        assert_eq!(rt.float("level"), 0.0);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_tab_strip_selects_the_child_a_click_lands_in() {
    for mut rt in targets() {
        rt.click(150.0, 50.0);
        assert_eq!(rt.int("tab"), 1);
        rt.click(250.0, 50.0);
        assert_eq!(rt.int("tab"), 2);
        rt.store.set_focused(rt.tabs().into());
        rt.key(Key::Left);
        assert_eq!(rt.int("tab"), 1);
        rt.key(Key::Home);
        assert_eq!(rt.int("tab"), 0);
    }
}

#[test]
fn a_text_field_writes_its_edited_text_back_and_submits() {
    for mut rt in targets() {
        rt.click(10.0, 70.0);
        rt.ime(ImeEvent::Preedit {
            text: "ni".into(),
            caret: 2,
        });
        rt.ime(ImeEvent::Commit {
            text: "你好".into(),
        });
        assert_eq!(rt.state("name"), Some(Value::Str(Rc::new("你好".into()))));
        rt.key(Key::Backspace);
        assert_eq!(rt.state("name"), Some(Value::Str(Rc::new("你".into()))));
        rt.key(Key::Enter);
        assert_eq!(rt.int("submits"), 1);
        assert!(!rt.faulted());
    }
}

#[test]
fn an_ancestor_handler_runs_once_per_sample() {
    for mut rt in targets() {
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("outer"), 1);
        rt.click(390.0, 290.0);
        assert_eq!(rt.int("outer"), 2);
    }
}

#[test]
fn a_pointer_position_is_local_to_the_node() {
    for mut rt in targets() {
        rt.pointer(30.0, 90.0, PointerPhase::Down);
        assert_eq!(rt.float("local"), 30.0);
        rt.pointer(30.0, 90.0, PointerPhase::Up);
    }
}

impl Rt {
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

    fn children_of_root(&self) -> Vec<NodeId> {
        self.children(self.root.expect("mounted"))
    }

    /// The tab strip, inside the root's third child.
    fn tabs(&self) -> NodeId {
        self.children(self.children_of_root()[2])[0]
    }
}

#[test]
fn a_control_a_region_mounts_responds_and_writes_back() {
    const REGIONS: &str = r#"
export component Checks {
    state show = true;
    state on = false;
    state ids = [1, 2, 3];
    state picked = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            if show {
                Toggle {
                    width: 40dp;
                    height: 20dp;
                    bind checked <=> on;
                }
            }
            for id in ids key id {
                Toggle {
                    width: 40dp;
                    height: 20dp;
                    checked: id == 2;
                    on changed(event) { picked = id; }
                }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(REGIONS), Rt::packaged(REGIONS)] {
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("on"), 1);
        rt.click(10.0, 70.0);
        assert_eq!(rt.int("picked"), 3);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_converted_bind_to_a_native_property_is_e3711() {
    let codes: Vec<String> = build_view_package(
        "export component Field {
            state amount = 5;
            view { Column { TextInput { bind value <=> amount using I64; } } }
        }",
        &origin(),
    )
    .expect_err("does not package")
    .into_iter()
    .map(|d| d.code.to_string())
    .collect();
    assert!(codes.contains(&"E3711".to_string()), "{codes:?}");
}
