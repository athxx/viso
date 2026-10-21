//! The native nodes a view drives: which control a node is, and the
//! handler-table entries it reads its value, range, look and semantics from.

use alloc::boxed::Box;
use alloc::vec::Vec;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};

use crate::interaction::Interaction;
use crate::semantics::{LiveRegion, Role};

use super::route::EventRoute;

/// Which view-driven native node a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ControlKind {
    /// `CheckBox`: a release or Space/Enter flips `checked`; it draws a box,
    /// ticked while checked, beside its caption.
    Check = 0,
    /// `Slider`: a press or a drag sets `value` from the pointer's position
    /// across the node; arrows, Page Up/Down and Home/End step it.
    Slider = 1,
    /// `Tabs` and `RadioGroup`: a release selects the child it lands in;
    /// arrows and Home/End move the selection.
    Select = 2,
    /// `TextInput`: pointer, key and IME samples edit its text; every settled
    /// edit reports the new text, and Enter submits. Its `value` seeds the text.
    TextInput = 3,
    /// `Text` and `Button`: shows the text of its `text`; it has no built-in
    /// response.
    Label = 4,
    /// Any other native node: it shows only its [`Look`] and has no built-in
    /// response.
    Plain = 5,
    /// `Image` and `Svg`: shows the asset of its `source`, fitted and tinted;
    /// it has no built-in response.
    Asset = 6,
    /// `ShaderView`: paints its box with the view's shader its `shader`
    /// names, the value's fields as the brush's parameters; it has no
    /// built-in response.
    Shader = 7,
    /// `Toggle`: flips `checked` as a [`Check`](Self::Check) does, drawing a
    /// track with its knob at the end while checked.
    Switch = 8,
}

impl ControlKind {
    /// Every kind, by discriminant.
    pub const ALL: [ControlKind; 9] = [
        ControlKind::Check,
        ControlKind::Slider,
        ControlKind::Select,
        ControlKind::TextInput,
        ControlKind::Label,
        ControlKind::Plain,
        ControlKind::Asset,
        ControlKind::Shader,
        ControlKind::Switch,
    ];

    /// The kind native widget `widget` is.
    pub fn of(widget: &str) -> ControlKind {
        match widget {
            "CheckBox" => ControlKind::Check,
            "Toggle" => ControlKind::Switch,
            "Slider" => ControlKind::Slider,
            "Tabs" | "RadioGroup" => ControlKind::Select,
            "TextInput" => ControlKind::TextInput,
            "Text" | "Button" => ControlKind::Label,
            "Image" | "Svg" => ControlKind::Asset,
            "ShaderView" => ControlKind::Shader,
            _ => ControlKind::Plain,
        }
    }

    /// The route of the control's event `event`, `None` for one it does not
    /// report.
    pub fn route(self, event: &str) -> Option<EventRoute> {
        let route = match event {
            "changed" => EventRoute::Changed,
            "submitted" => EventRoute::Submitted,
            "selected_changed" => EventRoute::SelectedChanged,
            _ => return None,
        };
        self.reports(route).then_some(route)
    }

    /// Whether the node responds to pointer and key samples itself.
    pub fn responds(self) -> bool {
        !matches!(
            self,
            ControlKind::Label | ControlKind::Plain | ControlKind::Asset | ControlKind::Shader
        )
    }

    /// Whether the control reports `route`.
    pub fn reports(self, route: EventRoute) -> bool {
        matches!(
            (self, route),
            (
                ControlKind::Check
                    | ControlKind::Switch
                    | ControlKind::Slider
                    | ControlKind::TextInput,
                EventRoute::Changed
            ) | (ControlKind::TextInput, EventRoute::Submitted)
                | (ControlKind::Select, EventRoute::SelectedChanged)
        )
    }

    /// Which of [`Control`]'s entries the widget property `property` fills; a
    /// group member is named by its dotted path (`transition.opacity`).
    pub fn input(self, property: &str) -> Option<ControlInput> {
        Some(match (self, property) {
            (_, "background") => ControlInput::Background,
            (_, "opacity") => ControlInput::Opacity,
            (_, "corner_radius") => ControlInput::CornerRadius,
            (_, "border") => ControlInput::Border,
            (_, "transition.background") => ControlInput::BackgroundTransition,
            (_, "transition.opacity") => ControlInput::OpacityTransition,
            (_, "translate") => ControlInput::Translate,
            (_, "transition.translate") => ControlInput::TranslateTransition,
            (_, "scale") => ControlInput::Scale,
            (_, "transition.scale") => ControlInput::ScaleTransition,
            (_, "rotation") => ControlInput::Rotation,
            (_, "transition.rotation") => ControlInput::RotationTransition,
            (_, "transform_origin") => ControlInput::TransformOrigin,
            (_, "transition.corner_radius") => ControlInput::CornerRadiusTransition,
            (_, "transition.width") => ControlInput::WidthTransition,
            (_, "transition.height") => ControlInput::HeightTransition,
            (ControlKind::Label | ControlKind::TextInput, "transition.color") => {
                ControlInput::ColorTransition
            }
            (_, "semantics.label") => ControlInput::Label,
            (_, "semantics.hint") => ControlInput::Hint,
            (_, "semantics.value") => ControlInput::SpokenValue,
            (_, "semantics.hidden") => ControlInput::Hidden,
            (_, "semantics.heading_level") => ControlInput::HeadingLevel,
            (_, "visible") => ControlInput::Visible,
            (_, "focusable") => ControlInput::Focusable,
            (_, "enabled") => ControlInput::Enabled,
            (_, "autofocus") => ControlInput::Autofocus,
            (_, "clip") => ControlInput::Clip,
            (ControlKind::Label | ControlKind::TextInput, "color") => ControlInput::TextColor,
            (ControlKind::Label | ControlKind::TextInput, "soft_wrap") => ControlInput::SoftWrap,
            (ControlKind::Label | ControlKind::TextInput, "font_weight") => {
                ControlInput::FontWeight
            }
            (ControlKind::Label | ControlKind::TextInput, "font_family") => {
                ControlInput::FontFamily
            }
            (ControlKind::Label | ControlKind::TextInput, "line_height") => {
                ControlInput::LineHeight
            }
            (ControlKind::Label | ControlKind::TextInput, "align") => ControlInput::TextAlign,
            (ControlKind::Label, "max_lines") => ControlInput::MaxLines,
            (ControlKind::Label, "overflow") => ControlInput::Overflow,
            (ControlKind::Check | ControlKind::Switch, "checked")
            | (ControlKind::Slider, "value")
            | (ControlKind::Select, "selected")
            | (ControlKind::TextInput, "value")
            | (ControlKind::Label, "text")
            | (ControlKind::Asset, "source")
            | (ControlKind::Shader, "shader") => ControlInput::Value,
            (ControlKind::TextInput, "placeholder") => ControlInput::Placeholder,
            (ControlKind::TextInput, "secure") => ControlInput::Secure,
            (ControlKind::Check | ControlKind::Switch, "label") => ControlInput::Caption,
            (ControlKind::Slider, "min") => ControlInput::Min,
            (ControlKind::Slider, "max") => ControlInput::Max,
            (ControlKind::Slider, "step") => ControlInput::Step,
            (ControlKind::Asset, "fit") => ControlInput::Fit,
            (ControlKind::Asset, "tint") => ControlInput::Tint,
            _ => return None,
        })
    }

