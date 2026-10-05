//! A view's tasks: what `start` runs once the transaction that starts it
//! commits.
//!
//! A task is a fiber of the host's VM carried by a chain of UI tasks: each
//! runs until the task awaits a native's work, and the UI task that awaits it
//! hands the result back in a continuation at the frame boundary, where the
//! fiber resumes. A task that returns runs its `success` / `error` handler as a
//! new transaction in the scope it started in.
//!
//! The UI tasks belong to the node of the component instance that started
//! them — the view's root, or the instance's root in region content — so
//! freeing that node drops the task and no handler runs. A named slot (`start
//! .. as job`) runs its tasks under the start's policy: `keep_latest` cancels
//! the running one, running its `cancelled` handler; `drop_new` ignores the new
//! one; `queue` and `parallel(n)` hold it until the slot has room. A hot reload
//! cancels every task with the code that started it; a continuation from
//! before a reload is inert.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use viso_behavior::native::NativeError;
use viso_behavior::{Fault, FaultKind, Fiber, Start, TaskPolicy, TaskStep, Value};
use viso_ui::context::UpdateCx;
use viso_ui::{Continuation, NodeId, NodeStore, StateStore, TaskFuture, TaskId};

use crate::host::{StateCells, ViewHost};
use crate::resources::Loading;
use crate::scope::Scope;

/// The tasks a view runs and the starts its slots hold.
#[derive(Default)]
pub(crate) struct Tasks {
    live: Vec<Live>,
    queued: Vec<Queued>,
    next: u64,
    /// The tokens of the tasks whose UI task was dropped unfinished, which
    /// freeing their owner does.
    dropped: Rc<RefCell<Vec<u64>>>,
}

/// A running task.
struct Live {
    token: u64,
    /// The UI task carrying it now.
    ui: Option<TaskId>,
    slot: Option<SlotKey>,
    sink: Sink,
    scope: Scope,
    owner: NodeId,
}

/// Where a task's value goes.
pub(crate) enum Sink {
    /// To the `start`'s handlers: `done` takes the value, `cancelled` runs on
    /// a cancellation.
    Handlers {
        done: Option<Value>,
        cancelled: Option<Value>,
    },
    /// To the resource that started it as its loader.
    Resource(Loading),
}

/// A start a slot holds until it has room.
struct Queued {
    slot: SlotKey,
    start: Start,
    scope: Scope,
    owner: NodeId,
}

/// A task slot: an instance's named slot, in the mount of region content that
/// keeps the instance (by address; `0` outside region content).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SlotKey {
    mount: usize,
    instance: u32,
    slot: u32,
    policy: TaskPolicy,
}

impl SlotKey {
    fn same(&self, other: &SlotKey) -> bool {
        (self.mount, self.instance, self.slot) == (other.mount, other.instance, other.slot)
    }

    /// How many of the slot's tasks run at once.
    fn room(&self) -> u32 {
        match self.policy {
            TaskPolicy::Parallel(n) => n,
            _ => 1,
        }
    }
}

/// What a continuation hands back to a task.
enum Resume {
    /// The work its fiber awaited finished.
    Fiber(Fiber, Result<Value, NativeError>),
    /// It returned.
    Done(Value),
}

/// Reports a UI task dropped before it finished.
struct Watch {
    token: u64,
    dropped: Rc<RefCell<Vec<u64>>>,
    armed: bool,
}

impl Watch {
    /// The task finished: dropping the watch now reports nothing.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        if self.armed
            && let Ok(mut dropped) = self.dropped.try_borrow_mut()
        {
            dropped.push(self.token);
        }
    }
}

impl ViewHost {
    /// Starts the tasks a committed transaction in `scope` queued.
    pub(crate) fn launch(&mut self, starts: Vec<Start>, scope: &Scope, cells: &mut dyn StateCells) {
        if starts.is_empty() {
            return;
        }
        self.forget_dropped();
        for start in starts {
            let mount = scope
                .locals
                .iter()
                .rev()
                .find_map(|locals| Some((locals, locals.owner_of(start.instance)?)));
            let Some(owner) = mount.map(|(_, owner)| owner).or(self.root) else {
                self.task_fault("the view is not mounted, so it starts no task");
                continue;
            };
            let slot = start.slot.map(|slot| SlotKey {
                mount: mount.map_or(0, |(locals, _)| Rc::as_ptr(locals).addr()),
                instance: start.instance,
                slot,
                policy: start.policy,
            });
            if let Some(slot) = slot {
                let running: Vec<u64> = self
                    .tasks
                    .live
                    .iter()
                    .filter(|live| live.slot.is_some_and(|s| s.same(&slot)))
                    .map(|live| live.token)
                    .collect();
                match slot.policy {
                    TaskPolicy::KeepLatest => {
                        for token in running {
                            self.cancel(token, cells);
                        }
                    }
                    TaskPolicy::DropNew if !running.is_empty() => continue,
                    TaskPolicy::Queue | TaskPolicy::Parallel(_)
                        if running.len() as u32 >= slot.room() =>
                    {
                        self.tasks.queued.push(Queued {
                            slot,
                            start,
                            scope: scope.clone(),
                            owner,
                        });
                        continue;
                    }
                    _ => {}
                }
            }
            let sink = Sink::Handlers {
                done: start.done.clone(),
                cancelled: start.cancelled.clone(),
            };
            self.run_start(start, slot, sink, scope.clone(), owner, cells);
        }
    }

