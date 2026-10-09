//! One todo-list app — a keyed `for` of a user component whose event a handler
//! takes, an `if` and a `match` — mounted by `view!`, by the hot-reload commit
//! and from the release package, and by `ui!` over a `component!` declaration.
//! A `component!` declares one component and a `ui!` instance takes no
//! property, so that form writes the user component's view where it is
//! mounted, as every other form inlines it. Every form must reach the same
//! tree and the same boxes after every click.

use std::cell::RefCell;
use std::rc::Rc;

use viso::render::Rect;
use viso::ui::{
    BindingTable, BuildCx, EffectStore, Handle, NodeId, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, SemanticProjector, StateStore, TextEdits, VirtualLists,
    run_structure_hooks,
};
use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload_view};
use viso_view::{ViewHost, load_view};

const SOURCE: &str = include_str!("fixtures/todo_app.vs");

viso::component! {
    TodoApp {
        state items = [1, 2, 3];
        state done = 0;
        state show = true;
        view {
            Column {
                width: 400dp;
                height: 300dp;
                Row {
                    width: 400dp;
                    height: 20dp;
                    Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2]; } }
                    Text { width: 20dp; height: 20dp; on click { items = [3, 1, 2, 4]; } }
                    Text { width: 20dp; height: 20dp; on click { items = [2, 4]; } }
                    Text { width: 20dp; height: 20dp; on click { show = !show; } }
                }
                Column {
                    width: 400dp;
                    height: 80dp;
                    for item in items key item {
                        Row {
                            width: 400dp;
                            height: 20dp;
                            Text { width: 20dp; height: 20dp; on click { done = item; } }
                        }
                    }
                }
                Column {
                    width: 400dp;
                    height: 20dp;
                    if show {
                        Text { width: 30dp; height: 20dp; }
                    } else {
                        Text { width: 10dp; height: 10dp; }
                        Text { width: 10dp; height: 10dp; }
                    }
                }
                Column {
                    width: 400dp;
                    height: 20dp;
                    match done {
                        0 => { },
                        2 => { Text { width: 50dp; height: 20dp; } },
                        _ => { Text { width: 5dp; height: 5dp; } },
                    }
                }
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

/// A mounted app and the stores it runs over.
#[derive(Default)]
struct Rt {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<Option<NodeId>>,
    view: Option<Rc<RefCell<ViewHost>>>,
}

impl Rt {
    /// The app a macro expansion builds.
    fn built(build: impl FnOnce(&mut BuildCx<'_>) -> Handle) -> Self {
        let mut rt = Rt::default();
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut rt.store,
                &mut rt.states,
                &mut rt.bindings,
                &mut rt.lists,
                &mut rt.text_edits,
                &mut rt.projectors,
            );
            build(&mut cx)
        };
        rt.root = Some(root.id());
        rt
    }

    /// The app a hot reload mounts.
    fn reloaded() -> Self {
        let mut rt = Rt::default();
        let mut live = LiveRuntime {
            store: &mut rt.store,
            states: &mut rt.states,
            bindings: &mut rt.bindings,
            effects: &mut rt.effects,
            lists: &mut rt.lists,
            text_edits: &mut rt.text_edits,
            projectors: &mut rt.projectors,
            root: None,
            scratch: &mut rt.scratch,
            nodes: &mut rt.nodes,
            view: &mut rt.view,
        };
        hot_reload_view(&mut live, &CandidatePlan::default(), SOURCE, &origin()).expect("reloads");
        rt.root = live.root;
        rt
    }

    /// The app loaded from its release package.
    fn packaged() -> Self {
        let blob = build_view_package(SOURCE, &origin()).expect("packages");
        let mut rt = Rt::default();
        let view = load_view(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        )
        .expect("loads");
        rt.root = view.root;
        rt.view = view.host;
        rt
    }

    /// A primary click at `(x, y)`, then the frame's flush and structure hooks.
    fn click(&mut self, x: f32, y: f32) {
        let root = self.root.expect("mounted");
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
                root,
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
            &mut self.effects,
            &changed,
        );
    }

    /// The rows of the keyed `for`, in order.
    fn rows(&self) -> Vec<NodeId> {
        let list = self.children(self.root.expect("mounted"))[1];
        self.children(list)
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

    /// Each node in pre-order: its depth and its laid-out box.
    fn snapshot(&mut self) -> Frame {
        let root = self.root.expect("mounted");
        self.store.layout(root, SURFACE, &mut Vec::new());
        let mut out = Vec::new();
        let mut stack = vec![(root, 0)];
        while let Some((node, depth)) = stack.pop() {
            let b = self.store.bounds(node);
            out.push((depth, [b.x, b.y, b.w, b.h]));
            let children = self.children(node);
            stack.extend(children.into_iter().rev().map(|c| (c, depth + 1)));
        }
        out
    }
}

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["todo".into()],
        language: None,
    }
}

