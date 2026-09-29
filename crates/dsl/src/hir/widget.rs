//! The standard widget schema baseline: the properties each built-in node type
//! accepts, with the value type a binding is typed against, whether the property
//! supports `bind`, and whether it declares a percent basis.
//!
//! This is the static subset the spec's widget tables fix (layout, transform, text,
//! focus, the interactive baseline, semantics, transitions, `VirtualList`, and the
//! `grid.*`/`stack.*`/`absolute.*` properties a container provides to its children).
//! A type the table does not list is not checked here: its schema comes from native
//! declarations this layer cannot see.

use super::ty::Ty;

/// The value type a property binding is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropKind {
    Bool,
    Str,
    F32,
    U16,
    U32,
    I32,
    Color,
    Angle,
    /// `MixedLength` (any length-family value widens to it).
    Length,
    OptString,
    OptF32,
    OptU32,
    OptU16,
    OptU8,
    OptColor,
    OptLength,
    /// A native schema type (`Sizing`, `EdgeInsets`, `Offset`, an enum, ...) this
    /// layer does not model, by its spec name; the value is inferred without an
    /// expectation.
    Opaque(&'static str),
}

impl PropKind {
    /// The type's source name.
    pub(crate) fn name(self) -> &'static str {
        match self {
            PropKind::Bool => "Bool",
            PropKind::Str => "String",
            PropKind::F32 => "F32",
            PropKind::U16 => "U16",
            PropKind::U32 => "U32",
            PropKind::I32 => "I32",
            PropKind::Color => "Color",
            PropKind::Angle => "Angle",
            PropKind::Length => "MixedLength",
            PropKind::OptString => "Option<String>",
            PropKind::OptF32 => "Option<F32>",
            PropKind::OptU32 => "Option<U32>",
            PropKind::OptU16 => "Option<U16>",
            PropKind::OptU8 => "Option<U8>",
            PropKind::OptColor => "Option<Color>",
            PropKind::OptLength => "Option<MixedLength>",
            PropKind::Opaque(name) => name,
        }
    }

    /// The type a binding's value is checked against, or `None` for an opaque one.
    pub(crate) fn ty(self) -> Option<Ty> {
        let opt = |t: Ty| Some(Ty::Option(Box::new(t)));
        match self {
            PropKind::Bool => Some(Ty::Bool),
            PropKind::Str => Some(Ty::String),
            PropKind::F32 => Some(Ty::F32),
            PropKind::U16 => Some(Ty::U16),
            PropKind::U32 => Some(Ty::U32),
            PropKind::I32 => Some(Ty::I32),
            PropKind::Color => Some(Ty::Color),
            PropKind::Angle => Some(Ty::Angle),
            PropKind::Length => Some(Ty::MixedLength),
            PropKind::OptString => opt(Ty::String),
            PropKind::OptF32 => opt(Ty::F32),
            PropKind::OptU32 => opt(Ty::U32),
            PropKind::OptU16 => opt(Ty::U16),
            PropKind::OptU8 => opt(Ty::U8),
            PropKind::OptColor => opt(Ty::Color),
            PropKind::OptLength => opt(Ty::MixedLength),
            PropKind::Opaque(_) => None,
        }
    }
}

/// One property of a widget schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PropSpec {
    pub(crate) name: &'static str,
    pub(crate) kind: PropKind,
    /// Whether the property may be the left side of `bind`.
    pub(crate) two_way: bool,
    /// Whether the property resolves a `Percent` against a basis.
    pub(crate) percent_basis: bool,
}

const fn prop(name: &'static str, kind: PropKind) -> PropSpec {
    PropSpec {
        name,
        kind,
        two_way: false,
        percent_basis: false,
    }
}

const fn based(name: &'static str, kind: PropKind) -> PropSpec {
    PropSpec {
        percent_basis: true,
        ..prop(name, kind)
    }
}

const fn two_way(name: &'static str, kind: PropKind) -> PropSpec {
    PropSpec {
        two_way: true,
        ..prop(name, kind)
    }
}

use PropKind::{
    Angle, Bool, Color, F32, I32, Length, Opaque, OptColor, OptF32, OptLength, OptString, OptU8,
    OptU16, OptU32, Str, U16, U32,
};

