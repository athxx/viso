//! The standard widget schema baseline: the properties each built-in node type
//! accepts, with the value type a binding is typed against, whether the property
//! supports `bind`, and whether it declares a percent basis.
//!
//! This is the static subset the spec's widget tables fix (layout, transform, text,
//! focus, the interactive baseline, semantics, transitions, `VirtualList`). A type
//! the table does not list is not checked here: its schema comes from native
//! declarations this layer cannot see.

use super::ty::Ty;

/// The value type a property binding is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropKind {
    Bool,
    Str,
    F32,
    U32,
    Color,
    Angle,
    /// `MixedLength` (any length-family value widens to it).
    Length,
    OptString,
    OptF32,
    OptU32,
    OptU8,
    OptColor,
    OptLength,
    /// A native schema type (`Sizing`, `EdgeInsets`, `Offset`, an enum, ...) this
    /// layer does not model; the value is inferred without an expectation.
    Opaque,
}

impl PropKind {
    /// The type a binding's value is checked against, or `None` for an opaque one.
    pub(crate) fn ty(self) -> Option<Ty> {
        let opt = |t: Ty| Some(Ty::Option(Box::new(t)));
        match self {
            PropKind::Bool => Some(Ty::Bool),
            PropKind::Str => Some(Ty::String),
            PropKind::F32 => Some(Ty::F32),
            PropKind::U32 => Some(Ty::U32),
            PropKind::Color => Some(Ty::Color),
            PropKind::Angle => Some(Ty::Angle),
            PropKind::Length => Some(Ty::MixedLength),
            PropKind::OptString => opt(Ty::String),
            PropKind::OptF32 => opt(Ty::F32),
            PropKind::OptU32 => opt(Ty::U32),
            PropKind::OptU8 => opt(Ty::U8),
            PropKind::OptColor => opt(Ty::Color),
            PropKind::OptLength => opt(Ty::MixedLength),
            PropKind::Opaque => None,
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
    Angle, Bool, Color, F32, Length, Opaque, OptColor, OptF32, OptLength, OptString, OptU8, OptU32,
    Str, U32,
};

/// Layout properties every node but `Fragment` accepts.
const LAYOUT: &[PropSpec] = &[
    based("width", Opaque),
    based("height", Opaque),
    based("min_width", Length),
    based("min_height", Length),
    based("max_width", OptLength),
    based("max_height", OptLength),
    based("padding", Opaque),
    based("margin", Opaque),
    based("corner_radius", Length),
    prop("background", OptColor),
    prop("border", Opaque),
    prop("styles", Opaque),
];

/// Transform, opacity and clip properties every node accepts.
const TRANSFORM: &[PropSpec] = &[
    prop("translate", Opaque),
    prop("scale", F32),
    prop("rotation", Angle),
    prop("transform_origin", Opaque),
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
    prop("role", Opaque),
    prop("label", OptString),
    prop("hint", OptString),
    prop("value", OptString),
    prop("live", Opaque),
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
    prop("justify", Opaque),
    prop("align", Opaque),
];

const FLEX_AXIS: &[PropSpec] = &[prop("axis", Opaque)];

const GRID: &[PropSpec] = &[
    based("columns", Opaque),
    based("rows", Opaque),
    based("auto_rows", Opaque),
    based("column_gap", Length),
    based("row_gap", Length),
    prop("align_items", Opaque),
    prop("areas", Opaque),
    based("adaptive_columns", Opaque),
];

const STACK: &[PropSpec] = &[prop("content_align", Opaque)];

const SCROLL: &[PropSpec] = &[prop("axis", Opaque)];

/// Text style properties shared by `Text` and `TextInput`.
const TEXT_STYLE: &[PropSpec] = &[
    based("font_size", Length),
    prop("font_weight", Opaque),
    prop("font_family", Opaque),
    based("line_height", OptLength),
    prop("color", Color),
    prop("soft_wrap", Bool),
    prop("max_lines", OptU32),
    prop("overflow", Opaque),
    prop("align", Opaque),
    prop("selectable", Bool),
    prop("locale", Opaque),
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

const KEY_SHORTCUT: &[PropSpec] = &[prop("chord", Opaque), prop("scope", Opaque)];

const VIRTUAL_LIST: &[PropSpec] = &[
    prop("axis", Opaque),
    prop("estimated_extent", Length),
    prop("overscan", U32),
];

/// The prefixes of properties a parent provides to its children (checked against
/// the parent, not the node itself).
const PARENT_PREFIXES: &[&str] = &["grid", "stack", "absolute"];

/// A widget's schema: its own property tables, and whether it takes the common
/// ones (every node but `Fragment` does).
#[derive(Debug, Clone, Copy)]
pub(crate) struct WidgetSchema {
    own: &'static [&'static [PropSpec]],
    common: bool,
}

/// What a property path names on a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropLookup {
    /// A declared property.
    Known(PropSpec),
    /// A property this schema does not check (a parent-provided prefix).
    Unchecked,
    /// No such property.
    Unknown,
}

/// The built-in widget schema for a node type name, when the baseline lists it.
pub(crate) fn builtin(name: &str) -> Option<WidgetSchema> {
    let own: &'static [&'static [PropSpec]] = match name {
        "Row" | "Column" => &[FLEX],
        "Flex" => &[FLEX, FLEX_AXIS],
        "Grid" => &[GRID],
        "Stack" => &[STACK],
        "Absolute" => &[],
        "Scroll" => &[SCROLL],
        "Fragment" => {
            return Some(WidgetSchema {
                own: &[],
                common: false,
            });
        }
        "Text" => &[TEXT, TEXT_STYLE],
        "TextInput" => &[TEXT_INPUT, TEXT_STYLE],
        "Button" => &[BUTTON],
        "Toggle" | "CheckBox" => &[CHECK],
        "Slider" => &[SLIDER],
        "Tabs" | "RadioGroup" => &[SELECTED],
        "FocusScope" => &[FOCUS_SCOPE],
        "KeyShortcut" => &[KEY_SHORTCUT],
        "VirtualList" => &[VIRTUAL_LIST],
        _ => return None,
    };
    Some(WidgetSchema { own, common: true })
}

impl WidgetSchema {
    /// The schema of a user component: the common properties only (its inputs are
    /// looked up by the caller first).
    pub(crate) fn user_component() -> Self {
        WidgetSchema {
            own: &[],
            common: true,
        }
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
                    PropLookup::Known(prop("transition", Opaque))
                } else {
                    PropLookup::Unknown
                }
            }
            [prefix, ..] if PARENT_PREFIXES.contains(prefix) => PropLookup::Unchecked,
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
    use super::{PropKind, PropLookup, builtin};

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
        assert_eq!(text.lookup(&["grid", "row"]), PropLookup::Unchecked);
        assert_eq!(text.lookup(&["txet"]), PropLookup::Unknown);

        let fragment = builtin("Fragment").expect("Fragment is in the baseline");
        assert_eq!(fragment.lookup(&["width"]), PropLookup::Unknown);
        assert!(builtin("Image").is_none());
    }

    #[test]
    fn two_way_properties() {
        let slider = builtin("Slider").expect("Slider is in the baseline");
        assert!(matches!(slider.lookup(&["value"]), PropLookup::Known(p) if p.two_way));
        assert!(matches!(slider.lookup(&["min"]), PropLookup::Known(p) if !p.two_way));
    }
}
