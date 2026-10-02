//! The adaptive environment under the macros: a `component!` reads `env` in a
//! region's choice and in a handler, and an instance a `view!` file's region
//! mounts reads its own `env` fields, each as of the last settle; the frame
//! loop settles the environment against each layout it runs.

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::render::Rect;
use viso::ui::adaptive::Environment;
use viso::ui::{
    BindingTable, BuildCx, EffectStore, Handle, NodeId, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, SemanticProjector, StateId, StateStore, StateValue, TextEdits,
    VirtualLists, run_structure_hooks,
};

viso::component! {
    Adaptive {
        state taps = 0;
        view {
            Column {
                width: 400dp;
                height: 300dp;
                Text {
                    width: 20dp;
                    height: 20dp;
                    on click { if env.text_scale > 1.5 { taps += 1; } }
                }
                Column {
                    width: 400dp;
                    height: 60dp;
                    if env.size_class == SizeClass::Compact {
                        Text { width: 10dp; height: 10dp; }
                    } else {
                        Text { width: 10dp; height: 10dp; }
                        Text { width: 10dp; height: 10dp; }
                    }
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
}

impl Mounted {
    fn mount(build: impl FnOnce(&mut BuildCx<'_>) -> Handle) -> Self {
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
            build(&mut cx)
        };
        let mut mounted = Self {
            store,
            states,
            bindings,
            effects: EffectStore::default(),
            root: root.id(),
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

    /// A primary click at `(x, y)`, then the frame's flush.
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
        self.flush();
    }

    /// Changes the environment, settles it against the last layout, then runs
    /// the frame's flush.
    fn update_env(&mut self, change: impl FnOnce(&mut Environment)) {
        self.states.update_env(change);
        self.states.settle_env(&self.store);
        self.flush();
    }

    fn flush(&mut self) {
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

#[test]
fn a_component_reads_the_settled_environment() {
    let mut taps: Option<StateId> = None;
    let mut form = Mounted::mount(|cx| {
        let (Adaptive { taps: id }, root) = Adaptive::build(cx);
        taps = Some(id);
        root
    });
    let taps = taps.expect("a component exposes its state");
    assert_eq!(form.region(1).len(), 1, "a zero-width window is compact");
    form.update_env(|e| e.window.width = 1000.0);
    assert_eq!(form.region(1).len(), 2, "expanded");
    form.click(5.0, 5.0);
    assert_eq!(form.states.get(taps), Some(StateValue::Int(0)));
    form.update_env(|e| e.text_scale = 2.0);
    form.click(5.0, 5.0);
    assert_eq!(form.states.get(taps), Some(StateValue::Int(1)));
}

#[test]
fn an_instance_a_region_mounts_reads_its_own_env() {
    let build = viso::view!("fixtures/adaptive.vs");
    let mut form = Mounted::mount(build);
    let panel = |form: &Mounted| {
        let root = *form.region(1).first()?;
        Some(form.children(root).len())
    };
    assert_eq!(panel(&form), None);
    form.click(5.0, 5.0);
    assert_eq!(panel(&form), Some(1), "compact");
    form.update_env(|e| e.window.width = 1000.0);
    assert_eq!(panel(&form), Some(2), "the anchored class moved");
    form.click(5.0, 5.0);
    assert_eq!(panel(&form), None);
    assert_eq!(
        form.states.env().anchor_count(),
        0,
        "the anchor is released"
    );
}

#[test]
fn a_component_under_an_adaptive_scope_reads_the_scope_class() {
    let build = viso::ui! {
        Row {
            width: 400dp;
            height: 300dp;
            Column {
                width: 300dp;
                height: 300dp;
                AdaptiveScope { Adaptive {} }
            }
        }
    };
    let mut form = Mounted::mount(build);
    let region = |form: &Mounted| {
        let column = form.children(form.root)[0];
        let scope = form.children(column)[0];
        let adaptive = form.children(scope)[0];
        form.children(form.children(adaptive)[1]).len()
    };
    form.update_env(|e| e.window.width = 1000.0);
    assert_eq!(region(&form), 1, "the scope classifies its 300dp");
}

/// Two adaptive scopes, 300dp and 900dp wide, each holding an `Adaptive`.
struct ScopedApp;

impl Application for ScopedApp {
    fn new(_cx: &mut AppCx) -> Self {
        ScopedApp
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let build = viso::ui! {
            Row {
                width: 1200dp;
                height: 300dp;
                Column {
                    width: 300dp;
                    height: 300dp;
                    AdaptiveScope { Adaptive {} }
                }
                Column {
                    width: 900dp;
                    height: 300dp;
                    AdaptiveScope { Adaptive {} }
                }
            }
        };
        build(cx);
    }
}

#[test]
fn the_frame_loop_settles_the_environment_against_its_layout() {
    let app = drive_scripted::<ScopedApp>(
        vec![RawEvent::RedrawRequested {
            window: WindowId(1),
        }],
        Duration::from_millis(16),
    );
    let store = app.store();
    let children = |node: NodeId| {
        let arena = store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(node).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    };
    let regions: Vec<usize> = children(app.root().expect("a root"))
        .into_iter()
        .map(|column| {
            let adaptive = children(children(column)[0])[0];
            children(children(adaptive)[1]).len()
        })
        .collect();
    assert_eq!(regions, [1, 2], "compact at 300dp, expanded at 900dp");
    assert!(!app.states().env().unsettled());
}