    /// Runs `start`'s task until it first awaits, then hands it to a UI task
    /// owned by `owner`; its value goes to `sink`. Returns its token while it
    /// runs.
    pub(crate) fn run_start(
        &mut self,
        start: Start,
        slot: Option<SlotKey>,
        sink: Sink,
        scope: Scope,
        owner: NodeId,
        cells: &mut dyn StateCells,
    ) -> Option<u64> {
        self.sync(&scope, &*cells);
        let step = self
            .vm
            .start_task(&mut self.instance, start.task, &start.args);
        self.instance.clear_dirty();
        let step = match step {
            Ok(step) => step,
            Err(fault) => {
                self.fault = Some(fault);
                self.make_room(slot, cells);
                return None;
            }
        };
        let token = self.tasks.next;
        self.tasks.next += 1;
        self.tasks.live.push(Live {
            token,
            ui: None,
            slot,
            sink,
            scope,
            owner,
        });
        self.carry(token, step, cells);
        self.tasks
            .live
            .iter()
            .any(|l| l.token == token)
            .then_some(token)
    }

    /// Hands `step` of task `token` to a UI task, whose continuation resumes
    /// or finishes it: a task that returned before it first awaited still
    /// finishes at the frame boundary, not inside the transaction that
    /// started it.
    fn carry(&mut self, token: u64, step: TaskStep, cells: &mut dyn StateCells) {
        let Some(at) = self.tasks.live.iter().position(|l| l.token == token) else {
            return;
        };
        let (host, epoch) = (Weak::clone(&self.this), self.epoch);
        let mut watch = Watch {
            token,
            dropped: Rc::clone(&self.tasks.dropped),
            armed: true,
        };
        let future: TaskFuture = match step {
            TaskStep::Done(value) => Box::pin(async move {
                watch.disarm();
                Some(continuation(host, epoch, token, Resume::Done(value)))
            }),
            TaskStep::Awaiting(fiber, work) => Box::pin(async move {
                let result = work.await;
                watch.disarm();
                Some(continuation(
                    host,
                    epoch,
                    token,
                    Resume::Fiber(fiber, result),
                ))
            }),
        };
        let owner = self.tasks.live[at].owner;
        match cells.spawn(owner, future) {
            Some(ui) => self.tasks.live[at].ui = Some(ui),
            None => {
                let live = self.tasks.live.remove(at);
                self.forget_dropped();
                if self.this.strong_count() == 0 {
                    self.task_fault("the view is not shared, so it runs no task");
                }
                self.make_room(live.slot, cells);
            }
        }
    }

    /// Continues task `token` with what its UI task handed back.
    fn advance(&mut self, token: u64, resume: Resume, cells: &mut dyn StateCells) {
        self.forget_dropped();
        if !self.tasks.live.iter().any(|l| l.token == token) {
            return;
        }
        match resume {
            Resume::Fiber(fiber, result) => {
                let step = self.vm.resume_task(&mut self.instance, fiber, result);
                self.instance.clear_dirty();
                match step {
                    Ok(TaskStep::Done(value)) => self.finish(token, Some(value), cells),
                    Ok(step) => self.carry(token, step, cells),
                    Err(fault) => {
                        self.fault = Some(fault);
                        self.finish(token, None, cells);
                    }
                }
            }
            Resume::Done(value) => self.finish(token, Some(value), cells),
        }
    }

    /// Ends task `token`, running its `done` handler with the value it
    /// returned, and starts what its slot held.
    fn finish(&mut self, token: u64, value: Option<Value>, cells: &mut dyn StateCells) {
        let Some(at) = self.tasks.live.iter().position(|l| l.token == token) else {
            return;
        };
        let live = self.tasks.live.remove(at);
        match (value, &live.sink) {
            (
                Some(value),
                Sink::Handlers {
                    done: Some(done), ..
                },
            ) => self.transact(done, &[value], &live.scope, cells),
            (value, Sink::Resource(loading)) => {
                self.settle(token, loading, value, &live.scope, live.owner, cells);
            }
            _ => {}
        }
        self.make_room(live.slot, cells);
    }