    /// The Rust variant name, for code that names a kind in tokens.
    pub fn variant(self) -> &'static str {
        match self {
            ControlKind::Check => "Check",
            ControlKind::Switch => "Switch",
            ControlKind::Slider => "Slider",
            ControlKind::Select => "Select",
            ControlKind::TextInput => "TextInput",
            ControlKind::Label => "Label",
            ControlKind::Plain => "Plain",
            ControlKind::Asset => "Asset",
            ControlKind::Shader => "Shader",
        }
    }

    /// The kind with discriminant `raw`.
    pub fn from_u8(raw: u8) -> Option<ControlKind> {
        ControlKind::ALL.get(usize::from(raw)).copied()
    }
}

/// The role a node of native widget `widget` has when its `semantics.role`
/// names none; `None` for a container, which is a mute group. An image's
/// role holds only while it has a label: one without is decoration.
pub fn widget_role(widget: &str) -> Option<Role> {
    Some(match widget {
        "Button" => Role::Button,
        "Toggle" | "CheckBox" => Role::CheckBox,
        "Slider" => Role::Slider,
        "TextInput" => Role::TextField,
        "Tabs" => Role::TabList,
        "Text" => Role::Label,
        "Image" | "Svg" => Role::Image,
        "VirtualList" => Role::List,
        "NavigationStack" => Role::Navigation,
        _ => return None,
    })
}

/// The role a `Role::<variant>` of the source names.
pub fn source_role(variant: &str) -> Option<Role> {
    Some(match variant {
        "group" => Role::Group,
        "button" => Role::Button,
        "check_box" => Role::CheckBox,
        "slider" => Role::Slider,
        "radio" => Role::Radio,
        "label" => Role::Label,
        "image" => Role::Image,
        "text_field" => Role::TextField,
        "tab" => Role::Tab,
        "tab_list" => Role::TabList,
        "navigation" => Role::Navigation,
        "dialog" => Role::Dialog,
        "status" => Role::Status,
        "region" => Role::Region,
        "tree" => Role::Tree,
        "tree_item" => Role::TreeItem,
        "heading" => Role::Heading,
        "list" => Role::List,
        "list_item" => Role::ListItem,
        "secure_field" => Role::SecureField,
        _ => return None,
    })
}

/// The `Role::<variant>` of the source naming `role`.
pub fn source_name(role: Role) -> &'static str {
    SOURCE_ROLES[usize::from(role_tag(role))]
}

/// Every variant [`source_role`] takes, in the order of [`Role::ALL`].
pub const SOURCE_ROLES: [&str; 20] = [
    "group",
    "button",
    "check_box",
    "slider",
    "radio",
    "label",
    "image",
    "text_field",
    "tab",
    "tab_list",
    "navigation",
    "dialog",
    "status",
    "region",
    "tree",
    "tree_item",
    "heading",
    "list",
    "list_item",
    "secure_field",
];

/// A layout property a node's value sets as the view's values change: one
/// that reads a value, which the compiler cannot fold into the node's style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum LayoutSlot {
    /// `width`: a `Sizing`, or a length for `fixed` of one.
    Width = 0,
    /// `height`: as `width`.
    Height = 1,
    /// `min_width`: a `MixedLength`.
    MinWidth = 2,
    /// `max_width`: an `Option<MixedLength>`.
    MaxWidth = 3,
    /// `min_height`: a `MixedLength`.
    MinHeight = 4,
    /// `max_height`: an `Option<MixedLength>`.
    MaxHeight = 5,
    /// `margin`: an `EdgeInsets`, or a length for `all` of one.
    Margin = 6,
    /// A container's `padding`: as `margin`.
    Padding = 7,
    /// A flex container's `gap` (`spacing`), a grid's both gaps: a
    /// `MixedLength`.
    Gap = 8,
    /// A grid's `column_gap`: a `MixedLength`.
    ColumnGap = 9,
    /// A grid's `row_gap`: a `MixedLength`.
    RowGap = 10,
    /// A flex container's `axis` (`direction`): an `Axis`.
    Axis = 11,
    /// A flex container's `justify`: a `Justify`.
    Justify = 12,
    /// A flex container's `align`: an `Align`.
    Align = 13,
    /// A grid's `columns`: a `List<Track>`.
    Columns = 14,
    /// A grid's `rows`: a `List<Track>`.
    Rows = 15,
    /// A grid's `auto_rows`: a `Track`.
    AutoRows = 16,
    /// A grid's `align_items`: a `GridAlign`.
    AlignItems = 17,
    /// A grid's `adaptive_columns`: an `Option<AdaptiveColumns>`.
    AdaptiveColumns = 18,
    /// A stack's `content_align`: an `Alignment2D`.
    ContentAlign = 19,
    /// A grid child's `grid.column`: an `Option<U16>`.
    GridColumn = 20,
    /// A grid child's `grid.row`: an `Option<U16>`.
    GridRow = 21,
    /// A grid child's `grid.column_span`: a `U16`.
    GridColumnSpan = 22,
    /// A grid child's `grid.row_span`: a `U16`.
    GridRowSpan = 23,
    /// A stack child's `stack.align`: an `Option<Alignment2D>`.
    StackAlign = 24,
    /// A stack child's `stack.layer`: an `I32`.
    StackLayer = 25,
    /// An absolute child's `absolute.top`: an `Option<MixedLength>`.
    AbsoluteTop = 26,
    /// An absolute child's `absolute.end`: as `absolute.top`.
    AbsoluteEnd = 27,
    /// An absolute child's `absolute.bottom`: as `absolute.top`.
    AbsoluteBottom = 28,
    /// An absolute child's `absolute.start`: as `absolute.top`.
    AbsoluteStart = 29,
    /// A text node's `font_size`: a `MixedLength`, its `em` and `%` of its
    /// parent's font size.
    FontSize = 30,
    /// An adaptive scope's `layout_direction`: an `Option<LayoutDirection>`.
    LayoutDirection = 31,
}

