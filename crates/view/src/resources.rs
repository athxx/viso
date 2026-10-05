//! A view's `resource`s: a state only its loader writes.
//!
//! A resource mounts as an effect whose gate evaluates its key: on the mount
//! and on each commit that changes the key, its state moves to `loading` — or
//! to `reloading(value)` while it holds a value — and its loader task starts,
//! after the `debounce` delay when the policy sets one. The task's `Ok` value
//! settles the state to `ready`, its `Err` to `error`, as one transaction, but
//! only while the key is still the one it loaded: a stale result never
//! overwrites the state of a newer key. `keep_latest` cancels the load in
//! flight when the key changes; without it, that load finishes and fills the
//! cache. `cache_for(d)` keeps a value under its key for `d` after it settled,
//! and a key that comes back within it settles at once without loading;
//! `cache_errors` caches errors too.
//!
//! The loads, the debounce and the cache's expiries are UI tasks of the node
//! the resource mounts on, so freeing it drops them all. A hot reload cancels
//! the loads with the code that started them and remounts the resource, which
//! keeps the state it carried and loads again: the new loader may load
//! differently, so the cache does not survive it.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use viso_behavior::{Aggregate, FaultKind, ResourceLoad, Value};
use viso_ui::context::UpdateCx;
use viso_ui::{NodeId, TaskFuture, TaskId};

use crate::effects::Mount;
use crate::host::{StateCells, ViewHost};
use crate::scope::Scope;
use crate::tasks::Sink;

/// The tags of the `ResourceState` variants.
const LOADING: i64 = 1;
const READY: u32 = 2;
const ERROR: u32 = 3;
const RELOADING: u32 = 4;

/// A mounted resource, shared by its effect's closures and its tasks.
pub(crate) struct Resource {
    mount: Rc<Mount>,
    /// The handler-table entry of the loader.
    loader: u32,
    load: ResourceLoad,
    /// The node it is mounted on, which owns its tasks.
    owner: NodeId,
    run: RefCell<Run>,
}

/// Where a resource's loading is.
#[derive(Default)]
struct Run {
    /// The key the gate last evaluated.
    key: Option<Value>,
    /// Raised by each key change, so a debounce from before it starts
    /// nothing.
    generation: u64,
    /// The tokens of the loads in flight.
    loads: Vec<u64>,
    /// The debounce waiting to start a load.
    debounce: Option<TaskId>,
    /// The cached states, by key.
    cache: Vec<Cached>,
    /// The next cache entry's stamp.
    stamp: u64,
}

/// A cached state and the task expiring it.
struct Cached {
    key: Value,
    state: Value,
    stamp: u64,
    expiry: Option<TaskId>,
}

/// A load in flight: the resource it settles and the key it loads.
pub(crate) struct Loading {
    resource: Weak<Resource>,
    key: Value,
}

impl Resource {
    pub(crate) fn new(
        mount: Rc<Mount>,
        loader: u32,
        load: ResourceLoad,
        owner: NodeId,
    ) -> Rc<Self> {
        Rc::new(Resource {
            mount,
            loader,
            load,
            owner,
            run: RefCell::default(),
        })
    }

    /// Records `key`, the value the gate evaluated.
    pub(crate) fn key(&self, key: &Value) {
        self.run.borrow_mut().key = Some(key.clone());
    }

