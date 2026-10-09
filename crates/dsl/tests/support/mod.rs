//! The runtime harness the view task and resource tests share: a view mounted
//! by a hot reload or loaded from its release package, a manual clock its
//! sleeps wait on, and the frame loop.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use viso_behavior::native::Timers;
use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::{Compiled, Origin, compile_file, compile_file_for};
use viso_dsl::hir::{CapabilitySet, TargetProfile};
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload_view};
use viso_dsl::schema::Natives;
use viso_dsl::view_behavior::view_behavior;
use viso_ui::adaptive::Environment;
use viso_ui::context::UpdateCx;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, ComputedStore, EffectStore, LengthEnv, NodeId, NodeStore, PointerButtons,
    PointerEvent, PointerPhase, PointerRouter, Rect, SemanticProjector, StateStore, TextEdits,
    settle_states,
};
use viso_view::{Value, ViewHost, load_view};

pub fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["tasks".into()],
        language: None,
    }
}

/// The surface every frame lays the view out into.
pub const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 400.0,
    h: 100.0,
};

/// A sleep the test fires.
#[derive(Default)]
struct Alarm {
    duration: Duration,
    rung: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

/// Timers that ring only when the test says so.
#[derive(Clone, Default)]
pub struct Clock(Rc<RefCell<Vec<Rc<Alarm>>>>);

impl Clock {
    /// Rings every pending sleep; returns how many.
    pub fn ring(&self) -> usize {
        self.ring_upto(Duration::MAX)
    }

    /// Rings every pending sleep of at most `longest`; returns how many.
    pub fn ring_upto(&self, longest: Duration) -> usize {
        let (alarms, rest) = std::mem::take(&mut *self.0.borrow_mut())
            .into_iter()
            .partition::<Vec<_>, _>(|alarm| alarm.duration <= longest);
        *self.0.borrow_mut() = rest;
        for alarm in &alarms {
            alarm.rung.set(true);
            if let Some(waker) = alarm.waker.borrow_mut().take() {
                waker.wake();
            }
        }
        alarms.len()
    }

    pub fn pending(&self) -> usize {
        self.0.borrow().len()
    }
}

struct Sleep(Rc<Alarm>);

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0.rung.get() {
            return Poll::Ready(());
        }
        *self.0.waker.borrow_mut() = Some(cx.waker().clone());
        Poll::Pending
    }
}

impl Timers for Clock {
    fn sleep(&mut self, duration: Duration) -> Pin<Box<dyn Future<Output = ()>>> {
        let alarm = Rc::new(Alarm {
            duration,
            ..Alarm::default()
        });
        self.0.borrow_mut().push(Rc::clone(&alarm));
        Box::pin(Sleep(alarm))
    }
}

/// A mounted view, the stores it runs over and its clock.
#[derive(Default)]
pub struct Rt {
    pub store: NodeStore,
    pub states: StateStore,
    pub bindings: BindingTable,
    pub computeds: ComputedStore,
    pub effects: EffectStore,
    pub lists: VirtualLists,
    pub text_edits: TextEdits,
    pub projectors: SemanticProjector,
    pub root: Option<NodeId>,
    pub scratch: Vec<NodeId>,
    pub nodes: Vec<Option<NodeId>>,
    pub view: Option<Rc<RefCell<ViewHost>>>,
    pub last_good: CandidatePlan,
    pub clock: Clock,
}

impl Rt {
    /// `source` mounted by a hot reload into a host with the test clock.
    pub fn mount(source: &str) -> Self {
        Rt::mount_compiled(source, compile_file(source, &origin()))
    }

    /// `source` mounted for a package granted `capabilities`.
    pub fn mount_granted(source: &str, capabilities: &[&str]) -> Self {
        let mut granted = CapabilitySet::new();
        for &capability in capabilities {
            granted.insert(capability);
        }
        let profile = TargetProfile {
            capabilities: granted,
            ..TargetProfile::default()
        };
        let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
        Rt::mount_compiled(source, compiled)
    }

    fn mount_compiled(source: &str, compiled: Compiled) -> Self {
        let errors: Vec<_> = compiled.errors().collect();
        assert!(errors.is_empty(), "{errors:#?}");
        let view = view_behavior(&compiled)
            .expect("mounts")
            .expect("has behavior");
        let mut host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("links");
        let mut rt = Rt::default();
        host.services_mut()
            .insert::<Box<dyn Timers>>(Box::new(rt.clock.clone()));
        rt.view = Some(host.shared());
        rt.reload(source);
        rt
    }