impl LayoutSlot {
    /// Every slot, by discriminant.
    pub const ALL: [LayoutSlot; 32] = [
        LayoutSlot::Width,
        LayoutSlot::Height,
        LayoutSlot::MinWidth,
        LayoutSlot::MaxWidth,
        LayoutSlot::MinHeight,
        LayoutSlot::MaxHeight,
        LayoutSlot::Margin,
        LayoutSlot::Padding,
        LayoutSlot::Gap,
        LayoutSlot::ColumnGap,
        LayoutSlot::RowGap,
        LayoutSlot::Axis,
        LayoutSlot::Justify,
        LayoutSlot::Align,
        LayoutSlot::Columns,
        LayoutSlot::Rows,
        LayoutSlot::AutoRows,
        LayoutSlot::AlignItems,
        LayoutSlot::AdaptiveColumns,
        LayoutSlot::ContentAlign,
        LayoutSlot::GridColumn,
        LayoutSlot::GridRow,
        LayoutSlot::GridColumnSpan,
        LayoutSlot::GridRowSpan,
        LayoutSlot::StackAlign,
        LayoutSlot::StackLayer,
        LayoutSlot::AbsoluteTop,
        LayoutSlot::AbsoluteEnd,
        LayoutSlot::AbsoluteBottom,
        LayoutSlot::AbsoluteStart,
        LayoutSlot::FontSize,
        LayoutSlot::LayoutDirection,
    ];

    /// The slot the layout property `property` fills; `None` for one that
    /// is no layout property. A parent-provided property is named by its
    /// dotted path (`grid.column`). Which node takes which slot is the
    /// compiler's to check.
    pub fn of(property: &str) -> Option<LayoutSlot> {
        Some(match property {
            "width" => LayoutSlot::Width,
            "height" => LayoutSlot::Height,
            "min_width" => LayoutSlot::MinWidth,
            "max_width" => LayoutSlot::MaxWidth,
            "min_height" => LayoutSlot::MinHeight,
            "max_height" => LayoutSlot::MaxHeight,
            "margin" => LayoutSlot::Margin,
            "padding" => LayoutSlot::Padding,
            "gap" | "spacing" => LayoutSlot::Gap,
            "column_gap" => LayoutSlot::ColumnGap,
            "row_gap" => LayoutSlot::RowGap,
            "axis" | "direction" => LayoutSlot::Axis,
            "justify" => LayoutSlot::Justify,
            "align" => LayoutSlot::Align,
            "columns" => LayoutSlot::Columns,
            "rows" => LayoutSlot::Rows,
            "auto_rows" => LayoutSlot::AutoRows,
            "align_items" => LayoutSlot::AlignItems,
            "adaptive_columns" => LayoutSlot::AdaptiveColumns,
            "content_align" => LayoutSlot::ContentAlign,
            "grid.column" => LayoutSlot::GridColumn,
            "grid.row" => LayoutSlot::GridRow,
            "grid.column_span" => LayoutSlot::GridColumnSpan,
            "grid.row_span" => LayoutSlot::GridRowSpan,
            "stack.align" => LayoutSlot::StackAlign,
            "stack.layer" => LayoutSlot::StackLayer,
            "absolute.top" => LayoutSlot::AbsoluteTop,
            "absolute.end" => LayoutSlot::AbsoluteEnd,
            "absolute.bottom" => LayoutSlot::AbsoluteBottom,
            "absolute.start" => LayoutSlot::AbsoluteStart,
            "font_size" => LayoutSlot::FontSize,
            "layout_direction" => LayoutSlot::LayoutDirection,
            _ => return None,
        })
    }

    /// The slot with discriminant `raw`.
    pub fn from_u8(raw: u8) -> Option<LayoutSlot> {
        LayoutSlot::ALL.get(usize::from(raw)).copied()
    }

    /// The Rust variant name, for code that names a slot in tokens.
    pub fn variant(self) -> &'static str {
        match self {
            LayoutSlot::Width => "Width",
            LayoutSlot::Height => "Height",
            LayoutSlot::MinWidth => "MinWidth",
            LayoutSlot::MaxWidth => "MaxWidth",
            LayoutSlot::MinHeight => "MinHeight",
            LayoutSlot::MaxHeight => "MaxHeight",
            LayoutSlot::Margin => "Margin",
            LayoutSlot::Padding => "Padding",
            LayoutSlot::Gap => "Gap",
            LayoutSlot::ColumnGap => "ColumnGap",
            LayoutSlot::RowGap => "RowGap",
            LayoutSlot::Axis => "Axis",
            LayoutSlot::Justify => "Justify",
            LayoutSlot::Align => "Align",
            LayoutSlot::Columns => "Columns",
            LayoutSlot::Rows => "Rows",
            LayoutSlot::AutoRows => "AutoRows",
            LayoutSlot::AlignItems => "AlignItems",
            LayoutSlot::AdaptiveColumns => "AdaptiveColumns",
            LayoutSlot::ContentAlign => "ContentAlign",
            LayoutSlot::GridColumn => "GridColumn",
            LayoutSlot::GridRow => "GridRow",
            LayoutSlot::GridColumnSpan => "GridColumnSpan",
            LayoutSlot::GridRowSpan => "GridRowSpan",
            LayoutSlot::StackAlign => "StackAlign",
            LayoutSlot::StackLayer => "StackLayer",
            LayoutSlot::AbsoluteTop => "AbsoluteTop",
            LayoutSlot::AbsoluteEnd => "AbsoluteEnd",
            LayoutSlot::AbsoluteBottom => "AbsoluteBottom",
            LayoutSlot::AbsoluteStart => "AbsoluteStart",
            LayoutSlot::FontSize => "FontSize",
            LayoutSlot::LayoutDirection => "LayoutDirection",
        }
    }

    /// Whether it is one of the node's box rules or its padding, which the
    /// node's lengths carry together.
    pub fn is_rule(self) -> bool {
        matches!(
            self,
            LayoutSlot::MinWidth
                | LayoutSlot::MaxWidth
                | LayoutSlot::MinHeight
                | LayoutSlot::MaxHeight
                | LayoutSlot::Margin
                | LayoutSlot::Padding
        )
    }

    /// Whether it is one of an absolute child's insets.
    pub fn is_inset(self) -> bool {
        matches!(
            self,
            LayoutSlot::AbsoluteTop
                | LayoutSlot::AbsoluteEnd
                | LayoutSlot::AbsoluteBottom
                | LayoutSlot::AbsoluteStart
        )
    }
}

