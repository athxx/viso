//! Interactive controls — widgets that respond to pointer and keyboard input.
//!
//! Where the Tier 1 presentational widgets (View/Label/Image/Icon) live as
//! single files in the crate root, interactive controls are grouped here: they
//! share the pattern of a focusable node with pointer and key handlers plus a
//! reactive visual state. The members so far are [`Button`], [`CheckBox`],
//! [`Toggle`], [`RadioGroup`], [`Slider`], [`TextInput`], [`Splitter`],
//! [`Tabs`], [`NavigationStack`], [`Popup`], [`Modal`], [`Sheet`], [`Toast`],
//! and [`Dock`].

mod button;
mod checkbox;
mod dock;
mod file_tree;
mod modal;
mod navigation_stack;
mod popup;
mod radio;
mod sheet;
mod slider;
mod splitter;
mod tabs;
mod text_input;
mod toast;
mod toggle;

pub use button::{Button, ButtonStyle, button};
pub use checkbox::{CheckBox, CheckBoxStyle, checkbox};
pub use dock::{
    Dock, DockHandle, DockHandleSlot, DockNode, DockStyle, DockTree, DropPart, Floating,
    PanelContent, PanelKey, dock,
};
pub use file_tree::{
    FileTree, FileTreeStyle, NodeKey, SelectMode, TreeNode, VisibleRow, file_tree,
};
pub use modal::{Modal, ModalHandle, ModalHandleSlot, ModalStyle, modal};
pub use navigation_stack::{
    NavHandle, NavHandleSlot, NavigationStack, NavigationStackStyle, navigation_stack,
};
pub use popup::{Popup, PopupHandle, PopupHandleSlot, PopupStyle, popup};
pub use radio::{RadioGroup, RadioStyle, radio_group};
pub use sheet::{Sheet, SheetEdge, SheetHandle, SheetHandleSlot, SheetStyle, sheet};
pub use slider::{Slider, SliderStyle, slider};
pub use splitter::{Splitter, SplitterStyle, splitter};
pub use tabs::{Tabs, TabsStyle, tabs};
pub use text_input::{TextInput, TextInputStyle, text_input};
pub use toast::{Toast, ToastEdge, ToastHandle, ToastHandleSlot, ToastStyle, toast};
pub use toggle::{Toggle, ToggleStyle, toggle};
