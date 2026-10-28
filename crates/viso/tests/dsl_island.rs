//! A `web-hybrid` build mounts the component of its view's `Island` as the
//! root: it starts with the values the page gives the inputs it binds, a
//! value the page pushes lands at the next frame, and an event it emits runs
//! the page's handler once delivered.

use std::cell::RefCell;
use std::rc::Rc;

use viso::__private::viso_behavior::Value;
use viso::__private::viso_dom::{self, island};
use viso::render::Rect;
use viso::ui::context::UpdateCx;
use viso::ui::{
    BindingTable, BuildCx, EffectStore, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, SemanticProjector, StateStore, TextEdits, VirtualLists, run_structure_hooks,
};

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 400.0,
    h: 300.0,
};

struct Island {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    root: viso::ui::NodeId,
}

impl Island {
    fn mount() -> Self {
        let build = viso::view!("fixtures/island.vs");
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            build(&mut cx).root
        };
        store.layout(root.id(), SURFACE, &mut Vec::new());
        Island {
            store,
            states,
            bindings,
            effects: EffectStore::new(),
            root: root.id(),
        }
    }

    /// Lands what the frame's tasks hand back and what changed.
    fn frame(&mut self) {
        let mut continuations = Vec::new();
        if self.store.tasks_woken() {
            self.store.poll_tasks(&mut continuations);
        }
        for then in continuations.into_iter().flatten() {
            then(&mut UpdateCx::__new(
                &mut self.states,
                &self.bindings,
                &mut self.store,
            ));
        }
        self.settle();
    }

    fn settle(&mut self) {
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
    }

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
        self.settle();
    }

    fn texts(&mut self) -> Vec<String> {
        let mut requests = Vec::new();
        self.store.take_text_requests(&mut requests);
        requests.into_iter().map(|(_, r)| r.text).collect()
    }
}

#[test]
fn the_island_s_component_is_the_root_and_crosses_with_the_page() {
    let mut view = Island::mount();
    // The island's column and its text: the page's own nodes are the DOM's.
    let mut count = 0;
    let mut stack = vec![view.root];
    while let Some(id) = stack.pop() {
        count += 1;
        stack.extend(view.store.arena().children(id));
    }
    assert_eq!(count, 2);
    view.frame();
    assert_eq!(view.texts(), ["one x1"], "the value the page holds");

    // The page pushes its state's new value; it lands at the next frame.
    let page = viso_dom::shared("App").expect("the page is open");
    let name = (0..4)
        .find(|&slot| page.borrow().state(slot) == Value::str("one"))
        .expect("the page holds `name`");
    island::push(0, 0, Value::str("two"));
    view.frame();
    view.frame();
    assert_eq!(view.texts(), ["two x1"]);

    // The island's event runs the page's handler with its record.
    let ran = Rc::new(RefCell::new(Vec::new()));
    let runs = Rc::clone(&ran);
    let handler = Rc::clone(&page);
    island::listen(
        move |entry, payload| {
            runs.borrow_mut().push(entry);
            handler
                .borrow_mut()
                .transact(entry, &[payload])
                .expect("the handler runs");
        },
        || {},
    );
    view.click(20.0, 20.0);
    view.click(20.0, 20.0);
    assert!(ran.borrow().is_empty(), "nothing runs before the delivery");
    island::deliver();
    assert_eq!(ran.borrow().len(), 2);
    let last = (0..4)
        .find(|&slot| slot != name && page.borrow().state(slot) == Value::Int(2))
        .map(|slot| page.borrow().state(slot));
    assert_eq!(last, Some(Value::Int(2)), "the page took the second pick");
}