/// Layout properties every node but `Fragment` accepts.
const LAYOUT: &[PropSpec] = &[
    based("width", Opaque("Sizing")),
    based("height", Opaque("Sizing")),
    based("min_width", Length),
    based("min_height", Length),
    based("max_width", OptLength),
    based("max_height", OptLength),
    based("padding", Opaque("EdgeInsets")),
    based("margin", Opaque("EdgeInsets")),
    based("corner_radius", Length),
    prop("background", OptColor),
    prop("border", Opaque("Option<Border>")),
    prop("styles", Opaque("List<StyleRef<T>>")),
];

/// Transform, opacity and clip properties every node accepts.
const TRANSFORM: &[PropSpec] = &[
    prop("translate", Opaque("Offset")),
    prop("scale", F32),
    prop("rotation", Angle),
    prop("transform_origin", Opaque("Alignment2D")),
    prop("opacity", F32),
    prop("clip", Bool),
    prop("visible", Bool),
];

/// Focus properties every node accepts.
const FOCUS: &[PropSpec] = &[
    prop("focusable", Bool),
    prop("autofocus", Bool),
    prop("enabled", Bool),
];

/// The members of the `semantics.*` group.
const SEMANTICS: &[PropSpec] = &[
    prop("role", Opaque("Role")),
    prop("label", OptString),
    prop("hint", OptString),
    prop("value", OptString),
    prop("live", Opaque("LiveRegion")),
    prop("hidden", Bool),
    prop("heading_level", OptU8),
];

/// The properties a `transition.*` entry may name (each takes a `Transition`).
const ANIMATABLE: &[&str] = &[
    "translate",
    "scale",
    "rotation",
    "opacity",
    "background",
    "color",
    "corner_radius",
    "width",
    "height",
];

const FLEX: &[PropSpec] = &[
    based("gap", Length),
    prop("justify", Opaque("Justify")),
    prop("align", Opaque("Align")),
];

const FLEX_AXIS: &[PropSpec] = &[prop("axis", Opaque("Axis"))];

const GRID: &[PropSpec] = &[
    based("columns", Opaque("List<Track>")),
    based("rows", Opaque("List<Track>")),
    based("auto_rows", Opaque("Track")),
    based("column_gap", Length),
    based("row_gap", Length),
    prop("align_items", Opaque("GridAlign")),
    prop("areas", Opaque("List<String>")),
    based("adaptive_columns", Opaque("Option<AdaptiveColumns>")),
];

const STACK: &[PropSpec] = &[prop("content_align", Opaque("Alignment2D"))];

const SCROLL: &[PropSpec] = &[prop("axis", Opaque("Axis"))];

/// Text style properties shared by `Text` and `TextInput`.
const TEXT_STYLE: &[PropSpec] = &[
    based("font_size", Length),
    prop("font_weight", Opaque("FontWeight")),
    prop("font_family", Opaque("Option<FontFamily>")),
    based("line_height", OptLength),
    prop("color", Color),
    prop("soft_wrap", Bool),
    prop("max_lines", OptU32),
    prop("overflow", Opaque("TextOverflow")),
    prop("align", Opaque("TextAlign")),
    prop("selectable", Bool),
    prop("locale", Opaque("Option<Locale>")),
];

const TEXT: &[PropSpec] = &[prop("text", Str)];

const TEXT_INPUT: &[PropSpec] = &[
    two_way("value", Str),
    prop("placeholder", Str),
    prop("secure", Bool),
    prop("invalid", Bool),
];

const BUTTON: &[PropSpec] = TEXT;

const CHECK: &[PropSpec] = &[two_way("checked", Bool), prop("label", Str)];

const SLIDER: &[PropSpec] = &[
    two_way("value", F32),
    prop("min", F32),
    prop("max", F32),
    prop("step", OptF32),
];

const SELECTED: &[PropSpec] = &[two_way("selected", U32)];

const FOCUS_SCOPE: &[PropSpec] = &[prop("trap", Bool), prop("restore_focus", Bool)];

const KEY_SHORTCUT: &[PropSpec] = &[
    prop("chord", Opaque("KeyChord")),
    prop("scope", Opaque("ShortcutScope")),
];

const VIRTUAL_LIST: &[PropSpec] = &[
    prop("axis", Opaque("Axis")),
    prop("estimated_extent", Length),
    prop("overscan", U32),
];

/// The properties a container provides to its direct children, written in a child's
/// body as `prefix.member`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChildProps {
    pub(crate) prefix: &'static str,
    /// The container that provides the group, for messages.
    pub(crate) container: &'static str,
    members: &'static [PropSpec],
}

/// An event a node takes: its name, the prelude record its payload is, if it has
/// one, and whether it bubbles up the ancestor route after its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EventSpec {
    pub(crate) name: &'static str,
    pub(crate) payload: Option<&'static str>,
    pub(crate) bubbles: bool,
}

