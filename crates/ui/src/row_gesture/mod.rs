//! Row gestures: what a list's rows answer to beside a press — a swipe that
//! slides a row's content aside to show the actions behind it ([`swipe`]).
//!
//! What one of them settles is told to the row's key handler on the next
//! frame ([`EventCx::row_gesture`](crate::context::EventCx::row_gesture)),
//! through [`KeyRouter::route_row_gestures`](crate::KeyRouter::route_row_gestures).

pub mod swipe;

pub use swipe::{SwipeEdge, SwipeParts};

use crate::component::NodeStore;
use crate::node::NodeId;

/// What a row gesture settled, told to the node it ran on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowGesture {
    /// A full swipe carried the row's content off toward an edge's far
    /// side: the first action on that edge runs.
    Swiped(SwipeEdge),
}

/// A window's row gestures and what they settled, not yet told. Cold: a
/// store with no swipe rows spends nothing on them.
#[derive(Debug, Default)]
pub(crate) struct RowGestures {
    pub(crate) swipes: swipe::Swipes,
    told: Vec<(NodeId, RowGesture)>,
}

impl RowGestures {
    pub(crate) fn clear(&mut self) {
        self.swipes.clear();
        self.told.clear();
    }

    /// Whether a row gesture moves something on its own.
    pub(crate) fn moving(&self) -> bool {
        self.swipes.moving()
    }
}

impl NodeStore {
    /// Queues `gesture` to tell node `node` on the next frame.
    pub(crate) fn tell_row_gesture(&mut self, node: NodeId, gesture: RowGesture) {
        self.row_gestures.told.push((node, gesture));
    }

    /// Whether a row gesture waits to be told.
    pub fn row_gestures_due(&self) -> bool {
        !self.row_gestures.told.is_empty()
    }

    /// Moves the row gestures waiting to be told into `out`, in the order
    /// they settled.
    pub fn take_row_gestures(&mut self, out: &mut Vec<(NodeId, RowGesture)>) {
        out.append(&mut self.row_gestures.told);
    }
}
