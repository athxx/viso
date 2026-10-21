//! A view lowered to HTML and CSS: each node an element with the semantics of
//! its widget and role, its layout as flexbox or grid, and each value the
//! interpreter fixes written in place. What can change becomes a wire the
//! page script drives — a property, a handler, a region — so a static page
//! and a scripted one come from one walk.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use viso_behavior::Value;
use viso_schema::view::{Control, ControlKind, EventRoute};
use viso_schema::{Align, Justify, LiveRegion, Role};

use super::css::{self, Sheet, Token};
use super::eval::Eval;
use super::{Finding, Need};
use crate::assets::{AssetUse, ResourceId};
use crate::behavior::ir::Site;
use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{
    Alignment2D, AxisIr, InsetIr, LengthIr, NodeKind, Placement, TermsIr, UiItem, UiNode,
};
use crate::syntax::TextRange;
use crate::view_behavior::{ViewBehavior, When};

/// The text color a view shows where it sets none: the runtime's default,
/// in sRGB.
pub(super) const TEXT_COLOR: &str = "#3f3f45";

/// A stretch of the page shown as one: the static view, or an arm of a
/// region the script mounts.
#[derive(Debug, Default)]
pub(super) struct Content {
    pub(super) nodes: Vec<Piece>,
    /// The elements the script wires, in document order: in HTML each carries
    /// a `data-v` attribute.
    pub(super) marks: Vec<Mark>,
    /// The component instances it mounts, each where its view first appears:
    /// each mount of the content keeps their states, runs their effects and
    /// owns the tasks they start.
    pub(super) instances: Vec<u32>,
}

/// A node of the lowered tree.
#[derive(Debug)]
pub(super) enum Piece {
    Element {
        el: Box<Element>,
        children: Vec<Piece>,
        /// Its index among the content's marks, when the script wires it.
        mark: Option<usize>,
    },
    /// Where region `region` mounts its content.
    Region { region: u32 },
}

/// An element the script wires.
#[derive(Debug)]
pub(super) struct Mark {
    pub(super) wires: Vec<Wire>,
}

/// What the script does to a marked element.
#[derive(Debug)]
pub(super) enum Wire {
    /// Shows the value of handler-table entry `entry` as `prop`.
    Prop { entry: u32, prop: Prop },
    /// Runs handler `handler` on `route`.
    On {
        route: EventRoute,
        handler: u32,
        capture: bool,
        kind: ControlKind,
    },
    /// Shows a control's current value again after its own event, from
    /// `entry` (its default without one): the view, not the browser, decides
    /// what a control shows.
    Sync {
        kind: ControlKind,
        entry: Option<u32>,
    },
    /// Mounts region `region` before the element, a `<template>` anchor.
    Region(u32),
    /// Follows the `env` fields instance `instance` reads that depend on
    /// where it sits: the element is its root.
    Anchor(u32),
}

/// A property a wire shows.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Prop {
    Text,
    TextColor,
    SoftWrap,
    Visible,
    Focusable,
    /// Whether it and its subtree take input and focus.
    Enabled,
    /// Whether it takes the focus as it mounts.
    Autofocus,
    /// Whether its children show only inside its box.
    Clip,
    /// The accessible name: `alt` of an image, else `aria-label`.
    Label {
        alt: bool,
    },
    Hint,
    Spoken,
    Hidden,
    HeadingLevel,
    Checked,
    /// A slider's `value`, `min`, `max` or `step`.
    Range(&'static str),
    InputValue,
    /// A text field's `placeholder` attribute.
    Placeholder,
    /// Whether a text field is a password one.
    Secure,
    Selected {
        radio: bool,
    },
    /// An asset's source, as an image or as a mask.
    Source {
        mask: bool,
    },
    Fit {
        mask: bool,
    },
    /// A custom property of the element that its own rules read.
    Var {
        name: String,
        token: VarToken,
    },
    /// A class of the element its own rules select by.
    Class(String),
}

/// What a custom property holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VarToken {
    Color,
    Opacity,
    /// A `MixedLength` corner radius, as `border-radius`.
    Radius,
    /// An `Option<Border>`, as an inset `box-shadow`: it strokes inside the
    /// box without moving its content.
    Border,
    /// An `Offset`, as `translate`.
    Translate,
    /// An `F32`, as `scale`.
    Scale,
    /// An `Angle` in degrees, as `rotate`.
    Rotation,
    /// An `Alignment2D`, as `transform-origin`: the variable named and its
    /// `-rtl` twin, which the right-to-left rule reads.
    Origin,
    /// A `Transition` for the CSS property named.
    Transition(&'static str),
}

/// A region the script mounts.
#[derive(Debug)]
pub(super) struct Region {
    pub(super) kind: RegionKind,
    /// The name a `for` binds each item to.
    pub(super) binding: Option<String>,
    pub(super) arms: Vec<Content>,
    pub(super) preserve: Vec<bool>,
}

/// What decides a region's content, by handler-table entry.
#[derive(Debug, Clone, Copy)]
pub(super) enum RegionKind {
    If { select: u32 },
    Match { scrutinee: u32, select: u32 },
    For { items: u32, key: Option<u32> },
}

/// The view as a page.
#[derive(Debug, Default)]
pub(super) struct Lowered {
    pub(super) root: Content,
    pub(super) regions: Vec<Region>,
    pub(super) sheet: Sheet,
    /// Every asset an element shows, by resource id: its path under
    /// `assets/`.
    pub(super) assets: BTreeMap<u64, String>,
    /// Whether a length has a device-pixel term.
    pub(super) device: bool,
}

/// Where a node sits, which decides how its size reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parent {
    /// The view's root, which the window sizes.
    Root,
    Flex(AxisIr),
    Grid,
    /// A stack, and where it puts a child that does not align itself.
    Stack(Alignment2D),
    Absolute,
    Scroll(AxisIr),
}

/// The walk.
pub(super) struct Lower<'v, 'e, 'a> {
    vb: Option<&'v ViewBehavior>,
    eval: Option<&'e mut Eval<'a>>,
    uses: &'v [AssetUse],
    static_only: bool,
    pub(super) findings: Vec<Finding>,
    pub(super) out: Lowered,
    /// The next node key, counting every node in pre-order as the behavior
    /// table does.
    key: u32,
    /// The instances a region's content mounts that a content claimed.
    claimed: BTreeSet<u32>,
    /// The instances whose root element was lowered.
    rooted: BTreeSet<u32>,
    /// Whether the view reads `env` fields that depend on where their reader
    /// sits, which the page measures from what its elements say of their
    /// layout.
    anchors: bool,
    /// Whether the flex container being lowered stretches its children.
    stretch: bool,
}

impl<'v, 'e, 'a> Lower<'v, 'e, 'a> {
    pub(super) fn new(
        vb: Option<&'v ViewBehavior>,
        eval: Option<&'e mut Eval<'a>>,
        uses: &'v [AssetUse],
        static_only: bool,
    ) -> Self {
        Lower {
            vb,
            eval,
            uses,
            static_only,
            findings: Vec::new(),
            out: Lowered::default(),
            key: 0,
            claimed: BTreeSet::new(),
            rooted: BTreeSet::new(),
            anchors: false,
            stretch: false,
        }
    }

    /// Lowers the view's root items.
    pub(super) fn view(&mut self, items: &[UiItem]) {
        self.anchors = !self.static_only
            && self
                .eval
                .as_deref()
                .is_some_and(|eval| eval.layout.env.iter().any(|e| e.field.anchored()));
        let mut root = Content::default();
        for item in items {
            match item {
                UiItem::Node(node) => self.node(node, Parent::Root, None, 0, false, &mut root),
                _ => self.skip(item),
            }
        }
        self.out.root = root;
    }

