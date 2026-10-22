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
//! RadioGroup, Slider, TextInput, Splitter). Tier 3 completes the layout
//! structures — [`scroll`] (Scroll), the virtualized [`list`] (VirtualList),
//! the two-dimensional track [`grid`] (Grid), and the draggable-pane Splitter.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod containers;
pub mod controls;
pub mod document;
pub mod grid;
pub mod icon;
pub mod image;
pub mod list;
pub mod scroll;
pub mod text;

pub use containers::{View, ViewStyle, view};
pub use controls::{
    Button, ButtonStyle, CaptionBar, CaptionBarStyle, CheckBox, CheckBoxStyle, Dock, DockHandle,
    DockHandleSlot, DockNode, DockStyle, DockTree, DropPart, FileTree, FileTreeHandle,
    FileTreeHandleSlot, FileTreeStyle, Floating, Modal, ModalHandle, ModalHandleSlot, ModalStyle,
    NavHandle, NavHandleSlot, NavigationStack, NavigationStackStyle, NodeKey, NumberField,
    PanelContent, PanelKey, Popup, PopupArrow, PopupHandle, PopupHandleSlot, PopupStyle,
    RadioGroup, RadioStyle, SelectMode, SelectOp, Sheet, SheetEdge, SheetHandle, SheetHandleSlot,
    SheetStyle, Slider, SliderStyle, Splitter, SplitterStyle, Tabs, TabsStyle, TextInput,
    TextInputStyle, Toast, ToastEdge, ToastHandle, ToastHandleSlot, ToastStyle, Toggle,
    ToggleStyle, TreeNode, VisibleRow, button, caption_bar, checkbox, dock, file_tree, modal,
    navigation_stack, number_field, popover, popup, radio_group, sheet, slider, splitter, tabs,
    text_input, toast, toggle,
};
pub use document::{Document, DocumentStyle, DocumentText, html, markdown};
pub use grid::{Grid, GridViewStyle, grid};
pub use icon::{Icon, IconStyle, icon};
pub use image::{Image, ImageStyle, image, image_asset};
pub use list::{VirtualList, VirtualListViewStyle, virtual_list};
pub use scroll::{Scroll, ScrollViewStyle, scroll};
pub use text::{Label, LabelStyle, label, rich_text};
