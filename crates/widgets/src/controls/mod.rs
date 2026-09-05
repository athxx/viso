//! Interactive controls — widgets that respond to pointer and keyboard input.
//!
//! Where the Tier 1 presentational widgets (View/Label/Image/Icon) live as
//! single files in the crate root, interactive controls are grouped here: they
//! share the pattern of a focusable node with pointer and key handlers plus a
//! reactive visual state, and this directory is where the family grows
//! (Toggle, Radio, Slider, TextInput). The members so far are [`Button`] and
//! [`CheckBox`].

mod button;
mod checkbox;

pub use button::{Button, ButtonStyle, button};
pub use checkbox::{CheckBox, CheckBoxStyle, checkbox};
