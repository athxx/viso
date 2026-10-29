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
//! A resource keeps its loads and cache in a [`Store`]: its own, under
//! `ResourceScope::component`, or under `ResourceScope::shared` its
//! declaration's pool in the view, which every instance of it shares. A key
//! already loading is joined rather than loaded again, and its result settles
//! every resource waiting on it; `keep_latest` cancels a load only once no
//! resource waits on it.
//!
//! A resource's own loads, debounce and cache expiries are UI tasks of the
//! node it mounts on, so freeing it drops them all; a pool's loads and
//! expiries are the view root's, so they outlive the instance that started
//! them. A hot reload cancels the loads with the code that started them and
//! remounts the resource, which keeps the state it carried and loads again:
//! the new loader may load differently, so no cache survives it.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use viso_behavior::{Aggregate, FaultKind, ResourceLoad, Value};
use viso_ui::context::UpdateCx;
use viso_ui::{NodeId, TaskFuture, TaskId};

use crate::effects::Mount;
use crate::host::{StateCells, ViewHost};
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
    /// The node it is mounted on, which owns its debounce, and its own loads
    /// and expiries.
    owner: NodeId,
    store: Rc<RefCell<Store>>,
    run: RefCell<Run>,
}

/// Where a resource's key is.
#[derive(Default)]
struct Run {
    /// The key the gate last evaluated.
    key: Option<Value>,
    /// Raised by each key change, so a debounce from before it starts
    /// nothing.
    generation: u64,
    /// The debounce waiting to start a load.
    debounce: Option<TaskId>,
}

/// The loads in flight and the cached states of one resource, or of every
/// instance of a shared one.
#[derive(Default)]
pub(crate) struct Store {
    flights: Vec<Flight>,
    cache: Vec<Cached>,
    /// The next cache entry's stamp.
    stamp: u64,
}

/// A load in flight and the resources waiting on its key.
struct Flight {
    token: u64,
    key: Value,
    waiters: Vec<Weak<Resource>>,
}

/// A cached state and the task expiring it.
struct Cached {
    key: Value,
    state: Value,
    stamp: u64,
    expiry: Option<TaskId>,
}

/// A load in flight: the store it settles in and the key it loads.
pub(crate) struct Loading {
    store: Weak<RefCell<Store>>,
    load: ResourceLoad,
    key: Value,
}

impl Store {
    /// `resource` stops waiting on every load of a key other than `key`; the
    /// tokens of those it leaves no one waiting on, which `cancel` drops.
    fn leave(&mut self, resource: &Resource, key: &Value, cancel: bool) -> Vec<u64> {
        let mut orphans = Vec::new();
        self.flights.retain_mut(|flight| {
            if flight.key == *key {
                return true;
            }
            flight
                .waiters
                .retain(|w| w.strong_count() > 0 && !std::ptr::eq(w.as_ptr(), resource));
            let orphan = cancel && flight.waiters.is_empty();
            if orphan {
                orphans.push(flight.token);
            }
            !orphan
        });
        orphans
    }
}

