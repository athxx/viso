//! A view's `ProgressBar`, `ProgressRing` and `Spinner`: a bar shows its
//! value's fraction of its range and reports the value, a value the view
//! writes eases in, a ring with no value runs, and a spinner turns.

use std::time::Duration;

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, ProgressShape, Role, SemanticProjector, StateStore, TextEdits,
    VirtualLists, run_structure_hooks,
};

viso::component! {
    Upload {
        state done: Option<F32> = Option::None;
        view {
            Column {
                width: 200dp;
                height: 100dp;
                Text {
                    width: 100dp;
                    height: 20dp;
                    on click { done = Option::Some(30.0); }
                }
                ProgressBar {
                    value: done;
                    max: 120.0;
                    label: "Uploading";
                }
                ProgressRing { tint: Option::Some(#ff0000); }
                Spinner { label: "Loading"; }
            }
        }
    }
}

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 400.0,
    h: 300.0,
};

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
            Upload::build(&mut cx).1.id()
        };
        store.layout(root, SURFACE, &mut Vec::new());
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
            &mut EffectStore::new(),
            &mut VirtualLists::new(),
            &changed,
        );
    }
}

#[test]
fn a_bar_runs_until_the_view_gives_it_a_value_which_then_eases_in() {
    let mut form = Mounted::new();
    let bar = form.child(1);
    let shown = form.store.progress(bar).unwrap();
    assert_eq!(shown.shape, ProgressShape::Bar);
    assert_eq!(shown.value, None, "no value yet: it runs");
    assert_eq!(form.store.bounds(bar).w, 160.0, "its natural width");
    assert_eq!(form.store.bounds(bar).h, 4.0);
    assert_eq!(
        form.store.semantics(bar).unwrap().role,
        Role::ProgressIndicator
    );
    assert_eq!(form.store.semantic_state(bar).unwrap().value, None);

    form.click(10.0, 10.0);
    let shown = form.store.progress(bar).unwrap();
    assert_eq!(shown.value, Some(0.25));
    assert_eq!(shown.shown, 0.0, "from nothing, run in");
    let state = form.store.semantic_state(bar).unwrap();
    assert_eq!(state.value, Some(30.0));
    assert_eq!(state.range, Some((0.0, 120.0)));
    for _ in 0..60 {
        form.store.tick_progress(Duration::from_millis(16));
    }
    assert_eq!(form.store.progress(bar).unwrap().shown, 0.25);
}

#[test]
fn a_ring_with_no_value_runs_and_a_spinner_turns() {
    let form = Mounted::new();
    let ring = form.store.progress(form.child(2)).unwrap();
    assert_eq!(ring.shape, ProgressShape::Ring);
    assert_eq!(ring.value, None);
    assert!(ring.color.r > 0.9 && ring.color.g < 0.1, "its tint");
    assert!(form.store.is_progressing());
    let spinner = form.child(3);
    assert!(form.store.spinner(spinner).unwrap().spinning);
    assert_eq!(
        form.store.semantics(spinner).unwrap().label.as_deref(),
        Some("Loading")
    );
}