    /// `source` loaded from its release package, with the test clock.
    pub fn packaged(source: &str) -> Self {
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
        if let Some(host) = &rt.view {
            host.borrow_mut()
                .services_mut()
                .insert::<Box<dyn Timers>>(Box::new(rt.clock.clone()));
        }
        rt.frame();
        rt
    }

    pub fn reload(&mut self, source: &str) {
        self.try_reload(source).expect("reloads");
    }

    /// A hot reload of `source`; on a rejected source the view keeps its last
    /// good version and the diagnostics come back.
    pub fn try_reload(&mut self, source: &str) -> Result<(), Vec<viso_dsl::Diagnostic>> {
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
        let done = hot_reload_view(&mut live, &self.last_good, source, &origin())?;
        self.root = live.root;
        self.last_good = done.candidate;
        self.frame();
        Ok(())
    }

    /// One frame: the due tasks' continuations, the settle and the layout.
    pub fn frame(&mut self) {
        self.settle();
        if let Some(root) = self.root {
            self.store.layout(root, SURFACE, &mut Vec::new());
        }
    }

    /// The due tasks' continuations and the settle, without the layout.
    pub fn settle(&mut self) {
        let mut continuations = Vec::new();
        if self.store.tasks_woken() {
            self.store.poll_tasks(&mut continuations);
        }
        for then in continuations.into_iter().flatten() {
            then(&mut UpdateCx::__new(
                &mut self.states,
                &self.bindings,
                &mut self.store,
            ));
        }
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
        .expect("settles");
    }

    /// Changes the environment as the platform reports it — the text scale
    /// also moves the `sp` lengths — settles it against the last layout,
    /// then settles the states, without a layout.
    pub fn update_env(&mut self, change: impl FnOnce(&mut Environment)) {
        self.states.update_env(change);
        let scale = self.states.env().environment().text_scale;
        let lengths = self.store.length_env();
        if lengths.text_scale != scale {
            self.store.set_length_env(LengthEnv {
                text_scale: scale,
                ..lengths
            });
        }
        self.states.settle_env(&self.store);
        self.settle();
    }

    /// Every node under the root, the root included, in pre-order.
    pub fn descendants(&self) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack: Vec<NodeId> = self.root.into_iter().collect();
        while let Some(node) = stack.pop() {
            out.push(node);
            stack.extend(self.children(node).into_iter().rev());
        }
        out
    }

    /// The incremental layout of what the settle left dirty: how many nodes
    /// it measured and placed.
    pub fn relayout(&mut self) -> (u32, u32) {
        let root = self.root.expect("mounted");
        self.store
            .relayout_dirty(root, SURFACE, &mut Vec::new(), &mut Vec::new())
    }

    /// Rings the pending sleeps, then runs a frame.
    pub fn ring(&mut self) -> usize {
        let rung = self.clock.ring();
        self.frame();
        rung
    }

    /// Rings the pending sleeps of at most `longest`, then runs a frame.
    pub fn ring_upto(&mut self, longest: Duration) -> usize {
        let rung = self.clock.ring_upto(longest);
        self.frame();
        rung
    }

    /// A primary click on the `n`th 20dp button of the root column, then a
    /// frame.
    pub fn click(&mut self, n: usize) {
        self.press(n);
        self.frame();
    }

    /// The click alone.
    pub fn press(&mut self, n: usize) {
        let root = self.root.expect("mounted");
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x: 5.0,
                y: 5.0 + 20.0 * n as f32,
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
    }

    /// The children of `node`, in order.
    pub fn children(&self, node: NodeId) -> Vec<NodeId> {
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
    pub fn region(&self, index: usize) -> Vec<NodeId> {
        let root = self.root.expect("mounted");
        self.children(self.children(root)[index])
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        let host = self.view.as_ref()?.borrow();
        let slot = host.state_slot(name)?;
        host.state(slot).and_then(Value::as_int)
    }

    pub fn ints<const N: usize>(&self, names: [&str; N]) -> [Option<i64>; N] {
        names.map(|name| self.int(name))
    }

    pub fn fault(&self) -> Option<viso_behavior::Fault> {
        self.view.as_ref()?.borrow().last_fault().cloned()
    }
}
