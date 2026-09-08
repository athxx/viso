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
//! After the arrangement settles, the step refreshes each visible row's live
//! `Role::TreeItem` accessibility state — `aria-expanded` for a directory and
//! `aria-selected` for the current selection — by writing the node's
//! [`SemanticState`](viso_ui::SemanticState) side column directly through
//! [`set_semantic_state`](viso_ui::NodeStore::set_semantic_state). These facts are
//! warm model data (fields of the flattened rows and the selection set), not a
//! reactive scalar cell, so the write is direct rather than routed through the
//! flush-phase projector the scalar-cell controls use. The refresh runs whenever an
//! expansion or a selection changed; a bare focus-cursor move touches neither and
//! so writes nothing.

use viso_ui::virtual_list::set_item_count;
use viso_ui::{NodeStore, VirtualLists};

use super::command::{FileTreeState, Intent};
use super::model::{apply_select, flatten};
use super::semantics::row_state;

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
/// Then, whenever an expansion *or* a selection changed, refreshes every visible row's
/// [`Role::TreeItem`](viso_ui::Role::TreeItem) live state — `expanded` for a directory
/// and `selected` for the selection — through [`refresh_row_semantics`]. A bare
/// focus-cursor move changes neither, so it writes no semantics.
///
/// Skips the whole drain when no intents were pending (the common per-frame case), so a
/// frame with no interaction does no work here.
pub(super) fn reconcile_open(
    state: &mut FileTreeState,
    intents: &std::rc::Rc<std::cell::RefCell<Vec<Intent>>>,
    store: &mut NodeStore,
    lists: &mut VirtualLists,
) {
    let mut queue = intents.borrow_mut();
    if queue.is_empty() {
        return;
    }
    // Whether any intent changed the open set — the only thing that changes the visible
    // rows. A focus/select-only frame leaves this false and skips the reflatten below.
    let mut structural = false;
    // Whether any intent changed the selection — an aria-selected change that must reach
    // the rows even on a frame that moved no row.
    let mut selection_changed = false;
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
                selection_changed = true;
            }
        }
    }
    drop(queue);

    if structural {
        // Reflatten into the shared cell the keyed list reads live, then tell the list
        // how many rows it now has — the one call that drives the whole structural diff.
        let rows = flatten(&state.roots, &state.open);
        let count = rows.len();
        *state.visible.borrow_mut() = rows;
        set_item_count(lists, state.viewport, count);
    }

    // Refresh per-row accessibility state whenever an expansion or a selection changed. A
    // structural change can flip a directory's expanded glyph and shift which rows are
    // visible; a selection change flips aria-selected without moving a row. A focus-only
    // frame changes neither and writes nothing.
    if structural || selection_changed {
        refresh_row_semantics(state, store);
    }
}

/// Rewrite each currently-visible row's live [`SemanticState`](viso_ui::SemanticState) —
/// `expanded` (a directory) and `selected` (in the selection) — onto its mounted host
/// node, addressed by key through `state.row_nodes`. Only rows the build walk has
/// mounted appear in that map, so a row scrolled out of the window (whose host was
/// recycled) is skipped; it re-authors its state from `row_state` when it next mounts.
/// A direct column write ([`set_semantic_state`]), not a projector: the expanded/selected
/// facts are warm model data, not a reactive scalar cell.
fn refresh_row_semantics(state: &FileTreeState, store: &mut NodeStore) {
    let visible = state.visible.borrow();
    let row_nodes = state.row_nodes.borrow();
    for row in visible.iter() {
        if let Some(&node) = row_nodes.get(&row.key) {
            let selected = state.selection.contains(&row.key);
            store.set_semantic_state(node, row_state(*row, selected));
        }
    }
}
