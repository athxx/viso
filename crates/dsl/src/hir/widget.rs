//! Widget schemas as the view checker reads them: a view over the native widget
//! declarations of the package's [`Natives`] — each node type's properties
//! with the value type a binding is typed against, whether it supports `bind`
//! and whether it declares a percent basis; its property groups, the group it
//! provides to its children, its events and its slots.
//!
//! A property type the checker models (`Bool`, `String`, the numeric and length
//! types, `Color`, `Angle`, their `Option`s) types its binding; any other
//! is a native schema type: a prelude enum (`Justify`, `Role`, `Sizing`) or
//! one of the prelude records [`PRELUDE_RECORDS`] types its binding as that
//! declaration. A `transition.*` binding is inferred on its own and checked
//! to be a `Transition` (`E3703`).

use viso_behavior::native::{
    COMPONENT, NativeWidget, Natives, PropertyGroup, WidgetEvent, WidgetNode, WidgetProperty,
    WidgetSlot,
};

use super::ty::Ty;

/// One property of a widget schema.
pub(crate) type PropSpec = &'static WidgetProperty;

/// An event a node takes.
pub(crate) type EventSpec = &'static WidgetEvent;

/// The schema spelling of a property taking a shader's parameters, checked as
/// such rather than against one type.
pub(crate) const SHADER_TY: &str = "Shader";

/// The prelude records a property binding is checked as: those a literal of
/// another type does not stand for (a length stands for an `EdgeInsets`).
pub(crate) const PRELUDE_RECORDS: &[&str] = &[
    "ImageAsset",
    "SvgAsset",
    "VideoAsset",
    "EdgeInsets",
    "Border",
    "Shadow",
    "AdaptiveColumns",
    "Offset",
];

/// The type a binding of a property whose schema type is `name` is checked
/// against: a type the checker models, a prelude enum (`Justify`, `Role`,
/// `Sizing`) or one of [`PRELUDE_RECORDS`], each also in an `Option` or a
/// `List`; `None` for a schema type the checker does not model yet
/// (`Offset`, `Transition`).
pub(crate) fn schema_ty(env: &dyn super::infer::TypeEnv, name: &str) -> Option<Ty> {
    if let Some(ty) = value_ty(name) {
        return Some(ty);
    }
    let generic = |outer: &str| {
        name.strip_prefix(outer)
            .and_then(|rest| rest.strip_prefix('<'))
            .and_then(|rest| rest.strip_suffix('>'))
    };
    if let Some(inner) = generic("Option") {
        return Some(Ty::Option(Box::new(schema_ty(env, inner)?)));
    }
    if let Some(inner) = generic("List") {
        return Some(Ty::List(Box::new(schema_ty(env, inner)?)));
    }
    // A native value type, by its path.
    if name.contains("::") {
        return Some(Ty::native(viso_behavior::native::NativeId::of(name)));
    }
    let id = env.standard_type(name)?;
    let is_enum = env.enum_variants(id).is_some();
    (is_enum || PRELUDE_RECORDS.contains(&name)).then(|| Ty::named(id))
}

/// The type a binding of a property typed `name` is checked against, or `None`
/// for a schema type the checker does not model.
pub(crate) fn value_ty(name: &str) -> Option<Ty> {
    if let Some(inner) = name
        .strip_prefix("Option<")
        .and_then(|rest| rest.strip_suffix('>'))
    {
        return Some(Ty::Option(Box::new(value_ty(inner)?)));
    }
    Some(match name {
        "Bool" => Ty::Bool,
        "String" => Ty::String,
        "F32" => Ty::F32,
        "U8" => Ty::U8,
        "U16" => Ty::U16,
        "U32" => Ty::U32,
        "I32" => Ty::I32,
        "Color" => Ty::Color,
        "Angle" => Ty::Angle,
        "MixedLength" => Ty::MixedLength,
        _ => return None,
    })
}

/// The properties a container provides to its direct children, written in a child's
/// body as `prefix.member`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChildProps {
    pub(crate) prefix: &'static str,
    /// The container that provides the group, for messages.
    pub(crate) container: &'static str,
    group: &'static PropertyGroup,
}

