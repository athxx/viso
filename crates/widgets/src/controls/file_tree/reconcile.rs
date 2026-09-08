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
use super::model::{apply_select, flatten};

/// Apply the committed intents, then — only if the tree's structure changed —
/// reflatten and re-drive the list.
///
/// Drains `intents` in order. The three structural intents edit `state.open`: a
/// [`Toggle`](Intent::Toggle) flips the key's membership, an [`Expand`](Intent::Expand)
/// inserts it, a [`Collapse`](Intent::Collapse) removes it. An intent naming a file or
/// an absent key is harmless — a file is never in the open set and the flatten never
/// descends it, so inserting its key adds no rows. The two non-structural intents move
/// the warm focus/selection: a [`SetFocus`](Intent::SetFocus) moves the focus cursor,
/// and a [`Select`](Intent::Select) applies the gesture to `state.selection` (via
/// [`apply_select`], which also returns the new range anchor) and moves the focus cursor
/// to the row. These edit only warm sets the accessibility section reads — they change
/// no visible row — so they do **not** reflatten.
///
/// After the drain, reflattens the forest into the shared `state.visible` cell (the
/// build walk's keyed closures read it live) and calls [`set_item_count`] so the
/// substrate re-anchors and rewrites the canvas extent on its next pass — but **only**
/// when a structural intent was seen. A frame carrying only focus/selection edits skips
/// the reflatten, so the keyboard's cheapest gesture (moving the cursor) stays cheap and
/// does not disturb the mounted rows.
///
/// Skips the whole drain when no intents were pending (the common per-frame case), so a
/// frame with no interaction does no work here.
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
    // Whether any intent changed the open set — the only thing that changes the visible
    // rows. A focus/select-only frame leaves this false and skips the reflatten below.
    let mut structural = false;
    for intent in queue.drain(..) {
        match intent {
            Intent::Toggle(key) => {
                if !state.open.remove(&key) {
                    state.open.insert(key);
                }
                structural = true;
            }
            Intent::Expand(key) => {
                state.open.insert(key);
                structural = true;
            }
            Intent::Collapse(key) => {
                state.open.remove(&key);
                structural = true;
            }
            Intent::SetFocus(key) => {
                state.focus = Some(key);
            }
            Intent::Select(key, op) => {
                // Apply the gesture against the current visible rows (a range needs the
                // contiguous run), record the anchor it leaves, and move focus to the row.
                let visible = state.visible.borrow();
                let anchor = apply_select(&mut state.selection, &visible, state.anchor, key, op);
                drop(visible);
                state.anchor = anchor;
                state.focus = Some(key);
            }
        }
    }
    drop(queue);

    if !structural {
        // Focus/selection moved but no row entered or left — the mounted list is still
        // correct, so leave it untouched.
        return;
    }

    // Reflatten into the shared cell the keyed list reads live, then tell the list how
    // many rows it now has — the one call that drives the whole structural diff.
    let rows = flatten(&state.roots, &state.open);
    let count = rows.len();
    *state.visible.borrow_mut() = rows;
    set_item_count(lists, state.viewport, count);
}
