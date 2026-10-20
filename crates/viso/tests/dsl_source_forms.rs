//! The three DSL source forms mount the same retained tree: an equivalent `ui!`
//! fragment, `component!` body and `view!` file build one node shape and layout.
//! The two forms that declare state bind each state to the same node with the same
//! dirty class and flush a state write through the same single static edge; a
//! fragment is stateless.

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, DirtyClass, Handle, NodeId, NodeStore, SemanticProjector, StateId,
    StateStore, StateValue, TextEdits, VirtualLists,
};

viso::component! {
    Counter {
        state count = 0;
        state enabled = true;
        view {
            Column {
                width: 120dp;
                Text { text: format("{}", count); }
                Text { visible: enabled; }
            }
        }
    }
}

/// A mounted form and the ids of its `count` and `enabled` states, none for a
/// fragment.
struct Mounted {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    ids: Option<[StateId; 2]>,
}

impl Mounted {
    /// Mounts `build`, which returns the root and the `count` and `enabled` ids.
    fn mount(build: impl FnOnce(&mut BuildCx<'_>) -> (Handle, Option<[StateId; 2]>)) -> Self {
        let mut states = StateStore::new();
        let mut store = NodeStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let (root, ids) = {
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
        Self {
            store,
            states,
            bindings,
            root: root.id(),
            ids,
        }
    }

    /// The nodes in pre-order.
    fn nodes(&self) -> Vec<NodeId> {
        fn walk(store: &NodeStore, node: NodeId, out: &mut Vec<NodeId>) {
            out.push(node);
            let mut child = store.arena().links(node).unwrap().first_child;
            while let Some(next) = child {
                walk(store, next, out);
                child = store.arena().links(next).unwrap().next_sibling;
            }
        }
        let mut out = Vec::new();
        walk(&self.store, self.root, &mut out);
        out
    }

    /// Each node's pre-order index, depth-first child count and laid-out box.
    fn shape(&mut self) -> Vec<(usize, [f32; 4])> {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 100.0,
        };
        self.store.layout(self.root, surface, &mut Vec::new());
        self.nodes()
            .into_iter()
            .map(|node| {
                let mut children = 0;
                let mut child = self.store.arena().links(node).unwrap().first_child;
                while let Some(next) = child {
                    children += 1;
                    child = self.store.arena().links(next).unwrap().next_sibling;
                }
                let world = self.store.world(node);
                (children, [world.x, world.y, world.w, world.h])
            })
            .collect()
    }

    /// Each state's bindings as `(pre-order node index, class)`.
    fn edges(&self) -> Vec<Vec<(usize, DirtyClass)>> {
        let nodes = self.nodes();
        self.ids
            .expect("a stateful form")
            .into_iter()
            .map(|state| {
                self.bindings
                    .for_state(state)
                    .map(|binding| {
                        let at = nodes.iter().position(|&n| n == binding.node).unwrap();
                        (at, binding.class)
                    })
                    .collect()
            })
            .collect()
    }

    /// Writes `count`, flushes, and returns the edges applied, each node's dirty
    /// classes in pre-order, and the static edges walked.
    fn flush_count(&mut self) -> (u32, Vec<DirtyClass>, u64) {
        let [count, _] = self.ids.expect("a stateful form");
        self.store.clear_dirty();
        assert!(self.states.set(count, StateValue::Int(7)));
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        let applied = self
            .store
            .flush_state_transactions(&changed, &self.bindings);
        let dirty = self
            .nodes()
            .into_iter()
            .map(|node| self.store.dirty(node))
            .collect();
        let counters = self.bindings.counters();
        assert_eq!(counters.dynamic_binding_eval(), 0);
        assert_eq!(counters.dynamic_fallback_nodes(), 0);
        (applied, dirty, counters.static_binding_eval())
    }
}

fn fragment() -> Mounted {
    let build = viso::ui! {
        Column {
            width: 120dp;
            Text { text: "0"; }
            Text { visible: true; }
        }
    };
    Mounted::mount(|cx| (build(cx), None))
}

fn component() -> Mounted {
    Mounted::mount(|cx| {
        let (Counter { count, enabled }, root) = Counter::build(cx);
        (root, Some([count, enabled]))
    })
}

fn file() -> Mounted {
    let build = viso::view!("fixtures/counter.vs");
    // The closure allocates the file's states into the empty store in declaration
    // order, so they take the ids a fresh store's first two allocations take.
    let mut fresh = StateStore::new();
    let count = fresh.alloc(StateValue::Int(0));
    let enabled = fresh.alloc(StateValue::Bool(true));
    Mounted::mount(|cx| (build(cx), Some([count, enabled])))
}

#[test]
fn component_states_start_from_their_initializers() {
    for form in [component(), file()] {
        let [count, enabled] = form.ids.unwrap();
        assert_eq!(form.states.get(count), Some(StateValue::Int(0)));
        assert_eq!(form.states.get(enabled), Some(StateValue::Bool(true)));
    }
}

#[test]
fn the_three_forms_mount_the_same_tree_and_bindings() {
    let mut reference = component();
    let shape = reference.shape();
    let edges = reference.edges();
    assert_eq!(shape.len(), 3, "a Column holding a Text and a Leaf");
    assert_eq!(shape[0].0, 2);
    assert_eq!(
        edges,
        [
            vec![(
                1,
                DirtyClass::MEASURE
                    | DirtyClass::LAYOUT
                    | DirtyClass::PAINT
                    | DirtyClass::SEMANTICS
            )],
            vec![(
                2,
                DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS
            )],
        ]
    );
    assert_eq!(fragment().shape(), shape);
    let mut file = file();
    assert_eq!(file.shape(), shape);
    assert_eq!(file.edges(), edges);
}

#[test]
fn the_two_stateful_forms_flush_a_write_through_the_same_static_edge() {
    let expected = component().flush_count();
    assert_eq!(expected.0, 1);
    assert_eq!(expected.2, 1);
    assert_eq!(file().flush_count(), expected);
}
