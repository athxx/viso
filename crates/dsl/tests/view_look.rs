//! A view's look on its nodes: `background` and `opacity`, constant or read
//! from state, reach the retained nodes at mount and again only when a state
//! they read changes, on the hot-reloaded and the packaged view, and inside a
//! region.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload_view};
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Rect, Rgba, SemanticProjector, StateStore, TextEdits, run_structure_hooks,
};
use viso_view::{ViewHost, load_view};

const SOURCE: &str = r#"
component Look {
    state lit = false;
    state level: F32 = 0.5f32;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            background: #102030;
            Text {
                width: 20dp;
                height: 20dp;
                background: if lit { #ff0000 } else { #00ff0080 };
                opacity: level;
                on click { lit = !lit; level = 1.0f32; }
            }
            Column {
                width: 100dp;
                height: 100dp;
                if lit {
                    Text { width: 10dp; height: 10dp; opacity: 0.25f32; }
                }
            }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["look".into()],
        language: None,
    }
}

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
}

impl Rt {
    fn reloaded() -> Self {
        let mut rt = Rt::default();
        let mut live = LiveRuntime {
            store: &mut rt.store,
            states: &mut rt.states,
            bindings: &mut rt.bindings,
            effects: &mut rt.effects,
            lists: &mut rt.lists,
            text_edits: &mut rt.text_edits,
            projectors: &mut rt.projectors,
            root: None,
            scratch: &mut rt.scratch,
            nodes: &mut rt.nodes,
            view: &mut rt.view,
        };
        hot_reload_view(&mut live, &CandidatePlan::default(), SOURCE, &origin()).expect("reloads");
        rt.root = live.root;
        rt.layout();
        rt
    }

    fn packaged() -> Self {
        let blob = build_view_package(SOURCE, &origin()).expect("packages");
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
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

#[test]
fn a_nodes_look_follows_the_state_it_reads() {
    for mut rt in [Rt::reloaded(), Rt::packaged()] {
        let root = rt.root.expect("mounted");
        let fill = rt.store.style(root).fill;
        assert_eq!(fill, rgba(16.0 / 255.0, 32.0 / 255.0, 48.0 / 255.0, 1.0));
        let [text, region] = rt.children(root)[..] else {
            panic!("a text and a region column");
        };
        assert_eq!(
            rt.store.style(text).fill,
            rgba(0.0, 1.0, 0.0, 128.0 / 255.0)
        );
        assert_eq!(rt.store.opacity(text), 0.5);
        assert!(rt.children(region).is_empty());

        rt.click(5.0, 5.0);
        assert_eq!(rt.store.style(text).fill, rgba(1.0, 0.0, 0.0, 1.0));
        assert_eq!(rt.store.opacity(text), 1.0);
        let [shown] = rt.children(region)[..] else {
            panic!("the region's text");
        };
        assert_eq!(
            rt.store.opacity(shown),
            0.25,
            "a region node shows its look"
        );
    }
}