impl Resource {
    pub(crate) fn new(
        mount: Rc<Mount>,
        loader: u32,
        load: ResourceLoad,
        owner: NodeId,
        store: Rc<RefCell<Store>>,
    ) -> Rc<Self> {
        Rc::new(Resource {
            mount,
            loader,
            load,
            owner,
            store,
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
        let (key, debounce) = {
            let mut run = self.run.borrow_mut();
            let Some(key) = run.key.clone() else {
                return;
            };
            run.generation += 1;
            (key, run.debounce.take())
        };
        let (orphans, hit) = {
            let mut store = self.store.borrow_mut();
            let orphans = store.leave(self, &key, self.load.keep_latest);
            let hit = store
                .cache
                .iter()
                .find(|c| c.key == key)
                .map(|c| c.state.clone());
            (orphans, hit)
        };
        if let Some(id) = debounce {
            cells.cancel_task(id);
        }
        for token in orphans {
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

    /// Waits on the load of `key` in flight, or runs the loader for it.
    fn start(self: &Rc<Self>, key: Value, cells: &mut dyn StateCells) {
        if let Some(flight) = self
            .store
            .borrow_mut()
            .flights
            .iter_mut()
            .find(|f| f.key == key)
        {
            if !flight
                .waiters
                .iter()
                .any(|w| std::ptr::eq(w.as_ptr(), &**self))
            {
                flight.waiters.push(Rc::downgrade(self));
            }
            return;
        }
        let Some(host) = self.mount.live() else {
            return;
        };
        let Ok(mut view) = host.try_borrow_mut() else {
            return;
        };
        let scope = &self.mount.scope;
        let owner = match self.load.shared {
            Some(_) => view.root.unwrap_or(self.owner),
            None => self.owner,
        };
        let Some(start) = view.run_load(self.loader, scope, cells) else {
            return;
        };
        let sink = Sink::Resource(Loading {
            store: Rc::downgrade(&self.store),
            load: self.load,
            key: key.clone(),
        });
        if let Some(token) = view.run_start(start, None, sink, scope.clone(), owner, cells) {
            self.store.borrow_mut().flights.push(Flight {
                token,
                key,
                waiters: vec![Rc::downgrade(self)],
            });
        }
    }
}

impl ViewHost {
    /// The store every instance of the shared resource of pool `pool` keeps
    /// its loads and cache in.
    pub(crate) fn pool(&mut self, pool: u32) -> Rc<RefCell<Store>> {
        let at = pool as usize;
        if self.pools.len() <= at {
            self.pools.resize_with(at + 1, Rc::default);
        }
        Rc::clone(&self.pools[at])
    }

    /// Settles the resource load `token`, owned by `owner`, with the value
    /// its task returned (`None` after a fault): caches it, and writes it to
    /// the state of each resource waiting on it whose key is still the one
    /// it loaded.
    pub(crate) fn settle(
        &mut self,
        token: u64,
        loading: &Loading,
        value: Option<Value>,
        owner: NodeId,
        cells: &mut dyn StateCells,
    ) {
        let Some(store) = loading.store.upgrade() else {
            return;
        };
        let waiters = {
            let mut store = store.borrow_mut();
            match store.flights.iter().position(|f| f.token == token) {
                Some(at) => store.flights.swap_remove(at).waiters,
                None => Vec::new(),
            }
        };
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
        let load = loading.load;
        let failed = matches!(&state, Value::Agg(agg) if agg.tag == ERROR);
        if let Some(keep) = load.cache_for
            && (!failed || load.cache_errors)
        {
            let wait = self.sleep(keep);
            let (stamp, stale) = {
                let mut store = store.borrow_mut();
                let stamp = store.stamp;
                store.stamp += 1;
                let stale = store
                    .cache
                    .iter()
                    .position(|c| c.key == loading.key)
                    .map(|at| store.cache.swap_remove(at));
                store.cache.push(Cached {
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
            let weak = Rc::downgrade(&store);
            let future: TaskFuture = Box::pin(async move {
                wait.await;
                Some(Box::new(move |_: &mut UpdateCx<'_>| {
                    if let Some(store) = weak.upgrade() {
                        store.borrow_mut().cache.retain(|c| c.stamp != stamp);
                    }
                }) as viso_ui::Continuation)
            });
            let id = cells.spawn(owner, future);
            if let Some(entry) = store
                .borrow_mut()
                .cache
                .iter_mut()
                .find(|c| c.stamp == stamp)
            {
                entry.expiry = id;
            }
        }
        for resource in waiters.iter().filter_map(Weak::upgrade) {
            let current = resource.mount.epoch() == self.epoch
                && resource.run.borrow().key.as_ref() == Some(&loading.key);
            if current {
                self.dispatch(
                    resource.load.write,
                    state.clone(),
                    &resource.mount.scope,
                    cells,
                );
            }
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