impl ChildProps {
    /// The member `name` of the group.
    pub(crate) fn member(&self, name: &str) -> Option<PropSpec> {
        self.group.member(name)
    }

    /// The member names, for suggestions.
    pub(crate) fn names(&self) -> Vec<&'static str> {
        self.group.members.iter().map(|p| p.name).collect()
    }
}

/// The child property group written with `prefix`, when a registered container
/// provides one.
pub(crate) fn child_props(natives: &Natives, prefix: &str) -> Option<ChildProps> {
    natives.widgets().iter().find_map(|entry| {
        let group = entry.widget.provides.filter(|g| g.prefix == prefix)?;
        Some(ChildProps {
            prefix: group.prefix,
            container: entry.widget.name,
            group,
        })
    })
}

/// A node type's schema: a registered widget, or what every user-component node
/// takes on top of its inputs.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WidgetSchema {
    widget: &'static NativeWidget,
}

/// What a property path names on a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PropLookup {
    /// A declared property.
    Known(PropSpec),
    /// No such property.
    Unknown,
}

/// The schema of the widget a view names `name`, when one is registered.
pub(crate) fn widget(natives: &Natives, name: &str) -> Option<WidgetSchema> {
    natives.widget(name).map(WidgetSchema::of)
}

impl WidgetSchema {
    /// The schema of a user component: the common properties and the standard
    /// events (its inputs, events and slots are looked up by the caller first).
    pub(crate) fn user_component() -> Self {
        WidgetSchema { widget: &COMPONENT }
    }

    /// The schema of the declaration `widget`.
    pub(crate) fn of(widget: &'static NativeWidget) -> Self {
        WidgetSchema { widget }
    }