/// A value a control reads from the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlInput {
    /// Its current value: `checked`, `value`, `selected`, a label's `text`,
    /// an image's `source` or a shader view's `shader`.
    Value,
    /// A slider's lower bound.
    Min,
    /// A slider's upper bound.
    Max,
    /// A slider's step.
    Step,
    /// An image's `fit`.
    Fit,
    /// An image's `tint`.
    Tint,
    /// The node's `background`.
    Background,
    /// The node's `opacity`.
    Opacity,
    /// The node's `corner_radius`.
    CornerRadius,
    /// The node's `border`.
    Border,
    /// The node's `transition.background`.
    BackgroundTransition,
    /// The node's `transition.opacity`.
    OpacityTransition,
    /// The node's `translate`: an `Offset` it and its subtree move by.
    Translate,
    /// The node's `transition.translate`.
    TranslateTransition,
    /// The node's `scale` about its `transform_origin`.
    Scale,
    /// The node's `transition.scale`.
    ScaleTransition,
    /// The node's clockwise `rotation` about its `transform_origin`.
    Rotation,
    /// The node's `transition.rotation`.
    RotationTransition,
    /// The node's `transform_origin`: where in its box it scales and rotates
    /// about.
    TransformOrigin,
    /// The node's `transition.corner_radius`.
    CornerRadiusTransition,
    /// A text's `transition.color`.
    ColorTransition,
    /// The node's `transition.width`.
    WidthTransition,
    /// The node's `transition.height`.
    HeightTransition,
    /// The node's `semantics.label`: its accessible name.
    Label,
    /// The node's `semantics.hint`.
    Hint,
    /// The node's `semantics.value`: its value as text.
    SpokenValue,
    /// The node's `semantics.hidden`.
    Hidden,
    /// The node's `semantics.heading_level`.
    HeadingLevel,
    /// The node's `visible`: whether it and its subtree lay out, paint and
    /// are read.
    Visible,
    /// The node's `focusable`.
    Focusable,
    /// The node's `enabled`: whether it and its subtree take input and
    /// focus.
    Enabled,
    /// The node's `autofocus`: whether it takes the focus as it mounts.
    Autofocus,
    /// The node's `clip`: whether its children are seen and hit only inside
    /// its box.
    Clip,
    /// A text's `color`.
    TextColor,
    /// A text's `soft_wrap`.
    SoftWrap,
    /// A text's `font_weight`.
    FontWeight,
    /// A text's `font_family`.
    FontFamily,
    /// A text's `line_height`.
    LineHeight,
    /// A text's `max_lines`.
    MaxLines,
    /// A text's `overflow`.
    Overflow,
    /// A text's `align`.
    TextAlign,
    /// A text field's `placeholder`: what it shows while empty.
    Placeholder,
    /// A text field's `secure`: whether it masks its text.
    Secure,
    /// A check's `label`: the caption it shows and is named by.
    Caption,
}

/// A view-driven native node: its kind and the handler-table entries that
/// evaluate its current value and range. An absent entry reads the property's
/// default: `false`, `0`, an empty text, a `0..1` range and no step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    /// Which control it is.
    pub kind: ControlKind,
    /// The entry of its current value.
    pub value: Option<u32>,
    /// The entry of a slider's lower bound.
    pub min: Option<u32>,
    /// The entry of a slider's upper bound.
    pub max: Option<u32>,
    /// The entry of a slider's step.
    pub step: Option<u32>,
    /// The entry of an image's fit.
    pub fit: Option<u32>,
    /// The entry of an image's tint.
    pub tint: Option<u32>,
    /// The entry of a text field's placeholder.
    pub placeholder: Option<u32>,
    /// The entry of whether a text field masks its text.
    pub secure: Option<u32>,
    /// The entry of a check's caption.
    pub caption: Option<u32>,
    /// The shader a shader view paints with, by its index in the view's
    /// shader table; its value gives the shader's parameters.
    pub shader: Option<u32>,
    /// The entries of the look it shows.
    pub look: Look,
    /// The look values its styles' `when` blocks select, the first that holds
    /// winning over the others and over [`look`](Self::look).
    pub arms: Box<[LookArm]>,
    /// Whether it shows and takes focus.
    pub node: NodeEntries,
    /// The style of the text it shows.
    pub text: TextEntries,
    /// What it tells assistive technology beside the state its kind
    /// projects.
    pub semantics: SemanticEntries,
    /// The entries of the layout properties it reads, by ascending slot.
    pub layout: Box<[(LayoutSlot, u32)]>,
}

/// The handler-table entries of whether a node shows, takes input and takes
/// focus: an absent entry leaves it as it was built (shown, enabled, and
/// focusable when its kind is).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeEntries {
    /// The entry of its `visible`.
    pub visible: Option<u32>,
    /// The entry of its `focusable`.
    pub focusable: Option<u32>,
    /// The entry of its `enabled`.
    pub enabled: Option<u32>,
    /// The entry of its `autofocus`.
    pub autofocus: Option<u32>,
    /// The entry of its `clip`.
    pub clip: Option<u32>,
}

/// The handler-table entries of the style of a text a node shows: an absent
/// entry reads the default (the view's text color, no wrapping, the regular
/// interface face at its own line spacing, every line, clipped, at the
/// start).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextEntries {
    /// The entry of its `color`.
    pub color: Option<u32>,
    /// The entry of its `soft_wrap`.
    pub soft_wrap: Option<u32>,
    /// The entry of its `font_weight`.
    pub font_weight: Option<u32>,
    /// The entry of its `font_family`.
    pub font_family: Option<u32>,
    /// The entry of its `line_height`.
    pub line_height: Option<u32>,
    /// The entry of its `max_lines`.
    pub max_lines: Option<u32>,
    /// The entry of its `overflow`.
    pub overflow: Option<u32>,
    /// The entry of its `align`.
    pub align: Option<u32>,
}

impl TextEntries {
    /// The entries in their encoded order: `color`, `soft_wrap`,
    /// `font_weight`, `font_family`, `line_height`, `max_lines`, `overflow`
    /// and `align`.
    pub fn all(&self) -> [Option<u32>; 8] {
        [
            self.color,
            self.soft_wrap,
            self.font_weight,
            self.font_family,
            self.line_height,
            self.max_lines,
            self.overflow,
            self.align,
        ]
    }
}

