//! `viso-widgets` — the official UI kit (section 9, Part XV).
//!
//! Primitive controls, layout containers, navigation, overlays, adaptive UI,
//! desktop shell, and theme defaults. Each widget is built natively on the viso
//! Component/Node/State/Layout/Input/Paint/Semantics model: a widget is a
//! [`viso_ui::Component`] that declares its subtree into a `BuildCx`, mapping to
//! retained nodes — never an `Rc<RefCell<dyn Widget>>` object.
//!
//! Tier 1: the primitive presentational controls — layout [`containers`] (View),
//! [`text`] (Label), [`image`] (Image), and [`icon`] (Icon). Tier 2 begins the
//! interactive controls under [`controls`] (Button, CheckBox, Toggle,
//! RadioGroup, Slider).

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod containers;
pub mod controls;
pub mod icon;
pub mod image;
pub mod text;

pub use containers::{View, ViewStyle, view};
pub use controls::{
    Button, ButtonStyle, CheckBox, CheckBoxStyle, RadioGroup, RadioStyle, Slider, SliderStyle,
    Toggle, ToggleStyle, button, checkbox, radio_group, slider, toggle,
};
pub use icon::{Icon, IconStyle, icon};
pub use image::{Image, ImageStyle, image};
pub use text::{Label, LabelStyle, label};