const fn event(name: &'static str, payload: &'static str) -> EventSpec {
    EventSpec {
        name,
        payload: Some(payload),
        bubbles: false,
    }
}

/// An input event routed capture → target → bubble.
const fn routed(name: &'static str, payload: &'static str) -> EventSpec {
    EventSpec {
        bubbles: true,
        ..event(name, payload)
    }
}

const fn signal(name: &'static str) -> EventSpec {
    EventSpec {
        name,
        payload: None,
        bubbles: false,
    }
}

/// The standard events every layout node takes (U7.1), and the end of an animation
/// it plays (U6.3).
pub(crate) const STANDARD_EVENTS: &[EventSpec] = &[
    routed("click", "ClickEvent"),
    routed("tap", "TapEvent"),
    routed("long_press", "LongPressEvent"),
    routed("drag_start", "DragEvent"),
    routed("drag_move", "DragEvent"),
    routed("drag_end", "DragEvent"),
    routed("pointer_down", "PointerEvent"),
    routed("pointer_move", "PointerEvent"),
    routed("pointer_up", "PointerEvent"),
    routed("pointer_cancel", "PointerEvent"),
    event("hover_enter", "HoverEvent"),
    event("hover_leave", "HoverEvent"),
    routed("scroll", "ScrollEvent"),
    routed("key_down", "KeyEvent"),
    routed("key_up", "KeyEvent"),
    event("focus", "FocusEvent"),
    event("blur", "FocusEvent"),
    event("animation_end", "AnimationEnd"),
];

const TOGGLE_EVENTS: &[EventSpec] = &[event("changed", "ToggleChanged")];
const SLIDER_EVENTS: &[EventSpec] = &[event("changed", "SliderChanged")];
const TEXT_INPUT_EVENTS: &[EventSpec] = &[event("changed", "TextChanged"), signal("submitted")];
const SELECTED_EVENTS: &[EventSpec] = &[event("selected_changed", "SelectionChanged")];
const SCROLL_EVENTS: &[EventSpec] = &[event("scroll_changed", "ScrollChanged")];
const KEY_SHORTCUT_EVENTS: &[EventSpec] = &[signal("triggered")];

const GRID_CHILD: ChildProps = ChildProps {
    prefix: "grid",
    container: "Grid",
    members: &[
        prop("column", OptU16),
        prop("row", OptU16),
        prop("column_span", U16),
        prop("row_span", U16),
        prop("area", OptString),
    ],
};

const STACK_CHILD: ChildProps = ChildProps {
    prefix: "stack",
    container: "Stack",
    members: &[
        prop("align", Opaque("Option<Alignment2D>")),
        prop("layer", I32),
    ],
};

const ABSOLUTE_CHILD: ChildProps = ChildProps {
    prefix: "absolute",
    container: "Absolute",
    members: &[
        based("top", OptLength),
        based("bottom", OptLength),
        based("start", OptLength),
        based("end", OptLength),
    ],
};

/// The child property group written with `prefix`, when a container provides one.
pub(crate) fn child_props(prefix: &str) -> Option<ChildProps> {
    [GRID_CHILD, STACK_CHILD, ABSOLUTE_CHILD]
        .into_iter()
        .find(|group| group.prefix == prefix)
}

impl ChildProps {
    /// The member `name` of the group.
    pub(crate) fn member(&self, name: &str) -> Option<PropSpec> {
        self.members.iter().find(|p| p.name == name).copied()
    }

    /// The member names, for suggestions.
    pub(crate) fn names(&self) -> Vec<&'static str> {
        self.members.iter().map(|p| p.name).collect()
    }
}

/// A widget's schema: its own property tables, whether it takes the common ones
/// (every node but `Fragment` does), the group it provides to its children, its own
/// events, and whether it takes the standard ones (every layout node does).
#[derive(Debug, Clone, Copy)]
pub(crate) struct WidgetSchema {
    own: &'static [&'static [PropSpec]],
    common: bool,
    provides: Option<ChildProps>,
    events: &'static [EventSpec],
    standard_events: bool,
}

/// What a property path names on a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropLookup {
    /// A declared property.
    Known(PropSpec),
    /// No such property.
    Unknown,
}

/// Every node type name the baseline lists, in [`builtin`]'s order.
pub(crate) const BUILTIN_NAMES: &[&str] = &[
    "Row",
    "Column",
    "Flex",
    "Grid",
    "Stack",
    "Absolute",
    "Scroll",
    "Fragment",
    "Text",
    "TextInput",
    "Button",
    "Toggle",
    "CheckBox",
    "Slider",
    "Tabs",
    "RadioGroup",
    "FocusScope",
    "KeyShortcut",
    "VirtualList",
];