/// What a control tells assistive technology beside the state its kind
/// projects: its role and live setting, and the handler-table entries of the
/// rest of its `semantics.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SemanticEntries {
    /// Its role, `None` for a node with no role of its own (a container that
    /// is a group).
    pub role: Option<Role>,
    /// How a change to its content is announced.
    pub live: LiveRegion,
    /// The entry of its `semantics.label`; without one, a label's name is the
    /// text it shows.
    pub label: Option<u32>,
    /// The entry of its `semantics.hint`.
    pub hint: Option<u32>,
    /// The entry of its `semantics.value`.
    pub value: Option<u32>,
    /// The entry of its `semantics.hidden`.
    pub hidden: Option<u32>,
    /// The entry of its `semantics.heading_level`.
    pub heading_level: Option<u32>,
}

impl SemanticEntries {
    /// Whether it says anything of its own: a node with nothing to say keeps
    /// what its builder gave it.
    pub fn says_anything(&self) -> bool {
        self.role.is_some()
            || self.live != LiveRegion::Off
            || [
                self.label,
                self.hint,
                self.value,
                self.hidden,
                self.heading_level,
            ]
            .iter()
            .any(Option::is_some)
    }
}

/// A look value a style's `when` selects: while `when` holds, `input` shows
/// the value of entry `entry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookArm {
    /// What it switches.
    pub target: Switched,
    /// The condition, in postfix order.
    pub when: Box<[When]>,
    /// The entry of the value.
    pub entry: u32,
}

/// One step of a [`LookArm`]'s condition, in postfix order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// The node is in this interaction state.
    State(Interaction),
    /// The `Bool` entry holds.
    Entry(u32),
    /// The negation of the condition before.
    Not,
    /// Both conditions before.
    And,
    /// Either condition before.
    Or,
}

/// What a style's `when` switches: a value the control reads, or one it lays
/// out by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Switched {
    Input(ControlInput),
    Layout(LayoutSlot),
}

impl ControlInput {
    /// Every input, by wire tag.
    pub const ALL: [ControlInput; 44] = [
        ControlInput::Value,
        ControlInput::Min,
        ControlInput::Max,
        ControlInput::Step,
        ControlInput::Fit,
        ControlInput::Tint,
        ControlInput::Background,
        ControlInput::Opacity,
        ControlInput::CornerRadius,
        ControlInput::Border,
        ControlInput::BackgroundTransition,
        ControlInput::OpacityTransition,
        ControlInput::Translate,
        ControlInput::TranslateTransition,
        ControlInput::Scale,
        ControlInput::ScaleTransition,
        ControlInput::Rotation,
        ControlInput::RotationTransition,
        ControlInput::TransformOrigin,
        ControlInput::CornerRadiusTransition,
        ControlInput::ColorTransition,
        ControlInput::WidthTransition,
        ControlInput::HeightTransition,
        ControlInput::Label,
        ControlInput::Hint,
        ControlInput::SpokenValue,
        ControlInput::Hidden,
        ControlInput::HeadingLevel,
        ControlInput::Visible,
        ControlInput::Focusable,
        ControlInput::Enabled,
        ControlInput::Autofocus,
        ControlInput::Clip,
        ControlInput::TextColor,
        ControlInput::SoftWrap,
        ControlInput::FontWeight,
        ControlInput::FontFamily,
        ControlInput::LineHeight,
        ControlInput::MaxLines,
        ControlInput::Overflow,
        ControlInput::TextAlign,
        ControlInput::Placeholder,
        ControlInput::Secure,
        ControlInput::Caption,
    ];

    /// Its wire tag: its place in [`ControlInput::ALL`].
    pub fn tag(self) -> u8 {
        ControlInput::ALL
            .iter()
            .position(|&input| input == self)
            .map_or(0, |at| at as u8)
    }

    /// The input of wire tag `tag`.
    pub fn from_tag(tag: u8) -> Option<ControlInput> {
        ControlInput::ALL.get(usize::from(tag)).copied()
    }
}

impl LookArm {
    /// Whether its condition reads an interaction state.
    pub fn reads_state(&self) -> bool {
        self.when.iter().any(|w| matches!(w, When::State(_)))
    }
}

/// The handler-table entries of the look a node shows: an absent entry leaves
/// the node as it was built (no fill, opaque, sharp corners, no border), and
/// a value with no transition shows at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Look {
    /// Its `background`: a color, or `None` for no fill.
    pub background: Option<u32>,
    /// Its `opacity`.
    pub opacity: Option<u32>,
    /// Its `corner_radius`: a `MixedLength`, its `%` of the shorter side of
    /// its box.
    pub corner_radius: Option<u32>,
    /// Its `border`: a `Border`, or `None` for no stroke.
    pub border: Option<u32>,
    /// Its `transition.background`: how a new background moves in.
    pub background_transition: Option<u32>,
    /// Its `transition.opacity`: how a new opacity moves in.
    pub opacity_transition: Option<u32>,
    /// Its `translate`: an `Offset`.
    pub translate: Option<u32>,
    /// Its `transition.translate`.
    pub translate_transition: Option<u32>,
    /// Its `transition.corner_radius`.
    pub corner_radius_transition: Option<u32>,
    /// Its text's `transition.color`.
    pub color_transition: Option<u32>,
    /// Its `transition.width`: how a new fixed width moves in.
    pub width_transition: Option<u32>,
    /// Its `transition.height`.
    pub height_transition: Option<u32>,
    /// Its `scale`: an `F32`.
    pub scale: Option<u32>,
    /// Its `transition.scale`.
    pub scale_transition: Option<u32>,
    /// Its `rotation`: an `Angle`.
    pub rotation: Option<u32>,
    /// Its `transition.rotation`.
    pub rotation_transition: Option<u32>,
    /// Its `transform_origin`: an `Alignment2D`.
    pub transform_origin: Option<u32>,
}

impl Control {
    /// A control of `kind` reading only defaults.
    pub fn new(kind: ControlKind) -> Control {
        Control {
            kind,
            value: None,
            min: None,
            max: None,
            step: None,
            fit: None,
            tint: None,
            placeholder: None,
            secure: None,
            caption: None,
            shader: None,
            look: Look::default(),
            arms: Box::new([]),
            node: NodeEntries::default(),
            text: TextEntries::default(),
            semantics: SemanticEntries::default(),
            layout: Box::new([]),
        }
    }

    /// Whether a `when` of its arms reads an interaction state.
    pub fn reads_state(&self) -> bool {
        self.arms.iter().any(LookArm::reads_state)
    }