    fn need(&mut self, need: Need, at: TextRange, message: impl Into<String>, notes: Vec<String>) {
        self.findings.push(Finding {
            need,
            at,
            message: message.into(),
            notes,
        });
    }

    /// Counts the nodes of an item no element shows past.
    fn skip(&mut self, item: &UiItem) {
        match item {
            UiItem::Node(node) => {
                self.key += 1;
                for child in &node.children {
                    self.skip(child);
                }
            }
            _ => {
                for items in crate::view_regions::arms(item) {
                    for item in items {
                        self.skip(item);
                    }
                }
            }
        }
    }

    /// The source span of handler-table entry `entry`.
    fn site(&self, entry: u32) -> Option<TextRange> {
        let eval = self.eval.as_deref()?;
        let (Site { at, .. }, _) = eval.layout.handlers.get(entry as usize)?;
        Some(*at)
    }

    /// What `entry` shows here: its value when it never changes, else
    /// `None`, a wire for the script — which a static export refuses.
    fn fixed(&mut self, entry: u32, what: &str, fallback: TextRange) -> Fixed {
        let at = self.site(entry).unwrap_or(fallback);
        let Some(eval) = self.eval.as_deref_mut() else {
            return Fixed::Changes;
        };
        if eval.constant(entry) {
            if let Some(path) = eval.theme_path(entry)
                && let Some((name, token)) = css::theme_var(&path)
                && token == Token::Color
            {
                return Fixed::Theme(name);
            }
            return match eval.value(entry, &[]) {
                Ok(value) => Fixed::Value(value),
                Err(fault) => {
                    self.need(
                        Need::Unsupported,
                        at,
                        format!("{what} faults as it is evaluated: {}", fault.message),
                        Vec::new(),
                    );
                    Fixed::Changes
                }
            };
        }
        if self.static_only {
            self.need(
                Need::Dynamic,
                at,
                format!("{what} changes while the page is open"),
                vec!["it reads state, or a region's binding, which a page without JavaScript cannot follow".to_owned()],
            );
        }
        Fixed::Changes
    }

    #[allow(clippy::too_many_arguments)]
    fn node(
        &mut self,
        node: &UiNode,
        parent: Parent,
        item: Option<(Role, Option<bool>)>,
        depth: u32,
        templated: bool,
        out: &mut Content,
    ) {
        let key = self.key;
        self.key += 1;
        let regional = self.eval.as_deref().is_some_and(|eval| {
            eval.layout
                .regional
                .iter()
                .any(|r| r.instance == node.instance)
        });
        if regional && self.claimed.insert(node.instance) {
            out.instances.push(node.instance);
        }
        let control = self.vb.and_then(|vb| vb.control(NodeKey(key)));
        let routes: Vec<_> = self
            .vb
            .map(|vb| vb.routes(NodeKey(key)).to_vec())
            .unwrap_or_default();
        let kind = ControlKind::of(&node.type_name);
        match (node.kind, kind) {
            (_, ControlKind::Shader) => {
                self.need(
                    Need::GpuOnly,
                    node.origin,
                    format!(
                        "a `{}` paints with a shader, which no HTML element runs",
                        node.type_name
                    ),
                    vec![
                        "build for the browser with the Viso renderer: `viso build web-gpu`, or with DOM and GPU islands: `viso build web-hybrid`".to_owned(),
                        "or rewrite the node with widgets HTML has".to_owned(),
                    ],
                );
                for child in &node.children {
                    self.skip(child);
                }
                return;
            }
            (NodeKind::Component, _) => {
                self.need(
                    Need::Unsupported,
                    node.origin,
                    format!(
                        "`{}` is a Rust component, which only the Viso runtime mounts",
                        node.type_name
                    ),
                    Vec::new(),
                );
                for child in &node.children {
                    self.skip(child);
                }
                return;
            }
            _ => {}
        }

        let mut el = Element::default();
        let widget = viso_schema::view::widget_role(&node.type_name);
        let role = match item {
            // A list's row is an item unless it says what else it is.
            Some((Role::ListItem, _)) => node.role.or(widget).or(Some(Role::ListItem)),
            item => node.role.or(item.map(|(role, _)| role)).or(widget),
        };
        let masked = control.as_ref().is_some_and(|c| c.tint.is_some());
        el.tag = match (node.type_name.as_str(), role) {
            ("Button", _) => "button",
            ("TextInput" | "Toggle" | "CheckBox" | "Slider", _) => "input",
            ("Image" | "Svg", _) if !masked => "img",
            ("Text", _) => "span",
            (_, Some(Role::Navigation)) => "nav",
            _ => "div",
        };
        el.base = match (node.kind, node.type_name.as_str()) {
            (NodeKind::Flex, _) => "vx-flex",
            (NodeKind::Grid, _) => "vx-grid",
            (NodeKind::Stack, _) => "vx-stack",
            (NodeKind::Absolute, _) => "vx-absolute",
            (NodeKind::Scroll | NodeKind::VirtualList, _) => "vx-scroll",
            (_, "Text") => "vx-text",
            (_, "Button") => "vx-button",
            (_, "TextInput") => "vx-input",
            (_, "Toggle" | "CheckBox") => "vx-check",
            (_, "Slider") => "vx-range",
            (_, "Image" | "Svg") if masked => "vx-mask",
            (_, "Image" | "Svg") => "vx-image",
            _ => "vx-leaf",
        };
        if node.kind == NodeKind::VirtualList {
            el.classes.push("vx-list".to_owned());
            if node.style.axis == Some(AxisIr::Row) {
                el.classes.push("vx-across".to_owned());
            }
        }
        match node.type_name.as_str() {
            "Button" => el.attr("type", "button"),
            "TextInput" => el.attr("type", "text"),
            "Toggle" => {
                el.attr("type", "checkbox");
                el.attr("role", "switch");
            }
            "CheckBox" => el.attr("type", "checkbox"),
            "Slider" => {
                el.attr("type", "range");
                el.attr("min", "0");
                el.attr("max", "1");
                el.attr("step", "any");
                el.attr("value", "0");
            }
            _ => {}
        }
        let unlabeled = control.as_ref().is_none_or(|c| c.semantics.label.is_none());
        match role {
            Some(Role::Image) if unlabeled => {}
            Some(role) => {
                if let Some(name) = aria_role(role, el.tag, &node.type_name) {
                    el.attr("role", name);
                }
            }
            None if node.type_name == "RadioGroup" => el.attr("role", "radiogroup"),
            None => {}
        }
        if let Some((role, selected)) = item {
            let state = if role == Role::Radio {
                "aria-checked"
            } else {
                "aria-selected"
            };
            if let Some(selected) = selected {
                el.attr(state, if selected { "true" } else { "false" });
                el.attr("tabindex", if selected { "0" } else { "-1" });
            }
        }
        match node.live {
            Some(LiveRegion::Polite) => el.attr("aria-live", "polite"),
            Some(LiveRegion::Assertive) => el.attr("aria-live", "assertive"),
            _ => {}
        }
        if parent == Parent::Root {
            el.id = Some("viso-root");
        }

        let mut decls: Vec<(&'static str, String)> = Vec::new();
        self.layout(node, parent, &mut decls);

        let own = format!("v{key}");
        let mut wires = Vec::new();
        let mut selected = None;
        if let Some(control) = &control {
            selected = self.control(node, key, control, &own, &mut el, &mut decls, &mut wires);
        }
        if !routes.is_empty() && !templated && self.static_only {
            for route in &routes {
                let at = self.site(route.handler).unwrap_or(node.origin);
                self.need(
                    Need::Dynamic,
                    at,
                    format!("the `{}` handler changes the page", route.event.event()),
                    vec!["a page without JavaScript runs no handler".to_owned()],
                );
            }
        }
        if !self.static_only {
            for route in &routes {
                wires.push(Wire::On {
                    route: route.event,
                    handler: route.handler,
                    capture: route.capture,
                    kind,
                });
            }
            if kind.responds() && routes.iter().any(|r| kind.reports(r.event)) {
                let entry = control.as_ref().and_then(|c| c.value);
                wires.push(Wire::Sync { kind, entry });
            }
        }
        let root_of = self.rooted.insert(node.instance);
        if let Some(named) = node.node_ref
            && !self.static_only
        {
            el.attr("data-node", &named.to_string());
        }
        if self.anchors {
            let reads = self.eval.as_deref().is_some_and(|eval| {
                eval.layout
                    .env
                    .iter()
                    .any(|e| e.instance == node.instance && e.field.anchored())
            });
            if root_of && reads {
                wires.push(Wire::Anchor(node.instance));
            }
            if parent == Parent::Root {
                el.attr("data-root", "");
            }
            let fit = self.fit(node, parent);
            if !fit.is_empty() {
                el.attr("data-fit", fit);
            }
            if let Some(scope) = node.style.scope {
                let basis = scope
                    .basis
                    .map_or_else(String::new, |b| css::number(f64::from(b)));
                el.attr("data-scope", &basis);
                if let Some(direction) = scope.direction {
                    el.attr(
                        "dir",
                        match direction {
                            viso_schema::LayoutDirection::Ltr => "ltr",
                            viso_schema::LayoutDirection::Rtl => "rtl",
                        },
                    );
                }
            }
        }
        if let Some(class) = self.out.sheet.class(&decls) {
            el.classes.push(class);
        }
        if el.own_class {
            el.classes.push(own);
        }
        if !wires.is_empty() {
            el.marked = true;
        }

        let mark = el.marked.then(|| {
            out.marks.push(Mark { wires });
            out.marks.len() - 1
        });
        let siblings = std::mem::take(&mut out.nodes);
        let child_parent = match node.kind {
            NodeKind::Flex => Some(Parent::Flex(node.style.axis.unwrap_or(AxisIr::Row))),
            NodeKind::Grid => Some(Parent::Grid),
            NodeKind::Stack => Some(Parent::Stack(node.style.content_align.unwrap_or_default())),
            NodeKind::Absolute => Some(Parent::Absolute),
            NodeKind::Scroll | NodeKind::VirtualList => {
                Some(Parent::Scroll(node.style.axis.unwrap_or(AxisIr::Column)))
            }
            _ => None,
        };
        let item_role = match node.type_name.as_str() {
            "Tabs" => Some(Role::Tab),
            "RadioGroup" => Some(Role::Radio),
            "VirtualList" => Some(Role::ListItem),
            _ => None,
        };
        let stretch = std::mem::replace(
            &mut self.stretch,
            node.kind == NodeKind::Flex && node.style.align == Some(Align::Stretch),
        );
        match child_parent {
            Some(child) => {
                let mut index = 0i64;
                for item in &node.children {
                    let item_of = item_role.map(|role| (role, selected.map(|s| s == index)));
                    match item {
                        UiItem::Node(n) => {
                            self.node(n, child, item_of, depth, templated, out);
                            index += 1;
                        }
                        region => {
                            let item_of = item_role.map(|role| (role, None));
                            self.region(region, child, item_of, depth, out);
                        }
                    }
                }
            }
            None => {
                for child in &node.children {
                    self.skip(child);
                }
            }
        }
        self.stretch = stretch;
        let children = std::mem::replace(&mut out.nodes, siblings);
        out.nodes.push(Piece::Element {
            el: Box::new(el),
            children,
            mark,
        });
    }

    /// The axes along which `node`'s content decides its extent — it fits
    /// its content, or fills the cross axis of a flex container that does
    /// not stretch its children — as `data-fit` names them.
    fn fit(&self, node: &UiNode, parent: Parent) -> &'static str {
        if parent == Parent::Root {
            return "";
        }
        let (width, height) = extents(node);
        let sized = |length: LengthIr, axis: AxisIr| match length {
            LengthIr::Fit | LengthIr::MinContent | LengthIr::MaxContent => true,
            LengthIr::Fill { .. } => {
                matches!(parent, Parent::Flex(main) if main != axis) && !self.stretch
            }
            LengthIr::Fixed(_) | LengthIr::Relative { .. } => false,
        };
        match (sized(width, AxisIr::Row), sized(height, AxisIr::Column)) {
            (true, true) => "wh",
            (true, false) => "w",
            (false, true) => "h",
            (false, false) => "",
        }
    }

