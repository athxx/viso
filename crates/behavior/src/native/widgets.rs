//! The standard widget library `viso::widgets`: the layout containers, text, the
//! interactive controls, focus and shortcut scopes and the virtual list, with
//! the common layout, transform, focus, semantics and transition properties and
//! the standard input events every layout node takes.

use super::NativeLibrary;
use super::widget::{
    FlexAxis, NativeWidget, PropertyGroup, SlotCardinality, WidgetEvent, WidgetNode,
    WidgetProperty, WidgetSlot,
};

const fn prop(name: &'static str, ty: &'static str) -> WidgetProperty {
    WidgetProperty::new(name, ty)
}

const fn based(name: &'static str, ty: &'static str) -> WidgetProperty {
    WidgetProperty::new(name, ty).based()
}

const fn two_way(name: &'static str, ty: &'static str) -> WidgetProperty {
    WidgetProperty::new(name, ty).two_way()
}

/// Layout properties every node but `Fragment` takes.
const LAYOUT: &[WidgetProperty] = &[
    based("width", "Sizing"),
    based("height", "Sizing"),
    based("min_width", "MixedLength"),
    based("min_height", "MixedLength"),
    based("max_width", "Option<MixedLength>"),
    based("max_height", "Option<MixedLength>"),
    based("padding", "EdgeInsets"),
    based("margin", "EdgeInsets"),
    based("corner_radius", "MixedLength"),
    prop("background", "Option<Color>"),
    prop("border", "Option<Border>"),
    prop("styles", "List<StyleRef<T>>"),
];

/// Transform, opacity and clip properties every node but `Fragment` takes.
const TRANSFORM: &[WidgetProperty] = &[
    prop("translate", "Offset"),
    prop("scale", "F32"),
    prop("rotation", "Angle"),
    prop("transform_origin", "Alignment2D"),
    prop("opacity", "F32"),
    prop("clip", "Bool"),
    prop("visible", "Bool"),
];

/// Focus properties every node but `Fragment` takes.
const FOCUS: &[WidgetProperty] = &[
    prop("focusable", "Bool"),
    prop("autofocus", "Bool"),
    prop("enabled", "Bool"),
];

/// The `semantics.*` group.
static SEMANTICS: PropertyGroup = PropertyGroup {
    prefix: "semantics",
    members: &[
        prop("role", "Role"),
        prop("label", "Option<String>"),
        prop("hint", "Option<String>"),
        prop("value", "Option<String>"),
        prop("live", "LiveRegion"),
        prop("hidden", "Bool"),
        prop("heading_level", "Option<U8>"),
    ],
};

/// The `transition.*` group: one entry per animatable property.
static TRANSITION: PropertyGroup = PropertyGroup {
    prefix: "transition",
    members: &[
        prop("translate", "Transition"),
        prop("scale", "Transition"),
        prop("rotation", "Transition"),
        prop("opacity", "Transition"),
        prop("background", "Transition"),
        prop("color", "Transition"),
        prop("corner_radius", "Transition"),
        prop("width", "Transition"),
        prop("height", "Transition"),
    ],
};

/// The groups every node but `Fragment` takes.
const GROUPS: &[&PropertyGroup] = &[&SEMANTICS, &TRANSITION];

const FLEX: &[WidgetProperty] = &[
    based("gap", "MixedLength"),
    prop("justify", "Justify"),
    prop("align", "Align"),
];

const FLEX_AXIS: &[WidgetProperty] = &[prop("axis", "Axis")];

const GRID: &[WidgetProperty] = &[
    based("columns", "List<Track>"),
    based("rows", "List<Track>"),
    based("auto_rows", "Track"),
    based("column_gap", "MixedLength"),
    based("row_gap", "MixedLength"),
    prop("align_items", "GridAlign"),
    prop("areas", "List<String>"),
    based("adaptive_columns", "Option<AdaptiveColumns>"),
];

const STACK: &[WidgetProperty] = &[prop("content_align", "Alignment2D")];

const SCROLL: &[WidgetProperty] = &[prop("axis", "Axis")];

/// Text style properties `Text` and `TextInput` share.
const TEXT_STYLE: &[WidgetProperty] = &[
    based("font_size", "MixedLength"),
    prop("font_weight", "FontWeight"),
    prop("font_family", "Option<FontFamily>"),
    based("line_height", "Option<MixedLength>"),
    prop("color", "Color"),
    prop("soft_wrap", "Bool"),
    prop("max_lines", "Option<U32>"),
    prop("overflow", "TextOverflow"),
    prop("align", "TextAlign"),
    prop("selectable", "Bool"),
    prop("locale", "Option<Locale>"),
];

