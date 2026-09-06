//! Interactive controls — widgets that respond to pointer and keyboard input.
//!
//! Where the Tier 1 presentational widgets (View/Label/Image/Icon) live as
//! single files in the crate root, interactive controls are grouped here: they
//! share the pattern of a focusable node with pointer and key handlers plus a
//! reactive visual state. The members so far are [`Button`], [`CheckBox`],
//! [`Toggle`], [`RadioGroup`], [`Slider`], [`TextInput`], [`Splitter`],
//! [`Tabs`], and [`NavigationStack`].

mod button;
mod checkbox;
mod navigation_stack;
mod radio;
mod slider;
mod splitter;
mod tabs;
mod text_input;
mod toggle;

pub use button::{Button, ButtonStyle, button};
pub use checkbox::{CheckBox, CheckBoxStyle, checkbox};
pub use navigation_stack::{
    NavHandle, NavHandleSlot, NavigationStack, NavigationStackStyle, navigation_stack,
};
pub use radio::{RadioGroup, RadioStyle, radio_group};
pub use slider::{Slider, SliderStyle, slider};
pub use splitter::{Splitter, SplitterStyle, splitter};
pub use tabs::{Tabs, TabsStyle, tabs};
pub use text_input::{TextInput, TextInputStyle, text_input};
pub use toggle::{Toggle, ToggleStyle, toggle};