    /// The declarations of `node`'s box: how it lays out its children, and
    /// how its parent sizes it.
    fn layout(&mut self, node: &UiNode, parent: Parent, decls: &mut Vec<(&'static str, String)>) {
        let style = &node.style;
        let lengths = style.lengths();
        let device = &mut self.out.device;
        match node.kind {
            NodeKind::Flex => {
                if style.axis == Some(AxisIr::Column) {
                    decls.push(("flex-direction", "column".into()));
                }
                match style.justify {
                    Some(Justify::Center) => decls.push(("justify-content", "center".into())),
                    Some(Justify::End) => decls.push(("justify-content", "flex-end".into())),
                    _ => {}
                }
                match style.align {
                    Some(Align::Center) => decls.push(("align-items", "center".into())),
                    Some(Align::End) => decls.push(("align-items", "flex-end".into())),
                    Some(Align::Stretch) => decls.push(("align-items", "stretch".into())),
                    _ => {}
                }
                if let Some(gap) = lengths.gap {
                    decls.push(("gap", css::terms(gap, device)));
                } else if let Some(gap) = style.gap.filter(|g| *g != 0.0) {
                    decls.push(("gap", css::px(f64::from(gap))));
                }
                padding(node, decls, device);
            }
            NodeKind::Grid | NodeKind::Stack | NodeKind::Absolute => padding(node, decls, device),
            NodeKind::Scroll | NodeKind::VirtualList => {
                if style.axis == Some(AxisIr::Row) {
                    decls.push(("flex-direction", "row".into()));
                    decls.push(("overflow-y", "hidden".into()));
                } else {
                    decls.push(("overflow-x", "hidden".into()));
                }
                // The page mounts every row; the browser skips laying out and
                // painting those off screen, sizing each at the estimate until
                // it has been shown.
                if node.kind == NodeKind::VirtualList
                    && let Some(list) = style.list
                {
                    decls.push(("--vx-row", css::terms(list.estimate, device)));
                }
            }
            _ => {}
        }
        if let Some(size) = lengths.font_size {
            decls.push(("font-size", css::terms(size, device)));
        }

        if parent == Parent::Root {
            decls.push(("width", "100%".into()));
            decls.push(("height", "100%".into()));
            return;
        }
        let (width, height) = extents(node);
        let width = Extent::of(width, lengths.width.map(|t| css::terms(t, device)));
        let height = Extent::of(height, lengths.height.map(|t| css::terms(t, device)));
        match parent {
            Parent::Root => {}
            Parent::Flex(axis) => {
                let (main, cross, main_name, cross_name) = match axis {
                    AxisIr::Row => (width, height, "width", "height"),
                    AxisIr::Column => (height, width, "height", "width"),
                };
                match main {
                    Extent::Set(value) => {
                        decls.push(("flex", "none".into()));
                        decls.push((main_name, value));
                    }
                    // A fit shrinks to its min-content when its row overflows.
                    Extent::Fit => decls.push(("flex", "0 1 auto".into())),
                    Extent::Fill(weight) => {
                        decls.push(("flex", format!("{} 0 0", css::number(f64::from(weight)))));
                        decls.push((min_of(main_name), "0".into()));
                    }
                }
                if let Extent::Set(value) = cross {
                    decls.push((cross_name, value));
                }
            }
            Parent::Grid => {
                for (extent, name, place) in [
                    (width, "width", "justify-self"),
                    (height, "height", "align-self"),
                ] {
                    match extent {
                        Extent::Set(value) => {
                            decls.push((name, value));
                            decls.push((place, "start".into()));
                        }
                        Extent::Fit => decls.push((place, "start".into())),
                        Extent::Fill(_) => decls.push((place, "stretch".into())),
                    }
                }
            }
            Parent::Stack(align) => {
                let placed = match node.style.place {
                    Some(Placement::Stack(placed)) => placed,
                    _ => Default::default(),
                };
                let (x, y) = placed.align.unwrap_or(align).fractions();
                let edge = |f: f32| match f {
                    0.0 => "start",
                    0.5 => "center",
                    _ => "end",
                };
                decls.push(("grid-area", "1 / 1".into()));
                for (extent, name, place, f) in [
                    (width, "width", "justify-self", x),
                    (height, "height", "align-self", y),
                ] {
                    match extent {
                        Extent::Set(value) => {
                            decls.push((name, value));
                            decls.push((place, edge(f).into()));
                        }
                        Extent::Fit => decls.push((place, edge(f).into())),
                        Extent::Fill(_) => decls.push((place, "stretch".into())),
                    }
                }
                if placed.layer != 0 {
                    decls.push(("z-index", placed.layer.to_string()));
                }
            }
            Parent::Absolute => {
                decls.push(("position", "absolute".into()));
                let span = |span: Option<crate::ir::ui_ir::Span>| {
                    span.map(|s| TermsIr {
                        dp: s.fixed,
                        pct: s.pct,
                        ..TermsIr::default()
                    })
                };
                let insets = match (node.style.place, lengths.insets) {
                    (_, Some(terms)) => [terms.top, terms.end, terms.bottom, terms.start],
                    (Some(Placement::Absolute(placed)), None) => [
                        span(placed.top),
                        span(placed.end),
                        span(placed.bottom),
                        span(placed.start),
                    ],
                    _ => [None; 4],
                };
                let [top, end, bottom, start] = insets;
                for (extent, name, near, far) in [
                    (
                        width,
                        "width",
                        ("inset-inline-start", start),
                        ("inset-inline-end", end),
                    ),
                    (
                        height,
                        "height",
                        ("inset-block-start", top),
                        ("inset-block-end", bottom),
                    ),
                ] {
                    // A fill takes the box between its edges, the content
                    // box's where it names none; so does a fit pinned to both.
                    let fill = matches!(extent, Extent::Fill(_));
                    let near_at = near.1.or((fill || far.1.is_none()).then(TermsIr::default));
                    let far_at = far.1.or(fill.then(TermsIr::default));
                    for (property, at) in [(near.0, near_at), (far.0, far_at)] {
                        if let Some(at) = at {
                            decls.push((property, css::terms(at, device)));
                        }
                    }
                    if let Extent::Set(value) = extent {
                        decls.push((name, value));
                    }
                }
            }
            Parent::Scroll(axis) => {
                let (main, main_name) = match axis {
                    AxisIr::Row => (width, "width"),
                    AxisIr::Column => (height, "height"),
                };
                decls.push(("flex", "none".into()));
                if let Extent::Set(value) = main
                    && !value.contains('%')
                {
                    decls.push((main_name, value));
                }
            }
        }
    }

    /// What `entry` shows, or the wire that keeps `prop` current.
    #[allow(clippy::too_many_arguments)]
    fn bind(
        &mut self,
        wires: &mut Vec<Wire>,
        entry: Option<u32>,
        prop: Prop,
        what: &str,
        at: TextRange,
    ) -> Option<Fixed> {
        let entry = entry?;
        match self.fixed(entry, what, at) {
            Fixed::Changes => {
                wires.push(Wire::Prop { entry, prop });
                None
            }
            fixed => Some(fixed),
        }
    }

    /// Writes what `control`'s entries show; returns the child a selection
    /// selects when it never changes.
    #[allow(clippy::too_many_arguments)]
    fn control(
        &mut self,
        node: &UiNode,
        key: u32,
        control: &Control,
        own: &str,
        el: &mut Element,
        decls: &mut Vec<(&'static str, String)>,
        wires: &mut Vec<Wire>,
    ) -> Option<i64> {
        let at = node.origin;
        let mut selected = None;
        let mask = el.tag != "img";
        match control.kind {
            ControlKind::Label => {
                if let Some(fixed) = self.bind(wires, control.value, Prop::Text, "the text", at) {
                    el.text = Some(fixed.text());
                }
            }
            ControlKind::TextInput => {
                if let Some(fixed) =
                    self.bind(wires, control.value, Prop::InputValue, "the text", at)
                {
                    el.attr("value", &fixed.text());
                }
                if let Some(fixed) = self.bind(
                    wires,
                    control.placeholder,
                    Prop::Placeholder,
                    "the placeholder",
                    at,
                ) {
                    el.attr("placeholder", &fixed.text());
                }
                if let Some(fixed) =
                    self.bind(wires, control.secure, Prop::Secure, "the masking", at)
                    && fixed.truth()
                {
                    el.attr("type", "password");
                }
            }
            ControlKind::Check | ControlKind::Switch => {
                if let Some(fixed) = self.bind(
                    wires,
                    control.value,
                    Prop::Checked,
                    "whether it is checked",
                    at,
                ) && fixed.truth()
                {
                    el.attr("checked", "");
                }
                // A check input holds no text: its caption names it.
                if control.semantics.label.is_none()
                    && let Some(fixed) = self.bind(
                        wires,
                        control.caption,
                        Prop::Label { alt: false },
                        "the caption",
                        at,
                    )
                {
                    el.attr("aria-label", &fixed.text());
                }
            }
            ControlKind::Slider => {
                for (entry, name, what) in [
                    (control.min, "min", "the slider's minimum"),
                    (control.max, "max", "the slider's maximum"),
                    (control.step, "step", "the slider's step"),
                    (control.value, "value", "the slider's value"),
                ] {
                    if let Some(fixed) = self.bind(wires, entry, Prop::Range(name), what, at) {
                        el.attr(name, &fixed.text());
                    }
                }
            }
            ControlKind::Select => {
                let radio = node.type_name == "RadioGroup";
                match control.value {
                    Some(entry) => {
                        if let Some(fixed) = self.bind(
                            wires,
                            Some(entry),
                            Prop::Selected { radio },
                            "the selection",
                            at,
                        ) {
                            selected = fixed.int();
                        }
                    }
                    None => selected = Some(0),
                }
            }
            ControlKind::Asset => {
                if let Some(fixed) = self.bind(
                    wires,
                    control.value,
                    Prop::Source { mask },
                    "the source",
                    at,
                ) {
                    match fixed.resource() {
                        Some(id) => {
                            let url = self.asset(id);
                            el.asset = Some(Shown { id, url, mask });
                        }
                        None if !mask => el.attr("src", ""),
                        None => {}
                    }
                }
                if let Some(fixed) =
                    self.bind(wires, control.fit, Prop::Fit { mask }, "the fit", at)
                {
                    let fit = fit(fixed.int().unwrap_or(0));
                    if mask {
                        decls.push(("mask-size", mask_size(fit).into()));
                        decls.push(("-webkit-mask-size", mask_size(fit).into()));
                    } else if fit != "contain" {
                        decls.push(("object-fit", fit.into()));
                    }
                }
                if let Some(tint) = control.tint {
                    let var = format!("--v{key}-tint");
                    match self.fixed(tint, "the tint", at) {
                        Fixed::Changes => {
                            wires.push(Wire::Prop {
                                entry: tint,
                                prop: Prop::Var {
                                    name: var.clone(),
                                    token: VarToken::Color,
                                },
                            });
                            decls.push(("background-color", format!("var({var})")));
                        }
                        fixed => decls.push(("background-color", fixed.color())),
                    }
                }
                if el.tag == "img" && control.semantics.label.is_none() {
                    el.attr("alt", "");
                }
            }
            ControlKind::Plain | ControlKind::Shader => {}
        }

        if let Some(fixed) = self.bind(
            wires,
            control.text.color,
            Prop::TextColor,
            "the text color",
            at,
        ) {
            decls.push(("color", fixed.color()));
        }
        if let Some(fixed) = self.bind(
            wires,
            control.text.soft_wrap,
            Prop::SoftWrap,
            "the wrapping",
            at,
        ) && fixed.truth()
        {
            decls.push(("white-space", "pre-wrap".into()));
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.visible,
            Prop::Visible,
            "whether it shows",
            at,
        ) && !fixed.truth()
        {
            el.style("display: none");
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.focusable,
            Prop::Focusable,
            "whether it takes focus",
            at,
        ) {
            el.attr("tabindex", if fixed.truth() { "0" } else { "-1" });
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.enabled,
            Prop::Enabled,
            "whether it takes input",
            at,
        ) && !fixed.truth()
        {
            el.attr("data-viso-off", "");
            el.attr("aria-disabled", "true");
            if matches!(el.tag, "button" | "input") {
                el.attr("disabled", "");
            }
            if el.attrs.iter().any(|(name, _)| *name == "tabindex") {
                el.attr("tabindex", "-1");
            }
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.autofocus,
            Prop::Autofocus,
            "whether it takes the focus",
            at,
        ) && fixed.truth()
        {
            el.attr("autofocus", "");
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.clip,
            Prop::Clip,
            "whether it clips its children",
            at,
        ) && fixed.truth()
        {
            el.style("overflow: hidden");
        }
        let alt = el.tag == "img";
        if let Some(fixed) = self.bind(
            wires,
            control.semantics.label,
            Prop::Label { alt },
            "the label",
            at,
        ) {
            el.attr(if alt { "alt" } else { "aria-label" }, &fixed.text());
        }
        if let Some(fixed) = self.bind(wires, control.semantics.hint, Prop::Hint, "the hint", at) {
            el.attr("aria-description", &fixed.text());
        }
        if let Some(fixed) = self.bind(
            wires,
            control.semantics.value,
            Prop::Spoken,
            "the spoken value",
            at,
        ) {
            el.attr("aria-valuetext", &fixed.text());
        }
        if let Some(fixed) = self.bind(
            wires,
            control.semantics.hidden,
            Prop::Hidden,
            "whether it is hidden",
            at,
        ) && fixed.truth()
        {
            el.attr("aria-hidden", "true");
        }
        if let Some(fixed) = self.bind(
            wires,
            control.semantics.heading_level,
            Prop::HeadingLevel,
            "the heading level",
            at,
        ) {
            el.attr("aria-level", &fixed.text());
        }

        self.look(control, key, at, own, el, decls, wires);
        selected
    }

    /// A look value as CSS: written in place, or a custom property of the
    /// element the script keeps current.
    fn look_value(
        &mut self,
        wires: &mut Vec<Wire>,
        entry: u32,
        var: String,
        token: VarToken,
        at: TextRange,
    ) -> String {
        let what = match token {
            VarToken::Color => "the background",
            VarToken::Opacity => "the opacity",
            VarToken::Radius => "the corner radius",
            VarToken::Border => "the border",
            VarToken::Translate => "the translate",
            VarToken::Scale => "the scale",
            VarToken::Rotation => "the rotation",
            VarToken::Origin => "the transform origin",
            VarToken::Transition(_) => "the transition",
        };
        match self.fixed(entry, what, at) {
            Fixed::Changes => {
                wires.push(Wire::Prop {
                    entry,
                    prop: Prop::Var {
                        name: var.clone(),
                        token,
                    },
                });
                format!("var({var})")
            }
            Fixed::Value(v) => match token {
                VarToken::Color => v
                    .as_int()
                    .map_or("transparent".to_owned(), |c| css::color(c as u32)),
                VarToken::Opacity => css::number(v.as_float().unwrap_or(1.0).clamp(0.0, 1.0)),
                VarToken::Radius => radius(&v),
                VarToken::Border => stroke(&v),
                VarToken::Translate => offset(&v),
                VarToken::Scale => css::number(v.as_float().unwrap_or(1.0)),
                VarToken::Rotation => format!("{}deg", css::number(v.as_float().unwrap_or(0.0))),
                VarToken::Origin => origin(&v, false),
                VarToken::Transition(_) => String::new(),
            },
            Fixed::Theme(name) => format!("var({name})"),
        }
    }

    /// The node's background, opacity, corner radius and border, each its
    /// base value and the values its styles' `when` blocks select, as rules
    /// of its own class; and how they move.
    #[allow(clippy::too_many_arguments)]
    fn look(
        &mut self,
        control: &Control,
        key: u32,
        at: TextRange,
        own: &str,
        el: &mut Element,
        decls: &mut Vec<(&'static str, String)>,
        wires: &mut Vec<Wire>,
    ) {
        let look = control.look;
        for (base, input, property, token) in [
            (
                look.background,
                viso_schema::view::ControlInput::Background,
                "background-color",
                VarToken::Color,
            ),
            (
                look.opacity,
                viso_schema::view::ControlInput::Opacity,
                "opacity",
                VarToken::Opacity,
            ),
            (
                look.corner_radius,
                viso_schema::view::ControlInput::CornerRadius,
                "border-radius",
                VarToken::Radius,
            ),
            (
                look.border,
                viso_schema::view::ControlInput::Border,
                "box-shadow",
                VarToken::Border,
            ),
            (
                look.translate,
                viso_schema::view::ControlInput::Translate,
                "translate",
                VarToken::Translate,
            ),
            (
                look.scale,
                viso_schema::view::ControlInput::Scale,
                "scale",
                VarToken::Scale,
            ),
            (
                look.rotation,
                viso_schema::view::ControlInput::Rotation,
                "rotate",
                VarToken::Rotation,
            ),
        ] {
            if let Some(entry) = base {
                let var = format!("--v{key}-{property}");
                let value = self.look_value(wires, entry, var, token, at);
                decls.push((property, value));
            }
            let arms: Vec<_> = control.arms.iter().filter(|a| a.input == input).collect();
            // The first arm that holds wins: the rules are written last first,
            // so an earlier one of equal specificity overrides the later.
            for (index, arm) in arms.iter().enumerate().rev() {
                let Some(selector) = self.selector(&arm.when, key, at, wires) else {
                    continue;
                };
                let var = format!("--v{key}-{property}-{index}");
                let value = self.look_value(wires, arm.entry, var, token, at);
                self.out
                    .sheet
                    .rule(None, format!(".{own}{selector}"), &[(property, value)]);
                el.own_class = true;
            }
        }
        // A start or end origin is the other side of the box right to left.
        if let Some(entry) = look.transform_origin {
            let var = format!("--v{key}-origin");
            let (ltr, rtl) = match self.fixed(entry, "the transform origin", at) {
                Fixed::Changes => {
                    wires.push(Wire::Prop {
                        entry,
                        prop: Prop::Var {
                            name: var.clone(),
                            token: VarToken::Origin,
                        },
                    });
                    (format!("var({var})"), format!("var({var}-rtl)"))
                }
                Fixed::Value(v) => (origin(&v, false), origin(&v, true)),
                Fixed::Theme(name) => (format!("var({name})"), format!("var({name})")),
            };
            if rtl != ltr {
                el.own_class = true;
                self.out.sheet.rule(
                    None,
                    format!(".{own}:dir(rtl)"),
                    &[("transform-origin", rtl)],
                );
            }
            decls.push(("transform-origin", ltr));
        }
        let mut moves = Vec::new();
        let mut reduce = false;
        for (entry, property) in [
            (look.background_transition, "background-color"),
            (look.opacity_transition, "opacity"),
            (look.translate_transition, "translate"),
            (look.scale_transition, "scale"),
            (look.rotation_transition, "rotate"),
            (look.corner_radius_transition, "border-radius"),
            (look.color_transition, "color"),
            (look.width_transition, "width"),
            (look.height_transition, "height"),
        ] {
            let Some(entry) = entry else { continue };
            match self.fixed(entry, "the transition", at) {
                Fixed::Value(spec) => {
                    if let Some((transition, instant)) = transition(property, &spec) {
                        moves.push(transition);
                        reduce |= instant;
                    }
                }
                Fixed::Changes => {
                    let var = format!("--v{key}-{property}-move");
                    wires.push(Wire::Prop {
                        entry,
                        prop: Prop::Var {
                            name: var.clone(),
                            token: VarToken::Transition(property),
                        },
                    });
                    moves.push(format!("var({var})"));
                }
                Fixed::Theme(..) => {}
            }
        }
        if !moves.is_empty() {
            decls.push(("transition", moves.join(", ")));
            if reduce {
                el.own_class = true;
                self.out.sheet.rule(
                    Some("@media (prefers-reduced-motion: reduce)"),
                    format!(".{own}"),
                    &[("transition", "none".into())],
                );
            }
        }
    }

    /// The selector suffix of a style's `when`, `None` when it never holds.
    fn selector(
        &mut self,
        when: &[When],
        key: u32,
        at: TextRange,
        wires: &mut Vec<Wire>,
    ) -> Option<String> {
        // Each step is `Some(suffix)` when it may hold, `None` when it never
        // does; an empty suffix always holds.
        let mut stack: Vec<Option<String>> = Vec::new();
        for step in when {
            let next = match *step {
                When::State(state) => Some(
                    match state {
                        viso_schema::Interaction::Hovered => ":hover",
                        viso_schema::Interaction::Pressed => ":active",
                        viso_schema::Interaction::Focused => ":focus",
                        viso_schema::Interaction::FocusVisible => ":focus-visible",
                    }
                    .to_owned(),
                ),
                When::Entry(entry) => match self.fixed(entry, "the style's condition", at) {
                    Fixed::Value(v) => (v.as_int().unwrap_or(0) != 0).then(String::new),
                    Fixed::Theme(..) => Some(String::new()),
                    Fixed::Changes => {
                        let class = format!("v{key}-c{entry}");
                        wires.push(Wire::Prop {
                            entry,
                            prop: Prop::Class(class.clone()),
                        });
                        Some(format!(".{class}"))
                    }
                },
                When::Not => match stack.pop()? {
                    Some(s) if s.is_empty() => None,
                    Some(s) => Some(format!(":not({s})")),
                    None => Some(String::new()),
                },
                When::And => {
                    let (b, a) = (stack.pop()?, stack.pop()?);
                    match (a, b) {
                        (Some(a), Some(b)) => Some(a + &b),
                        _ => None,
                    }
                }
                When::Or => {
                    let (b, a) = (stack.pop()?, stack.pop()?);
                    match (a, b) {
                        (Some(a), _) | (_, Some(a)) if a.is_empty() => Some(String::new()),
                        (Some(a), Some(b)) => Some(format!(":is({a}, {b})")),
                        (Some(a), None) | (None, Some(a)) => Some(a),
                        (None, None) => None,
                    }
                }
            };
            stack.push(next);
        }
        stack.pop().flatten()
    }

    /// The url of the asset `id` shows, recorded for the export to copy.
    fn asset(&mut self, id: u64) -> String {
        let path = self
            .uses
            .iter()
            .find(|u| u.id == ResourceId(id))
            .map(|u| u.path.clone());
        match path {
            Some(path) => {
                self.out.assets.insert(id, path.clone());
                format!("assets/{}", url_path(&path))
            }
            None => String::new(),
        }
    }

    /// Every asset the view names, which a changing source may show.
    pub(super) fn all_assets(&mut self) {
        for asset in self.uses {
            self.out.assets.insert(asset.id.0, asset.path.clone());
        }
    }

    /// Lowers a region among the children of a node laid out as `parent`.
    fn region(
        &mut self,
        item: &UiItem,
        parent: Parent,
        item_of: Option<(Role, Option<bool>)>,
        depth: u32,
        out: &mut Content,
    ) {
        let origin = match item {
            UiItem::If(r) => r.origin,
            UiItem::For(r) => r.origin,
            UiItem::Match(r) => r.origin,
            UiItem::Node(_) => unreachable!("a node is not a region"),
        };
        if self.static_only {
            let what = match item {
                UiItem::If(_) => "an `if`",
                UiItem::For(_) => "a `for`",
                _ => "a `match`",
            };
            self.need(
                Need::Dynamic,
                origin,
                format!("{what} decides what the page shows while it is open"),
                vec!["a page without JavaScript shows one content for good".to_owned()],
            );
            self.skip(item);
            return;
        }
        let Some(layout) = self.eval.as_deref().map(|e| e.layout) else {
            self.skip(item);
            return;
        };
        let entry = |at: Option<TextRange>, instance: u32| {
            at.and_then(|at| {
                layout.handler(Site {
                    instance,
                    at,
                    part: 0,
                })
            })
        };
        let (kind, inner) = match item {
            UiItem::If(r) => (
                entry(Some(r.origin), r.instance).map(|select| RegionKind::If { select }),
                depth,
            ),
            UiItem::Match(r) => (
                entry(r.scrutinee, r.instance)
                    .zip(entry(Some(r.origin), r.instance))
                    .map(|(scrutinee, select)| RegionKind::Match { scrutinee, select }),
                depth + 1,
            ),
            UiItem::For(r) => (
                entry(r.iterable, r.instance).map(|items| RegionKind::For {
                    items,
                    key: r.key.and_then(|k| entry(Some(k), r.instance)),
                }),
                depth + 1,
            ),
            UiItem::Node(_) => unreachable!("a node is not a region"),
        };
        let Some(kind) = kind else {
            self.need(
                Need::Unsupported,
                origin,
                "internal: the region's entries were not lowered",
                Vec::new(),
            );
            self.skip(item);
            return;
        };
        let index = self.out.regions.len() as u32;
        self.out.regions.push(Region {
            kind,
            binding: match item {
                UiItem::For(r) => r.binding.clone(),
                _ => None,
            },
            arms: Vec::new(),
            preserve: Vec::new(),
        });
        let mut arms = Vec::new();
        let mut preserve = Vec::new();
        let arm_items: Vec<(&[UiItem], bool)> = match item {
            UiItem::If(r) => r
                .arms
                .iter()
                .map(|a| (a.items.as_slice(), a.preserve.is_some()))
                .collect(),
            UiItem::Match(r) => r.arms.iter().map(|a| (a.items.as_slice(), false)).collect(),
            UiItem::For(r) => vec![(r.body.as_slice(), false)],
            UiItem::Node(_) => unreachable!("a node is not a region"),
        };
        for (items, keep) in arm_items {
            let mut content = Content::default();
            for item in items {
                match item {
                    UiItem::Node(n) => {
                        let item = item_of.map(|(role, _)| (role, None));
                        self.node(n, parent, item, inner, true, &mut content);
                    }
                    region => self.region(region, parent, item_of, inner, &mut content),
                }
            }
            arms.push(content);
            preserve.push(keep);
        }
        let region = &mut self.out.regions[index as usize];
        region.arms = arms;
        region.preserve = preserve;
        out.marks.push(Mark {
            wires: vec![Wire::Region(index)],
        });
        out.nodes.push(Piece::Region { region: index });
    }
}

/// How `node`'s parent sizes it: as authored, else filling for a container
/// and nothing for a leaf.
fn extents(node: &UiNode) -> (LengthIr, LengthIr) {
    let style = &node.style;
    let lengths = style.lengths();
    let authored = style.width.is_some()
        || style.height.is_some()
        || lengths.width.is_some()
        || lengths.height.is_some();
    if authored {
        (
            style.width.unwrap_or(LengthIr::Fit),
            style.height.unwrap_or(LengthIr::Fit),
        )
    } else if node.kind.is_container()
        && !matches!(node.kind, NodeKind::VirtualList | NodeKind::Stack)
    {
        (
            LengthIr::Fill { weight: 1.0 },
            LengthIr::Fill { weight: 1.0 },
        )
    } else {
        (LengthIr::Fixed(0.0), LengthIr::Fixed(0.0))
    }
}

/// What an entry shows where it is written.
enum Fixed {
    /// One value for good.
    Value(Value),
    /// The theme field its custom property holds.
    Theme(String),
    /// A value the script keeps current.
    Changes,
}

impl Fixed {
    fn text(&self) -> String {
        match self {
            Fixed::Value(v) => text_of(v),
            _ => String::new(),
        }
    }

    fn truth(&self) -> bool {
        matches!(self, Fixed::Value(v) if v.as_int().is_some_and(|i| i != 0))
    }

    fn int(&self) -> Option<i64> {
        match self {
            Fixed::Value(v) => v.as_int(),
            _ => None,
        }
    }

    fn color(&self) -> String {
        match self {
            Fixed::Value(v) => v
                .as_int()
                .map_or("transparent".to_owned(), |c| css::color(c as u32)),
            Fixed::Theme(name) => format!("var({name})"),
            Fixed::Changes => String::new(),
        }
    }

    fn resource(&self) -> Option<u64> {
        match self {
            Fixed::Value(Value::Agg(asset)) => {
                asset.fields.first()?.as_int().map(i64::cast_unsigned)
            }
            _ => None,
        }
    }
}

/// A value's text where the page shows it as text.
fn text_of(v: &Value) -> String {
    match v {
        Value::Str(s) => s.as_str().to_owned(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        _ => String::new(),
    }
}

/// One side of a node's box.
#[derive(Debug, Clone, PartialEq)]
enum Extent {
    /// A CSS extent: fixed, relative or from the environment.
    Set(String),
    Fit,
    Fill(f32),
}

impl Extent {
    fn of(length: LengthIr, terms: Option<String>) -> Extent {
        if let Some(terms) = terms {
            return Extent::Set(terms);
        }
        match length {
            LengthIr::Fill { weight } => Extent::Fill(weight.max(0.0)),
            LengthIr::Fit => Extent::Fit,
            length => Extent::Set(css::extent(length).unwrap_or_default()),
        }
    }
}

fn min_of(name: &str) -> &'static str {
    if name == "width" {
        "min-width"
    } else {
        "min-height"
    }
}

/// A container's padding: plain dp physically, or terms that follow the
/// layout direction; a safe area adds the display's insets.
fn padding(node: &UiNode, decls: &mut Vec<(&'static str, String)>, device: &mut bool) {
    let safe = node.style.avoid == Some(crate::ir::ui_ir::Avoid::SafeArea);
    if let Some(edges) = node.style.lengths().padding {
        for (name, terms, inset) in [
            ("padding-top", edges.top, "top"),
            ("padding-inline-end", edges.end, "right"),
            ("padding-bottom", edges.bottom, "bottom"),
            ("padding-inline-start", edges.start, "left"),
        ] {
            let value = css::terms(terms, device);
            decls.push((name, safe_inset(value, inset, safe)));
        }
        return;
    }
    let InsetIr {
        left,
        top,
        right,
        bottom,
    } = node.style.padding.unwrap_or_default();
    if safe {
        for (name, v, inset) in [
            ("padding-top", top, "top"),
            ("padding-right", right, "right"),
            ("padding-bottom", bottom, "bottom"),
            ("padding-left", left, "left"),
        ] {
            decls.push((name, safe_inset(css::px(f64::from(v)), inset, true)));
        }
    } else if [left, top, right, bottom].iter().any(|v| *v != 0.0) {
        let [t, r, b, l] = [top, right, bottom, left].map(|v| css::px(f64::from(v)));
        let value = if t == b && r == l {
            if t == r { t } else { format!("{t} {r}") }
        } else {
            format!("{t} {r} {b} {l}")
        };
        decls.push(("padding", value));
    }
}

fn safe_inset(value: String, inset: &str, safe: bool) -> String {
    if !safe {
        value
    } else if value == "0" {
        format!("env(safe-area-inset-{inset}, 0px)")
    } else {
        format!("calc({value} + env(safe-area-inset-{inset}, 0px))")
    }
}

/// The CSS `transition` of `property` by a `Transition` value, and whether a
/// reader who asks for reduced motion sees it instantly.
fn transition(property: &str, spec: &Value) -> Option<(String, bool)> {
    let Value::Agg(spec) = spec else {
        return None;
    };
    let time = |at: usize| spec.fields.get(at).and_then(Value::as_float).unwrap_or(0.0);
    let tag = |at: usize| spec.fields.get(at).and_then(Value::as_int).unwrap_or(0);
    Some((
        format!(
            "{property} {} {} {}",
            css::seconds(time(0)),
            css::easing(tag(2)),
            css::seconds(time(1))
        ),
        tag(3) == 0,
    ))
}

/// An `ImageFit` variant as `object-fit`.
pub(super) fn fit(tag: i64) -> &'static str {
    match tag {
        1 => "cover",
        2 => "fill",
        _ => "contain",
    }
}

pub(super) fn mask_size(fit: &str) -> &'static str {
    match fit {
        "cover" => "cover",
        "fill" => "100% 100%",
        _ => "contain",
    }
}

/// The `role` attribute of `role` on a `tag` element of widget `widget`,
/// `None` where the element says it already.
fn aria_role(role: Role, tag: &str, widget: &str) -> Option<&'static str> {
    Some(match (role, tag, widget) {
        (Role::Button, "button", _)
        | (Role::CheckBox, "input", "CheckBox")
        | (Role::CheckBox, "input", "Toggle")
        | (Role::Slider, "input", _)
        | (Role::TextField | Role::SecureField, "input", _)
        | (Role::Image, "img", _)
        | (Role::Navigation, "nav", _)
        | (Role::Label, _, _) => return None,
        (Role::Group, ..) => "group",
        (Role::Button, ..) => "button",
        (Role::CheckBox, ..) => "checkbox",
        (Role::Slider, ..) => "slider",
        (Role::Radio, ..) => "radio",
        (Role::Image, ..) => "img",
        (Role::TextField | Role::SecureField, ..) => "textbox",
        (Role::Tab, ..) => "tab",
        (Role::TabList, ..) => "tablist",
        (Role::Navigation, ..) => "navigation",
        (Role::Dialog, ..) => "dialog",
        (Role::Status, ..) => "status",
        (Role::Region, ..) => "region",
        (Role::Tree, ..) => "tree",
        (Role::TreeItem, ..) => "treeitem",
        (Role::Heading, ..) => "heading",
        (Role::List, ..) => "list",
        (Role::ListItem, ..) => "listitem",
    })
}

/// An asset an element shows: its resource id, its URL under the export, and
/// whether it shows as a mask its tint fills.
#[derive(Debug, Clone)]
pub(super) struct Shown {
    pub(super) id: u64,
    pub(super) url: String,
    pub(super) mask: bool,
}

/// An element as it is opened and closed.
#[derive(Debug, Default)]
pub(super) struct Element {
    pub(super) tag: &'static str,
    pub(super) id: Option<&'static str>,
    pub(super) base: &'static str,
    pub(super) classes: Vec<String>,
    pub(super) attrs: Vec<(&'static str, String)>,
    pub(super) style: Vec<String>,
    pub(super) text: Option<String>,
    /// The asset it shows for good.
    pub(super) asset: Option<Shown>,
    /// Whether the node has rules of its own class.
    own_class: bool,
    /// Whether the script wires it.
    marked: bool,
}

impl Element {
    fn attr(&mut self, name: &'static str, value: &str) {
        match self.attrs.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) => *v = value.to_owned(),
            None => self.attrs.push((name, value.to_owned())),
        }
    }

