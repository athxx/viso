//! The runtime of a compiled view's behavior.
//!
//! A `.vs` component's handlers compile to chunks of a behavior module. This
//! crate mounts one component instance per view on the behavior VM
//! ([`ViewHost`]), mirrors its states into the UI state store so bindings see
//! every write, and installs node event handlers that turn an input sample into
//! the DSL event payload and dispatch it ([`attach`]). The macros, the hot
//! reload commit and the release package ([`ViewPackage`]) all install handlers
//! through it, so the three targets run a handler the same way.
//!
//! A host is shared as `Rc<RefCell<ViewHost>>` by the `'static` handler boxes
//! of every node of its view. The sharing is the point: the nodes of one view
//! run against one instance. Dispatch is single-threaded and never re-entrant
//! (the router takes a handler out of the store before calling it, and state
//! writes are deferred to the flush), and the borrow is taken per dispatch on
//! the cold event path, never during layout or paint.

mod attach;
mod host;
mod package;
mod regions;
mod route;

pub use attach::{Route, attach, attach_node};
pub use host::{__embedded, HostError, StateCells, ViewHost};
pub use package::{
    LoadedView, ViewHandler, ViewLoadError, ViewPackage, ViewState, instantiate_view, load_view,
};
pub use regions::{
    __mount_embedded, ArmTemplate, CLOSED, GroupTemplate, HALF_OPEN, ItemTemplate, MAX_RANGE_ITEMS,
    RegionKind, RegionTemplate, SlotTemplate, ViewRegions, mount_regions,
};
pub use route::{EventRoute, PAYLOAD_ENUMS, PAYLOAD_RECORDS};
pub use viso_behavior::{Fault, Module, Value};
