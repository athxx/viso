//! A view component's `@persist` state: loaded from the host's store before
//! the view mounts, stored when a committed write changes it, made durable
//! on suspend, kept across a hot reload, and in the release package;
//! `E9106` when a component persisting state mounts inside another's view,
//! `E6103` at run time when the view is not granted `storage.persist`; a
//! host with no store of its own persisting through the one the app installed.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::game::{MemoryStore, PERSIST_CAPABILITY, Persist, PersistReport};
use viso_dsl::aot::build_view_package_for;
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{CapabilitySet, TargetProfile};
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, plan_view_for, transact};
use viso_dsl::schema::Natives;
use viso_dsl::view_behavior::view_behavior;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, ComputedStore, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, Rect, SemanticProjector, StateStore, TextEdits, settle_states,
};
use viso_view::persist::SharedStore;
use viso_view::{Value, ViewHost, load_view_with};

const SOURCE: &str = r#"
export component Counter {
    @persist("clicks")
    state clicks = 0;
    @persist("title")
    state title = "a";
    state taps = 0;
    view {
        Column {
            width: 100dp;
            height: 100dp;
            Text { width: 20dp; height: 20dp; on click { clicks += 1; taps += 1; title = title + "a"; } }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["counter".into()],
        language: None,
    }
}

fn profile(granted: bool) -> TargetProfile {
    let mut capabilities = CapabilitySet::new();
    if granted {
        capabilities.insert(PERSIST_CAPABILITY);
    }
    TargetProfile {
        capabilities,
        ..TargetProfile::default()
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
}

impl Rt {
    /// `source` loaded from its release package, persisting through `store`.
    fn packaged(source: &str, store: &MemoryStore) -> Self {
        let blob = build_view_package_for(source, &origin(), profile(true)).expect("packages");
        let mut rt = Rt::default();
        let view = load_view_with(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
            &mut |host| host.services_mut().insert(Persist::new(store.clone())),
        )
        .expect("loads");
        rt.root = view.root;
        rt.view = view.host;
        rt.settle();
        rt
    }

    /// `source` mounted by a hot reload into a host persisting through
    /// `store`, linked with `storage.persist` granted or not.
    fn reloaded(source: &str, store: &MemoryStore, granted: bool) -> Self {
        let compiled = compile_file_for(source, &origin(), Natives::standard(), profile(true));
        let view = view_behavior(&compiled)
            .expect("mounts")
            .expect("has behavior");
        let grant: &[&str] = if granted { &[PERSIST_CAPABILITY] } else { &[] };
        let mut host = ViewHost::with_capabilities(Rc::clone(&view.module), &view.component, grant)
            .expect("links");
        host.services_mut().insert(Persist::new(store.clone()));
        let mut rt = Rt {
            view: Some(host.shared()),
            ..Rt::default()
        };
        rt.reload(source);
        rt
    }

    /// `source` mounted by a hot reload into a host with no store of its
    /// own, granted `storage.persist`.
    fn reloaded_from_installed(source: &str) -> Self {
        let compiled = compile_file_for(source, &origin(), Natives::standard(), profile(true));
        let view = view_behavior(&compiled)
            .expect("mounts")
            .expect("has behavior");
        let host = ViewHost::with_capabilities(
            Rc::clone(&view.module),
            &view.component,
            &[PERSIST_CAPABILITY],
        )
        .expect("links");
        let mut rt = Rt {
            view: Some(host.shared()),
            ..Rt::default()
        };
        rt.reload(source);
        rt
    }

    fn reload(&mut self, source: &str) {
        let candidate = plan_view_for(source, &origin(), profile(true)).expect("compiles");
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
        let done = transact(&mut live, &self.last_good, candidate);
        self.root = live.root;
        self.last_good = done.candidate;
        self.settle();
    }

    fn settle(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        if let Some(root) = self.root {
            self.store.layout(root, surface, &mut Vec::new());
        }
        settle_states(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.computeds,
            &mut self.projectors,
            &mut self.effects,
            &mut Vec::new(),
        )
        .expect("settles");
    }

    fn click(&mut self) {
        let root = self.root.expect("mounted");
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x: 5.0,
                y: 5.0,
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
        self.settle();
    }

    fn host(&self) -> std::cell::RefMut<'_, ViewHost> {
        self.view.as_ref().expect("a host").borrow_mut()
    }

    /// The value of state `name`, read through its cell.
    fn state(&self, name: &str) -> Value {
        let host = self.host();
        let slot = host.state_slot(name).expect("a state");
        host.current(slot, &self.states).expect("a value")
    }

    fn suspend(&mut self) {
        let host = Rc::clone(self.view.as_ref().expect("a host"));
        host.borrow_mut().suspend(&self.states);
    }

    /// What did not load or store: queued app-wide at the mount, held by
    /// the host after a direct suspend.
    fn reports(&self) -> Vec<PersistReport> {
        let mut reports = viso_view::take_persist_reports();
        reports.extend(self.host().take_persist_reports());
        reports
    }
}

#[test]
fn a_packaged_view_loads_and_stores_its_persisted_state() {
    let store = MemoryStore::default();
    let mut rt = Rt::packaged(SOURCE, &store);
    assert_eq!(rt.state("clicks"), Value::Int(0));
    assert!(store.keys().is_empty(), "nothing changed yet");
    rt.click();
    rt.click();
    assert_eq!(rt.state("clicks"), Value::Int(2));
    rt.suspend();
    assert_eq!(store.keys(), ["clicks", "title"]);
    assert!(rt.reports().is_empty());

    let rt = Rt::packaged(SOURCE, &store);
    assert_eq!(rt.state("clicks"), Value::Int(2), "loaded before mount");
    assert_eq!(rt.state("title"), Value::str("aaa"));
    assert_eq!(rt.state("taps"), Value::Int(0), "not persisted");
}

#[test]
fn a_view_without_a_store_of_its_own_persists_through_the_installed_one() {
    let store = MemoryStore::default();
    viso_view::install_persistence(Some(SharedStore::new(store.clone())));
    let blob = build_view_package_for(SOURCE, &origin(), profile(true)).expect("packages");
    let load = |rt: &mut Rt| {
        let view = load_view_with(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
            &mut |_| {},
        )
        .expect("loads");
        rt.root = view.root;
        rt.view = view.host;
        rt.settle();
    };
    let mut rt = Rt::default();
    load(&mut rt);
    assert_eq!(rt.store.suspend_hook_count(), 1);
    rt.click();
    assert_eq!(store.keys(), ["clicks", "title"], "stored at the commit");
    rt.store.suspend(&rt.states);
    assert!(rt.reports().is_empty());
    assert_eq!(
        rt.store.suspend_hook_count(),
        1,
        "kept while the view lives"
    );

    let mut reloaded = Rt::reloaded_from_installed(SOURCE);
    assert_eq!(reloaded.state("clicks"), Value::Int(1));
    reloaded.reload(SOURCE);
    assert_eq!(
        reloaded.store.suspend_hook_count(),
        1,
        "a reload registers no second hook"
    );
    viso_view::install_persistence(None);
}

#[test]
fn a_hot_reload_keeps_the_live_value_and_loads_a_newly_persisted_state() {
    let store = MemoryStore::default();
    let mut rt = Rt::reloaded(SOURCE, &store, true);
    rt.click();
    rt.suspend();
    let mut seeded = Rt::reloaded(SOURCE, &store, true);
    assert_eq!(
        seeded.state("clicks"),
        Value::Int(1),
        "loaded on first mount"
    );
    seeded.click();
    seeded.click();
    seeded.reload(&SOURCE.replace("state taps", "@persist(\"taps\")\n    state taps"));
    assert_eq!(
        seeded.state("clicks"),
        Value::Int(3),
        "the reload keeps the live value"
    );
    assert_eq!(
        seeded.state("taps"),
        Value::Int(2),
        "a state the edit persists starts from what it held"
    );
    seeded.suspend();
    assert!(store.keys().contains(&"taps".to_owned()));
}

#[test]
fn an_ungranted_view_reports_and_stores_nothing() {
    let store = MemoryStore::default();
    let mut rt = Rt::reloaded(SOURCE, &store, false);
    let reports = rt.reports();
    let codes: Vec<_> = reports.iter().map(|r| (r.code, &*r.key)).collect();
    assert_eq!(codes, [("E6103", "clicks"), ("E6103", "title")]);
    rt.click();
    rt.suspend();
    assert!(store.keys().is_empty());
    rt.reload(SOURCE);
    assert!(rt.reports().is_empty(), "each state reports once");
}

#[test]
fn a_persisting_component_mounts_only_as_the_view_s_own() {
    let source = r#"
component Inner {
    @persist("k")
    state n = 0;
    view { Text { text: "x"; } }
}

export component Outer {
    view { Column { Inner {} } }
}
"#;
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile(true));
    let errors: Vec<_> = compiled.errors().map(|d| d.code).collect();
    assert_eq!(errors, ["E9106"], "{:#?}", compiled.diagnostics);
    let own = compile_file_for(SOURCE, &origin(), Natives::standard(), profile(true));
    assert!(!own.has_errors(), "{:#?}", own.diagnostics);
    let ungranted = compile_file_for(SOURCE, &origin(), Natives::standard(), profile(false));
    let errors: Vec<_> = ungranted.errors().map(|d| d.code).collect();
    assert_eq!(errors, ["E9106", "E9106"]);
}
