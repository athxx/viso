//! The file tree's structural reconcile step — turn committed expand/collapse
//! intents into a reflattened, re-driven virtual list.
//!
//! This is the store-mutating half of the handler-writes-intent /
//! reconcile-mutates-store split ([ADR 0023](../../../../docs/adr/0023-dock-live-relayout-and-tree-state.md)):
//! a command holds only an [`EventCx`](viso_ui::EventCx) and can do no more than push
//! an [`Intent`](super::command::Intent); this step, holding `&mut NodeStore` and
//! `&mut VirtualLists`, is where the arrangement actually changes. It runs in the
//! frame's Layout phase *before* the substrate's
//! [`virtual_list::reconcile`](viso_ui::virtual_list::reconcile), the same phase
//! relationship the dock's reconcile uses, so the item-count change it commits is
//! visible to the keyed diff on the same frame.
//!
//! The effect is deliberately small (architecture section 12.4): drain the intents,
//! edit the [`NodeKey`](super::model::NodeKey)-keyed open set, reflatten the tree into
//! the shared visible-row cell, and drive the list's item count with
//! [`set_item_count`](viso_ui::virtual_list::set_item_count). That single item-count
//! change (which internally marks the list data-dirty) is all the substrate needs: on
//! its next pass it diffs the new key set against the mounted rows, mounts the rows
//! that entered the window, recycles those that left, and reuses every survivor's
//! host node — the tree is never rebuilt, and a row that stayed visible keeps its host,
//! its state, and (a later section) its focus.
//!
//! Writing per-row `Role::TreeItem` semantics (expanded/selected) is deferred to the
//! accessibility section; this version reflattens and re-drives the count only, so the
//! `store` argument is threaded through but unused here.

use viso_ui::virtual_list::set_item_count;
use viso_ui::{NodeStore, VirtualLists};

use super::command::{FileTreeState, Intent};
use super::model::flatten;

/// Apply the committed expand/collapse intents, then reflatten and re-drive the list.
///
/// Drains `intents` in order, editing `state.open`: a [`Toggle`](Intent::Toggle)
/// flips the key's membership, an [`Expand`](Intent::Expand) inserts it, a
/// [`Collapse`](Intent::Collapse) removes it. An intent naming a file or an absent key
/// is harmless — a file is never in the open set and the flatten never descends it, so
/// inserting its key adds no rows. After editing the open set, reflattens the forest
/// into the shared `state.visible` cell (the build walk's keyed closures read it live)
/// and calls [`set_item_count`] so the substrate re-anchors and rewrites the canvas
/// extent on its next pass.
///
/// Skips the reflatten entirely when no intents were pending (the common per-frame
/// case), so a frame with no expand/collapse does no work here.
pub(super) fn reconcile_open(
    state: &mut FileTreeState,
    intents: &std::rc::Rc<std::cell::RefCell<Vec<Intent>>>,
    _store: &mut NodeStore,
    lists: &mut VirtualLists,
) {
    let mut queue = intents.borrow_mut();
    if queue.is_empty() {
        return;
    }
    for intent in queue.drain(..) {
        match intent {
            Intent::Toggle(key) => {
                if !state.open.remove(&key) {
                    state.open.insert(key);
                }
            }
            Intent::Expand(key) => {
                state.open.insert(key);
            }
            Intent::Collapse(key) => {
                state.open.remove(&key);
            }
        }
    }
    drop(queue);

    // Reflatten into the shared cell the keyed list reads live, then tell the list how
    // many rows it now has — the one call that drives the whole structural diff.
    let rows = flatten(&state.roots, &state.open);
    let count = rows.len();
    *state.visible.borrow_mut() = rows;
    set_item_count(lists, state.viewport, count);
}
