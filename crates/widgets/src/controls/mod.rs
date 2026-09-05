//! Interactive controls — widgets that respond to pointer and keyboard input.
//!
//! Where the Tier 1 presentational widgets (View/Label/Image/Icon) live as
//! single files in the crate root, interactive controls are grouped here: they
//! share the pattern of a focusable node with pointer and key handlers plus a
//! reactive visual state, and this directory is where the family grows
//! (TextInput). The members so far are [`Button`], [`CheckBox`], [`Toggle`],
//! [`RadioGroup`], and [`Slider`].

mod button;
mod checkbox;
mod radio;
mod slider;
mod toggle;

pub use button::{Button, ButtonStyle, button};
pub use checkbox::{CheckBox, CheckBoxStyle, checkbox};
pub use radio::{RadioGroup, RadioStyle, radio_group};
pub use slider::{Slider, SliderStyle, slider};
pub use toggle::{Toggle, ToggleStyle, toggle};
