//! A `component!`'s `effect` runs under the macros: the build mounts it on the
//! view's root, the next settle runs it, and a write that changes its
//! dependency runs it again in the settle after.

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, ComputedStore, EffectStore, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, SemanticProjector, StateStore, StateValue, TextEdits,
    VirtualLists, settle_states,
};

viso::component! {
    Follower {
        state count = 0;
        state runs = 0;
        view {
            Column {
                width: 200dp;
                height: 100dp;
                Text { width: 100dp; height: 50dp; on click { count += 1; } }
            }
        }
        effect follow when (count) {
            transaction { runs += 1; }
        }
    }
}

#[test]
fn a_component_effect_runs_after_the_build_and_on_change() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let mut computeds = ComputedStore::new();
    let mut effects = EffectStore::new();
    let (Follower { count, runs }, root) = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        Follower::build(&mut cx)
    };
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    store.layout(root.id(), surface, &mut Vec::new());
    let mut changed = Vec::new();
    let mut settle =
        |store: &mut NodeStore, states: &mut StateStore, bindings: &mut BindingTable| {
            settle_states(
                store,
                states,
                bindings,
                &mut computeds,
                &mut projectors,
                &mut effects,
                &mut changed,
            )
            .expect("settles")
        };
    assert!(store.has_pending_effects(), "the build mounts the effect");
    settle(&mut store, &mut states, &mut bindings);
    assert_eq!(states.get(runs), Some(StateValue::Int(1)));
    assert_eq!(states.get(count), Some(StateValue::Int(0)));

    let mut chain = Vec::new();
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let event = PointerEvent {
            x: 20.0,
            y: 20.0,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Default::default(),
        };
        PointerRouter::route(
            &mut store,
            &mut states,
            &bindings,
            root.id(),
            event,
            &mut chain,
        );
    }
    settle(&mut store, &mut states, &mut bindings);
    assert_eq!(states.get(count), Some(StateValue::Int(1)));
    assert_eq!(states.get(runs), Some(StateValue::Int(2)));
}
