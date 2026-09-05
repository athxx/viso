//! Interactive controls — widgets that respond to pointer and keyboard input.
//!
//! Where the Tier 1 presentational widgets (View/Label/Image/Icon) live as
//! single files in the crate root, interactive controls are grouped here: they
//! share the pattern of a focusable node with pointer and key handlers plus a
//! reactive visual state, and this directory is where the family grows
//! (CheckBox, Toggle, Radio, Slider, TextInput). The first member is [`Button`].

mod button;

pub use button::{Button, ButtonStyle, button};
