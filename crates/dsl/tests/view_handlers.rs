//! A view's handlers under hot reload and the release package: a click runs the
//! handler in the behavior VM and its write lands in the UI cell; a reload keeps
//! the running state and swaps the handler; a reload whose handler does not mount
//! keeps the last-good handler; a faulting handler is kept, not a panic.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, LiveAnchors, LiveRuntime, hot_reload_view};
use viso_dsl::view_behavior::UNMOUNTED_HANDLER;
use viso_ui::Rect;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, SemanticProjector, StateStore, StateValue, TextEdits,
};
use viso_view::{ViewHost, load_view};

/// A clicker whose handler body is `body`.
fn clicker(body: &str) -> String {
    format!(
        "component Clicker {{
            state count = 0;
            view {{
                Column {{
                    width: 200dp;
                    height: 100dp;
                    Text {{ width: 100dp; height: 50dp; {body} }}
                }}
            }}
        }}"
    )
}

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["clicker".into()],
        language: None,
    }
}

/// The live runtime a reload commits into.
#[derive(Default)]
struct Live {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    view: Option<Rc<RefCell<ViewHost>>>,
    last_good: CandidatePlan,
}

impl Live {
    fn reload(&mut self, source: &str) -> Result<(), Vec<viso_dsl::Diagnostic>> {
        let mut rt = LiveRuntime {
            store: &mut self.store,
            states: &mut self.states,
            bindings: &mut self.bindings,
            effects: &mut self.effects,
            lists: &mut self.lists,
            text_edits: &mut self.text_edits,
            projectors: &mut self.projectors,
            root: self.root,
            scratch: &mut self.scratch,
            view: &mut self.view,
        };
        let done = hot_reload_view(
            &mut rt,
            &self.last_good,
            source,
            &origin(),
            &LiveAnchors::default(),
        )?;
        self.root = rt.root;
        self.last_good = done.candidate;
        layout(&mut self.store, self.root);
        Ok(())
    }

    fn count(&self) -> Option<StateValue> {
        let symbol = self.last_good.symbol_for_name("count")?;
        let key = viso_ui::state::StateKey::from_parts(symbol.hi, symbol.lo);
        self.states.get(self.states.id_for_key(key)?)
    }
}

fn layout(store: &mut NodeStore, root: Option<NodeId>) {
    let surface = Rect {
        x: 0.0,
        y: 0.0,
        w: 400.0,
        h: 300.0,
    };
    if let Some(root) = root {
        store.layout(root, surface, &mut Vec::new());
    }
}

/// A primary click inside the leaf, then the state flush.
fn click(store: &mut NodeStore, states: &mut StateStore, bindings: &BindingTable, root: NodeId) {
    let mut chain = Vec::new();
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let event = PointerEvent {
            x: 20.0,
            y: 20.0,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Default::default(),
        };
        PointerRouter::route(store, states, bindings, root, event, &mut chain);
    }
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    store.flush_state_transactions(&changed, bindings);
}

impl Live {
    fn click(&mut self) {
        let root = self.root.expect("mounted");
        click(&mut self.store, &mut self.states, &self.bindings, root);
    }
}

#[test]
fn a_reload_keeps_the_count_and_swaps_the_handler() {
    let mut live = Live::default();
    live.reload(&clicker("on click { count += 1; }"))
        .expect("mounts");
    live.click();
    assert_eq!(live.count(), Some(StateValue::Int(1)));

    live.reload(&clicker("on click { count += 5; }"))
        .expect("reloads");
    assert_eq!(
        live.count(),
        Some(StateValue::Int(1)),
        "the reload keeps the count"
    );
    live.click();
    assert_eq!(live.count(), Some(StateValue::Int(6)));
}

#[test]
fn a_handler_that_does_not_mount_keeps_the_last_good_one() {
    let mut live = Live::default();
    live.reload(&clicker("on click { count += 1; }"))
        .expect("mounts");
    let errors = live
        .reload(&clicker("on long_press { count += 5; }"))
        .expect_err("the runtime does not deliver long presses");
    assert!(
        errors.iter().all(|e| e.code == UNMOUNTED_HANDLER),
        "{errors:?}"
    );
    live.click();
    assert_eq!(live.count(), Some(StateValue::Int(1)));
}

#[test]
fn a_reload_without_handlers_drops_the_host_and_its_handlers() {
    let mut live = Live::default();
    live.reload(&clicker("on click { count += 1; }"))
        .expect("mounts");
    live.reload(&clicker("")).expect("reloads");
    assert!(live.view.is_none());
    live.click();
    assert_eq!(live.count(), Some(StateValue::Int(0)));
}

#[test]
fn a_faulting_handler_is_kept_and_writes_nothing() {
    let mut live = Live::default();
    live.reload(&clicker("on click { count = count / (count - count); }"))
        .expect("mounts");
    live.click();
    assert_eq!(live.count(), Some(StateValue::Int(0)));
    let host = live.view.as_ref().expect("a host");
    assert!(host.borrow().last_fault().is_some(), "the fault is kept");
}

#[test]
fn the_release_package_runs_the_same_handler() {
    let blob =
        build_view_package(&clicker("on click { count += 1; }"), &origin()).expect("packages");
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let view = load_view(&blob, &mut store, &mut states, &mut bindings, &mut lists).expect("loads");
    let root = view.root.expect("a root");
    layout(&mut store, Some(root));
    assert!(view.host.is_some());
    let count = || {
        let host = view.host.as_ref().unwrap().borrow();
        host.state(0).cloned()
    };
    click(&mut store, &mut states, &bindings, root);
    click(&mut store, &mut states, &bindings, root);
    assert_eq!(count(), Some(viso_view::Value::Int(2)));
}
