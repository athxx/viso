//! A view's `effect`s at run time and their checks: an effect first runs in
//! the flush after the commit that mounts it, again when a value its
//! `when (..)` list computes changes, and its cleanup runs before each re-run,
//! when its node is freed, and before a hot reload swaps the code that
//! created it. An effect of a component region content mounts runs per
//! mount; one feeding itself stops as a reactive cycle naming it.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::native::Clipboard;
use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::{Origin, compile_file, compile_file_for};
use viso_dsl::hir::{CapabilitySet, TargetProfile};
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload_view};
use viso_dsl::schema::Natives;
use viso_dsl::view_behavior::view_behavior;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, ComputedStore, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, ReactiveCycle, Rect, SemanticProjector, StateStore, TextEdits,
    settle_states,
};
use viso_view::{Value, ViewHost, load_view};

const SOURCE: &str = r#"
import viso::clipboard;

component Item {
    view { Text { width: 20dp; height: 20dp; } }
    effect hello {
        clipboard::write_text("hello");
        cleanup { clipboard::write_text("bye"); }
    }
}

export component Probe {
    state count = 0;
    state show = true;
    state mounted = 0;
    state runs = 0;
    state big = 0;
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text { width: 20dp; height: 20dp; on click { count += 1; } }
            Text { width: 20dp; height: 20dp; on click { show = !show; } }
            Column {
                width: 400dp;
                height: 20dp;
                if show { Item {} }
            }
        }
    }
    effect on_mount {
        transaction { mounted += 1; }
    }
    effect follow when (count) {
        transaction { runs += 1; }
        cleanup { clipboard::write_text("follow"); }
    }
    effect past_one when (count > 1) run EffectRun::change {
        transaction { big += 1; }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["probe".into()],
        language: None,
    }
}

/// A clipboard recording every text written to it.
struct Board(Rc<RefCell<Vec<String>>>);

impl Clipboard for Board {
    fn read_text(&mut self) -> Option<String> {
        self.0.borrow().last().cloned()
    }

    fn write_text(&mut self, text: &str) {
        self.0.borrow_mut().push(text.to_owned());
    }
}

/// A mounted view and the stores it runs over.
#[derive(Default)]
struct Rt {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    computeds: ComputedStore,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<(viso_dsl::ir::binding_ir::NodeKey, NodeId)>,
    view: Option<Rc<RefCell<ViewHost>>>,
    last_good: CandidatePlan,
    board: Rc<RefCell<Vec<String>>>,
}

impl Rt {
    /// `source` mounted by a hot reload into a host granted the clipboard,
    /// then settled.
    fn reloaded(source: &str) -> Self {
        let mut capabilities = CapabilitySet::new();
        capabilities.insert("clipboard.write");
        let profile = TargetProfile {
            capabilities,
            ..TargetProfile::default()
        };
        let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
        let view = view_behavior(&compiled)
            .expect("mounts")
            .expect("has behavior");
        let mut host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("links");
        let mut rt = Rt::default();
        host.services_mut()
            .insert::<Box<dyn Clipboard>>(Box::new(Board(Rc::clone(&rt.board))));
        rt.view = Some(Rc::new(RefCell::new(host)));
        rt.reload(source);
        rt
    }

    /// `source` loaded from its release package, then settled.
    fn packaged(source: &str) -> Self {
        let blob = build_view_package(source, &origin()).expect("packages");
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
        rt.layout();
        rt.settle().expect("settles");
        rt
    }

    fn reload(&mut self, source: &str) {
        let mut live = LiveRuntime {
            store: &mut self.store,
            states: &mut self.states,
            bindings: &mut self.bindings,
            effects: &mut self.effects,
            lists: &mut self.lists,
            text_edits: &mut self.text_edits,
            projectors: &mut self.projectors,
            root: self.root,
            scratch: &mut self.scratch,
            nodes: &mut self.nodes,
            view: &mut self.view,
        };
        let done = hot_reload_view(&mut live, &self.last_good, source, &origin()).expect("reloads");
        self.root = live.root;
        self.last_good = done.candidate;
        self.layout();
        self.settle().expect("settles");
    }

