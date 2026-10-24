//! What a control's parts write to the tree as they change, the same from
//! an event handler — which the flush applies — and from a structure pass,
//! which writes at once: so one control's logic serves both the input it
//! answers and the values a view delivers to it.

use crate::component::{ScrollBehavior, ScrollRequest};
use crate::content::TextRequest;
use crate::context::EventCx;
use crate::node::NodeId;
use crate::semantics::Semantics;
use crate::state::{StateId, StateValue};
use crate::structure::StructureCx;
use crate::text_edit::{EditIntent, FieldSeed};

/// The writes a control's parts make to the tree and its cells.
pub trait PartWrites {
    /// The current value of cell `id`.
    fn get(&self, id: StateId) -> Option<StateValue>;
    /// Writes cell `id`.
    fn set(&mut self, id: StateId, value: StateValue);
    /// Hides or shows node `id` and its subtree.
    fn set_hidden(&mut self, id: NodeId, hidden: bool);
    /// Shows `request` on text node `id`.
    fn set_text(&mut self, id: NodeId, request: TextRequest);
    /// Gives node `id` `semantics`.
    fn set_semantics(&mut self, id: NodeId, semantics: Semantics);
    /// Shows `active`, a descendant of `id`, as the one active while `id`
    /// keeps the focus.
    fn set_active_descendant(&mut self, id: NodeId, active: Option<NodeId>);
    /// Scrolls the viewports around `id` the least that shows it.
    fn scroll_into_view(&mut self, id: NodeId);
    /// The node with the focus.
    fn focused(&self) -> Option<NodeId>;
    /// Puts `seed`'s text in text field `field`: as one edit from a
    /// handler, which undo takes back; as a new seed from a structure pass.
    fn replace_text(&mut self, field: NodeId, seed: FieldSeed);
}

impl PartWrites for EventCx<'_> {
    fn get(&self, id: StateId) -> Option<StateValue> {
        EventCx::get(self, id)
    }

    fn set(&mut self, id: StateId, value: StateValue) {
        EventCx::set(self, id, value);
    }

    fn set_hidden(&mut self, id: NodeId, hidden: bool) {
        EventCx::set_hidden(self, id, hidden);
    }

    fn set_text(&mut self, id: NodeId, request: TextRequest) {
        EventCx::set_text(self, id, request);
    }

    fn set_semantics(&mut self, id: NodeId, semantics: Semantics) {
        EventCx::set_semantics(self, id, semantics);
    }

    fn set_active_descendant(&mut self, id: NodeId, active: Option<NodeId>) {
        EventCx::set_active_descendant(self, id, active);
    }

    fn scroll_into_view(&mut self, id: NodeId) {
        EventCx::scroll_into_view(self, id);
    }

    fn focused(&self) -> Option<NodeId> {
        EventCx::focused(self)
    }

    fn replace_text(&mut self, field: NodeId, seed: FieldSeed) {
        self.edit(field, EditIntent::SelectAll);
        self.edit(field, EditIntent::Paste(seed.request.text));
    }
}

impl PartWrites for StructureCx<'_> {
    fn get(&self, id: StateId) -> Option<StateValue> {
        self.states.get(id)
    }

    fn set(&mut self, id: StateId, value: StateValue) {
        self.states.set(id, value);
    }

    fn set_hidden(&mut self, id: NodeId, hidden: bool) {
        self.store.set_hidden(id, hidden);
    }

    fn set_text(&mut self, id: NodeId, request: TextRequest) {
        self.store.set_text(id, request);
    }

    fn set_semantics(&mut self, id: NodeId, semantics: Semantics) {
        self.store.set_semantics(id, semantics);
    }

    fn set_active_descendant(&mut self, id: NodeId, active: Option<NodeId>) {
        self.store.set_active_descendant(id, active);
    }

    fn scroll_into_view(&mut self, id: NodeId) {
        self.store
            .request_scroll_with(id, ScrollRequest::Reveal, ScrollBehavior::Instant);
    }

    fn focused(&self) -> Option<NodeId> {
        self.store.focused()
    }

    fn replace_text(&mut self, field: NodeId, seed: FieldSeed) {
        self.store.seed_text(field, seed);
    }
}