    /// The key changed (or the resource mounted): settles from the cache, or
    /// moves the state to `loading` and starts the loader, now or after the
    /// debounce.
    pub(crate) fn changed(self: &Rc<Self>, cells: &mut dyn StateCells) {
        let Some(host) = self.mount.live() else {
            return;
        };
        let Ok(mut view) = host.try_borrow_mut() else {
            return;
        };
        let scope = &self.mount.scope;
        let (key, debounce, flight, hit) = {
            let mut run = self.run.borrow_mut();
            let Some(key) = run.key.clone() else {
                return;
            };
            run.generation += 1;
            let flight = if self.load.keep_latest {
                std::mem::take(&mut run.loads)
            } else {
                Vec::new()
            };
            let hit = run
                .cache
                .iter()
                .find(|c| c.key == key)
                .map(|c| c.state.clone());
            (key, run.debounce.take(), flight, hit)
        };
        if let Some(id) = debounce {
            cells.cancel_task(id);
        }
        for token in flight {
            view.cancel(token, cells);
        }
        if let Some(state) = hit {
            view.dispatch(self.load.write, state, scope, cells);
            return;
        }
        let current = match view.evaluate(self.load.state, scope, None, &*cells) {
            Ok(current) => current,
            Err(fault) => {
                view.record_fault(fault);
                return;
            }
        };
        let next = match &current {
            Value::Agg(agg) if matches!(agg.tag, READY | RELOADING) => {
                let value = agg.fields.first().cloned().unwrap_or_default();
                payload(RELOADING, value)
            }
            _ => Value::Int(LOADING),
        };
        view.dispatch(self.load.write, next, scope, cells);
        match self.load.debounce {
            Some(delay) => {
                let generation = self.run.borrow().generation;
                let wait = view.sleep(delay);
                let resource = Rc::downgrade(self);
                let future: TaskFuture = Box::pin(async move {
                    wait.await;
                    Some(Box::new(move |cx: &mut UpdateCx<'_>| {
                        let Some(resource) = resource.upgrade() else {
                            return;
                        };
                        let current = {
                            let mut run = resource.run.borrow_mut();
                            run.debounce = None;
                            run.generation == generation
                        };
                        if current {
                            resource.start(key, cx);
                        }
                    }) as viso_ui::Continuation)
                });
                drop(view);
                let id = cells.spawn(self.owner, future);
                self.run.borrow_mut().debounce = id;
            }
            None => {
                drop(view);
                self.start(key, cells);
            }
        }
    }

    /// Runs the loader for `key`.
    fn start(self: &Rc<Self>, key: Value, cells: &mut dyn StateCells) {
        let Some(host) = self.mount.live() else {
            return;
        };
        let Ok(mut view) = host.try_borrow_mut() else {
            return;
        };
        let scope = &self.mount.scope;
        let Some(start) = view.run_load(self.loader, scope, cells) else {
            return;
        };
        let sink = Sink::Resource(Loading {
            resource: Rc::downgrade(self),
            key,
        });
        if let Some(token) = view.run_start(start, None, sink, scope.clone(), self.owner, cells) {
            self.run.borrow_mut().loads.push(token);
        }
    }
}

impl ViewHost {
    /// Settles the resource load `token` with the value its task returned
    /// (`None` after a fault): caches it, and writes it to the state while
    /// the key is still the one it loaded.
    pub(crate) fn settle(
        &mut self,
        token: u64,
        loading: &Loading,
        value: Option<Value>,
        scope: &Scope,
        owner: NodeId,
        cells: &mut dyn StateCells,
    ) {
        let Some(resource) = loading.resource.upgrade() else {
            return;
        };
        resource.run.borrow_mut().loads.retain(|&t| t != token);
        let Some(value) = value else {
            return;
        };
        let state = match &value {
            Value::Agg(agg) if agg.tag <= 1 && agg.fields.len() == 1 => {
                let tag = if agg.tag == 0 { READY } else { ERROR };
                payload(tag, agg.fields[0].clone())
            }
            _ => {
                self.fault = Some(viso_behavior::Fault {
                    kind: FaultKind::Internal,
                    at: None,
                    message: "a resource's loader returned no `Result`".to_owned(),
                });
                return;
            }
        };
        let load = resource.load;
        let failed = matches!(&state, Value::Agg(agg) if agg.tag == ERROR);
        if let Some(keep) = load.cache_for
            && (!failed || load.cache_errors)
        {
            let wait = self.sleep(keep);
            let (stamp, stale) = {
                let mut run = resource.run.borrow_mut();
                let stamp = run.stamp;
                run.stamp += 1;
                let stale = run
                    .cache
                    .iter()
                    .position(|c| c.key == loading.key)
                    .map(|at| run.cache.swap_remove(at));
                run.cache.push(Cached {
                    key: loading.key.clone(),
                    state: state.clone(),
                    stamp,
                    expiry: None,
                });
                (stamp, stale)
            };
            if let Some(id) = stale.and_then(|c| c.expiry) {
                cells.cancel_task(id);
            }
            let weak = Rc::downgrade(&resource);
            let future: TaskFuture = Box::pin(async move {
                wait.await;
                Some(Box::new(move |_: &mut UpdateCx<'_>| {
                    if let Some(resource) = weak.upgrade() {
                        resource.run.borrow_mut().cache.retain(|c| c.stamp != stamp);
                    }
                }) as viso_ui::Continuation)
            });
            let id = cells.spawn(owner, future);
            if let Some(entry) = resource
                .run
                .borrow_mut()
                .cache
                .iter_mut()
                .find(|c| c.stamp == stamp)
            {
                entry.expiry = id;
            }
        }
        let current = resource.run.borrow().key.as_ref() == Some(&loading.key);
        if current {
            self.dispatch(load.write, state, scope, cells);
        }
    }
}

/// The `ResourceState` variant `tag` carrying `value`.
fn payload(tag: u32, value: Value) -> Value {
    Value::Agg(Rc::new(Aggregate {
        tag,
        fields: Box::new([value]),
    }))
}
