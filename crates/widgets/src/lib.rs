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
//! the two-dimensional track [`grid`] (Grid), the draggable-pane Splitter, and
//! the pan-and-zoom [`interactive_viewer`] (InteractiveViewer), and the
//! [`swipe_row`] (SwipeRow) whose content slides aside to show its actions.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod avatar;
pub mod badge;
pub mod containers;
pub mod controls;
pub mod document;
pub mod drag_drop;
pub mod grid;
pub mod icon;
pub mod image;
pub mod interactive_viewer;
pub mod lazy_grid;
pub mod list;
pub mod pager;
pub mod progress;
pub mod reorder_handle;
pub mod scroll;
pub mod swipe_row;
pub mod text;

pub use avatar::{Avatar, avatar};
pub use badge::{Badge, badge, dot_badge};
pub use containers::{View, ViewStyle, view};
pub use controls::{
    Button, ButtonStyle, CaptionBar, CaptionBarStyle, CheckBox, CheckBoxStyle, Dock, DockHandle,
    DockHandleSlot, DockNode, DockStyle, DockTree, DropPart, FileTree, FileTreeHandle,
    FileTreeHandleSlot, FileTreeStyle, Floating, Labelled, Modal, ModalHandle, ModalHandleSlot,
    ModalStyle, NavHandle, NavHandleSlot, NavigationStack, NavigationStackStyle, NodeKey,
    NumberField, PanelContent, PanelKey, Popup, PopupArrow, PopupHandle, PopupHandleSlot,
    PopupStyle, RadioGroup, RadioStyle, RangeSlider, SelectMode, SelectOp, Sheet, SheetDetent,
    SheetEdge, SheetHandle, SheetHandleSlot, SheetStyle, Slider, SliderStyle, Splitter,
    SplitterStyle, Tabs, TabsStyle, TextInput, TextInputStyle, Toast, ToastEdge, ToastHandle,
    ToastHandleSlot, ToastStyle, Toggle, ToggleStyle, TreeNode, VisibleRow, button, caption_bar,
    checkbox, dock, file_tree, knob, labelled, modal, navigation_stack, number_field, popover,
    popup, radio_group, range_slider, sheet, slider, splitter, tabs, text_input, toast, toggle,
};
pub use controls::{Calendar, CalendarStyle, DateNames, calendar};
pub use controls::{Chip, ChipStyle, chip};
pub use controls::{ColorPicker, ColorPickerLabels, ColorPickerStyle, color_picker};
pub use controls::{ComboBox, ComboBoxHandle, ComboBoxHandleSlot, ComboBoxStyle, combo_box};
pub use controls::{
    ContextMenu, Menu, MenuBar, MenuBarHandle, MenuBarHandleSlot, MenuBarStyle, MenuEntry,
    MenuHandle, MenuHandleSlot, MenuItem, MenuStyle, Shortcut, context_menu, menu, menu_bar,
    menu_item, separator,
};
pub use controls::{DatePicker, DatePickerStyle, date_picker};
pub use controls::{
    SearchField, SearchFieldHandle, SearchFieldHandleSlot, SearchFieldStyle, search_field,
};
pub use controls::{SegmentedControl, SegmentedStyle, segmented_control};
pub use controls::{
    Select, SelectHandle, SelectHandleSlot, SelectKind, SelectOption, SelectStyle, select,
    select_option,
};
pub use controls::{Stepper, StepperStyle, stepper};
pub use controls::{TOOLTIP_GRACE, TOOLTIP_LINGER, TOOLTIP_TOUCH, Tooltip, TooltipStyle, tooltip};
pub use document::{Document, DocumentStyle, DocumentText, html, markdown};
pub use drag_drop::{Draggable, DraggableStyle, DropZone, DropZoneStyle, draggable, drop_zone};
pub use grid::{Grid, GridViewStyle, grid};
pub use icon::{Icon, IconStyle, icon};
pub use image::{Image, ImageStyle, image, image_asset};
pub use interactive_viewer::{
    InteractiveViewer, InteractiveViewerStyle, ViewerHandle, ViewerHandleSlot, interactive_viewer,
};
pub use lazy_grid::{LazyGrid, LazyGridStyle, lazy_grid};
pub use list::{VirtualList, VirtualListViewStyle, virtual_list};
pub use pager::{Carousel, CarouselStyle, Pager, PagerStyle, carousel, pager};
pub use progress::{ProgressIndicator, Spinner, progress_bar, progress_ring, spinner};
pub use reorder_handle::{ReorderHandle, ReorderHandleStyle, reorder_handle};
pub use scroll::{Scroll, ScrollViewStyle, scroll};
pub use swipe_row::{
    SwipeAction, SwipeRow, SwipeRowHandle, SwipeRowHandleSlot, SwipeRowStyle, swipe_action,
    swipe_row,
};
pub use text::{Label, LabelStyle, label, link_label, rich_text};
