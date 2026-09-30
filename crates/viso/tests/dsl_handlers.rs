//! A view's `on` handlers run under the macros: a click on a `component!` or
//! `view!` node runs its handler body in the behavior VM, whose state write lands
//! in the UI cell and flushes through the view's static edges; a native control
//! writes its change back through a `bind`.

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, Handle, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, SemanticProjector, StateId, StateStore, StateValue, TextEdits, VirtualLists,
};

viso::component! {
    Clicker {
        state count = 0;
        view {
            Column {
                width: 200dp;
                height: 100dp;
                Text {
                    width: 100dp;
                    height: 50dp;
                    on click { count += 1; }
                    on key_down(event) { count += 10; }
                }
            }
        }
    }
}

viso::component! {
    Switch {
        state on = false;
        view {
            Column {
                width: 200dp;
                height: 100dp;
                Toggle {
                    width: 40dp;
                    height: 20dp;
                    bind checked <=> on;
                }
            }
        }
    }
}

/// A mounted clicker, laid out at the origin.
struct Mounted {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
    count: StateId,
}

impl Mounted {
    fn mount(build: impl FnOnce(&mut BuildCx<'_>) -> (Handle, StateId)) -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let (root, count) = {
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
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        store.layout(root.id(), surface, &mut Vec::new());
        Self {
            store,
            states,
            bindings,
            root: root.id(),
            count,
        }
    }

    /// A primary click at `(x, y)`, then the state flush.
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
    }

    fn count(&self) -> Option<StateValue> {
        self.states.get(self.count)
    }
}

fn component() -> Mounted {
    Mounted::mount(|cx| {
        let (Clicker { count }, root) = Clicker::build(cx);
        (root, count)
    })
}

fn file() -> Mounted {
    let build = viso::view!("fixtures/clicker.vs");
    let mut fresh = StateStore::new();
    let count = fresh.alloc(StateValue::Int(0));
    Mounted::mount(|cx| (build(cx), count))
}

#[test]
fn a_click_runs_the_handler_once_per_press() {
    for mut form in [component(), file()] {
        assert_eq!(form.count(), Some(StateValue::Int(0)));
        form.click(20.0, 20.0);
        assert_eq!(form.count(), Some(StateValue::Int(1)));
        form.click(20.0, 20.0);
        assert_eq!(form.count(), Some(StateValue::Int(2)));
    }
}

#[test]
fn a_click_outside_the_node_runs_nothing() {
    for mut form in [component(), file()] {
        form.click(150.0, 80.0);
        assert_eq!(form.count(), Some(StateValue::Int(0)));
    }
}

#[test]
fn a_bound_toggle_writes_its_flip_back() {
    let mut form = Mounted::mount(|cx| {
        let (Switch { on }, root) = Switch::build(cx);
        (root, on)
    });
    form.click(10.0, 10.0);
    assert_eq!(form.count(), Some(StateValue::Bool(true)));
    form.click(10.0, 10.0);
    assert_eq!(form.count(), Some(StateValue::Bool(false)));
}
