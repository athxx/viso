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
    /// `Toggle` and `CheckBox`: a release or Space/Enter flips `checked`.
    Toggle = 0,
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
}

impl ControlKind {
    /// Every kind, by discriminant.
    pub const ALL: [ControlKind; 8] = [
        ControlKind::Toggle,
        ControlKind::Slider,
        ControlKind::Select,
        ControlKind::TextInput,
        ControlKind::Label,
        ControlKind::Plain,
        ControlKind::Asset,
        ControlKind::Shader,
    ];

    /// The kind native widget `widget` is.
    pub fn of(widget: &str) -> ControlKind {
        match widget {
            "Toggle" | "CheckBox" => ControlKind::Toggle,
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
                ControlKind::Toggle | ControlKind::Slider | ControlKind::TextInput,
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
            (_, "transition.background") => ControlInput::BackgroundTransition,
            (_, "transition.opacity") => ControlInput::OpacityTransition,
            (_, "semantics.label") => ControlInput::Label,
            (_, "semantics.hint") => ControlInput::Hint,
            (_, "semantics.value") => ControlInput::SpokenValue,
            (_, "semantics.hidden") => ControlInput::Hidden,
            (_, "semantics.heading_level") => ControlInput::HeadingLevel,
            (_, "visible") => ControlInput::Visible,
            (_, "focusable") => ControlInput::Focusable,
            (ControlKind::Label | ControlKind::TextInput, "color") => ControlInput::TextColor,
            (ControlKind::Label | ControlKind::TextInput, "soft_wrap") => ControlInput::SoftWrap,
            (ControlKind::Toggle, "checked")
            | (ControlKind::Slider, "value")
            | (ControlKind::Select, "selected")
            | (ControlKind::TextInput, "value")
            | (ControlKind::Label, "text")
            | (ControlKind::Asset, "source")
            | (ControlKind::Shader, "shader") => ControlInput::Value,
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
            ControlKind::Toggle => "Toggle",
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
        _ => return None,
    })
}

/// The `Role::<variant>` of the source naming `role`.
pub fn source_name(role: Role) -> &'static str {
    SOURCE_ROLES[usize::from(role_tag(role))]
}

/// Every variant [`source_role`] takes, in the order of [`Role::ALL`].
pub const SOURCE_ROLES: [&str; 19] = [
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
];

/// A value a control reads from the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// The node's `transition.background`.
    BackgroundTransition,
    /// The node's `transition.opacity`.
    OpacityTransition,
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
    /// A text's `color`.
    TextColor,
    /// A text's `soft_wrap`.
    SoftWrap,
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
}

/// The handler-table entries of whether a node shows and takes focus: an
/// absent entry leaves it as it was built (shown, and focusable when its kind
/// is).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NodeEntries {
    /// The entry of its `visible`.
    pub visible: Option<u32>,
    /// The entry of its `focusable`.
    pub focusable: Option<u32>,
}

/// The handler-table entries of the style of a text a node shows: an absent
/// entry reads the default (the view's text color, no wrapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextEntries {
    /// The entry of its `color`.
    pub color: Option<u32>,
    /// The entry of its `soft_wrap`.
    pub soft_wrap: Option<u32>,
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
    /// [`ControlInput::Background`] or [`ControlInput::Opacity`].
    pub input: ControlInput,
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

impl LookArm {
    /// Whether its condition reads an interaction state.
    pub fn reads_state(&self) -> bool {
        self.when.iter().any(|w| matches!(w, When::State(_)))
    }
}