    fn style(&mut self, declaration: &str) {
        self.style.push(declaration.to_owned());
    }

    fn open(&self, out: &mut String) {
        out.push('<');
        out.push_str(self.tag);
        if let Some(id) = self.id {
            let _ = write!(out, " id=\"{id}\"");
        }
        out.push_str(" class=\"");
        out.push_str(self.base);
        for class in &self.classes {
            out.push(' ');
            out.push_str(class);
        }
        out.push('"');
        for (name, value) in &self.attrs {
            out.push(' ');
            out.push_str(name);
            if !(value.is_empty()
                && matches!(
                    *name,
                    "checked" | "disabled" | "autofocus" | "data-viso-off"
                ))
            {
                out.push_str("=\"");
                escape_attr(value, out);
                out.push('"');
            }
        }
        let mut style = self.style.clone();
        if let Some(shown) = &self.asset {
            if shown.mask {
                let url = &shown.url;
                style.insert(
                    0,
                    format!("mask-image: url(\"{url}\"); -webkit-mask-image: url(\"{url}\")"),
                );
            } else {
                out.push_str(" src=\"");
                escape_attr(&shown.url, out);
                out.push('"');
            }
        }
        if !style.is_empty() {
            out.push_str(" style=\"");
            escape_attr(&style.join("; "), out);
            out.push('"');
        }
        if self.marked {
            out.push_str(" data-v");
        }
        out.push('>');
    }

