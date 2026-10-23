//! A view's `Slider` and `Knob`: each draws a slider node — a bar along its
//! axis, marked at its steps, filled from its origin, or a knob — standing at
//! its value; the keys step the bound state and the thumb follows.

use viso::render::Rect;
use viso::ui::slider::SliderShape;
use viso::ui::{
    Axis, BindingTable, BuildCx, EffectStore, Key, KeyEvent, KeyRouter, NodeId, NodeStore, Role,
    SemanticProjector, StateStore, TextEdits, VirtualLists, focus_node, run_structure_hooks,
};

viso::component! {
    Mixer {
        state level: F32 = 0.25;
        view {
            Column {
                width: 300dp;
                height: 300dp;
                Slider {
                    height: 120dp;
                    bind value <=> level;
                    axis: Axis::column;
                    marks: true;
                    step: Option::Some(0.25);
                    origin: Option::Some(0.5);
                }
                Knob { min: 0.0; max: 10.0; value: 7.5; }
            }
        }
    }
}

struct Mounted {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
}

impl Mounted {
    fn new() -> Self {
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
            Mixer::build(&mut cx).1.id()
        };
        store.layout(root, Rect::new(0.0, 0.0, 400.0, 400.0), &mut Vec::new());
        Self {
            store,
            states,
            bindings,
            root,
        }
    }

    fn child(&self, n: usize) -> NodeId {
        self.store.arena().children(self.root).nth(n).unwrap()
    }

    fn key(&mut self, node: NodeId, key: Key) {
        focus_node(&mut self.store, node);
        let ev = KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut TextEdits::new(),
            self.root,
            ev,
            &mut Vec::new(),
        );
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut EffectStore::new(),
            &mut VirtualLists::new(),
            &changed,
        );
    }
}

#[test]
fn a_slider_runs_up_marked_from_its_origin_and_a_knob_stands_at_its_value() {
    let view = Mounted::new();
    let (bar, knob) = (view.child(0), view.child(1));
    let s = *view.store.slider(bar).expect("a slider draws");
    assert_eq!(s.shape, SliderShape::Bar(Axis::Column));
    assert_eq!((s.marks, s.origin, s.value[1]), (4, Some(0.5), 0.25));
    let b = view.store.bounds(bar);
    assert_eq!((b.w, b.h), (24.0, 120.0), "a bar up is its own width");
    let k = *view.store.slider(knob).expect("a knob draws");
    assert_eq!((k.shape, k.value[1]), (SliderShape::Knob, 0.75));
    let b = view.store.bounds(knob);
    assert_eq!((b.w, b.h), (40.0, 40.0));
    for node in [bar, knob] {
        assert_eq!(view.store.semantics(node).unwrap().role, Role::Slider);
        assert!(view.store.focusable(node));
    }
}

#[test]
fn the_keys_step_the_bound_state_and_the_thumb_follows() {
    let mut view = Mounted::new();
    let bar = view.child(0);
    view.key(bar, Key::Up);
    let s = *view.store.slider(bar).unwrap();
    assert_eq!(s.value[1], 0.5, "a step up of a quarter");
    view.key(bar, Key::End);
    assert_eq!(view.store.slider(bar).unwrap().value[1], 1.0);
    let state = view.store.semantic_state(bar).unwrap();
    assert_eq!((state.value, state.range), (Some(1.0), Some((0.0, 1.0))));
}
