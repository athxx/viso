//! Interactive controls — widgets that respond to pointer and keyboard input.
//!
//! Where the Tier 1 presentational widgets (View/Label/Image/Icon) live as
//! single files in the crate root, interactive controls are grouped here: they
//! share the pattern of a focusable node with pointer and key handlers plus a
//! reactive visual state. The members so far are [`Button`], [`CheckBox`],
//! [`Toggle`], [`Chip`], [`RadioGroup`], [`Slider`], [`TextInput`], [`Splitter`],
//! [`Tabs`], [`NavigationStack`], [`Popup`], [`Modal`], [`Sheet`], [`Toast`],
//! and [`Dock`].

mod button;
mod caption_bar;
mod check;
mod checkbox;
mod chip;
mod combo_box;
mod dock;
mod file_tree;
mod glyphs;
mod labelled;
mod menu;
mod mnemonic;
mod modal;
mod navigation_stack;
mod number_field;
mod popup;
mod radio;
mod select;
mod sheet;
mod slider;
mod splitter;
mod tabs;
mod text_input;
mod toast;
mod toggle;
mod tooltip;

pub use button::{Button, ButtonStyle, button};
pub use caption_bar::{CaptionBar, CaptionBarStyle, caption_bar};
pub use checkbox::{CheckBox, CheckBoxStyle, checkbox};
pub use chip::{Chip, ChipStyle, chip};
pub use combo_box::{ComboBox, ComboBoxHandle, ComboBoxHandleSlot, ComboBoxStyle, combo_box};
pub use dock::{
    Dock, DockHandle, DockHandleSlot, DockNode, DockStyle, DockTree, DropPart, Floating,
    PanelContent, PanelKey, dock,
};
pub use file_tree::{
    FileTree, FileTreeHandle, FileTreeHandleSlot, FileTreeStyle, NodeKey, SelectMode, SelectOp,
    TreeNode, VisibleRow, file_tree,
};
pub use labelled::{Labelled, labelled};
pub use menu::{
    ContextMenu, HOVER_GRACE, HOVER_OPEN, Menu, MenuBar, MenuBarHandle, MenuBarHandleSlot,
    MenuBarStyle, MenuEntry, MenuHandle, MenuHandleSlot, MenuItem, MenuStyle, Shortcut, TYPE_AHEAD,
    context_menu, menu, menu_bar, menu_item, separator,
};
pub use modal::{Modal, ModalHandle, ModalHandleSlot, ModalStyle, modal};
pub use navigation_stack::{
    NavHandle, NavHandleSlot, NavigationStack, NavigationStackStyle, navigation_stack,
};
pub use number_field::{NumberField, number_field};
pub use popup::{Popup, PopupArrow, PopupHandle, PopupHandleSlot, PopupStyle, popover, popup};
pub use radio::{RadioGroup, RadioStyle, radio_group};
pub use select::{
    Select, SelectHandle, SelectHandleSlot, SelectKind, SelectOption, SelectStyle, select,
    select_option,
};
pub use sheet::{Sheet, SheetDetent, SheetEdge, SheetHandle, SheetHandleSlot, SheetStyle, sheet};
pub use slider::{Slider, SliderStyle, slider};
pub use splitter::{Splitter, SplitterStyle, splitter};
pub use tabs::{Tabs, TabsStyle, tabs};
pub use text_input::{TextInput, TextInputStyle, text_input};
pub use toast::{Toast, ToastEdge, ToastHandle, ToastHandleSlot, ToastStyle, toast};
pub use toggle::{Toggle, ToggleStyle, toggle};
pub use tooltip::{TOOLTIP_GRACE, TOOLTIP_LINGER, TOOLTIP_TOUCH, Tooltip, TooltipStyle, tooltip};
