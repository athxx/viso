//! Reactive property values under hot reload and the release package: a label
//! and the labels a keyed `for` repeats show their text at mount and again when
//! a state they read changes, a reload that keeps the tree replaces the view's
//! value hook rather than adding one, and a handler's writes commit as one
//! transaction.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload_view};
use viso_ui::Rect;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, SemanticProjector, StateId, StateStore, StateValue, TextEdits,
    run_structure_hooks,
};
use viso_view::{ViewHost, load_view};

const SOURCE: &str = r#"
component Delivered {
    state count = 1;
    state items = [1, 2];
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: 400dp;
                height: 20dp;
                Text { width: 20dp; height: 20dp; on click { count += 1; } }
                Text { width: 20dp; height: 20dp; on click { items = [2, 3]; } }
            }
            Text { width: 100dp; height: 20dp; text: format("{} items", count); }
            Row {
                width: 400dp;
                height: 20dp;
                for item in items key item {
                    Text { width: 20dp; height: 20dp; text: format("{}:{}", item, count); }
                }
            }
        }
    }
}
"#;

const FLAT: &str = r#"
component Flat {
    state count = 1;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Text { width: 20dp; height: 20dp; on click { count += 1; } }
            Text { width: 100dp; height: 20dp; text: format("{} items", count); }
        }
    }
}
"#;

const BATCH: &str = r#"
component Batch {
    state count = 0;
    state items = [1];
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Text {
                width: 100dp;
                height: 20dp;
                text: format("{} items", count);
                on click { count += 1; items = [1, 2]; count += 1; items = [3]; }
            }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["delivered".into()],
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
        let done = hot_reload_view(&mut live, &self.last_good, source, &origin()).expect("reloads");
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
    /// Returns the cells the frame changed and how many hooks ran.
    fn click(&mut self, x: f32, y: f32) -> (Vec<StateId>, u32) {
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
        let ran = run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &changed,
        );
        self.layout();
        (changed, ran)
    }

    /// The texts declared since the last call, by node.
    fn declared(&mut self) -> Vec<(NodeId, String)> {
        let mut requests = Vec::new();
        self.store.take_text_requests(&mut requests);
        requests.into_iter().map(|(id, r)| (id, r.text)).collect()
    }

    /// The texts declared since the last call, sorted.
    fn texts(&mut self) -> Vec<String> {
        let mut texts: Vec<String> = self.declared().into_iter().map(|(_, t)| t).collect();
        texts.sort();
        texts
    }
}

#[test]
fn a_label_and_repeated_labels_show_their_values_under_every_target() {
    for mut rt in [Rt::reloaded(SOURCE), Rt::packaged(SOURCE)] {
        assert_eq!(rt.texts(), ["1 items", "1:1", "2:1"]);
        rt.click(5.0, 5.0);
        assert_eq!(rt.texts(), ["1:2", "2 items", "2:2"], "count 1 -> 2");
        rt.click(25.0, 5.0);
        assert_eq!(rt.texts(), ["3:2"], "only the new item shows a text");
        rt.click(25.0, 5.0);
        assert!(rt.texts().is_empty(), "an unchanged list shows nothing new");
    }
}

#[test]
fn a_reload_keeping_the_tree_replaces_the_value_hook() {
    let mut rt = Rt::reloaded(FLAT);
    let label = match rt.declared().as_slice() {
        [(id, text)] if text == "1 items" => *id,
        other => panic!("one label text, not {other:?}"),
    };
    assert_eq!(rt.store.structure_hook_count(), 1);

    rt.reload(&FLAT.replace("{} items", "{} things"));
    assert_eq!(rt.declared(), [(label, "1 things".to_owned())]);
    assert_eq!(rt.store.structure_hook_count(), 1, "the hook is replaced");

    rt.click(5.0, 5.0);
    assert_eq!(rt.declared(), [(label, "2 things".to_owned())], "once");
}

#[test]
fn a_handler_writing_twice_commits_one_revision_and_one_delivery() {
    for mut rt in [Rt::reloaded(BATCH), Rt::packaged(BATCH)] {
        assert_eq!(rt.texts(), ["0 items"]);
        let (changed, ran) = rt.click(5.0, 5.0);
        let mut values: Vec<Option<StateValue>> =
            changed.iter().map(|&id| rt.states.get(id)).collect();
        values.sort_by_key(|value| format!("{value:?}"));
        assert_eq!(
            values,
            [Some(StateValue::Int(1)), Some(StateValue::Int(2))],
            "`items` raises its revision once and `count` lands once"
        );
        assert_eq!(ran, 1, "one hook run for the frame");
        assert_eq!(rt.texts(), ["2 items"], "one delivery");
    }
}