/// The handler-table entries of the look a node shows: an absent entry leaves
/// the node as it was built (no fill, opaque), and a value with no transition
/// shows at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Look {
    /// Its `background`: a color, or `None` for no fill.
    pub background: Option<u32>,
    /// Its `opacity`.
    pub opacity: Option<u32>,
    /// Its `transition.background`: how a new background moves in.
    pub background_transition: Option<u32>,
    /// Its `transition.opacity`: how a new opacity moves in.
    pub opacity_transition: Option<u32>,
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
            shader: None,
            look: Look::default(),
            arms: Box::new([]),
            node: NodeEntries::default(),
            text: TextEntries::default(),
            semantics: SemanticEntries::default(),
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
            ControlInput::BackgroundTransition => self.look.background_transition,
            ControlInput::OpacityTransition => self.look.opacity_transition,
            ControlInput::Label => self.semantics.label,
            ControlInput::Hint => self.semantics.hint,
            ControlInput::SpokenValue => self.semantics.value,
            ControlInput::Hidden => self.semantics.hidden,
            ControlInput::HeadingLevel => self.semantics.heading_level,
            ControlInput::Visible => self.node.visible,
            ControlInput::Focusable => self.node.focusable,
            ControlInput::TextColor => self.text.color,
            ControlInput::SoftWrap => self.text.soft_wrap,
        }
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
            ControlInput::BackgroundTransition => &mut self.look.background_transition,
            ControlInput::OpacityTransition => &mut self.look.opacity_transition,
            ControlInput::Label => &mut self.semantics.label,
            ControlInput::Hint => &mut self.semantics.hint,
            ControlInput::SpokenValue => &mut self.semantics.value,
            ControlInput::Hidden => &mut self.semantics.hidden,
            ControlInput::HeadingLevel => &mut self.semantics.heading_level,
            ControlInput::Visible => &mut self.node.visible,
            ControlInput::Focusable => &mut self.node.focusable,
            ControlInput::TextColor => &mut self.text.color,
            ControlInput::SoftWrap => &mut self.text.soft_wrap,
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
            self.shader,
            look.background,
            look.opacity,
            look.background_transition,
            look.opacity_transition,
            semantics.label,
            semantics.hint,
            semantics.value,
            semantics.hidden,
            semantics.heading_level,
            self.node.visible,
            self.node.focusable,
            self.text.color,
            self.text.soft_wrap,
        ] {
            enc.write_varint(entry.map_or(0, |e| u64::from(e) + 1));
        }
        enc.write_varint(self.arms.len() as u64);
        for arm in self.arms.iter() {
            enc.write_u8(u8::from(arm.input == ControlInput::Opacity));
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
            shader: entry()?,
            look: Look {
                background: entry()?,
                opacity: entry()?,
                background_transition: entry()?,
                opacity_transition: entry()?,
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
        };
        control.node = NodeEntries {
            visible: entry()?,
            focusable: entry()?,
        };
        control.text = TextEntries {
            color: entry()?,
            soft_wrap: entry()?,
        };
        let index = |dec: &mut Decoder<'_>| -> Result<u32, DecodeError> {
            let offset = dec.position();
            u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })
        };
        let arms = index(dec)?;
        let mut decoded = Vec::new();
        for _ in 0..arms {
            let offset = dec.position();
            let input = match dec.read_u8()? {
                0 => ControlInput::Background,
                1 => ControlInput::Opacity,
                _ => return Err(DecodeError::Malformed { offset }),
            };
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
                input,
                when: when.into(),
                entry: index(dec)?,
            });
        }
        control.arms = decoded.into();
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
        assert_eq!(ControlKind::from_u8(8), None);
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
        assert_eq!(ControlKind::of("CheckBox"), ControlKind::Toggle);
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
        assert_eq!(ControlKind::Label.input("transition.scale"), None);
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
    fn a_control_round_trips_and_rejects_a_bad_kind() {
        let control = Control {
            kind: ControlKind::Slider,
            value: Some(0),
            min: None,
            max: Some(7),
            step: None,
            fit: None,
            tint: Some(8),
            shader: Some(2),
            look: Look {
                background: Some(3),
                opacity: None,
                background_transition: None,
                opacity_transition: Some(4),
            },
            arms: Box::new([LookArm {
                input: ControlInput::Opacity,
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
            },
            text: TextEntries {
                color: None,
                soft_wrap: Some(13),
            },
        };
        let bytes = control.encode_to_vec();
        assert_eq!(Control::decode_from_slice(&bytes), Ok(control));
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
        let state = bad.len() - 6;
        bad[state] = 9;
        assert!(
            Control::decode_from_slice(&bad).is_err(),
            "no such interaction state"
        );
    }
}
