//! The application scene seam: `Application::build` authors a retained scene
//! through a reactive `BuildCx`, exactly as the facade's launch path drives it.
//!
//! Like the other facade tests, this drives the ui stores directly (reachable
//! through `viso::ui`) instead of standing up a scheduler and a window: it
//! constructs the app via `Application::new`, then calls `build` against a
//! `BuildCx::with_reactive` over sibling `NodeStore`/`StateStore`/`BindingTable`
//! — the same three the driver owns as fields and borrows together on launch.

use viso::prelude::*;
use viso::ui::{BindingTable, NodeStore, StateStore};

// An `AppCx` for a headless build, matching how the facade constructs the app.
fn app_cx() -> AppCx<'static> {
    AppCx::__new()
}

/// The default `build` is empty: an application that overrides nothing declares
/// no scene, so the tree has no root — the true minimal empty window.
#[test]
fn default_build_declares_no_scene() {
    struct Empty;
    impl Application for Empty {
        fn new(_cx: &mut AppCx) -> Self {
            Empty
        }
        // no `build` override — the default runs.
    }

    let mut app = Empty::new(&mut app_cx());
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let root = {
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings);
        app.build(&mut cx);
        cx.root()
    };
    assert!(root.is_none(), "the default build authors nothing");
}

/// A custom `build` authors a real scene: it allocates a state cell, declares a
/// node tree, and wires a binding — all reachable afterward through the stores.
#[test]
fn custom_build_authors_a_reactive_scene() {
    struct App {
        count: Option<StateId>,
    }
    impl Application for App {
        fn new(_cx: &mut AppCx) -> Self {
            App { count: None }
        }
        fn build(&mut self, cx: &mut BuildCx<'_>) {
            let count = cx.state(StateValue::Int(0));
            self.count = Some(count);
            cx.flex(FlexStyle::default(), |cx| {
                let bar = cx.leaf(LeafStyle::default());
                cx.bind(count, bar, DirtyClass::PAINT);
            });
        }
    }

    let mut app = App::new(&mut app_cx());
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let root = {
        let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings);
        app.build(&mut cx);
        cx.root()
    };

    // The scene has a root, and the app stashed the state id it allocated.
    assert!(root.is_some(), "the custom build declared a tree");
    let count = app.count.expect("build stashed the state id");
    assert_eq!(states.get(count), Some(StateValue::Int(0)));

    // Writing the cell and flushing marks the bound node — the wired edge lives.
    assert!(states.set(count, StateValue::Int(1)));
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    let applied = store.flush_state_transactions(&changed, &bindings);
    assert_eq!(applied, 1, "the single bound edge applies");
}
