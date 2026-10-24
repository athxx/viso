//! The standard widget library `viso::widgets`: the layout containers, text, the
//! interactive controls, images, the shader view, focus and shortcut scopes and the
//! virtual list, with the common layout, transform, focus, semantics and
//! transition properties and the standard input events every layout node takes.

use crate::carry::MigratableState;
use crate::widget::{
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

/// A property a style may bind.
const fn look(name: &'static str, ty: &'static str) -> WidgetProperty {
    WidgetProperty::new(name, ty).styleable()
}

/// A property a style may bind that resolves a `Percent` against a basis.
const fn based_look(name: &'static str, ty: &'static str) -> WidgetProperty {
    WidgetProperty::new(name, ty).based().styleable()
}

/// Layout properties every node but `Fragment` takes.
const LAYOUT: &[WidgetProperty] = &[
    based_look("width", "Sizing"),
    based_look("height", "Sizing"),
    based_look("min_width", "MixedLength"),
    based_look("min_height", "MixedLength"),
    based_look("max_width", "Option<MixedLength>"),
    based_look("max_height", "Option<MixedLength>"),
    based_look("padding", "EdgeInsets"),
    based_look("margin", "EdgeInsets"),
    based_look("corner_radius", "MixedLength"),
    look("background", "Option<Color>"),
    look("border", "Option<Border>"),
    look("shadow", "Option<Shadow>"),
    prop("styles", "List<StyleRef<T>>"),
];

/// Transform, opacity and clip properties every node but `Fragment` takes.
const TRANSFORM: &[WidgetProperty] = &[
    look("translate", "Offset"),
    look("scale", "F32"),
    look("rotation", "Angle"),
    look("transform_origin", "Alignment2D"),
    look("opacity", "F32"),
    look("clip", "Bool"),
    look("visible", "Bool"),
    prop("sticky", "Bool"),
    prop("hero", "Option<String>"),
];

/// Focus, pointer and drag-and-drop properties every node but `Fragment`
/// takes: `drag` makes a node a drag source carrying that text as its
/// `drag_kind` (`"text"`, `"uri"`, `"files"` one path a line, or a kind the
/// app names), and `drop_accepts` a drop target taking the kinds it lists.
const FOCUS: &[WidgetProperty] = &[
    prop("focusable", "Bool"),
    prop("autofocus", "Bool"),
    prop("focus_ring", "Bool"),
    prop("access_key", "String"),
    prop("drag_axis", "Option<Axis>"),
    prop("cursor", "Cursor"),
    prop("enabled", "Bool"),
    prop("drag", "Option<String>"),
    prop("drag_kind", "String"),
    prop("drop_accepts", "List<String>"),
    prop("drop_operation", "DropOperation"),
];

/// The `semantics.*` group.
static SEMANTICS: PropertyGroup = PropertyGroup {
    prefix: "semantics",
    members: &[
        prop("role", "Role"),
        prop("label", "Option<String>").localizable(),
        prop("hint", "Option<String>").localizable(),
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
        look("translate", "Transition"),
        look("scale", "Transition"),
        look("rotation", "Transition"),
        look("opacity", "Transition"),
        look("background", "Transition"),
        look("color", "Transition"),
        look("corner_radius", "Transition"),
        look("width", "Transition"),
        look("height", "Transition"),
    ],
};

/// The groups every node but `Fragment` takes.
const GROUPS: &[&PropertyGroup] = &[&SEMANTICS, &TRANSITION];

const FLEX: &[WidgetProperty] = &[
    based_look("gap", "MixedLength"),
    look("justify", "Justify"),
    look("align", "Align"),
];

const FLEX_AXIS: &[WidgetProperty] = &[prop("axis", "Axis")];

const GRID: &[WidgetProperty] = &[
    based("columns", "List<Track>"),
    based("rows", "List<Track>"),
    based("auto_rows", "Track"),
    based_look("column_gap", "MixedLength"),
    based_look("row_gap", "MixedLength"),
    look("align_items", "GridAlign"),
    prop("areas", "List<String>"),
    based("adaptive_columns", "Option<AdaptiveColumns>"),
];

const STACK: &[WidgetProperty] = &[look("content_align", "Alignment2D")];

const SCROLL: &[WidgetProperty] = &[
    prop("axis", "Axis"),
    prop("across", "Bool"),
    prop("scroll_bars", "Bool"),
    prop("tail", "Bool"),
    prop("refreshing", "Bool"),
    prop("snap", "Snap"),
    prop("snap_inset", "MixedLength"),
    prop("page_dots", "Bool"),
];

/// Text style properties `Text` and `TextInput` share.
const TEXT_STYLE: &[WidgetProperty] = &[
    based_look("font_size", "MixedLength"),
    look("font_weight", "FontWeight"),
    look("font_style", "FontStyle"),
    look("font_family", "Option<FontFamily>"),
    based_look("line_height", "Option<MixedLength>"),
    look("color", "Color"),
    look("soft_wrap", "Bool"),
    look("max_lines", "Option<U32>"),
    look("overflow", "TextOverflow"),
    look("align", "TextAlign"),
    prop("selectable", "Bool"),
    prop("locale", "Option<Locale>"),
];

const TEXT: &[WidgetProperty] = &[prop("text", "String").localizable()];

/// A `Text`'s pieces drawn each its own way, which replace its `text`, and
/// whether its text can be selected and copied.
const SPANS: &[WidgetProperty] = &[prop("spans", "List<Span>"), prop("selectable", "Bool")];

/// A `LinkLabel`'s `url`: where its text leads.
const LINK: &[WidgetProperty] = &[prop("url", "String")];

const TEXT_INPUT: &[WidgetProperty] = &[
    two_way("value", "String"),
    prop("placeholder", "String").localizable(),
    prop("secure", "Bool"),
    prop("multiline", "Bool"),
    prop("input", "InputKind"),
    prop("max_length", "Option<U32>"),
    prop("invalid", "Bool"),
];

/// A `NumberField`'s number, its bounds, its step and the decimal places
/// it shows, as many as `step` has when it gives none.
const NUMBER_FIELD: &[WidgetProperty] = &[
    two_way("value", "F64"),
    prop("min", "Option<F64>"),
    prop("max", "Option<F64>"),
    prop("step", "F64"),
    prop("precision", "Option<U8>"),
];

const CHECK: &[WidgetProperty] = &[
    two_way("checked", "Bool"),
    prop("label", "String").localizable(),
];

/// A `Chip`'s `selected`, none for a chip that does not select, whether it
/// is `removable`, and its `label`.
const CHIP: &[WidgetProperty] = &[
    two_way("selected", "Bool"),
    prop("removable", "Bool"),
    prop("label", "String").localizable(),
];

/// A `Stepper`'s `value`, kept within `min..=max` and stepped by `step`,
/// whether a step past a bound `wrap`s round to the other, its ink `tint`
/// and the `label` it is named by.
const STEPPER: &[WidgetProperty] = &[
    two_way("value", "F64"),
    prop("min", "Option<F64>"),
    prop("max", "Option<F64>"),
    prop("step", "F64"),
    prop("wrap", "Bool"),
    look("tint", "Option<Color>"),
    prop("label", "String").localizable(),
];

/// A `Slider`'s `value` within `min..=max`, on a grid of `step` when it
/// has one; the value its fill runs `origin` from, whether it `marks` each
/// step, the `axis` it runs along and the `tint` it fills with.
const SLIDER: &[WidgetProperty] = &[
    two_way("value", "F32"),
    prop("min", "F32"),
    prop("max", "F32"),
    prop("step", "Option<F32>"),
    prop("origin", "Option<F32>"),
    prop("marks", "Bool"),
    prop("axis", "Axis"),
    look("tint", "Option<Color>"),
];

/// A `Knob`'s `value` within `min..=max`, on a grid of `step` when it has
/// one; the value its ring fills `origin` from and the `tint` it fills with.
const KNOB: &[WidgetProperty] = &[
    two_way("value", "F32"),
    prop("min", "F32"),
    prop("max", "F32"),
    prop("step", "Option<F32>"),
    prop("origin", "Option<F32>"),
    look("tint", "Option<Color>"),
];

/// A `ColorPicker`'s `value`, whether it picks an opacity (`alpha`), whether
/// its hue goes round a ring (`wheel`), and the `label` that names it.
const COLOR_PICKER: &[WidgetProperty] = &[
    two_way("value", "Color"),
    prop("alpha", "Bool"),
    prop("wheel", "Bool"),
    prop("label", "String").localizable(),
];

/// A `ComboBox`'s `value` — the chosen option's label, or with `free_text`
/// any text — its `options`, what it shows while empty and when nothing
/// matches, and the `label` that names it.
const COMBO_BOX: &[WidgetProperty] = &[
    two_way("value", "String"),
    prop("options", "List<String>"),
    prop("free_text", "Bool"),
    prop("placeholder", "String").localizable(),
    prop("empty_text", "String").localizable(),
    prop("label", "String").localizable(),
];

/// A `SearchField`'s `value` — its query — the `suggestions` it shows, how
/// long the typing rests before the query is reported (`debounce`), what it
/// shows while empty, and the `label` that names it.
const SEARCH_FIELD: &[WidgetProperty] = &[
    two_way("value", "String"),
    prop("suggestions", "List<String>"),
    prop("debounce", "Duration"),
    prop("placeholder", "String").localizable(),
    prop("label", "String").localizable(),
];

/// A `ProgressBar`'s or `ProgressRing`'s `value` within `min..=max`, none
/// while work of unknown length runs; its fill's `tint` and the `label` it is
/// named by.
const PROGRESS: &[WidgetProperty] = &[
    prop("value", "Option<F32>"),
    prop("min", "F32"),
    prop("max", "F32"),
    look("tint", "Option<Color>"),
    prop("label", "String").localizable(),
];

/// A `Badge`'s `count`, none for a dot, shown up to `max` and as `max`
/// and a plus past it; its pill's `tint` and the `label` it is named by in
/// place of its count.
const BADGE: &[WidgetProperty] = &[
    prop("count", "Option<I32>"),
    prop("max", "I32"),
    look("tint", "Option<Color>"),
    prop("label", "String").localizable(),
];

/// An `Avatar`'s person's `name`, the picture `image` shows in place of
/// their initials, and the `tint` in place of the color the name picks.
const AVATAR: &[WidgetProperty] = &[
    prop("name", "String").localizable(),
    prop("image", "Option<ImageAsset>"),
    look("tint", "Option<Color>"),
];

/// A `Spinner`'s `tint` and the `label` it is named by.
const SPINNER: &[WidgetProperty] = &[
    look("tint", "Option<Color>"),
    prop("label", "String").localizable(),
];

/// The parameters of the brush shader painting a `ShaderView`'s box.
const SHADER_VIEW: &[WidgetProperty] = &[prop("shader", "Shader")];

/// The rig a `GameViewport` sees the World through; the stage's camera
/// without one.
const GAME_VIEWPORT: &[WidgetProperty] = &[prop("camera", "Option<viso::game::kit::CameraRig>")];

/// An `Image`'s raster, from `asset("…")`, and how it fills the box.
const IMAGE: &[WidgetProperty] = &[
    prop("source", "ImageAsset"),
    look("fit", "ImageFit"),
    look("tint", "Option<Color>"),
];

/// An `Svg`'s scene, from `asset("…")`, and how it fills the box.
const SVG: &[WidgetProperty] = &[
    prop("source", "SvgAsset"),
    look("fit", "ImageFit"),
    look("tint", "Option<Color>"),
];

const SELECTED: &[WidgetProperty] = &[two_way("selected", "U32")];

/// A `SegmentedControl`'s `selected` segment and the `tint` its track,
/// thumb and hairlines are inked with.
const SEGMENTED: &[WidgetProperty] = &[two_way("selected", "U32"), look("tint", "Option<Color>")];

/// Whether a `Disclosure` shows its body.
const DISCLOSURE: &[WidgetProperty] = &[two_way("expanded", "Bool")];
const OPEN: &[WidgetProperty] = &[two_way("open", "Bool")];

/// Whether a `Sheet` shows, the detent it rests at among its `detents`
/// (none for all of its panel), the edge it slides in from, and whether a
/// drag past its least detent dismisses it.
const SHEET: &[WidgetProperty] = &[
    two_way("open", "Bool"),
    two_way("detent", "U32"),
    based("detents", "List<MixedLength>"),
    prop("edge", "SheetEdge"),
    prop("dismissible", "Bool"),
];

/// The page a `NavigationStack` shows, and how its pages trade places.
const CURRENT: &[WidgetProperty] = &[
    two_way("current", "U32"),
    look("page_transition", "PageTransition"),
];

/// The constant width an `AdaptiveScope` classifies instead of the width its
/// parent gives it, and the direction its subtree's lines run in instead of
/// its parent's.
const ADAPTIVE_SCOPE: &[WidgetProperty] = &[
    prop("basis", "Option<MixedLength>"),
    prop("layout_direction", "Option<LayoutDirection>"),
];

/// How far an `InteractiveViewer`'s content may scale and move past its
/// bounds, and the scale a double tap zooms to (none for `None`).
const VIEWER: &[WidgetProperty] = &[
    prop("min_scale", "F32"),
    prop("max_scale", "F32"),
    prop("pan_margin", "MixedLength"),
    prop("double_tap", "Option<F32>"),
];

/// Which edges of a `SwipeRow` a full swipe runs the first action of.
const SWIPE_ROW: &[WidgetProperty] = &[prop("full_swipe", "FullSwipe")];

/// The edge of its `SwipeRow` a `SwipeActions` panel waits past.
const SWIPE_ACTIONS: &[WidgetProperty] = &[prop("edge", "SwipeEdge")];

const FOCUS_SCOPE: &[WidgetProperty] = &[prop("trap", "Bool"), prop("restore_focus", "Bool")];

const KEY_SHORTCUT: &[WidgetProperty] =
    &[prop("chord", "KeyChord"), prop("scope", "ShortcutScope")];

/// The slot of the enclosing component a `SlotOutlet` places, by name.
const SLOT_OUTLET: &[WidgetProperty] = &[prop("slot", "SlotName")];

const VIRTUAL_LIST: &[WidgetProperty] = &[
    prop("axis", "Axis"),
    prop("estimated_extent", "MixedLength"),
    prop("overscan", "U32"),
    prop("across", "Bool"),
    prop("scroll_bars", "Bool"),
    prop("tail", "Bool"),
    prop("refreshing", "Bool"),
    prop("snap", "Snap"),
    prop("snap_inset", "MixedLength"),
    prop("page_dots", "Bool"),
    prop("pages", "Bool"),
    prop("columns", "U16"),
    prop("min_column", "MixedLength"),
    prop("masonry", "Bool"),
    prop("gap", "MixedLength"),
    prop("column_gap", "MixedLength"),
];

/// The `list.*` group a `VirtualList` provides to its rows.
static LIST_CHILD: PropertyGroup = PropertyGroup {
    prefix: "list",
    members: &[prop("full_line", "Bool")],
};

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
    WidgetEvent::routed("double_tap", "TapEvent"),
    WidgetEvent::routed("swipe", "SwipeEvent"),
    WidgetEvent::routed("pinch_start", "PinchEvent"),
    WidgetEvent::routed("pinch_move", "PinchEvent"),
    WidgetEvent::routed("pinch_end", "PinchEvent"),
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
    WidgetEvent::routed("link", "LinkEvent"),
    WidgetEvent::routed("drop", "DropEvent"),
    WidgetEvent::routed("drop_enter", "DropEvent"),
    WidgetEvent::routed("drop_leave", "DropEvent"),
    WidgetEvent::routed("drop_end", "DropEnd"),
];

