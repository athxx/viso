//! The item template of a `VirtualList`: the keyed `for` it virtualizes.
//!
//! The region evaluates its items and their keys as a `for` does, but mounts
//! none of them itself. It makes its parent, the list's scroll viewport, a
//! virtual list of that many rows keyed by each key's hash, and the list
//! mounts the rows of the window it shows: it builds each from the template on
//! a host of its own, through the view's regions, and recycles the host when
//! the row leaves the window. The region keeps what each mounted row is — its
//! host, key, item and fragment — so a change of the items re-delivers the
//! rows that stay, and a row the list builds on a recycled host drops what the
//! host showed before: its nodes went with the host's body, its instance
//! states go here.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use viso_behavior::{FaultKind, Value};
use viso_schema::LengthTerms;
use viso_ui::virtual_list::{
    ItemBuilder, ItemKey as RowKey, KeyOf, ListShape, StickyOf, attach_list, set_item_count,
    set_sticky_of,
};
use viso_ui::{NodeId, StructureCx};

use super::{Frag, Key, Mounted, Patch};
use crate::scope::Scope;

/// The entries and shape of a `VirtualList`'s item template.
#[derive(Clone, Copy)]
pub(super) struct ListEntries {
    pub(super) items: u32,
    pub(super) key: u32,
    pub(super) estimate: LengthTerms,
    pub(super) overscan: u32,
    pub(super) sticky: Option<u32>,
    pub(super) pages: bool,
}

/// A mounted `VirtualList` item template.
pub(super) struct Rows {
    /// The viewport its list is attached to, once it is.
    viewport: Option<NodeId>,
    /// Shared with the list's row builder.
    set: Rc<RefCell<RowSet>>,
    /// Each item's key hash, by index, shared with the list's key map.
    hashes: Rc<RefCell<Vec<u64>>>,
    /// Whether each item's row heads a section, by index, shared with the
    /// list.
    heads: Rc<RefCell<Vec<bool>>>,
}

/// What the list builds rows from and the rows it has mounted.
pub(super) struct RowSet {
    region: u32,
    /// The bindings the region runs with.
    scope: Scope,
    values: Vec<Value>,
    keys: Vec<Key>,
    pub(super) rows: Vec<Row>,
}

/// One mounted row.
pub(super) struct Row {
    host: NodeId,
    pub(super) key: Key,
    value: Value,
    pub(super) frag: Frag,
}

