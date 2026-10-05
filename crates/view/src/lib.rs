//! The runtime of a compiled view's behavior.
//!
//! A `.vs` component's handlers compile to chunks of a behavior module. This
//! crate mounts one component instance per view on the behavior VM
//! ([`ViewHost`]), mirrors its states into the UI state store so bindings see
//! every write, and installs node event handlers that turn an input sample into
//! the DSL event payload and dispatch it ([`attach`]). The macros, the hot
//! reload commit and the release package ([`ViewPackage`]) all install handlers
//! through it, so the three targets run a handler the same way. The values its
//! nodes show — a label's text, a text field's seeded buffer — are delivered
//! the same way ([`mount_values`]). A `start` runs its task on the UI task
//! protocol, owned by the component instance's node ([`release_tasks`]); a
//! `resource` loads through the same tasks, gated on its key like an effect.
//!
//! A host is shared as `Rc<RefCell<ViewHost>>` by the `'static` handler boxes
//! of every node of its view. The sharing is the point: the nodes of one view
//! run against one instance. Dispatch is single-threaded and never re-entrant
//! (the router takes a handler out of the store before calling it, and state
//! writes are deferred to the flush), and the borrow is taken per dispatch on
//! the cold event path, never during layout or paint.

mod attach;
mod control;
mod effects;
mod env;
mod host;
#[cfg(feature = "hot-reload")]
mod mounts;
mod package;
mod regions;
mod resources;
mod route;
mod scope;
mod tasks;
mod theme;
mod values;

pub use attach::{Route, attach, attach_node};
pub use control::{Control, ControlInput, ControlKind, Look, LookArm, When};
pub use effects::{__mount_effects, mount_effects, release_effects};
pub use env::{__link_env, env_value};
pub use host::{__embedded, HostError, StateCells, ViewHost, cell_value, vm_value};
#[cfg(feature = "hot-reload")]
pub use mounts::{__mounted_view, __static_nodes, MountRecord, take_mounts};
pub use package::{
    LoadedView, ViewControl, ViewEnv, ViewHandler, ViewLoadError, ViewPackage, ViewState,
    instantiate_view, instantiate_view_with, load_view, load_view_with,
};
pub use regions::{
    __mount_embedded, ArmTemplate, CLOSED, CellRef, EffectTemplate, EnvTemplate, GroupTemplate,
    HALF_OPEN, ItemKey, ItemTemplate, LocalTemplate, MAX_RANGE_ITEMS, RegionKind, RegionNode,
    RegionTemplate, SlotTemplate, StarterTemplate, ViewRegions, mount_regions,
};
pub use route::{EventRoute, PAYLOAD_ENUMS, PAYLOAD_RECORDS};
pub use scope::Scope;
pub use tasks::release_tasks;
pub use theme::{default_theme, set_theme};
pub use values::{__mount_values, mount_values};
pub use viso_behavior::{Fault, Module, Value};

/// Records nothing: the build has no development session to hand mounts to.
#[cfg(not(feature = "hot-reload"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __record_mount {
    ($($record:tt)*) => {};
}