const TOGGLE_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("changed", "ToggleChanged")];
const CHIP_EVENTS: &[WidgetEvent] = &[
    WidgetEvent::new("changed", "ToggleChanged"),
    WidgetEvent::signal("removed"),
];
const SLIDER_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("changed", "SliderChanged")];
const COLOR_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("changed", "ColorChanged")];
const TEXT_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("changed", "TextChanged")];
const NUMBER_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("changed", "NumberChanged")];
const TEXT_INPUT_EVENTS: &[WidgetEvent] = &[
    WidgetEvent::new("changed", "TextChanged"),
    WidgetEvent::signal("submitted"),
];
const SELECTED_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("selected_changed", "SelectionChanged")];
const CURRENT_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("current_changed", "SelectionChanged")];
const SHEET_EVENTS: &[WidgetEvent] = &[
    WidgetEvent::new("changed", "ToggleChanged"),
    WidgetEvent::new("detent_changed", "SelectionChanged"),
];
const SCROLL_EVENTS: &[WidgetEvent] = &[
    WidgetEvent::new("scroll_changed", "ScrollChanged"),
    WidgetEvent::signal("refresh"),
    WidgetEvent::new("snap_changed", "SelectionChanged"),
    WidgetEvent::new("reordered", "ReorderEvent"),
];
const VIRTUAL_LIST_EVENTS: &[WidgetEvent] = &[
    WidgetEvent::new("visible_range_changed", "VisibleRange"),
    WidgetEvent::signal("refresh"),
    WidgetEvent::new("snap_changed", "SelectionChanged"),
    WidgetEvent::new("reordered", "ReorderEvent"),
];
/// A flex list's rows carried elsewhere by their handles.
const REORDER_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("reordered", "ReorderEvent")];
const SWIPE_ROW_EVENTS: &[WidgetEvent] = &[WidgetEvent::new("full_swiped", "FullSwipeEvent")];
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
        .migratable(MigratableState::FOCUS.with(MigratableState::ANIMATION))
}