impl Rows {
    pub(super) fn new(region: u32) -> Rows {
        Rows {
            viewport: None,
            set: Rc::new(RefCell::new(RowSet {
                region,
                scope: Scope::default(),
                values: Vec::new(),
                keys: Vec::new(),
                rows: Vec::new(),
            })),
            hashes: Rc::new(RefCell::new(Vec::new())),
            heads: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub(super) fn shared(&self) -> &RefCell<RowSet> {
        &self.set
    }
}

/// The hash a row's key goes by in its list, `None` for a value without a
/// stable identity.
pub(crate) fn key_hash(value: &Value) -> Option<u64> {
    Key::of(value).map(|key| hash(&key))
}

fn hash(key: &Key) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

impl Patch<'_, '_> {
    /// Brings the item template under the viewport `parent` up to date: when
    /// its cells changed, its items and keys, the list's count and the rows
    /// that stay; then the mounted rows under `scope`.
    pub(super) fn rows(
        &mut self,
        rows: &mut Rows,
        list: ListEntries,
        scope: &Scope,
        parent: NodeId,
        touched: bool,
        force: bool,
    ) {
        let mut moved = false;
        if touched
            && let Some(values) = self.iterate(list.items, scope)
            && let Some(keys) = self.keys(Some(list.key), &values, scope)
        {
            let count = values.len();
            *rows.hashes.borrow_mut() = keys.iter().map(hash).collect();
            if let Some(sticky) = list.sticky {
                let heads: Vec<bool> = values
                    .iter()
                    .map(|value| {
                        self.evaluate(sticky, scope, Some(value))
                            .and_then(|head| head.as_int())
                            .is_some_and(|head| head != 0)
                    })
                    .collect();
                *rows.heads.borrow_mut() = heads;
            }
            {
                let mut set = rows.set.borrow_mut();
                set.values = values;
                set.keys = keys;
            }
            if rows.viewport == Some(parent) && self.cx.lists.get(parent).is_some() {
                set_item_count(self.cx.lists, parent, count);
                if let Some(state) = self.cx.lists.get_mut(parent) {
                    state.mark_data_dirty();
                }
            } else {
                self.attach(rows, list, parent, count);
            }
            moved = true;
        }
        let mut set = rows.set.borrow_mut();
        if !same_scope(&set.scope, scope) {
            set.scope = scope.clone();
        }
        let RowSet {
            values,
            keys,
            rows: mounted,
            ..
        } = &mut *set;
        let at: HashMap<&Key, usize> = if moved && !mounted.is_empty() {
            keys.iter().enumerate().map(|(at, key)| (key, at)).collect()
        } else {
            HashMap::new()
        };
        for row in mounted.iter_mut() {
            if moved {
                // A row whose key is gone leaves the window at the list's
                // next reconcile; until then it shows what it showed.
                let Some(&index) = at.get(&row.key) else {
                    continue;
                };
                row.value = values[index].clone();
            }
            self.frag(
                &mut row.frag,
                scope,
                Some(&row.value),
                row.host,
                None,
                force,
            );
        }
    }

    /// Makes `parent` the list of `count` rows the template mounts, in place
    /// of any list it was.
    fn attach(&mut self, rows: &mut Rows, list: ListEntries, parent: NodeId, count: usize) {
        let Some(axis) = self.cx.store.scroll_axis(parent) else {
            self.fail::<()>(
                FaultKind::Internal,
                "a `VirtualList`'s items mount under a node that does not scroll",
            );
            return;
        };
        if let Some(prior) = self.cx.lists.get(parent) {
            let canvas = prior.canvas();
            self.cx
                .lists
                .remove(parent, self.cx.store, &mut *self.cx.effects);
            self.cx
                .store
                .free_tree(canvas, &mut *self.cx.effects, &mut *self.scratch);
            self.freed = true;
        }
        let shape = ListShape {
            axis,
            estimated_row: self.cx.store.fold_extent(parent, list.estimate).max(1.0),
            overscan: list.overscan as usize,
            pages: list.pages,
        };
        let me = self.me.clone();
        let set = Rc::clone(&rows.set);
        let builder: ItemBuilder = Box::new(move |logical, cx| {
            let Some(host) = cx.parent() else {
                return;
            };
            let Some(mounted) = me.upgrade() else {
                return;
            };
            let Ok(mut mounted) = mounted.try_borrow_mut() else {
                return;
            };
            cx.structure(|cx| mounted.build_row(cx, &set, logical, host));
        });
        let hashes = Rc::clone(&rows.hashes);
        let key_of: KeyOf =
            Box::new(move |at| RowKey(hashes.borrow().get(at).copied().unwrap_or(at as u64)));
        attach_list(
            self.cx.store,
            self.cx.lists,
            parent,
            shape,
            count,
            builder,
            Some(key_of),
        );
        if list.sticky.is_some() {
            let heads = Rc::clone(&rows.heads);
            let sticky_of: StickyOf =
                Box::new(move |at| heads.borrow().get(at).copied().unwrap_or(false));
            set_sticky_of(self.cx.lists, parent, Some(sticky_of));
        }
        rows.viewport = Some(parent);
    }

    /// Frees the instance states of every row `rows` mounted and the list's
    /// parked hosts; the viewport's own subtree goes with its node.
    pub(super) fn drop_rows(&mut self, rows: Rows) {
        let mounted = std::mem::take(&mut rows.set.borrow_mut().rows);
        for row in mounted {
            self.release(row.frag);
        }
        if let Some(viewport) = rows.viewport {
            self.cx
                .lists
                .remove(viewport, self.cx.store, &mut *self.cx.effects);
        }
    }
}

impl Mounted {
    /// Builds row `logical` of the item template `set` describes on the list
    /// host `host`, dropping the row the host showed before.
    fn build_row(
        &mut self,
        cx: &mut StructureCx<'_>,
        set: &Rc<RefCell<RowSet>>,
        logical: usize,
        host: NodeId,
    ) {
        let Mounted {
            me,
            regions,
            templates,
            cells,
            env,
            pulse,
            host: view,
            scratch,
            names,
            ..
        } = self;
        let mut patch = Patch {
            cx,
            me,
            regions,
            templates,
            cells,
            env,
            pulse: *pulse,
            host: view,
            changed: &[],
            scratch,
            names,
            freed: false,
        };
        let mut set = set.borrow_mut();
        if let Some(at) = set.rows.iter().position(|row| row.host == host) {
            let row = set.rows.swap_remove(at);
            patch.release(row.frag);
        }
        let (Some(value), Some(key)) = (
            set.values.get(logical).cloned(),
            set.keys.get(logical).cloned(),
        ) else {
            return;
        };
        let scope = set.scope.clone();
        let mut frag = patch.build(set.region, 0, &scope, Some(&value), host);
        patch.frag(&mut frag, &scope, Some(&value), host, None, false);
        set.rows.push(Row {
            host,
            key,
            value,
            frag,
        });
        if patch.freed {
            patch.cx.bindings.collect(patch.cx.store, patch.cx.states);
        }
    }
}

/// Whether `a` and `b` bind the same values and keep the same states.
fn same_scope(a: &Scope, b: &Scope) -> bool {
    a.values == b.values
        && a.locals.len() == b.locals.len()
        && a.locals
            .iter()
            .zip(&b.locals)
            .all(|(x, y)| Rc::ptr_eq(x, y))
        && a.named.len() == b.named.len()
        && a.named.iter().zip(&b.named).all(|(x, y)| Rc::ptr_eq(x, y))
}