/// The built-in widget schema for a node type name, when the baseline lists it.
pub(crate) fn builtin(name: &str) -> Option<WidgetSchema> {
    let layout = |own, provides, events| WidgetSchema {
        own,
        common: true,
        provides,
        events,
        standard_events: true,
    };
    let schema = match name {
        "Row" | "Column" => layout(&[FLEX], None, &[]),
        "Flex" => layout(&[FLEX, FLEX_AXIS], None, &[]),
        "Grid" => layout(&[GRID], Some(GRID_CHILD), &[]),
        "Stack" => layout(&[STACK], Some(STACK_CHILD), &[]),
        "Absolute" => layout(&[], Some(ABSOLUTE_CHILD), &[]),
        "Scroll" => layout(&[SCROLL], None, SCROLL_EVENTS),
        "Fragment" => WidgetSchema {
            own: &[],
            common: false,
            provides: None,
            events: &[],
            standard_events: false,
        },
        "Text" => layout(&[TEXT, TEXT_STYLE], None, &[]),
        "TextInput" => layout(&[TEXT_INPUT, TEXT_STYLE], None, TEXT_INPUT_EVENTS),
        "Button" => layout(&[BUTTON], None, &[]),
        "Toggle" | "CheckBox" => layout(&[CHECK], None, TOGGLE_EVENTS),
        "Slider" => layout(&[SLIDER], None, SLIDER_EVENTS),
        "Tabs" | "RadioGroup" => layout(&[SELECTED], None, SELECTED_EVENTS),
        "FocusScope" => WidgetSchema {
            standard_events: false,
            ..layout(&[FOCUS_SCOPE], None, &[])
        },
        "KeyShortcut" => WidgetSchema {
            standard_events: false,
            ..layout(&[KEY_SHORTCUT], None, KEY_SHORTCUT_EVENTS)
        },
        "VirtualList" => layout(&[VIRTUAL_LIST], None, &[]),
        _ => return None,
    };
    Some(schema)
}

impl WidgetSchema {
    /// The schema of a user component: the common properties only (its inputs are
    /// looked up by the caller first).
    pub(crate) fn user_component() -> Self {
        WidgetSchema {
            own: &[],
            common: true,
            provides: None,
            events: &[],
            standard_events: true,
        }
    }

    /// The event `name` this node takes: one of its own, else a standard one.
    pub(crate) fn event(&self, name: &str) -> Option<EventSpec> {
        self.event_specs().find(|e| e.name == name)
    }