    /// The entry `input` reads.
    pub fn entry(&self, input: ControlInput) -> Option<u32> {
        match input {
            ControlInput::Value => self.value,
            ControlInput::Min => self.min,
            ControlInput::Max => self.max,
            ControlInput::Step => self.step,
            ControlInput::Fit => self.fit,
            ControlInput::Tint => self.tint,
            ControlInput::Background => self.look.background,
            ControlInput::Opacity => self.look.opacity,
            ControlInput::CornerRadius => self.look.corner_radius,
            ControlInput::Border => self.look.border,
            ControlInput::BackgroundTransition => self.look.background_transition,
            ControlInput::OpacityTransition => self.look.opacity_transition,
            ControlInput::Translate => self.look.translate,
            ControlInput::TranslateTransition => self.look.translate_transition,
            ControlInput::Scale => self.look.scale,
            ControlInput::ScaleTransition => self.look.scale_transition,
            ControlInput::Rotation => self.look.rotation,
            ControlInput::RotationTransition => self.look.rotation_transition,
            ControlInput::TransformOrigin => self.look.transform_origin,
            ControlInput::CornerRadiusTransition => self.look.corner_radius_transition,
            ControlInput::ColorTransition => self.look.color_transition,
            ControlInput::WidthTransition => self.look.width_transition,
            ControlInput::HeightTransition => self.look.height_transition,
            ControlInput::Label => self.semantics.label,
            ControlInput::Hint => self.semantics.hint,
            ControlInput::SpokenValue => self.semantics.value,
            ControlInput::Hidden => self.semantics.hidden,
            ControlInput::HeadingLevel => self.semantics.heading_level,
            ControlInput::Visible => self.node.visible,
            ControlInput::Focusable => self.node.focusable,
            ControlInput::Enabled => self.node.enabled,
            ControlInput::Autofocus => self.node.autofocus,
            ControlInput::Clip => self.node.clip,
            ControlInput::TextColor => self.text.color,
            ControlInput::SoftWrap => self.text.soft_wrap,
            ControlInput::FontWeight => self.text.font_weight,
            ControlInput::FontFamily => self.text.font_family,
            ControlInput::LineHeight => self.text.line_height,
            ControlInput::MaxLines => self.text.max_lines,
            ControlInput::Overflow => self.text.overflow,
            ControlInput::TextAlign => self.text.align,
            ControlInput::Placeholder => self.placeholder,
            ControlInput::Secure => self.secure,
            ControlInput::Caption => self.caption,
        }
    }

    /// Points the layout slot `slot` at entry `entry`.
    pub fn set_layout(&mut self, slot: LayoutSlot, entry: u32) {
        let mut layout = core::mem::take(&mut self.layout).into_vec();
        match layout.binary_search_by_key(&slot, |&(s, _)| s) {
            Ok(at) => layout[at].1 = entry,
            Err(at) => layout.insert(at, (slot, entry)),
        }
        self.layout = layout.into();
    }

    /// Points `input` at entry `entry`.
    pub fn set_entry(&mut self, input: ControlInput, entry: u32) {
        let slot = match input {
            ControlInput::Value => &mut self.value,
            ControlInput::Min => &mut self.min,
            ControlInput::Max => &mut self.max,
            ControlInput::Step => &mut self.step,
            ControlInput::Fit => &mut self.fit,
            ControlInput::Tint => &mut self.tint,
            ControlInput::Background => &mut self.look.background,
            ControlInput::Opacity => &mut self.look.opacity,
            ControlInput::CornerRadius => &mut self.look.corner_radius,
            ControlInput::Border => &mut self.look.border,
            ControlInput::BackgroundTransition => &mut self.look.background_transition,
            ControlInput::OpacityTransition => &mut self.look.opacity_transition,
            ControlInput::Translate => &mut self.look.translate,
            ControlInput::TranslateTransition => &mut self.look.translate_transition,
            ControlInput::Scale => &mut self.look.scale,
            ControlInput::ScaleTransition => &mut self.look.scale_transition,
            ControlInput::Rotation => &mut self.look.rotation,
            ControlInput::RotationTransition => &mut self.look.rotation_transition,
            ControlInput::TransformOrigin => &mut self.look.transform_origin,
            ControlInput::CornerRadiusTransition => &mut self.look.corner_radius_transition,
            ControlInput::ColorTransition => &mut self.look.color_transition,
            ControlInput::WidthTransition => &mut self.look.width_transition,
            ControlInput::HeightTransition => &mut self.look.height_transition,
            ControlInput::Label => &mut self.semantics.label,
            ControlInput::Hint => &mut self.semantics.hint,
            ControlInput::SpokenValue => &mut self.semantics.value,
            ControlInput::Hidden => &mut self.semantics.hidden,
            ControlInput::HeadingLevel => &mut self.semantics.heading_level,
            ControlInput::Visible => &mut self.node.visible,
            ControlInput::Focusable => &mut self.node.focusable,
            ControlInput::Enabled => &mut self.node.enabled,
            ControlInput::Autofocus => &mut self.node.autofocus,
            ControlInput::Clip => &mut self.node.clip,
            ControlInput::TextColor => &mut self.text.color,
            ControlInput::SoftWrap => &mut self.text.soft_wrap,
            ControlInput::FontWeight => &mut self.text.font_weight,
            ControlInput::FontFamily => &mut self.text.font_family,
            ControlInput::LineHeight => &mut self.text.line_height,
            ControlInput::MaxLines => &mut self.text.max_lines,
            ControlInput::Overflow => &mut self.text.overflow,
            ControlInput::TextAlign => &mut self.text.align,
            ControlInput::Placeholder => &mut self.placeholder,
            ControlInput::Secure => &mut self.secure,
            ControlInput::Caption => &mut self.caption,
        };
        *slot = Some(entry);
    }
}

impl Encode for Control {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(self.kind as u8);
        let semantics = self.semantics;
        enc.write_u8(semantics.role.map_or(0, |role| role_tag(role) + 1));
        enc.write_u8(live_tag(semantics.live));
        let look = self.look;
        for entry in [
            self.value,
            self.min,
            self.max,
            self.step,
            self.fit,
            self.tint,
            self.placeholder,
            self.secure,
            self.caption,
            self.shader,
            look.background,
            look.opacity,
            look.corner_radius,
            look.border,
            look.background_transition,
            look.opacity_transition,
            look.translate,
            look.translate_transition,
            look.corner_radius_transition,
            look.color_transition,
            look.width_transition,
            look.height_transition,
            look.scale,
            look.scale_transition,
            look.rotation,
            look.rotation_transition,
            look.transform_origin,
            semantics.label,
            semantics.hint,
            semantics.value,
            semantics.hidden,
            semantics.heading_level,
            self.node.visible,
            self.node.focusable,
            self.node.enabled,
            self.node.autofocus,
            self.node.clip,
        ]
        .into_iter()
        .chain(self.text.all())
        {
            enc.write_varint(entry.map_or(0, |e| u64::from(e) + 1));
        }
        enc.write_varint(self.arms.len() as u64);
        for arm in self.arms.iter() {
            match arm.target {
                Switched::Input(input) => {
                    enc.write_u8(0);
                    enc.write_u8(input.tag());
                }
                Switched::Layout(slot) => {
                    enc.write_u8(1);
                    enc.write_u8(slot as u8);
                }
            }
            enc.write_varint(arm.when.len() as u64);
            for step in arm.when.iter() {
                match *step {
                    When::State(state) => {
                        enc.write_u8(0);
                        enc.write_u8(state.tag());
                    }
                    When::Entry(entry) => {
                        enc.write_u8(1);
                        enc.write_varint(u64::from(entry));
                    }
                    When::Not => enc.write_u8(2),
                    When::And => enc.write_u8(3),
                    When::Or => enc.write_u8(4),
                }
            }
            enc.write_varint(u64::from(arm.entry));
        }
        enc.write_varint(self.layout.len() as u64);
        for &(slot, entry) in self.layout.iter() {
            enc.write_u8(slot as u8);
            enc.write_varint(u64::from(entry));
        }
    }
}

