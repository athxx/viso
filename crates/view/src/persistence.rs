//! The store an app's views persist their `@persist` states through.
//!
//! The app installs one. A view host that has no [`Persist`] service of its
//! own when it mounts persists through it, loading before the tree is built,
//! storing each committed change, and registering a suspend hook on its
//! window's store that makes the writes durable when the app goes to the
//! background or the window closes. A host whose component persists nothing
//! yet holds the service too, so a hot reload that adds a persisted state
//! loads it. What did not load or store is
//! queued for the app to report ([`take_persist_reports`]).

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use viso_behavior::game::{Persist, PersistReport, SharedStore};
use viso_ui::{BuildCx, NodeStore, StateStore};

use crate::ViewHost;

thread_local! {
    static STORE: RefCell<Option<SharedStore>> = const { RefCell::new(None) };
    static REPORTS: RefCell<Vec<PersistReport>> = const { RefCell::new(Vec::new()) };
}

/// Installs `store` as the one the views of this thread persist through,
/// replacing the one installed before; `None` removes it, and views mounted
/// later persist nothing unless their host has a [`Persist`] service of its
/// own. A [`LazyStore`](viso_behavior::game::LazyStore) opens only once a
/// state is persisted.
pub fn install_persistence(store: Option<SharedStore>) {
    STORE.with(|installed| *installed.borrow_mut() = store);
}

fn installed() -> Option<SharedStore> {
    STORE.with(|installed| installed.borrow().clone())
}

/// The persisted states that did not load and the writes that failed since
/// the last call, across every view of this thread.
pub fn take_persist_reports() -> Vec<PersistReport> {
    REPORTS.with(|reports| std::mem::take(&mut *reports.borrow_mut()))
}

fn queue(host: &mut ViewHost) {
    let reports = host.take_persist_reports();
    if !reports.is_empty() {
        REPORTS.with(|queued| queued.borrow_mut().extend(reports));
    }
}

/// Loads `host`'s persisted states into `states` before its tree is built,
/// through its own [`Persist`] service or else the installed store, and
/// registers, the first time, the suspend hook that makes its writes durable
/// on `store`. A host already mounted only loads what an edit newly persists.
#[doc(hidden)]
pub fn __mount_persisted(
    host: &Rc<RefCell<ViewHost>>,
    store: &mut NodeStore,
    states: &mut StateStore,
) {
    let mut view = host.borrow_mut();
    let first = !view.persist_decided();
    if first
        && view.services_mut().get_mut::<Persist>().is_none()
        && let Some(shared) = installed()
    {
        view.services_mut().insert(Persist::new(shared));
    }
    view.load_persisted(states);
    queue(&mut view);
    if !first || !view.persisting() {
        return;
    }
    let weak: Weak<RefCell<ViewHost>> = Rc::downgrade(host);
    store.__on_suspend(move |states| {
        let Some(host) = weak.upgrade() else {
            return false;
        };
        let mut host = host.borrow_mut();
        host.suspend(states);
        queue(&mut host);
        true
    });
}

/// [`__mount_persisted`] for a `view!` or `component!` host, before its tree.
#[doc(hidden)]
pub fn __load_persisted(cx: &mut BuildCx<'_>, host: &Rc<RefCell<ViewHost>>) {
    cx.structure(|cx| __mount_persisted(host, cx.store, cx.states));
}