const ROW: WidgetNode = WidgetNode::Flex(FlexAxis::Row);
const COLUMN: WidgetNode = WidgetNode::Flex(FlexAxis::Column);
const FLEX_NODE: WidgetNode = WidgetNode::Flex(FlexAxis::Property);

/// What every user-component node takes on top of its inputs, events and
/// slots: the common properties and groups and the standard events.
pub static COMPONENT: NativeWidget =
    layout("Component", WidgetNode::Leaf, &[LAYOUT, TRANSFORM, FOCUS]);

/// The standard widgets, in declaration order.
pub const STANDARD: &[NativeWidget] = &[
    layout("Row", ROW, &[FLEX, LAYOUT, TRANSFORM, FOCUS])
        .events(&[REORDER_EVENTS, STANDARD_EVENTS])
        .slots(CHILDREN),
    layout("Column", COLUMN, &[FLEX, LAYOUT, TRANSFORM, FOCUS])
        .events(&[REORDER_EVENTS, STANDARD_EVENTS])
        .slots(CHILDREN),
    layout(
        "Flex",
        FLEX_NODE,
        &[FLEX, FLEX_AXIS, LAYOUT, TRANSFORM, FOCUS],
    )
    .slots(CHILDREN),
    layout(
        "AdaptiveScope",
        WidgetNode::AdaptiveScope,
        &[ADAPTIVE_SCOPE, FLEX, LAYOUT, TRANSFORM, FOCUS],
    )
    .slots(CHILDREN),
    layout(
        "SafeArea",
        WidgetNode::SafeArea,
        &[FLEX, LAYOUT, TRANSFORM, FOCUS],
    )
    .slots(CHILDREN),
    layout(
        "KeyboardAvoiding",
        WidgetNode::KeyboardAvoiding,
        &[FLEX, LAYOUT, TRANSFORM, FOCUS],
    )
    .slots(CHILDREN),
    layout("Grid", WidgetNode::Grid, &[GRID, LAYOUT, TRANSFORM, FOCUS])
        .provides(&GRID_CHILD)
        .slots(CHILDREN),
    layout(
        "Stack",
        WidgetNode::Stack,
        &[STACK, LAYOUT, TRANSFORM, FOCUS],
    )
    .provides(&STACK_CHILD)
    .slots(CHILDREN),
    layout(
        "Absolute",
        WidgetNode::Absolute,
        &[LAYOUT, TRANSFORM, FOCUS],
    )
    .provides(&ABSOLUTE_CHILD)
    .slots(CHILDREN),
    layout(
        "InteractiveViewer",
        WidgetNode::Stack,
        &[VIEWER, LAYOUT, TRANSFORM, FOCUS],
    )
    .slots(CONTENT)
    .interactive(),
    layout(
        "SwipeRow",
        WidgetNode::Stack,
        &[SWIPE_ROW, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[SWIPE_ROW_EVENTS, STANDARD_EVENTS])
    .slots(CHILDREN),
    layout(
        "SwipeActions",
        ROW,
        &[SWIPE_ACTIONS, FLEX, LAYOUT, TRANSFORM, FOCUS],
    )
    .slots(CHILDREN),
    layout(
        "DragPreview",
        WidgetNode::Stack,
        &[STACK, LAYOUT, TRANSFORM, FOCUS],
    )
    .provides(&STACK_CHILD)
    .slots(CHILDREN),
    layout(
        "ReorderHandle",
        WidgetNode::Stack,
        &[STACK, LAYOUT, TRANSFORM, FOCUS],
    )
    .provides(&STACK_CHILD)
    .slots(CHILDREN)
    .interactive(),
    layout(
        "Scroll",
        WidgetNode::Scroll,
        &[SCROLL, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[SCROLL_EVENTS, STANDARD_EVENTS])
    .slots(CONTENT)
    .migratable(MigratableState::SCROLL),
    NativeWidget::new("Fragment", WidgetNode::Fragment).slots(CHILDREN),
    NativeWidget::new("SlotOutlet", WidgetNode::Outlet).properties(&[SLOT_OUTLET]),
    layout(
        "Text",
        WidgetNode::Leaf,
        &[TEXT, SPANS, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout(
        "LinkLabel",
        WidgetNode::Leaf,
        &[TEXT, LINK, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout(
        "TextInput",
        WidgetNode::Leaf,
        &[TEXT_INPUT, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[TEXT_INPUT_EVENTS, STANDARD_EVENTS])
    .migratable(MigratableState::SELECTION)
    .interactive(),
    layout(
        "NumberField",
        WidgetNode::Leaf,
        &[NUMBER_FIELD, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[NUMBER_EVENTS, STANDARD_EVENTS])
    .migratable(MigratableState::SELECTION)
    .interactive(),
    layout(
        "Button",
        WidgetNode::Leaf,
        &[TEXT, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
    )
    .interactive(),
    layout(
        "Toggle",
        WidgetNode::Leaf,
        &[CHECK, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[TOGGLE_EVENTS, STANDARD_EVENTS])
    .interactive(),
    layout(
        "CheckBox",
        WidgetNode::Leaf,
        &[CHECK, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[TOGGLE_EVENTS, STANDARD_EVENTS])
    .interactive(),
    layout(
        "Chip",
        WidgetNode::Leaf,
        &[CHIP, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[CHIP_EVENTS, STANDARD_EVENTS])
    .interactive(),
    layout(
        "Stepper",
        WidgetNode::Leaf,
        &[STEPPER, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[NUMBER_EVENTS, STANDARD_EVENTS])
    .interactive(),
    layout(
        "Slider",
        WidgetNode::Leaf,
        &[SLIDER, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[SLIDER_EVENTS, STANDARD_EVENTS])
    .interactive(),
    layout("Knob", WidgetNode::Leaf, &[KNOB, LAYOUT, TRANSFORM, FOCUS])
        .events(&[SLIDER_EVENTS, STANDARD_EVENTS])
        .interactive(),
    // Their parts are the framework's: they take no children.
    layout("ComboBox", COLUMN, &[COMBO_BOX, LAYOUT, TRANSFORM, FOCUS])
        .events(&[TEXT_EVENTS, STANDARD_EVENTS])
        .interactive(),
    layout(
        "SearchField",
        COLUMN,
        &[SEARCH_FIELD, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[TEXT_INPUT_EVENTS, STANDARD_EVENTS])
    .interactive(),
    layout(
        "ColorPicker",
        COLUMN,
        &[COLOR_PICKER, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[COLOR_EVENTS, STANDARD_EVENTS])
    .interactive(),
    layout(
        "Image",
        WidgetNode::Leaf,
        &[IMAGE, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout("Svg", WidgetNode::Leaf, &[SVG, LAYOUT, TRANSFORM, FOCUS]),
    layout(
        "ProgressBar",
        WidgetNode::Leaf,
        &[PROGRESS, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout(
        "ProgressRing",
        WidgetNode::Leaf,
        &[PROGRESS, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout(
        "Spinner",
        WidgetNode::Leaf,
        &[SPINNER, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout(
        "Badge",
        WidgetNode::Leaf,
        &[BADGE, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout(
        "Avatar",
        WidgetNode::Leaf,
        &[AVATAR, TEXT_STYLE, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout(
        "ShaderView",
        WidgetNode::Leaf,
        &[SHADER_VIEW, LAYOUT, TRANSFORM, FOCUS],
    ),
    layout(
        "GameViewport",
        WidgetNode::Leaf,
        &[GAME_VIEWPORT, LAYOUT, TRANSFORM, FOCUS],
    )
    .interactive(),
    layout("Tabs", ROW, &[SELECTED, LAYOUT, TRANSFORM, FOCUS])
        .events(&[SELECTED_EVENTS, STANDARD_EVENTS])
        .slots(CHILDREN)
        .migratable(
            MigratableState::FOCUS
                .with(MigratableState::ANIMATION)
                .with(MigratableState::ACTIVE_CHILD),
        )
        .interactive(),
    layout(
        "NavigationStack",
        WidgetNode::Stack,
        &[CURRENT, STACK, LAYOUT, TRANSFORM, FOCUS],
    )
    .provides(&STACK_CHILD)
    .events(&[CURRENT_EVENTS, STANDARD_EVENTS])
    .slots(CHILDREN)
    .migratable(
        MigratableState::FOCUS
            .with(MigratableState::ANIMATION)
            .with(MigratableState::ACTIVE_CHILD),
    )
    .interactive(),
    layout(
        "Disclosure",
        COLUMN,
        &[DISCLOSURE, FLEX, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[TOGGLE_EVENTS, STANDARD_EVENTS])
    .slots(CHILDREN)
    .interactive(),
    layout(
        "Modal",
        WidgetNode::Stack,
        &[OPEN, STACK, LAYOUT, TRANSFORM, FOCUS],
    )
    .provides(&STACK_CHILD)
    .events(&[TOGGLE_EVENTS, STANDARD_EVENTS])
    .slots(CHILDREN)
    .interactive(),
    layout(
        "Sheet",
        WidgetNode::Stack,
        &[SHEET, STACK, LAYOUT, TRANSFORM, FOCUS],
    )
    .provides(&STACK_CHILD)
    .events(&[SHEET_EVENTS, STANDARD_EVENTS])
    .slots(CHILDREN)
    .interactive(),
    layout(
        "Toast",
        WidgetNode::Stack,
        &[OPEN, STACK, LAYOUT, TRANSFORM, FOCUS],
    )
    .provides(&STACK_CHILD)
    .events(&[TOGGLE_EVENTS, STANDARD_EVENTS])
    .slots(CHILDREN),
    layout("RadioGroup", COLUMN, &[SELECTED, LAYOUT, TRANSFORM, FOCUS])
        .events(&[SELECTED_EVENTS, STANDARD_EVENTS])
        .slots(CHILDREN)
        .interactive(),
    layout(
        "SegmentedControl",
        ROW,
        &[SEGMENTED, LAYOUT, TRANSFORM, FOCUS],
    )
    .events(&[SELECTED_EVENTS, STANDARD_EVENTS])
    .slots(CHILDREN)
    .interactive(),
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
    )
    .provides(&LIST_CHILD)
    .events(&[VIRTUAL_LIST_EVENTS, STANDARD_EVENTS])
    .slots(CHILDREN),
];
