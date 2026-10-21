//! A component's named nodes as a `NodeRef`: its handlers focus, blur and
//! scroll them once the view settles, each component instance of a `for`
//! reaches its own, and a named node outside handlers and actions is `E2001`.

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Rect, StateStore, Vec2, run_structure_hooks,
};
use viso_view::load_view;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["nodes".into()],
        language: None,
        ..Default::default()
    }
}

struct Page {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    root: NodeId,
}

impl Page {
    fn load(source: &str) -> Self {
        let blob = build_view_package(source, &origin()).expect("packages");
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let view =
            load_view(&blob, &mut store, &mut states, &mut bindings, &mut lists).expect("loads");
        let mut page = Self {
            store,
            states,
            bindings,
            effects: EffectStore::default(),
            root: view.root.expect("a root"),
        };
        page.layout();
        page
    }

    fn layout(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        self.store.layout(self.root, surface, &mut Vec::new());
    }

    /// A primary click in the middle of `node`, the state flush, and layout.
    fn click(&mut self, node: NodeId) {
        let rect = self.store.world(node);
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x: rect.x + rect.w * 0.5,
                y: rect.y + rect.h * 0.5,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                self.root,
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
            &mut VirtualLists::new(),
            &changed,
        );
        self.layout();
    }

    fn child(&self, parent: NodeId, at: usize) -> NodeId {
        self.store
            .arena()
            .children(parent)
            .nth(at)
            .expect("a child")
    }
}

#[test]
fn a_handler_focuses_blurs_and_scrolls_its_named_nodes() {
    let mut page = Page::load(
        "component Form {
            state open = false;
            view {
                Column {
                    width: 200dp;
                    height: 300dp;
                    Button {
                        width: 100dp; height: 20dp; text: \"go\";
                        on click { field.focus(); list.scroll_to(Offset { x: 0dp, y: 30dp }); }
                    }
                    Button {
                        width: 100dp; height: 20dp; text: \"more\";
                        on click { list.scroll_by(Offset { x: 0dp, y: 15dp }); field.blur(); }
                    }
                    node field: TextInput { width: 100dp; height: 20dp; }
                    node list: Scroll {
                        width: 100dp; height: 50dp;
                        Text { width: 100dp; height: 400dp; text: \"tall\"; }
                    }
                }
            }
        }",
    );
    let go = page.child(page.root, 0);
    let more = page.child(page.root, 1);
    let field = page.child(page.root, 2);
    let list = page.child(page.root, 3);
    assert_eq!(page.store.focused(), None);

    page.click(go);
    assert_eq!(page.store.focused(), Some(field));
    assert_eq!(page.store.scroll(list), Vec2 { x: 0.0, y: 30.0 });

    page.click(more);
    assert_ne!(page.store.focused(), Some(field), "blurred");
    assert_eq!(page.store.scroll(list), Vec2 { x: 0.0, y: 45.0 });
}

#[test]
fn each_instance_of_a_for_reaches_its_own_named_node() {
    let mut page = Page::load(
        "component Entry {
            view {
                Column {
                    width: 100dp; height: 40dp;
                    Button { width: 100dp; height: 20dp; text: \"edit\"; on click { field.focus(); } }
                    node field: TextInput { width: 100dp; height: 20dp; }
                }
            }
        }
        export component Entries {
            state items = [1, 2, 3];
            view {
                Column {
                    width: 200dp; height: 300dp;
                    for item in items key item { Entry {} }
                }
            }
        }",
    );
    for at in [1, 0, 2] {
        let entry = page.child(page.root, at);
        let edit = page.child(entry, 0);
        let field = page.child(entry, 1);
        page.click(edit);
        assert_eq!(page.store.focused(), Some(field), "entry {at}");
    }
}

#[test]
fn a_named_node_is_a_node_ref_only_in_handlers_and_actions() {
    let source = "component Form {
        view {
            Column {
                node field: TextInput { width: 100dp; height: 20dp; }
                Text { text: format(\"{}\", field); }
            }
        }
    }";
    let errors = build_view_package(source, &origin()).expect_err("rejected");
    assert!(
        errors.iter().any(|d| d.code == "E2001"),
        "{:?}",
        errors.iter().map(|d| &d.code).collect::<Vec<_>>()
    );
}
