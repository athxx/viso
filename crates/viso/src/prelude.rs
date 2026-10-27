//! The curated default prelude: `use viso::prelude::*;`.

pub use crate::{Application, run};
// `cx.services()` on a handler's context: the file dialog, share sheet and
// permission prompts an ordinary app reaches for.
pub use crate::services::ServicesExt;
pub use viso_ui::context::AppCx;
pub use viso_ui::dirty::DirtyClass;
pub use viso_ui::node::NodeId;
// Scene authoring: the mental model for a real app is `Application::build`
// declaring nodes and wiring reactive state, so the authoring context, the
// layout styles, the semantics facts, and the state cell handles belong in
// the default set (commonly used, stable, unambiguous).
pub use viso_ui::{
    AdaptiveColumns, BuildCx, CursorIcon, FlexStyle, GridPlacement, GridStyle, InputKind,
    LeafStyle, Role, Semantics, SpanStyle, State, StateId, StateValue, TextRequest, TrackMax,
    TrackSizing, VirtualListStyle,
};
// Typed routes: the back stack, its outlet, and the route → page table.
pub use viso_ui::{Navigator, router_view, routes};
// What a hand-written component is and lays out with: the trait, its size
// request, the flex axis, alignment along and across it, insets, a box's
// fill and corners, and a colour; and the context a handler writes state
// through (`Application::on_open_url`, `on_click`).
pub use viso_render::Rgba;
pub use viso_ui::{Align, Axis, BoxStyle, Component, EventCx, Inset, Justify, Sizing};

// Tier 1 widgets: the base layout container, the static text control, the
// texture-backed image control, and the vector icon control — plus the Tier 2
// interactive controls, Button, CheckBox, Toggle, Chip, Stepper, RadioGroup,
// Slider, and TextInput, the Tier 3 layout structures — the Scroll viewport, the
// virtualized VirtualList, the two-dimensional Grid, and the draggable-pane
// Splitter — and the Tier 4 navigation/overlay controls, the panel-switching
// Tabs, the page-stack NavigationStack (with its app-captured NavHandle for
// programmatic push/pop), the Popup (a persistent anchor with a floating
// top-layer content, with its app-captured PopupHandle for programmatic
// open/close/toggle), the Modal (a full-surface dialog that dims the scene
// and traps focus while open, with its app-captured ModalHandle), and the
// Sheet (an edge panel that slides in over a dimming scrim, rests at its
// detents and drags off, trapping focus while open, with its app-captured
// SheetHandle for programmatic open/close/toggle). A
// widget is a `Component` an app authors and builds into its `BuildCx`, so
// these belong in the default set as they land.
pub use viso_widgets::{
    Avatar, Badge, Button, ButtonStyle, Calendar, CheckBox, CheckBoxStyle, Chip, ChipStyle,
    ColorPicker, DatePicker, Grid, GridViewStyle, Icon, IconStyle, Image, ImageStyle, Label,
    LabelStyle, Modal, ModalHandle, ModalStyle, NavHandle, NavigationStack, NavigationStackStyle,
    NumberField, Popup, PopupHandle, PopupStyle, ProgressIndicator, RadioGroup, RadioStyle,
    RangeSlider, Scroll, ScrollViewStyle, SearchField, SearchFieldHandle, SegmentedControl,
    SegmentedStyle, Sheet, SheetDetent, SheetEdge, SheetHandle, SheetStyle, Slider, SliderStyle,
    Spinner, SplitAt, Splitter, SplitterStyle, Stepper, StepperStyle, Tabs, TabsStyle, TextInput,
    TextInputStyle, TimePicker, Toast, ToastEdge, ToastHandle, ToastStyle, Toggle, ToggleStyle,
    View, ViewStyle, VirtualList, VirtualListViewStyle, avatar, badge, button, calendar, checkbox,
    chip, color_picker, date_picker, dot_badge, grid, icon, image, knob, label, link_label, modal,
    navigation_stack, number_field, popup, progress_bar, progress_ring, radio_group, range_slider,
    rich_text, scroll, search_field, segmented_control, sheet, slider, spinner, splitter, stepper,
    tabs, text_input, time_picker, toast, toggle, view, virtual_list,
};
// Forms: a checked field, the form gathering fields, a group of fields.
pub use viso_widgets::{Field, FieldValue, Fieldset, Form, field, fieldset};
// The data grid: a table that scrolls both ways, with its handle.
pub use viso_widgets::{
    DataGrid, DataGridHandle, GridSelect, GridSelection, data_grid, grid_column,
};
// The top-level Window control (public application lifecycle): a window
// is not a node control — it does not enter the UI tree — so it appears as an
// application-level handle, not a `Component`. `window(cfg).content(..).open(ev)`
// opens another OS window mid-session and hands back a `WindowHandle` to close
// it. `WindowConfig` is the (title, size) shape the author fills in. These are
// commonly used, stable, and unambiguous, so they belong in the default set.
pub use crate::{WindowBuilder, WindowChrome, WindowConfig, WindowHandle, window};
// The application-menu model (application lifecycle): an app returns a `Menu` tree
// from `Application::menu`, wiring custom items to `MenuCommandId`s it picks
// and shortcuts through `Accel`; standard entries use `SystemAction`. Menus
// are an application-level concern like `window`, so they belong here.
pub use viso_platform::{Accel, Menu, MenuCommandId, SystemAction};
// The DSL entry points: a small local `ui! { … }` fragment, a Rust-adjacent
// `component! { … }`, and an external `view!("x.vs")`, each lowered at Rust
// compile time to a static `BuildCx` builder.
pub use crate::{component, ui, view};
// 2D geometry, owned by `viso-math`.
pub use viso_math::{Point, Rect, Vec2};
// `asset!` is exported from the crate root (`viso::asset!`), not from here.