    /// Whether it is a void element, which has no children and no end tag.
    pub(super) fn void(&self) -> bool {
        self.tag == "input" || self.tag == "img"
    }

    fn close(&self, out: &mut String) {
        if !self.void() {
            let _ = write!(out, "</{}>", self.tag);
        }
    }
}

/// `nodes` as HTML, each region an anchor `<template>` holding one template
/// per arm, which the script clones in front of it.
pub(super) fn write_html(nodes: &[Piece], regions: &[Region], out: &mut String) {
    for piece in nodes {
        match piece {
            Piece::Element { el, children, .. } => {
                el.open(out);
                if let Some(text) = &el.text {
                    escape_text(text, out);
                }
                write_html(children, regions, out);
                el.close(out);
            }
            Piece::Region { region, .. } => {
                out.push_str("<template data-v>");
                for arm in &regions[*region as usize].arms {
                    out.push_str("<template>");
                    write_html(&arm.nodes, regions, out);
                    out.push_str("</template>");
                }
                out.push_str("</template>");
            }
        }
    }
}

pub(super) fn escape_text(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
}

pub(super) fn escape_attr(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            c => out.push(c),
        }
    }
}

/// An asset path as a relative URL: each segment percent-encoded.
pub(super) fn url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A `MixedLength` corner radius as `border-radius`: a share of 50% or more
/// of the shorter side rounds a pill; a smaller one reads the box's width
/// and height apart, which rounds a non-square box elliptically.
fn radius(value: &Value) -> String {
    let Value::Agg(agg) = value else {
        return "0".to_owned();
    };
    let term = |at: usize| agg.fields.get(at).and_then(Value::as_float).unwrap_or(0.0);
    let mut parts: Vec<String> = [
        (0, "1px"),
        (1, "var(--viso-px)"),
        (2, "0.0625rem"),
        (3, "1em"),
    ]
    .into_iter()
    .filter(|&(at, _)| term(at) != 0.0)
    .map(|(at, unit)| format!("{} * {unit}", css::number(term(at))))
    .collect();
    match term(4) {
        pct if pct >= 50.0 => parts.push("9999px".to_owned()),
        pct if pct > 0.0 => parts.push(format!("{}%", css::number(pct))),
        _ => {}
    }
    match parts.as_slice() {
        [] => "0".to_owned(),
        _ => format!("calc({})", parts.join(" + ")),
    }
}