impl Decode for Control {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let offset = dec.position();
        let kind = ControlKind::from_u8(dec.read_u8()?).ok_or(DecodeError::Malformed { offset })?;
        let offset = dec.position();
        let role = match dec.read_u8()? {
            0 => None,
            tag => Some(
                *Role::ALL
                    .get(usize::from(tag - 1))
                    .ok_or(DecodeError::Malformed { offset })?,
            ),
        };
        let offset = dec.position();
        let live = *LiveRegion::ALL
            .get(usize::from(dec.read_u8()?))
            .ok_or(DecodeError::Malformed { offset })?;
        let mut entry = || -> Result<Option<u32>, DecodeError> {
            let offset = dec.position();
            let raw =
                u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })?;
            Ok(raw.checked_sub(1))
        };
        let mut control = Control {
            kind,
            value: entry()?,
            min: entry()?,
            max: entry()?,
            step: entry()?,
            fit: entry()?,
            tint: entry()?,
            placeholder: entry()?,
            secure: entry()?,
            caption: entry()?,
            shader: entry()?,
            look: Look {
                background: entry()?,
                opacity: entry()?,
                corner_radius: entry()?,
                border: entry()?,
                background_transition: entry()?,
                opacity_transition: entry()?,
                translate: entry()?,
                translate_transition: entry()?,
                corner_radius_transition: entry()?,
                color_transition: entry()?,
                width_transition: entry()?,
                height_transition: entry()?,
                scale: entry()?,
                scale_transition: entry()?,
                rotation: entry()?,
                rotation_transition: entry()?,
                transform_origin: entry()?,
            },
            arms: Box::new([]),
            semantics: SemanticEntries {
                role,
                live,
                label: entry()?,
                hint: entry()?,
                value: entry()?,
                hidden: entry()?,
                heading_level: entry()?,
            },
            node: NodeEntries::default(),
            text: TextEntries::default(),
            layout: Box::new([]),
        };
        control.node = NodeEntries {
            visible: entry()?,
            focusable: entry()?,
            enabled: entry()?,
            autofocus: entry()?,
            clip: entry()?,
        };
        control.text = TextEntries {
            color: entry()?,
            soft_wrap: entry()?,
            font_weight: entry()?,
            font_family: entry()?,
            line_height: entry()?,
            max_lines: entry()?,
            overflow: entry()?,
            align: entry()?,
        };
        let index = |dec: &mut Decoder<'_>| -> Result<u32, DecodeError> {
            let offset = dec.position();
            u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })
        };
        let arms = index(dec)?;
        let mut decoded = Vec::new();
        for _ in 0..arms {
            let offset = dec.position();
            let target = match (dec.read_u8()?, dec.read_u8()?) {
                (0, tag) => ControlInput::from_tag(tag).map(Switched::Input),
                (1, tag) => LayoutSlot::from_u8(tag).map(Switched::Layout),
                _ => None,
            }
            .ok_or(DecodeError::Malformed { offset })?;
            let steps = index(dec)?;
            let mut when = Vec::new();
            for _ in 0..steps {
                let offset = dec.position();
                when.push(match dec.read_u8()? {
                    0 => {
                        let offset = dec.position();
                        When::State(
                            Interaction::from_tag(dec.read_u8()?)
                                .ok_or(DecodeError::Malformed { offset })?,
                        )
                    }
                    1 => When::Entry(index(dec)?),
                    2 => When::Not,
                    3 => When::And,
                    4 => When::Or,
                    _ => return Err(DecodeError::Malformed { offset }),
                });
            }
            decoded.push(LookArm {
                target,
                when: when.into(),
                entry: index(dec)?,
            });
        }
        control.arms = decoded.into();
        let slots = index(dec)?;
        let mut layout: Vec<(LayoutSlot, u32)> = Vec::new();
        for _ in 0..slots {
            let offset = dec.position();
            let slot =
                LayoutSlot::from_u8(dec.read_u8()?).ok_or(DecodeError::Malformed { offset })?;
            if layout.last().is_some_and(|&(last, _)| last >= slot) {
                return Err(DecodeError::Malformed { offset });
            }
            layout.push((slot, index(dec)?));
        }
        control.layout = layout.into();
        Ok(control)
    }
}

/// The wire tag of `role`: its place in [`Role::ALL`].
fn role_tag(role: Role) -> u8 {
    Role::ALL.iter().position(|r| *r == role).unwrap_or(0) as u8
}

/// The wire tag of `live`: its place in [`LiveRegion::ALL`].
fn live_tag(live: LiveRegion) -> u8 {
    LiveRegion::ALL.iter().position(|l| *l == live).unwrap_or(0) as u8
}

