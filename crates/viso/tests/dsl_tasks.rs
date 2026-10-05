//! A `component!`'s `start` runs under the macros: the task's UI task belongs
//! to the view's root, the frame that polls it hands the value back, and the
//! `success` handler's write settles like any other. A sleep without host
//! timers waits on the timer thread, whose wake brings the frame.

use std::time::{Duration, Instant};

use viso::render::Rect;
use viso::ui::context::UpdateCx;
use viso::ui::{
    BindingTable, BuildCx, ComputedStore, EffectStore, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, SemanticProjector, StateStore, StateValue, TextEdits,
    VirtualLists, settle_states,
};

viso::component! {
    import viso::time;

    Starter {
        state got = 0;
        state slept = 0;
        task five() -> I64 { 5 }
        task nap() -> I64 { await time::sleep(5ms); 9 }
        view {
            Column {
                width: 200dp;
                height: 100dp;
                Text {
                    width: 100dp; height: 50dp;
                    on click {
                        start five() { success(v) { got = v; } };
                        start nap() { success(v) { slept = v; } };
                    }
                }
            }
        }
    }
}

#[test]
fn a_component_task_hands_its_value_back_at_a_frame_boundary() {
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let mut computeds = ComputedStore::new();
    let mut effects = EffectStore::new();
    let (Starter { got, slept }, root) = {
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        Starter::build(&mut cx)
    };
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    store.layout(root.id(), surface, &mut Vec::new());
    let mut changed = Vec::new();
    let mut frame =
        |store: &mut NodeStore, states: &mut StateStore, bindings: &mut BindingTable| {
            let mut continuations = Vec::new();
            if store.tasks_woken() {
                store.poll_tasks(&mut continuations);
            }
            for then in continuations.into_iter().flatten() {
                then(&mut UpdateCx::__new(states, bindings, store));
            }
            settle_states(
                store,
                states,
                bindings,
                &mut computeds,
                &mut projectors,
                &mut effects,
                &mut changed,
            )
            .expect("settles");
        };
    frame(&mut store, &mut states, &mut bindings);

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
    assert_eq!(store.task_count(), 2, "both tasks belong to the root");
    assert_eq!(states.get(got), Some(StateValue::Int(0)));
    frame(&mut store, &mut states, &mut bindings);
    assert_eq!(states.get(got), Some(StateValue::Int(5)));

    let deadline = Instant::now() + Duration::from_secs(10);
    while states.get(slept) != Some(StateValue::Int(9)) {
        assert!(
            Instant::now() < deadline,
            "the timer thread never woke the task"
        );
        std::thread::sleep(Duration::from_millis(1));
        frame(&mut store, &mut states, &mut bindings);
    }
    assert_eq!(store.task_count(), 0);
}