    /// Cancels task `token`: drops its UI task and runs its `cancelled`
    /// handler.
    pub(crate) fn cancel(&mut self, token: u64, cells: &mut dyn StateCells) {
        let Some(at) = self.tasks.live.iter().position(|l| l.token == token) else {
            return;
        };
        let live = self.tasks.live.remove(at);
        if let Some(ui) = live.ui {
            cells.cancel_task(ui);
        }
        if let Sink::Handlers {
            cancelled: Some(cancelled),
            ..
        } = &live.sink
        {
            self.transact(cancelled, &[], &live.scope, cells);
        }
    }

    /// Cancels every task, each running its `cancelled` handler, and drops
    /// what the slots held: what a hot reload does before it swaps the code.
    pub(crate) fn cancel_tasks(&mut self, cells: &mut dyn StateCells) {
        self.forget_dropped();
        self.tasks.queued.clear();
        while let Some(live) = self.tasks.live.first() {
            let token = live.token;
            self.cancel(token, cells);
        }
    }

    /// The number of running tasks.
    pub fn task_count(&self) -> usize {
        self.tasks.live.len()
    }

    /// Runs `handler` with `args` in `scope` as one transaction, then the
    /// starts it queued.
    fn transact(
        &mut self,
        handler: &Value,
        args: &[Value],
        scope: &Scope,
        cells: &mut dyn StateCells,
    ) {
        self.sync(scope, &*cells);
        match self.vm.call_value(&mut self.instance, handler, args) {
            Ok(outcome) => {
                self.events.extend(outcome.events);
                self.write_back(scope, cells);
                self.launch(outcome.starts, scope, cells);
            }
            Err(fault) => {
                self.instance.clear_dirty();
                self.fault = Some(fault);
            }
        }
    }

    /// Starts the first start `slot` holds, when it has room now.
    fn make_room(&mut self, slot: Option<SlotKey>, cells: &mut dyn StateCells) {
        let Some(slot) = slot else {
            return;
        };
        let running = self
            .tasks
            .live
            .iter()
            .filter(|l| l.slot.is_some_and(|s| s.same(&slot)))
            .count() as u32;
        if running >= slot.room() {
            return;
        }
        let Some(at) = self.tasks.queued.iter().position(|q| q.slot.same(&slot)) else {
            return;
        };
        let queued = self.tasks.queued.remove(at);
        let sink = Sink::Handlers {
            done: queued.start.done.clone(),
            cancelled: queued.start.cancelled.clone(),
        };
        self.run_start(
            queued.start,
            Some(queued.slot),
            sink,
            queued.scope,
            queued.owner,
            cells,
        );
    }

    /// Forgets the tasks whose owner went away, and what their slots held.
    fn forget_dropped(&mut self) {
        let dropped = std::mem::take(&mut *self.tasks.dropped.borrow_mut());
        for token in dropped {
            let Some(at) = self.tasks.live.iter().position(|l| l.token == token) else {
                continue;
            };
            let live = self.tasks.live.remove(at);
            if let Some(slot) = live.slot {
                self.tasks.queued.retain(|q| !q.slot.same(&slot));
            }
        }
    }

    fn task_fault(&mut self, message: &str) {
        self.fault = Some(Fault {
            kind: FaultKind::Internal,
            at: None,
            message: message.to_owned(),
        });
    }
}

/// The continuation handing `resume` back to task `token` of the host, unless
/// a hot reload replaced the code since.
fn continuation(
    host: Weak<RefCell<ViewHost>>,
    epoch: u64,
    token: u64,
    resume: Resume,
) -> Continuation {
    Box::new(move |cx: &mut UpdateCx<'_>| {
        let Some(host) = host.upgrade() else {
            return;
        };
        let Ok(mut host) = host.try_borrow_mut() else {
            return;
        };
        if host.epoch == epoch {
            host.advance(token, resume, cx);
        }
    })
}

/// Cancels every task the view `host` runs, each running its `cancelled`
/// handler with the code that started it: what a hot reload does first.
pub fn release_tasks(host: &Rc<RefCell<ViewHost>>, store: &mut NodeStore, states: &mut StateStore) {
    if let Ok(mut view) = host.try_borrow_mut() {
        view.cancel_tasks(&mut Frame { store, states });
    }
}

/// The cells and tasks outside any dispatch.
struct Frame<'a> {
    store: &'a mut NodeStore,
    states: &'a mut StateStore,
}

impl StateCells for Frame<'_> {
    fn get(&self, id: viso_ui::StateId) -> Option<viso_ui::StateValue> {
        self.states.get(id)
    }

    fn set(&mut self, id: viso_ui::StateId, value: viso_ui::StateValue) -> bool {
        self.states.set(id, value)
    }

    fn env(&self) -> &viso_ui::adaptive::AdaptiveEnv {
        self.states.env()
    }

    fn spawn(&mut self, owner: NodeId, future: TaskFuture) -> Option<TaskId> {
        self.store.spawn_task(owner, future)
    }

    fn cancel_task(&mut self, id: TaskId) {
        self.store.cancel_task(id);
    }
}