    /// The names of the events this node takes, for suggestions.
    pub(crate) fn event_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.event_specs().map(|e| e.name)
    }

    /// Every event this node takes: its own, then the standard ones.
    pub(crate) fn event_specs(&self) -> impl Iterator<Item = EventSpec> + '_ {
        let standard = if self.standard_events {
            STANDARD_EVENTS
        } else {
            &[]
        };
        self.events.iter().chain(standard).copied()
    }

    /// The group this node provides to its direct children, if any.
    pub(crate) fn provides(&self) -> Option<ChildProps> {
        self.provides
    }

    /// Whether this is `Fragment`, which forms no parent: its children take the
    /// properties of the enclosing real parent.
    pub(crate) fn is_fragment(&self) -> bool {
        !self.common
    }

    /// What the property path `segments` names on this node.
    pub(crate) fn lookup(&self, segments: &[&str]) -> PropLookup {
        match segments {
            [name] => self
                .flat()
                .find(|p| p.name == *name)
                .map_or(PropLookup::Unknown, |p| PropLookup::Known(*p)),
            [group, member] if *group == "semantics" && self.common => SEMANTICS
                .iter()
                .find(|p| p.name == *member)
                .map_or(PropLookup::Unknown, |p| PropLookup::Known(*p)),
            [group, member] if *group == "transition" && self.common => {
                if ANIMATABLE.contains(member) {
                    PropLookup::Known(prop("transition", Opaque("Transition")))
                } else {
                    PropLookup::Unknown
                }
            }
            _ => PropLookup::Unknown,
        }
    }

    /// The names a misspelled property path might have meant, for suggestions.
    pub(crate) fn names(&self, segments: &[&str]) -> Vec<&'static str> {
        match segments {
            [group, _] if *group == "semantics" => SEMANTICS.iter().map(|p| p.name).collect(),
            [group, _] if *group == "transition" => ANIMATABLE.to_vec(),
            _ => self.flat().map(|p| p.name).collect(),
        }
    }

    /// Every property this node takes, by its path: its own and the common ones,
    /// then the `semantics.*` group.
    pub(crate) fn properties(&self) -> impl Iterator<Item = (String, PropSpec)> + '_ {
        let semantics: &'static [PropSpec] = if self.common { SEMANTICS } else { &[] };
        self.flat().map(|p| (p.name.to_owned(), *p)).chain(
            semantics
                .iter()
                .map(|p| (format!("semantics.{}", p.name), *p)),
        )
    }

    fn flat(&self) -> impl Iterator<Item = &'static PropSpec> + '_ {
        let common: &'static [&'static [PropSpec]] = if self.common {
            &[LAYOUT, TRANSFORM, FOCUS]
        } else {
            &[]
        };
        self.own.iter().chain(common).flat_map(|t| t.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::{PropKind, PropLookup, builtin, child_props};

    #[test]
    fn every_listed_name_has_a_schema() {
        for name in super::BUILTIN_NAMES {
            assert!(builtin(name).is_some(), "{name}");
        }
    }

    #[test]
    fn lookup_covers_own_common_and_groups() {
        let text = builtin("Text").expect("Text is in the baseline");
        assert!(matches!(text.lookup(&["text"]), PropLookup::Known(p) if p.kind == PropKind::Str));
        let input = builtin("TextInput").expect("TextInput is in the baseline");
        assert!(
            matches!(input.lookup(&["value"]), PropLookup::Known(p) if p.kind == PropKind::Str)
        );
        assert!(matches!(text.lookup(&["width"]), PropLookup::Known(p) if p.percent_basis));
        assert!(matches!(text.lookup(&["translate"]), PropLookup::Known(p) if !p.percent_basis));
        assert!(matches!(
            text.lookup(&["semantics", "label"]),
            PropLookup::Known(_)
        ));
        assert!(matches!(
            text.lookup(&["transition", "opacity"]),
            PropLookup::Known(_)
        ));
        assert_eq!(text.lookup(&["transition", "text"]), PropLookup::Unknown);
        assert_eq!(text.lookup(&["grid", "row"]), PropLookup::Unknown);
        assert_eq!(text.lookup(&["txet"]), PropLookup::Unknown);

        let fragment = builtin("Fragment").expect("Fragment is in the baseline");
        assert_eq!(fragment.lookup(&["width"]), PropLookup::Unknown);
        assert!(builtin("Image").is_none());
    }

    #[test]
    fn containers_provide_child_groups() {
        let grid = builtin("Grid").and_then(|s| s.provides());
        assert_eq!(grid.map(|g| g.prefix), Some("grid"));
        let row = grid.and_then(|g| g.member("row"));
        assert!(matches!(row, Some(p) if p.kind == PropKind::OptU16));
        let absolute = child_props("absolute").and_then(|g| g.member("top"));
        assert!(matches!(absolute, Some(p) if p.percent_basis));
        assert!(builtin("Row").and_then(|s| s.provides()).is_none());
        assert!(child_props("flex").is_none());
    }

    #[test]
    fn events_are_own_then_standard() {
        let slider = builtin("Slider").expect("Slider is in the baseline");
        assert_eq!(
            slider.event("changed").and_then(|e| e.payload),
            Some("SliderChanged")
        );
        assert_eq!(
            slider.event("click").and_then(|e| e.payload),
            Some("ClickEvent")
        );
        let input = builtin("TextInput").expect("TextInput is in the baseline");
        assert!(matches!(input.event("submitted"), Some(e) if e.payload.is_none()));
        let shortcut = builtin("KeyShortcut").expect("KeyShortcut is in the baseline");
        assert!(shortcut.event("triggered").is_some());
        assert!(shortcut.event("click").is_none());
        assert!(
            builtin("FocusScope")
                .and_then(|s| s.event("click"))
                .is_none()
        );
        assert!(builtin("Fragment").and_then(|s| s.event("click")).is_none());
        assert!(builtin("Row").and_then(|s| s.event("changed")).is_none());
    }

    #[test]
    fn two_way_properties() {
        let slider = builtin("Slider").expect("Slider is in the baseline");
        assert!(matches!(slider.lookup(&["value"]), PropLookup::Known(p) if p.two_way));
        assert!(matches!(slider.lookup(&["min"]), PropLookup::Known(p) if !p.two_way));
    }
}
