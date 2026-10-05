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
use viso_dsl::frontend::{Origin, compile_file};
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload_view};
use viso_dsl::view_behavior::view_behavior;
use viso_ui::context::UpdateCx;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, ComputedStore, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent,
    PointerPhase, PointerRouter, Rect, SemanticProjector, StateStore, TextEdits, settle_states,
};
use viso_view::{Value, ViewHost, load_view};

pub fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["tasks".into()],
        language: None,
    }
}

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
    pub nodes: Vec<(viso_dsl::ir::binding_ir::NodeKey, NodeId)>,
    pub view: Option<Rc<RefCell<ViewHost>>>,
    pub last_good: CandidatePlan,
    pub clock: Clock,
}

impl Rt {
    /// `source` mounted by a hot reload into a host with the test clock.
    pub fn mount(source: &str) -> Self {
        let compiled = compile_file(source, &origin());
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
        self.frame();
    }

    /// One frame: the due tasks' continuations, the settle and the layout.
    pub fn frame(&mut self) {
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
        if let Some(root) = self.root {
            let surface = Rect {
                x: 0.0,
                y: 0.0,
                w: 400.0,
                h: 100.0,
            };
            self.store.layout(root, surface, &mut Vec::new());
        }
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
