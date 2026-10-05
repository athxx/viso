//! A view's `effect`s, run by the node store's effect scheduler after the
//! commit that mounts them and after each commit that changes what they
//! depend on.
//!
//! An effect is two closures on the node it is mounted on. Its gate reads the
//! cells its `when (..)` list reads — the dependencies the scheduler wakes it
//! on — then evaluates the list and compares it with the last value: the run
//! policy decides from that whether the body runs. The body runs as one
//! transaction whose writes land like a handler's, and returns the cleanup
//! closure the scheduler runs before the next run and when the node is freed.
//!
//! A hot reload cancels the effects mounted against the prior module before it
//! swaps the module in, so each cleanup runs against the code that created it;
//! an effect it missed is inert once the swap raised the host's epoch.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use viso_behavior::{ComponentEffect, EffectRun, Value};
use viso_ui::adaptive::AdaptiveEnv;
use viso_ui::{
    BuildCx, Cleanup, ComputeCx, EffectCx, EffectStore, NodeId, NodeStore, StateId, StateValue,
    TaskFuture, TaskId,
};

use crate::host::{StateCells, ViewHost};
use crate::scope::Scope;

/// Mounts each effect of the view `host` runs on `owner`, its root node, in
/// the empty scope. They first run in the next flush. The root also owns the
/// tasks the view starts outside region content.
pub fn mount_effects(store: &mut NodeStore, host: &Rc<RefCell<ViewHost>>, owner: NodeId) {
    let effects = match host.try_borrow_mut() {
        Ok(mut view) => {
            view.set_root(owner);
            if view.effects().is_empty() {
                return;
            }
            view.own_effects(owner);
            view.effects().to_vec()
        }
        Err(_) => return,
    };
    for effect in effects {
        mount_effect(store, host, effect, Scope::EMPTY, owner);
    }
}

/// [`mount_effects`] for the view a macro expansion built, on its root `root`.
#[doc(hidden)]
pub fn __mount_effects(cx: &mut BuildCx<'_>, host: &Rc<RefCell<ViewHost>>, root: NodeId) {
    cx.structure(|cx| mount_effects(cx.store, host, root));
}

/// Cancels every effect mounted against the view `host` runs, each running
/// its cleanup: what a hot reload does before it swaps the module, and what
/// dropping the view does.
pub fn release_effects(host: &Rc<RefCell<ViewHost>>, effects: &mut EffectStore) {
    let owners = match host.try_borrow_mut() {
        Ok(mut view) => view.take_effect_owners(),
        Err(_) => return,
    };
    for owner in owners {
        effects.cancel_for_node(owner);
    }
}

/// Mounts `effect` of the view `host` runs on `owner`, run in `scope`. An
/// effect region content mounts is owned by the node it mounts on alone: the
/// hot reload of a view with regions rebuilds the view, freeing that node.
pub(crate) fn mount_effect(
    store: &mut NodeStore,
    host: &Rc<RefCell<ViewHost>>,
    effect: ComponentEffect,
    scope: Scope,
    owner: NodeId,
) {
    let Ok(epoch) = host.try_borrow().map(|view| view.epoch()) else {
        return;
    };
    let mount = Rc::new(Mount {
        host: Rc::downgrade(host),
        scope,
        epoch,
    });
    let mut gate = Gate {
        mount: Rc::clone(&mount),
        deps: effect.deps,
        run: effect.run,
        mounted: false,
        last: None,
        cells: Vec::new(),
    };
    let body = effect.body;
    store.add_effect(
        owner,
        move |cx| gate.open(cx),
        move |cx| mount.run(body, cx),
    );
}

/// What an effect's closures share: the view and the scope it runs in.
struct Mount {
    host: Weak<RefCell<ViewHost>>,
    scope: Scope,
    /// The host's epoch when it mounted.
    epoch: u64,
}

impl Mount {
    /// The host, while it still runs the module the effect was mounted from.
    fn live(&self) -> Option<Rc<RefCell<ViewHost>>> {
        self.host.upgrade().filter(|host| {
            host.try_borrow()
                .is_ok_and(|view| view.epoch() == self.epoch)
        })
    }

    /// Runs body `body`, returning its cleanup.
    fn run(self: &Rc<Self>, body: u32, cx: &mut EffectCx<'_>) -> Option<Cleanup> {
        let host = self.live()?;
        let value = host
            .try_borrow_mut()
            .ok()?
            .run_effect(body, &self.scope, &mut Writes(cx))?;
        if !matches!(value, Value::Closure(_)) {
            return None;
        }
        let mount = Rc::clone(self);
        Some(Box::new(move || {
            if let Some(host) = mount.live()
                && let Ok(mut view) = host.try_borrow_mut()
            {
                view.run_cleanup(&value, &mount.scope);
            }
        }))
    }
}

/// An effect's gate: its dependencies and when its body runs.
struct Gate {
    mount: Rc<Mount>,
    /// The region entry computing the dependency values.
    deps: Option<u32>,
    run: EffectRun,
    /// Whether the gate opened before.
    mounted: bool,
    /// The dependency values the last opening computed.
    last: Option<Value>,
    /// Scratch for the cells the dependencies read.
    cells: Vec<StateId>,
}

impl Gate {
    /// Reads the dependencies' cells and says whether the body runs now.
    fn open(&mut self, cx: &mut ComputeCx<'_>) -> bool {
        let first = !std::mem::replace(&mut self.mounted, true);
        let Some(host) = self.mount.live() else {
            return false;
        };
        let Some(deps) = self.deps else {
            return first;
        };
        let Ok(mut view) = host.try_borrow_mut() else {
            return false;
        };
        self.cells.clear();
        view.entry_cells(deps, &self.mount.scope, &mut self.cells);
        for &id in &self.cells {
            cx.get(id);
        }
        let value = match view.evaluate(deps, &self.mount.scope, None, &Reads(cx)) {
            Ok(value) => value,
            Err(fault) => {
                view.record_fault(fault);
                return false;
            }
        };
        let changed = self.last.as_ref() != Some(&value);
        self.last = Some(value);
        match self.run {
            EffectRun::Mount => first,
            EffectRun::Change => !first && changed,
            EffectRun::MountAndChange => first || changed,
        }
    }
}

/// The cells a gate evaluates against: reading records nothing, and nothing
/// writes.
struct Reads<'a, 'b>(&'a ComputeCx<'b>);

impl StateCells for Reads<'_, '_> {
    fn get(&self, id: StateId) -> Option<StateValue> {
        self.0.peek(id)
    }

    fn set(&mut self, _: StateId, _: StateValue) -> bool {
        false
    }

    fn env(&self) -> &AdaptiveEnv {
        self.0.env()
    }
}

/// The cells a body runs against: reading records nothing, and a write lands
/// in the next round of the flush.
struct Writes<'a, 'b>(&'a mut EffectCx<'b>);

impl StateCells for Writes<'_, '_> {
    fn get(&self, id: StateId) -> Option<StateValue> {
        self.0.peek(id)
    }

    fn set(&mut self, id: StateId, value: StateValue) -> bool {
        self.0.set(id, value)
    }

    fn env(&self) -> &AdaptiveEnv {
        self.0.env()
    }

    fn spawn(&mut self, owner: NodeId, future: TaskFuture) -> Option<TaskId> {
        Some(self.0.__spawn_for(owner, future))
    }

    fn cancel_task(&mut self, id: TaskId) {
        self.0.cancel_task(id);
    }
}