    fn settle(&mut self) -> Result<u32, ReactiveCycle> {
        let mut changed = Vec::new();
        settle_states(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.computeds,
            &mut self.projectors,
            &mut self.effects,
            &mut changed,
        )
    }

    fn layout(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 100.0,
        };
        if let Some(root) = self.root {
            self.store.layout(root, surface, &mut Vec::new());
        }
    }

    /// A primary click at `(x, y)`, then the frame's settle.
    fn click(&mut self, x: f32, y: f32) -> Result<u32, ReactiveCycle> {
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
        let settled = self.settle();
        self.layout();
        settled
    }

    fn count(&mut self) {
        self.click(5.0, 5.0).expect("settles");
    }

    fn toggle(&mut self) {
        self.click(5.0, 25.0).expect("settles");
    }

    fn state(&self, name: &str) -> Option<Value> {
        let host = self.view.as_ref()?.borrow();
        let slot = host.state_slot(name)?;
        host.state(slot).cloned()
    }

    /// `mounted`, `runs` and `big`.
    fn runs(&self) -> [Option<Value>; 3] {
        ["mounted", "runs", "big"].map(|name| self.state(name))
    }

    /// The texts written to the clipboard since the last call.
    fn written(&self) -> Vec<String> {
        std::mem::take(&mut *self.board.borrow_mut())
    }

    fn fault(&self) -> Option<viso_behavior::Fault> {
        self.view.as_ref()?.borrow().last_fault().cloned()
    }
}

fn ints(values: [i64; 3]) -> [Option<Value>; 3] {
    values.map(|v| Some(Value::Int(v)))
}

#[test]
fn an_effect_runs_after_its_mount_and_on_a_changed_dependency_value() {
    let mut rt = Rt::reloaded(SOURCE);
    assert_eq!(rt.fault(), None);
    assert_eq!(rt.runs(), ints([1, 1, 0]), "the mount runs mount effects");
    assert_eq!(rt.written(), ["hello"], "and the region's instance's");

    rt.count();
    assert_eq!(
        rt.runs(),
        ints([1, 2, 0]),
        "`count > 1` kept its value, so `change` did not run"
    );
    assert_eq!(rt.written(), ["follow"], "a re-run cleans up first");

    rt.count();
    assert_eq!(rt.runs(), ints([1, 3, 1]));
    assert_eq!(rt.written(), ["follow"]);

    rt.toggle();
    assert_eq!(rt.written(), ["bye"], "freeing the instance cleans it up");
    rt.toggle();
    assert_eq!(rt.written(), ["hello"], "each mount runs its own");
    assert_eq!(rt.runs(), ints([1, 3, 1]));
    assert_eq!(rt.fault(), None);
}

#[test]
fn a_hot_reload_cleans_up_with_the_prior_code_and_mounts_the_new() {
    let mut rt = Rt::reloaded(SOURCE);
    rt.count();
    rt.count();
    rt.written();
    let edited = SOURCE.replace("write_text(\"follow\")", "write_text(\"follow 2\")");
    rt.reload(&edited);
    let mut written = rt.written();
    written.sort();
    assert_eq!(
        written,
        ["bye", "follow", "hello"],
        "the prior cleanups ran once, then the new mount"
    );
    assert_eq!(
        rt.runs(),
        ints([2, 4, 1]),
        "the recompiled effects mount again; `change` waits for a change"
    );
    rt.count();
    assert_eq!(rt.written(), ["follow 2"]);
    assert_eq!(rt.fault(), None);
}

#[test]
fn freeing_the_view_runs_every_cleanup_once() {
    let mut rt = Rt::reloaded(SOURCE);
    rt.written();
    let root = rt.root.take().expect("mounted");
    rt.store.free_tree(root, &mut rt.effects, &mut rt.scratch);
    let mut written = rt.written();
    written.sort();
    assert_eq!(written, ["bye", "follow"]);
    rt.settle().expect("settles");
    assert!(rt.written().is_empty());
}

