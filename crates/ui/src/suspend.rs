//! What a tree does when its app goes to the background or its window
//! closes: each hook stores what must survive, reading the cells of the
//! window's [`StateStore`], and blocks until it is durable. A view with
//! persisted states registers one; a hook that answers `false` (its view is
//! gone) is dropped.

use crate::component::NodeStore;
use crate::state::StateStore;

/// One registered hook.
type Hook = Box<dyn FnMut(&StateStore) -> bool>;

/// The suspend hooks of a store. Cold: empty in a tree that persists
/// nothing, and run only at a lifecycle change.
#[derive(Default)]
pub(crate) struct SuspendHooks {
    hooks: Vec<Hook>,
}

impl SuspendHooks {
    pub(crate) fn clear(&mut self) {
        self.hooks.clear();
    }
}

impl NodeStore {
    /// Registers `hook` to run at every [`suspend`](Self::suspend) for as
    /// long as it answers `true`.
    #[doc(hidden)]
    pub fn __on_suspend(&mut self, hook: impl FnMut(&StateStore) -> bool + 'static) {
        self.suspend_hooks_mut().hooks.push(Box::new(hook));
    }

    /// Runs every suspend hook against `states`, the window's cells, dropping
    /// those whose owner is gone: the app goes to the background, or the
    /// window is about to close.
    pub fn suspend(&mut self, states: &StateStore) {
        self.suspend_hooks_mut()
            .hooks
            .retain_mut(|hook| hook(states));
    }

    /// The number of suspend hooks registered.
    pub fn suspend_hook_count(&self) -> usize {
        self.suspend_hooks().hooks.len()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    #[test]
    fn a_hook_runs_at_each_suspend_until_it_declines() {
        let mut store = NodeStore::new();
        let states = StateStore::new();
        let runs = Rc::new(Cell::new(0));
        let counted = Rc::clone(&runs);
        store.__on_suspend(move |_| {
            counted.set(counted.get() + 1);
            counted.get() < 2
        });
        assert_eq!(store.suspend_hook_count(), 1);
        store.suspend(&states);
        store.suspend(&states);
        store.suspend(&states);
        assert_eq!(runs.get(), 2, "dropped once it answered false");
        assert_eq!(store.suspend_hook_count(), 0);
    }
}
