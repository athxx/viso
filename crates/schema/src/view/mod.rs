//! The view package: what a compiled view with behavior carries from the
//! compiler to the runtime that mounts it.

pub mod control;
pub mod package;
pub mod regions;
pub mod route;

pub use control::{
    Control, ControlInput, ControlKind, LayoutSlot, Look, LookArm, NodeEntries, SOURCE_ROLES,
    SemanticEntries, TextEntries, When, source_live, source_name, source_role, widget_role,
};
pub use package::{
    ViewAsset, ViewControl, ViewEnv, ViewHandler, ViewNamed, ViewPackage, ViewPackageView,
    ViewShader, ViewState,
};
pub use regions::{
    ArmTemplate, CLOSED, CellRef, EffectTemplate, EnvTemplate, GroupTemplate, HALF_OPEN,
    ItemTemplate, LocalTemplate, RegionKind, RegionTemplate, SlotTemplate, StarterTemplate,
    ViewRegions,
};
pub use route::{EventRoute, PAYLOAD_ENUMS, PAYLOAD_RECORDS, Route};