/// The clicks the app is driven by: pick the second item, reorder, pick the
/// first, append, pick the third, hide, remove, pick the second and show again.
const CLICKS: [(f32, f32); 9] = [
    (5.0, 45.0),
    (5.0, 5.0),
    (5.0, 25.0),
    (25.0, 5.0),
    (5.0, 65.0),
    (65.0, 5.0),
    (45.0, 5.0),
    (5.0, 45.0),
    (65.0, 5.0),
];

/// The click that reorders `[1, 2, 3]` to `[3, 1, 2]`.
const REORDER: usize = 1;

type Frame = Vec<(usize, [f32; 4])>;

/// The snapshot after mounting and after each click.
fn run(mut rt: Rt) -> Vec<Frame> {
    let mut frames = vec![rt.snapshot()];
    for (step, (x, y)) in CLICKS.into_iter().enumerate() {
        let before = rt.rows();
        rt.click(x, y);
        if step == REORDER {
            assert_eq!(
                rt.rows(),
                [before[2], before[0], before[1]],
                "a reorder moves the keyed rows"
            );
        }
        frames.push(rt.snapshot());
    }
    if let Some(host) = &rt.view {
        assert!(host.borrow().last_fault().is_none(), "the app faulted");
    }
    frames
}

/// The widths of the depth-2 nodes whose top lies in `top`.
fn widths(frame: &Frame, top: std::ops::Range<f32>) -> Vec<f32> {
    frame
        .iter()
        .filter(|(depth, b)| *depth == 2 && top.contains(&b[1]))
        .map(|(_, b)| b[2])
        .collect()
}

#[test]
fn a_todo_app_runs_identically_under_every_mount_path() {
    let file = viso::view!("fixtures/todo_app.vs");
    let fragment = viso::ui! { TodoApp { } };
    let reference = run(Rt::built(file));
    let forms = [
        ("ui!", run(Rt::built(fragment))),
        ("hot reload", run(Rt::reloaded())),
        ("package", run(Rt::packaged())),
    ];
    for (name, frames) in forms {
        for (step, (got, want)) in frames.iter().zip(&reference).enumerate() {
            assert_eq!(got, want, "{name} differs from view! after {step} clicks");
        }
    }

    let rows: Vec<usize> = reference
        .iter()
        .map(|f| widths(f, 20.0..100.0).len())
        .collect();
    assert_eq!(rows, [3, 3, 3, 3, 4, 4, 4, 2, 2, 2], "one row per item");
    let shown: Vec<usize> = reference
        .iter()
        .map(|f| widths(f, 100.0..120.0).len())
        .collect();
    assert_eq!(
        shown,
        [1, 1, 1, 1, 1, 1, 2, 2, 2, 1],
        "the `if` switches arms"
    );
    let done: Vec<Vec<f32>> = reference.iter().map(|f| widths(f, 120.0..140.0)).collect();
    assert_eq!(
        done,
        [
            vec![],
            vec![50.0],
            vec![50.0],
            vec![5.0],
            vec![5.0],
            vec![50.0],
            vec![50.0],
            vec![50.0],
            vec![5.0],
            vec![5.0],
        ],
        "each pick reaches the handler with its item"
    );
}