    /// The declaration.
    pub(crate) fn native(&self) -> &'static NativeWidget {
        self.widget
    }

    /// The event `name` this node takes.
    pub(crate) fn event(&self, name: &str) -> Option<EventSpec> {
        self.widget.event(name)
    }

    /// The names of the events this node takes, for suggestions.
    pub(crate) fn event_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.event_specs().map(|e| e.name)
    }

    /// Every event this node takes, table by table: its own, then a Rust
    /// widget's box's.
    pub(crate) fn event_specs(&self) -> impl Iterator<Item = EventSpec> + '_ {
        self.widget
            .own_events()
            .chain(self.widget.base().into_iter().flat_map(|b| b.own_events()))
    }

    /// The group this node provides to its direct children, if any.
    pub(crate) fn provides(&self) -> Option<ChildProps> {
        self.widget.provides.map(|group| ChildProps {
            prefix: group.prefix,
            container: self.widget.name,
            group,
        })
    }

    /// Whether this is `Fragment`, which forms no parent: its children take the
    /// properties of the enclosing real parent.
    pub(crate) fn is_fragment(&self) -> bool {
        self.widget.node == WidgetNode::Fragment
    }

    /// The slots its caller fills.
    pub(crate) fn slots(&self) -> impl Iterator<Item = &'static WidgetSlot> + '_ {
        self.widget.slots.iter()
    }

    /// What the property path `segments` names on this node.
    pub(crate) fn lookup(&self, segments: &[&str]) -> PropLookup {
        let found = match segments {
            [name] => self.widget.property(name),
            [group, member] => self.widget.group(group).and_then(|g| g.member(member)),
            _ => None,
        };
        found.map_or(PropLookup::Unknown, PropLookup::Known)
    }

    /// The names a misspelled property path might have meant, for suggestions.
    pub(crate) fn names(&self, segments: &[&str]) -> Vec<&'static str> {
        match segments {
            [group, _] => self
                .widget
                .group(group)
                .map(|g| g.members.iter().map(|p| p.name).collect())
                .unwrap_or_default(),
            _ => self.flat().map(|p| p.name).collect(),
        }
    }

    /// Every property this node takes, by its path: its own tables in order,
    /// then each group's members.
    pub(crate) fn properties(&self) -> impl Iterator<Item = (String, PropSpec)> + '_ {
        self.flat()
            .map(|p| (p.name.to_owned(), p))
            .chain(self.groups().flat_map(|g| {
                g.members
                    .iter()
                    .map(move |p| (format!("{}.{}", g.prefix, p.name), p))
            }))
    }

    /// Its properties' tables flattened: its own, then a Rust widget's box's.
    fn flat(&self) -> impl Iterator<Item = PropSpec> + '_ {
        self.widget.own_properties().chain(
            self.widget
                .base()
                .into_iter()
                .flat_map(|b| b.own_properties()),
        )
    }

    /// Its groups: its own, then a Rust widget's box's.
    fn groups(&self) -> impl Iterator<Item = &'static PropertyGroup> + '_ {
        self.widget
            .groups
            .iter()
            .chain(self.widget.base().into_iter().flat_map(|b| b.groups.iter()))
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use viso_behavior::native::Natives;

    use super::{PropLookup, child_props, value_ty, widget};
    use crate::hir::ty::Ty;

    fn builtin(name: &str) -> Option<super::WidgetSchema> {
        widget(&Natives::standard(), name)
    }

    fn ty(lookup: PropLookup) -> Option<Ty> {
        match lookup {
            PropLookup::Known(p) => value_ty(p.ty),
            PropLookup::Unknown => None,
        }
    }

    #[test]
    fn lookup_covers_own_common_and_groups() {
        let text = builtin("Text").expect("Text is registered");
        assert_eq!(ty(text.lookup(&["text"])), Some(Ty::String));
        let input = builtin("TextInput").expect("TextInput is registered");
        assert_eq!(ty(input.lookup(&["value"])), Some(Ty::String));
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

        let fragment = builtin("Fragment").expect("Fragment is registered");
        assert_eq!(fragment.lookup(&["width"]), PropLookup::Unknown);
        let image = builtin("Image").expect("Image is registered");
        assert!(matches!(image.lookup(&["source"]), PropLookup::Known(_)));
        assert!(builtin("Picture").is_none());
    }

    #[test]
    fn containers_provide_child_groups() {
        let natives = Natives::standard();
        let grid = builtin("Grid").and_then(|s| s.provides());
        assert_eq!(grid.map(|g| g.prefix), Some("grid"));
        let row = grid.and_then(|g| g.member("row"));
        assert_eq!(
            row.and_then(|p| value_ty(p.ty)),
            Some(Ty::Option(Box::new(Ty::U16)))
        );
        let absolute = child_props(&natives, "absolute").and_then(|g| g.member("top"));
        assert!(matches!(absolute, Some(p) if p.percent_basis));
        assert!(builtin("Row").and_then(|s| s.provides()).is_none());
        assert!(child_props(&natives, "flex").is_none());
    }

    #[test]
    fn events_are_own_then_standard() {
        let slider = builtin("Slider").expect("Slider is registered");
        assert_eq!(
            slider.event("changed").and_then(|e| e.payload),
            Some("SliderChanged")
        );
        assert_eq!(
            slider.event("click").and_then(|e| e.payload),
            Some("ClickEvent")
        );
        let input = builtin("TextInput").expect("TextInput is registered");
        assert!(matches!(input.event("submitted"), Some(e) if e.payload.is_none()));
        let shortcut = builtin("KeyShortcut").expect("KeyShortcut is registered");
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
    fn two_way_properties_and_slots() {
        let slider = builtin("Slider").expect("Slider is registered");
        assert!(matches!(slider.lookup(&["value"]), PropLookup::Known(p) if p.two_way));
        assert!(matches!(slider.lookup(&["min"]), PropLookup::Known(p) if !p.two_way));
        assert!(slider.native().default_slot().is_none());
        let column = builtin("Column").expect("Column is registered");
        assert_eq!(
            column.native().default_slot().map(|s| s.name),
            Some("children")
        );
    }

    #[test]
    fn modelled_types_parse_and_others_stay_opaque() {
        assert_eq!(
            value_ty("Option<MixedLength>"),
            Some(Ty::Option(Box::new(Ty::MixedLength)))
        );
        assert_eq!(value_ty("Sizing"), None);
        assert_eq!(value_ty("Option<Border>"), None);
    }
}
