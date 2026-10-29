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
//! A component's `@persist` states load and store through the store the app
//! installs ([`install_persistence`]), and its `viso::clipboard` natives
//! reach the clipboard the app installs ([`install_clipboard`]).
//!
//! A host is shared as `Rc<RefCell<ViewHost>>` by the `'static` handler boxes
//! of every node of its view. The sharing is the point: the nodes of one view
//! run against one instance. Dispatch is single-threaded and never re-entrant
//! (the router takes a handler out of the store before calling it, and state
//! writes are deferred to the flush), and the borrow is taken per dispatch on
//! the cold event path, never during layout or paint.

mod attach;
mod clipboard;
mod control;
#[cfg(feature = "hot-reload")]
pub mod dev;
mod effects;
mod env;
mod game;
mod host;
mod input;
pub mod inspect;
#[doc(hidden)]
pub mod kinds;
mod layout_values;
#[cfg(feature = "inspect")]
mod mounts;
mod nodes;
mod package;
mod persistence;
mod regions;
mod resources;
mod route;
mod scope;
mod shaders;
mod tasks;
mod text_style;
mod theme;
mod values;
mod widget;
mod world_scene;

pub use attach::{Route, attach, attach_node};
pub use clipboard::{ClipboardRead, HostClipboard, install_clipboard};
pub use control::{
    Control, ControlInput, ControlKind, LayoutSlot, Look, LookArm, NodeEntries, RustEntries,
    SOURCE_ROLES, SemanticEntries, Switched, TextEntries, When, source_live, source_name,
    source_role, widget_role,
};
pub use effects::{__mount_effects, mount_effects, release_effects};
pub use env::{__link_env, env_value};
pub use game::{
    __link_game, GameAudioDevice, GameAudioStart, GameReloadError, GameSession, GameSwap,
    StartedAudio, describe_fault, game_session, install_game_audio, keep_game_history, mount_game,
    mount_view_game, take_game_faults,
};
pub use host::{__Links, __embedded, HostError, StateCells, ViewHost, cell_value, vm_value};
pub use input::{
    __Route, __mount_input, __mount_navigator, __pushed_input, MountedInput, ViewInput, input_kind,
};
#[cfg(feature = "inspect")]
pub use mounts::{__mounted_view, MountRecord, take_mounts};
pub use nodes::{__mount_nodes, mount_nodes};
pub use package::{
    LoadedView, ViewAsset, ViewControl, ViewEnv, ViewHandler, ViewLoad, ViewLoadError, ViewPackage,
    ViewPackageView, ViewShader, ViewState, load_static_view, load_view, load_view_with,
};
pub use persistence::{
    __load_persisted, __mount_persisted, install_persistence, take_persist_reports,
};
pub use regions::{
    __mount_embedded, ArmTemplate, CLOSED, CellRef, EffectTemplate, EnvTemplate, GroupTemplate,
    HALF_OPEN, ItemKey, ItemTemplate, LocalTemplate, MAX_RANGE_ITEMS, RegionKind, RegionNode,
    RegionTemplate, SlotTemplate, StarterTemplate, ViewRegions, mount_regions,
};
pub use route::{EventRoute, PAYLOAD_ENUMS, PAYLOAD_RECORDS};
pub use scope::Scope;
pub use shaders::{__embedded_shader, load_shader, load_shaders};
pub use viso_dev_protocol::{headless, profile, tape};
// The expression `view!` picks the target backend's code of a shader with.
pub use tasks::release_tasks;
pub use theme::{default_dark_theme, default_theme, palette, set_theme};
pub use values::{__mount_values, mount_values};
pub use viso_behavior::{Fault, Module, Value};
pub use viso_ui::shader::__per_backend;
pub use widget::{RustWidget, WidgetEvents, WidgetImpl, check_widgets};
pub use world_scene::{WorldScene, game_viewport};
/// The stores a view's `@persist` states persist through.
pub mod persist {
    #[cfg(not(target_family = "wasm"))]
    pub use viso_behavior::game::DirStore;
    pub use viso_behavior::game::{
        LazyStore, MemoryStore, PersistReport, PersistStore, SharedStore,
    };
}

/// Records nothing: the build has no tool to hand mounts to.
#[cfg(not(feature = "inspect"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __record_mount {
    ($($record:tt)*) => {};
}
