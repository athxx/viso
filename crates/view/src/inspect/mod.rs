//! What a running app shows, node by node, for a tool to inspect and select
//! nodes from: the data of [`viso_dev_protocol::inspect`], and the runtime
//! walks that fill it in — a window's [`census`], the [`static_nodes`] of a
//! mounted view, a node's [`node_detail`].

mod census;
mod detail;

pub use census::{CensusView, census, static_nodes};
pub use detail::node_detail;
pub use viso_dev_protocol::inspect::*;
