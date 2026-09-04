//! `viso-widgets` — the official UI kit (section 9, Part XV).
//!
//! Primitive controls, layout containers, navigation, overlays, adaptive UI,
//! desktop shell, and theme defaults. Each widget is built natively on the viso
//! Component/Node/State/Layout/Input/Paint/Semantics model: a widget is a
//! [`viso_ui::Component`] that declares its subtree into a `BuildCx`, mapping to
//! retained nodes — never an `Rc<RefCell<dyn Widget>>` object.
//!
//! Tier 1 (this phase): layout [`containers`] first (View), then the primitive
//! label/image/icon controls.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod containers;
pub mod text;

pub use containers::{View, ViewStyle, view};
pub use text::{Label, LabelStyle, label};