/// A `MixedLength` as a CSS length.
fn css_length(value: Option<&Value>) -> String {
    let Some(Value::Agg(agg)) = value else {
        return "0px".to_owned();
    };
    let term = |at: usize| agg.fields.get(at).and_then(Value::as_float).unwrap_or(0.0);
    let parts: Vec<String> = [
        (0, "1px"),
        (1, "var(--viso-px)"),
        (2, "0.0625rem"),
        (3, "1em"),
    ]
    .into_iter()
    .filter(|&(at, _)| term(at) != 0.0)
    .map(|(at, unit)| format!("{} * {unit}", css::number(term(at))))
    .collect();
    match parts.as_slice() {
        [] => "0px".to_owned(),
        _ => format!("calc({})", parts.join(" + ")),
    }
}

/// An `Offset` as a CSS `translate`.
/// An `Alignment2D` as a `transform-origin`, laid out right to left when
/// `rtl`.
fn origin(value: &Value, rtl: bool) -> String {
    let tag = value.as_int().unwrap_or(4).clamp(0, 8);
    let across = match (tag % 3, rtl) {
        (1, _) => "center",
        (0, false) | (2, true) => "left",
        _ => "right",
    };
    let down = ["top", "center", "bottom"][(tag / 3) as usize];
    format!("{across} {down}")
}

fn offset(value: &Value) -> String {
    let Value::Agg(agg) = value else {
        return "none".to_owned();
    };
    format!(
        "{} {}",
        css_length(agg.fields.first()),
        css_length(agg.fields.get(1))
    )
}

/// An `Option<Border>` as an inset `box-shadow`, `none` for no border.
fn stroke(value: &Value) -> String {
    let Value::Agg(agg) = value else {
        return "none".to_owned();
    };
    let [width, paint] = &agg.fields[..] else {
        return "none".to_owned();
    };
    let width = match width {
        Value::Agg(terms) => terms.fields.first().and_then(Value::as_float),
        width => width.as_float(),
    };
    let color = paint
        .as_int()
        .map_or("transparent".to_owned(), |c| css::color(c as u32));
    format!(
        "inset 0 0 0 {} {color}",
        css::px(width.unwrap_or(0.0).max(0.0))
    )
}