const TEXT: &[WidgetProperty] = &[prop("text", "String")];

const TEXT_INPUT: &[WidgetProperty] = &[
    two_way("value", "String"),
    prop("placeholder", "String"),
    prop("secure", "Bool"),
    prop("invalid", "Bool"),
];

const CHECK: &[WidgetProperty] = &[two_way("checked", "Bool"), prop("label", "String")];

const SLIDER: &[WidgetProperty] = &[
    two_way("value", "F32"),
    prop("min", "F32"),
    prop("max", "F32"),
    prop("step", "Option<F32>"),
];

const SELECTED: &[WidgetProperty] = &[two_way("selected", "U32")];

const FOCUS_SCOPE: &[WidgetProperty] = &[prop("trap", "Bool"), prop("restore_focus", "Bool")];

const KEY_SHORTCUT: &[WidgetProperty] =
    &[prop("chord", "KeyChord"), prop("scope", "ShortcutScope")];

const VIRTUAL_LIST: &[WidgetProperty] = &[
    prop("axis", "Axis"),
    prop("estimated_extent", "MixedLength"),
    prop("overscan", "U32"),
];

/// The `grid.*` group a `Grid` provides to its children.
static GRID_CHILD: PropertyGroup = PropertyGroup {
    prefix: "grid",
    members: &[
        prop("column", "Option<U16>"),
        prop("row", "Option<U16>"),
        prop("column_span", "U16"),
        prop("row_span", "U16"),
        prop("area", "Option<String>"),
    ],
};

/// The `stack.*` group a `Stack` provides to its children.
static STACK_CHILD: PropertyGroup = PropertyGroup {
    prefix: "stack",
    members: &[prop("align", "Option<Alignment2D>"), prop("layer", "I32")],
};

/// The `absolute.*` group an `Absolute` provides to its children.
static ABSOLUTE_CHILD: PropertyGroup = PropertyGroup {
    prefix: "absolute",
    members: &[
        based("top", "Option<MixedLength>"),
        based("bottom", "Option<MixedLength>"),
        based("start", "Option<MixedLength>"),
        based("end", "Option<MixedLength>"),
    ],
};

/// The input events every layout node takes, and the end of an animation it
/// plays.
const STANDARD_EVENTS: &[WidgetEvent] = &[
    WidgetEvent::routed("click", "ClickEvent"),
    WidgetEvent::routed("tap", "TapEvent"),
    WidgetEvent::routed("long_press", "LongPressEvent"),
    WidgetEvent::routed("drag_start", "DragEvent"),
    WidgetEvent::routed("drag_move", "DragEvent"),
    WidgetEvent::routed("drag_end", "DragEvent"),
    WidgetEvent::routed("pointer_down", "PointerEvent"),
    WidgetEvent::routed("pointer_move", "PointerEvent"),
    WidgetEvent::routed("pointer_up", "PointerEvent"),
    WidgetEvent::routed("pointer_cancel", "PointerEvent"),
    WidgetEvent::new("hover_enter", "HoverEvent"),
    WidgetEvent::new("hover_leave", "HoverEvent"),
    WidgetEvent::routed("scroll", "ScrollEvent"),
    WidgetEvent::routed("key_down", "KeyEvent"),
    WidgetEvent::routed("key_up", "KeyEvent"),
    WidgetEvent::new("focus", "FocusEvent"),
    WidgetEvent::new("blur", "FocusEvent"),
    WidgetEvent::new("animation_end", "AnimationEnd"),
];

const TOGGLE_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("changed", "ToggleChanged")];
const SLIDER_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("changed", "SliderChanged")];
const TEXT_INPUT_EVENTS: &[WidgetEvent] = &[
    WidgetEvent::new("changed", "TextChanged"),
    WidgetEvent::signal("submitted"),
];
const SELECTED_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("selected_changed", "SelectionChanged")];
const SCROLL_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("scroll_changed", "ScrollChanged")];
const KEY_SHORTCUT_EVENTS: &[WidgetEvent] = &[WidgetEvent::signal("triggered")];

/// Any number of children, in the default slot `children`.
const CHILDREN: &[WidgetSlot] = &[WidgetSlot::default("children", SlotCardinality::Many)];

/// At most one child, in the default slot `content`.
const CONTENT: &[WidgetSlot] = &[WidgetSlot::default("content", SlotCardinality::Optional)];

