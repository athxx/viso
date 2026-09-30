//! A view's control-flow regions under the macros: a `component!` and a
//! `view!` mount their static nodes directly and their `if`, `match` and keyed
//! `for` regions through the runtime's structure hooks, which switch arms and
//! move retained nodes after the flush.

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, EffectStore, Handle, NodeId, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, SemanticProjector, StateId, StateStore, StateValue, TextEdits,
    VirtualLists, run_structure_hooks,
};

viso::component! {
    Regions {
        state open = false;
        state mode = 0;
        state picked = 0;
        state items = [1, 2, 3];
        view {
            Column {
                width: 400dp;
                height: 300dp;
                Row {
                    width: 400dp;
                    height: 20dp;
                    Text { width: 20dp; height: 20dp; on click { open = !open; } }
                    Text { width: 20dp; height: 20dp; on click { mode += 1; } }
                    Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2]; } }
                    Text { width: 20dp; height: 20dp; on click { items = [1, 1]; } }
                }
                Column {
                    width: 400dp;
                    height: 60dp;
                    if open preserve "panel" {
                        Text { width: 10dp; height: 10dp; }
                    } else {
                        Text { width: 10dp; height: 10dp; }
                        Text { width: 10dp; height: 10dp; }
                    }
                }
                Column {
                    width: 400dp;
                    height: 60dp;
                    match mode {
                        0 => { Text { width: 10dp; height: 10dp; } },
                        1 => { Row { width: 10dp; height: 10dp; Text { width: 5dp; height: 5dp; } } },
                        _ => { },
                    }
                }
                Row {
                    width: 400dp;
                    height: 20dp;
                    for item in items key item {
                        Text { width: 20dp; height: 20dp; on click { picked = item; } }
                    }
                }
            }
        }
    }
}

viso::component! {
    Tally {
        state open = false;
        view {
            Column {
                width: 20dp;
                height: 40dp;
                Text { width: 20dp; height: 20dp; on click { open = !open; } }
                if open {
                    Text { width: 20dp; height: 20dp; }
                }
            }
        }
    }
}

/// A mounted view, laid out at the origin.
struct Mounted {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    root: NodeId,
    picked: Option<StateId>,
}

impl Mounted {
    fn mount(build: impl FnOnce(&mut BuildCx<'_>) -> (Handle, Option<StateId>)) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let (root, picked) = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            build(&mut cx)
        };
        let mut mounted = Self {
            store,
            states,
            bindings,
            effects: EffectStore::default(),
            root: root.id(),
            picked,
        };
        mounted.layout();
        mounted
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

    /// A primary click at `(x, y)`, then the flush and the structure hooks.
    fn click(&mut self, x: f32, y: f32) {
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
        self.children(self.children(self.root)[index])
    }
}

fn component() -> Mounted {
    Mounted::mount(|cx| {
        let (Regions { picked, .. }, root) = Regions::build(cx);
        (root, Some(picked))
    })
}

fn file() -> Mounted {
    let build = viso::view!("fixtures/regions.vs");
    Mounted::mount(|cx| (build(cx), None))
}

#[test]
fn an_if_switches_arms_and_keeps_a_preserved_arm() {
    for mut form in [component(), file()] {
        assert_eq!(form.region(1).len(), 2, "the else arm");
        form.click(5.0, 5.0);
        let panel = form.region(1);
        assert_eq!(panel.len(), 1, "the then arm");
        form.click(5.0, 5.0);
        assert_eq!(form.region(1).len(), 2, "back to the else arm");
        form.click(5.0, 5.0);
        assert_eq!(form.region(1), panel, "the preserved node comes back");
    }
}

#[test]
fn a_match_switches_on_its_scrutinee() {
    for mut form in [component(), file()] {
        let region = form.region(2);
        assert_eq!(region.len(), 1);
        assert!(form.children(region[0]).is_empty(), "arm 0 is a leaf");
        form.click(25.0, 5.0);
        let region = form.region(2);
        assert_eq!(form.children(region[0]).len(), 1, "arm 1 is a row");
        form.click(25.0, 5.0);
        assert!(form.region(2).is_empty(), "the wildcard arm is empty");
    }
}

#[test]
fn a_keyed_for_moves_its_nodes_on_a_reorder() {
    for mut form in [component(), file()] {
        let before = form.region(3);
        assert_eq!(before.len(), 3);
        form.click(45.0, 5.0);
        assert_eq!(form.region(3), vec![before[2], before[0], before[1]]);
        form.click(65.0, 5.0);
        assert_eq!(
            form.region(3),
            vec![before[2], before[0], before[1]],
            "a repeated key keeps the items"
        );
    }
}

#[test]
fn a_for_item_reaches_its_handler() {
    let mut form = component();
    let picked = form.picked.expect("a component exposes its state");
    form.click(25.0, 150.0);
    assert_eq!(form.states.get(picked), Some(StateValue::Int(2)));
    form.click(45.0, 5.0);
    form.click(5.0, 150.0);
    assert_eq!(form.states.get(picked), Some(StateValue::Int(3)));
}

#[test]
fn each_component_a_fragment_mounts_keeps_its_own_state() {
    let build = viso::ui! {
        Row {
            width: 400dp;
            height: 40dp;
            Tally { }
            Tally { }
        }
    };
    let mut form = Mounted::mount(|cx| (build(cx), None));
    assert_eq!([form.region(0).len(), form.region(1).len()], [1, 1]);
    form.click(25.0, 5.0);
    assert_eq!([form.region(0).len(), form.region(1).len()], [1, 2]);
    form.click(5.0, 5.0);
    assert_eq!([form.region(0).len(), form.region(1).len()], [2, 2]);
    form.click(25.0, 5.0);
    assert_eq!([form.region(0).len(), form.region(1).len()], [2, 1]);
}