/// The live setting a `LiveRegion::<variant>` of the source names.
pub fn source_live(variant: &str) -> Option<LiveRegion> {
    Some(match variant {
        "off" => LiveRegion::Off,
        "polite" => LiveRegion::Polite,
        "assertive" => LiveRegion::Assertive,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use viso_ende::{Decode, Encode};

    #[test]
    fn every_kind_round_trips_through_its_discriminant() {
        for kind in ControlKind::ALL {
            assert_eq!(ControlKind::from_u8(kind as u8), Some(kind));
        }
        assert_eq!(ControlKind::from_u8(9), None);
        assert_eq!(ControlKind::of("Svg"), ControlKind::Asset);
        assert_eq!(ControlKind::of("ShaderView"), ControlKind::Shader);
        assert_eq!(
            ControlKind::Shader.input("shader"),
            Some(ControlInput::Value)
        );
        assert!(!ControlKind::Shader.responds());
        assert_eq!(ControlKind::Asset.input("fit"), Some(ControlInput::Fit));
        assert_eq!(ControlKind::Plain.input("fit"), None);
        assert!(!ControlKind::Asset.responds());
        assert_eq!(ControlKind::of("CheckBox"), ControlKind::Check);
        assert_eq!(ControlKind::of("Toggle"), ControlKind::Switch);
        assert!(ControlKind::Switch.responds());
        assert_eq!(ControlKind::of("Button"), ControlKind::Label);
        assert_eq!(ControlKind::of("Column"), ControlKind::Plain);
        assert!(!ControlKind::Label.responds());
        assert!(!ControlKind::Plain.responds());
        assert_eq!(
            ControlKind::Plain.input("opacity"),
            Some(ControlInput::Opacity)
        );
        assert_eq!(ControlKind::Plain.input("text"), None);
        assert_eq!(
            ControlKind::Label.input("transition.opacity"),
            Some(ControlInput::OpacityTransition)
        );
        assert_eq!(
            ControlKind::Label.input("transition.scale"),
            Some(ControlInput::ScaleTransition)
        );
        assert_eq!(ControlKind::Plain.input("transition.color"), None);
        assert_eq!(
            ControlKind::TextInput.input("value"),
            Some(ControlInput::Value)
        );
        assert_eq!(
            ControlKind::Select.route("selected_changed"),
            Some(EventRoute::SelectedChanged)
        );
        assert_eq!(ControlKind::Select.route("changed"), None);
        assert_eq!(
            ControlKind::Plain.input("semantics.label"),
            Some(ControlInput::Label)
        );
        assert_eq!(
            ControlKind::Plain.input("visible"),
            Some(ControlInput::Visible)
        );
        assert_eq!(
            ControlKind::Label.input("color"),
            Some(ControlInput::TextColor)
        );
        assert_eq!(ControlKind::Plain.input("color"), None);
        assert_eq!(
            ControlKind::TextInput.input("secure"),
            Some(ControlInput::Secure)
        );
        assert_eq!(
            ControlKind::Check.input("label"),
            Some(ControlInput::Caption)
        );
        assert_eq!(ControlKind::Label.input("placeholder"), None);
        assert_eq!(
            ControlKind::Plain.input("corner_radius"),
            Some(ControlInput::CornerRadius)
        );
        assert_eq!(
            ControlKind::Asset.input("border"),
            Some(ControlInput::Border)
        );
    }

    #[test]
    fn every_source_role_names_a_role_and_widgets_have_theirs() {
        let roles: Vec<Role> = SOURCE_ROLES
            .iter()
            .map(|name| source_role(name).expect(name))
            .collect();
        assert_eq!(roles, Role::ALL);
        assert_eq!(source_role("checkbox"), None);
        assert_eq!(source_name(Role::TextField), "text_field");
        assert_eq!(widget_role("Button"), Some(Role::Button));
        assert_eq!(widget_role("CheckBox"), Some(Role::CheckBox));
        assert_eq!(widget_role("Text"), Some(Role::Label));
        assert_eq!(widget_role("Column"), None);
    }

    #[test]
    fn every_layout_slot_round_trips_through_its_discriminant() {
        for slot in LayoutSlot::ALL {
            assert_eq!(LayoutSlot::from_u8(slot as u8), Some(slot));
        }
        assert_eq!(LayoutSlot::from_u8(32), None);
        assert_eq!(LayoutSlot::of("spacing"), Some(LayoutSlot::Gap));
        assert_eq!(
            LayoutSlot::of("grid.row_span"),
            Some(LayoutSlot::GridRowSpan)
        );
        assert_eq!(LayoutSlot::of("color"), None);
    }

    #[test]
    fn a_control_round_trips_and_rejects_a_bad_kind() {
        let control = Control {
            kind: ControlKind::Slider,
            value: Some(0),
            min: None,
            max: Some(7),
            step: None,
            fit: None,
            tint: Some(8),
            placeholder: Some(14),
            secure: None,
            caption: Some(15),
            shader: Some(2),
            look: Look {
                background: Some(3),
                opacity: None,
                corner_radius: Some(19),
                border: None,
                background_transition: None,
                opacity_transition: Some(4),
                translate: Some(20),
                translate_transition: None,
                corner_radius_transition: Some(21),
                color_transition: None,
                width_transition: Some(22),
                height_transition: None,
                scale: Some(23),
                scale_transition: Some(24),
                rotation: None,
                rotation_transition: None,
                transform_origin: Some(25),
            },
            arms: Box::new([LookArm {
                target: Switched::Input(ControlInput::Border),
                when: Box::new([
                    When::State(Interaction::Hovered),
                    When::Entry(5),
                    When::Not,
                    When::And,
                ]),
                entry: 6,
            }]),
            semantics: SemanticEntries {
                role: Some(Role::Slider),
                live: LiveRegion::Polite,
                label: Some(9),
                hint: Some(10),
                value: None,
                hidden: Some(11),
                heading_level: None,
            },
            node: NodeEntries {
                visible: Some(12),
                focusable: None,
                enabled: Some(19),
                autofocus: Some(20),
                clip: Some(21),
            },
            text: TextEntries {
                color: None,
                soft_wrap: Some(13),
                font_weight: Some(16),
                max_lines: Some(17),
                align: Some(18),
                ..TextEntries::default()
            },
            layout: Box::new([(LayoutSlot::Width, 14), (LayoutSlot::AbsoluteStart, 15)]),
        };
        let bytes = control.encode_to_vec();
        assert_eq!(Control::decode_from_slice(&bytes), Ok(control));
        let mut unordered = bytes.clone();
        let at = unordered.len() - 4;
        assert_eq!(unordered[at], LayoutSlot::Width as u8);
        unordered[at] = LayoutSlot::AbsoluteStart as u8;
        assert!(
            Control::decode_from_slice(&unordered).is_err(),
            "layout slots ascend"
        );
        let mut bad = bytes.clone();
        bad[0] = 9;
        assert!(Control::decode_from_slice(&bad).is_err());
        let mut bad = bytes.clone();
        bad[1] = 99;
        assert!(Control::decode_from_slice(&bad).is_err(), "no such role");
        let mut bad = bytes.clone();
        bad[2] = 3;
        assert!(
            Control::decode_from_slice(&bad).is_err(),
            "no such live setting"
        );
        let mut bad = bytes;
        let state = bad.len() - 11;
        bad[state] = 9;
        assert!(
            Control::decode_from_slice(&bad).is_err(),
            "no such interaction state"
        );
    }
}