/// A layout node with `own` properties, the common ones and the standard
/// events.
const fn layout(
    name: &'static str,
    node: WidgetNode,
    properties: &'static [&'static [WidgetProperty]],
) -> NativeWidget {
    NativeWidget::new(name, node)
        .properties(properties)
        .groups(GROUPS)
        .events(&[STANDARD_EVENTS])
}

const ROW: WidgetNode = WidgetNode::Flex(FlexAxis::Row);
const COLUMN: WidgetNode = WidgetNode::Flex(FlexAxis::Column);
const FLEX_NODE: WidgetNode = WidgetNode::Flex(FlexAxis::Property);

/// What every user-component node takes on top of its inputs, events and
/// slots: the common properties and groups and the standard events.
pub static COMPONENT: NativeWidget =
    layout("Component", WidgetNode::Leaf, &[LAYOUT, TRANSFORM, FOCUS]);

/// The standard widget library.
pub(super) static WIDGETS: NativeLibrary = NativeLibrary {
    path: "viso::widgets",
    version: 1,
    functions: &[],
    types: &[],
    widgets: &[
        layout("Row", ROW, &[FLEX, LAYOUT, TRANSFORM, FOCUS]).slots(CHILDREN),
        layout("Column", COLUMN, &[FLEX, LAYOUT, TRANSFORM, FOCUS]).slots(CHILDREN),
        layout(
            "Flex",
            FLEX_NODE,
            &[FLEX, FLEX_AXIS, LAYOUT, TRANSFORM, FOCUS],
        )
        .slots(CHILDREN),
        layout("Grid", WidgetNode::Grid, &[GRID, LAYOUT, TRANSFORM, FOCUS])
            .provides(&GRID_CHILD)
            .slots(CHILDREN),
        layout("Stack", FLEX_NODE, &[STACK, LAYOUT, TRANSFORM, FOCUS])
            .provides(&STACK_CHILD)
            .slots(CHILDREN),
        layout("Absolute", FLEX_NODE, &[LAYOUT, TRANSFORM, FOCUS])
            .provides(&ABSOLUTE_CHILD)
            .slots(CHILDREN),
        layout(
            "Scroll",
            WidgetNode::Scroll,
            &[SCROLL, LAYOUT, TRANSFORM, FOCUS],
        )
        .events(&[SCROLL_EVENTS, STANDARD_EVENTS])
        .slots(CONTENT),
        NativeWidget::new("Fragment", WidgetNode::Fragment).slots(CHILDREN),
        layout(
            "Text",
            WidgetNode::Leaf,
            &[TEXT, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
        ),
        layout(
            "TextInput",
            WidgetNode::Leaf,
            &[TEXT_INPUT, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
        )
        .events(&[TEXT_INPUT_EVENTS, STANDARD_EVENTS]),
        layout(
            "Button",
            WidgetNode::Leaf,
            &[TEXT, LAYOUT, TRANSFORM, FOCUS],
        ),
        layout(
            "Toggle",
            WidgetNode::Leaf,
            &[CHECK, LAYOUT, TRANSFORM, FOCUS],
        )
        .events(&[TOGGLE_EVENTS, STANDARD_EVENTS]),
        layout(
            "CheckBox",
            WidgetNode::Leaf,
            &[CHECK, LAYOUT, TRANSFORM, FOCUS],
        )
        .events(&[TOGGLE_EVENTS, STANDARD_EVENTS]),
        layout(
            "Slider",
            WidgetNode::Leaf,
            &[SLIDER, LAYOUT, TRANSFORM, FOCUS],
        )
        .events(&[SLIDER_EVENTS, STANDARD_EVENTS]),
        layout("Tabs", ROW, &[SELECTED, LAYOUT, TRANSFORM, FOCUS])
            .events(&[SELECTED_EVENTS, STANDARD_EVENTS])
            .slots(CHILDREN),
        layout("RadioGroup", COLUMN, &[SELECTED, LAYOUT, TRANSFORM, FOCUS])
            .events(&[SELECTED_EVENTS, STANDARD_EVENTS])
            .slots(CHILDREN),
        layout(
            "FocusScope",
            COLUMN,
            &[FOCUS_SCOPE, LAYOUT, TRANSFORM, FOCUS],
        )
        .events(&[])
        .slots(CHILDREN),
        layout(
            "KeyShortcut",
            WidgetNode::Leaf,
            &[KEY_SHORTCUT, LAYOUT, TRANSFORM, FOCUS],
        )
        .events(&[KEY_SHORTCUT_EVENTS]),
        layout(
            "VirtualList",
            WidgetNode::VirtualList,
            &[VIRTUAL_LIST, LAYOUT, TRANSFORM, FOCUS],
        ),
    ],
};