#[test]
fn the_release_package_runs_the_same_effects() {
    let source = SOURCE.replace("clipboard::write_text", "untracked");
    let mut rt = Rt::packaged(&source);
    assert_eq!(rt.runs(), ints([1, 1, 0]));
    rt.count();
    rt.count();
    assert_eq!(rt.runs(), ints([1, 3, 1]));
    assert_eq!(rt.fault(), None);
}

#[test]
fn an_effect_feeding_its_own_dependency_stops_as_a_cycle_naming_it() {
    let source = r#"
export component Loop {
    state count = 0;
    view { Text { width: 20dp; height: 20dp; on click { count += 1; } } }
    effect bump when (count) run EffectRun::change {
        transaction { count += 1; }
    }
}
"#;
    let mut rt = Rt::reloaded(source);
    let cycle = rt.click(5.0, 5.0).expect_err("a cycle");
    assert_eq!(ReactiveCycle::CODE, "E4202");
    assert_eq!(cycle.effects, [rt.root.expect("mounted")]);
}

/// No diagnostic.
const CLEAN: [&str; 0] = [];

/// The diagnostic codes compiling `members` into a component reports.
fn codes(members: &str) -> Vec<&'static str> {
    let source = format!(
        "import viso::clipboard;\nexport component C {{\n    state a = 0;\n    state b = 0;\n    \
         computed twice = a * 2;\n    action reset() {{ a = 0; }}\n    {members}\n    \
         view {{ Text {{}} }}\n}}\n"
    );
    let compiled = compile_file(&source, &origin());
    compiled.errors().map(|d| d.code).collect()
}

#[test]
fn the_run_policy_must_fit_the_dependency_list() {
    assert_eq!(codes("effect e { }"), CLEAN);
    assert_eq!(codes("effect e when (a) run EffectRun::change { }"), CLEAN);
    assert_eq!(codes("effect e run EffectRun::mount { }"), CLEAN);
    assert_eq!(
        codes("effect e when (a) run EffectRun::mount { }"),
        ["E4203"]
    );
    assert_eq!(codes("effect e run EffectRun::change { }"), ["E4203"]);
    assert_eq!(
        codes("effect e run EffectRun::mount_and_change { }"),
        ["E4203"]
    );
    assert_eq!(
        codes("effect e when (a) run EffectRun::sometimes { }"),
        ["E4203"]
    );
}

#[test]
fn a_read_the_dependencies_do_not_cover_is_reported() {
    assert_eq!(codes("effect e when (a) { let x = b; }"), ["E4201"]);
    assert_eq!(codes("effect e { let x = a; }"), ["E4201"]);
    assert_eq!(codes("effect e when (a, b) { let x = a + b; }"), CLEAN);
    assert_eq!(
        codes("effect e when (a) { let x = twice; }"),
        CLEAN,
        "a computed derived from the list is covered"
    );
    assert_eq!(codes("effect e when (twice) { let x = a; }"), ["E4201"]);
    assert_eq!(codes("effect e { let x = untracked(a); }"), CLEAN);
}

#[test]
fn an_effect_writes_state_only_in_a_transaction() {
    assert_eq!(codes("effect e { a = 1; }"), ["E2501"]);
    assert_eq!(codes("effect e { transaction { a = 1; } }"), CLEAN);
    assert_eq!(
        codes("effect e { cleanup { transaction { a = 1; } } }"),
        ["E2501"]
    );
    assert_eq!(codes("effect e { reset(); }"), ["E2501"]);
    assert_eq!(codes("effect e { transaction { reset(); } }"), CLEAN);
    assert_eq!(codes("effect e { cleanup { reset(); } }"), ["E2501"]);
    assert_eq!(
        codes("effect e { cleanup { clipboard::write_text(\"x\"); } }"),
        CLEAN
    );
    assert_eq!(codes("action f() { let x = untracked(a); }"), ["E2501"]);
}

#[test]
fn the_dependency_list_is_pure() {
    assert_eq!(codes("effect e when (reset()) { }"), ["E2502"]);
}
