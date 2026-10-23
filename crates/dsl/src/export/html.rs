//! A view lowered to HTML and CSS: each node an element with the semantics of
//! its widget and role, its layout as flexbox or grid, and each value the
//! interpreter fixes written in place. What can change becomes a wire the
//! page script drives — a property, a handler, a region — so a static page
//! and a scripted one come from one walk.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use viso_behavior::Value;
use viso_schema::layout::SheetDetent;
use viso_schema::number::NumberFormat;
use viso_schema::view::{Control, ControlKind, EventRoute};
use viso_schema::{Align, Justify, LiveRegion, Role};

use super::css::{self, Sheet, Token};
use super::eval::Eval;
use super::{Finding, Need};
use crate::assets::{AssetUse, ResourceId};
use crate::behavior::ir::Site;
use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{
    Alignment2D, AotSnap, AxisIr, InsetIr, LengthIr, NodeKind, Placement, TermsIr, UiItem, UiNode,
    UiTree,
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
    /// Makes the element a viewer of its child: it pans and zooms it.
    Viewer,
    /// Makes the element a swipe row: a drag across slides its content
    /// aside to show its panels.
    SwipeRow,
    /// Makes the element a sheet: its panel slides in from its edge to its
    /// detent while open, and a drag carries it between them or off.
    Sheet,
    /// Has the page carry drags of `data-drag` elements to `data-drop-accepts`
    /// ones.
    DragDrop,
}

/// The style of a sticky element: held at its scroller's start edge, over
/// the siblings that pass under it.
const STICKY: &str = "position: sticky; inset-block-start: 0; inset-inline-start: 0; z-index: 1";

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
    /// Whether it stays at its scroller's start edge while its parent
    /// passes under it, as `position: sticky`.
    Sticky,
    /// The pointer shape over it, as its CSS `cursor`.
    Cursor,
    /// Its text's `font-weight`.
    FontWeight,
    /// Whether its text is italic.
    FontStyle,
    /// A text's pieces, each a `<span>` styled its own way.
    Spans,
    /// Whether a text can be selected, as its `vx-selectable` class.
    Selectable,
    /// The accessible name: `alt` of an image, else `aria-label`.
    Label {
        alt: bool,
    },
    Hint,
    Spoken,
    Hidden,
    HeadingLevel,
    Checked,
    /// A disclosure's `expanded`, its `<details>`' `open`.
    Open,
    /// Whether a layer shows: a dialog's `<dialog>` open as a modal one, a
    /// toast not `hidden`.
    Layer {
        dialog: bool,
    },
    /// A slider's `value`, `min`, `max` or `step`.
    Range(&'static str),
    /// A link label's `url`, its `<a>`'s `href`.
    Href,
    /// A badge's `count` or `max`, which the script shows as its text.
    Badge(&'static str),
    /// An avatar's `name` or `image`, which the script shows as its
    /// initials, fill and name, or its picture.
    Avatar(&'static str),
    /// A progress indicator's `value`, `min` or `max`: its `aria-value*`,
    /// and the fraction `--vx-at` its fill shows; with no value it runs.
    Progress(&'static str),
    InputValue,
    /// A text field's `placeholder` attribute.
    Placeholder,
    /// Whether a text field is a password one.
    Secure,
    /// What a text field holds: its `inputmode` and, for a number, the
    /// `pattern` it holds to.
    InputKind,
    /// A text field's `maxlength`.
    MaxLength,
    /// A number field's number, kept while its text reads as it.
    Number,
    Selected {
        radio: bool,
    },
    /// The page a navigation stack shows.
    Page,
    /// The tag a node flies between a navigation stack's pages by.
    Hero,
    /// Whether it wears the browser's focus outline.
    FocusRing,
    /// The letter that, with the browser's access-key modifiers, reaches it.
    AccessKey,
    /// The axis it drags along alone, if any.
    DragAxis,
    /// A `data-` attribute the script reads: a viewer's limit or double-tap
    /// scale, a swipe row's full swipe, a swipe panel's edge.
    Data(&'static str),
    /// How a navigation stack's pages trade places.
    Turn,
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
    /// An `Option<Border>`, as an inset `box-shadow` in `--vx-border`: it
    /// strokes inside the box without moving its content.
    Border,
    /// An `Option<Shadow>`, as a `box-shadow` in `--vx-shadow`.
    Shadow,
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
    /// Whether the node being lowered is a disclosure's header, its first
    /// child: the `<summary>` of its `<details>`.
    summary: bool,
    /// Whether the node being lowered is a page its navigation stack folds
    /// away: not the one it shows.
    away: bool,
    /// The tree being lowered, whose spans of other files' components it
    /// reports where the page's source places them.
    tree: Option<&'v UiTree>,
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
            summary: false,
            away: false,
            tree: None,
        }
    }

    /// `at`, a span of instance `instance`'s view, as the page's source has
    /// it.
    fn own(&self, instance: u32, at: TextRange) -> TextRange {
        self.tree.map_or(at, |tree| tree.own_span(instance, at))
    }

    /// Lowers the view's root items.
    pub(super) fn view(&mut self, tree: &'v UiTree) {
        self.tree = Some(tree);
        let items = &tree.items[..];
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
        let (Site { at, instance, .. }, _) = eval.layout.handlers.get(entry as usize)?;
        Some(self.own(*instance, *at))
    }

    /// What `entry` shows here: its value when it never changes, else
    /// `None`, a wire for the script — which a static export refuses.
    /// Whether the constant entry `entry` holds; `None` when it changes.
    fn constant_truth(&mut self, entry: u32) -> Option<bool> {
        let eval = self.eval.as_deref_mut()?;
        if !eval.constant(entry) {
            return None;
        }
        let value = eval.value(entry, &[]).ok()?;
        Some(value.as_int().is_some_and(|v| v != 0))
    }

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
            (_, ControlKind::Shader | ControlKind::Viewport) => {
                let what = if kind == ControlKind::Shader {
                    "paints with a shader"
                } else {
                    "draws a 3D scene"
                };
                self.need(
                    Need::GpuOnly,
                    self.own(node.instance, node.origin),
                    format!(
                        "a `{}` {what}, which no HTML element runs",
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
                    self.own(node.instance, node.origin),
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
        // A field that holds lines is a `<textarea>`, which one that turns
        // multi-line on the open page cannot become. A secure field holds
        // one line.
        let field = control
            .as_ref()
            .filter(|c| c.kind == ControlKind::TextInput);
        let secure = field
            .and_then(|c| c.secure)
            .is_some_and(|entry| self.constant_truth(entry) == Some(true));
        let lines = match field.and_then(|c| c.multiline).filter(|_| !secure) {
            Some(entry) => self.constant_truth(entry).unwrap_or_else(|| {
                let at = self.site(entry).unwrap_or(self.own(node.instance, node.origin));
                self.need(
                    Need::Unsupported,
                    at,
                    "whether the field holds lines changes while the page is open".to_owned(),
                    vec!["HTML holds lines in a `<textarea>` and one line in an `<input>`; give `multiline` a constant".to_owned()],
                );
                false
            }),
            None => false,
        };
        let summary = std::mem::take(&mut self.summary);
        if std::mem::take(&mut self.away) {
            el.attr("data-vx-away", "");
        }
        el.tag = match (node.type_name.as_str(), role) {
            _ if summary => "summary",
            ("Disclosure", _) => "details",
            ("Modal" | "Sheet", _) => "dialog",
            ("Button", _) => "button",
            ("TextInput", _) if lines => "textarea",
            ("TextInput" | "NumberField" | "Toggle" | "CheckBox" | "Slider", _) => "input",
            ("Image" | "Svg", _) if !masked => "img",
            ("Text", _) => "span",
            ("LinkLabel", _) => "a",
            ("Badge" | "Avatar", _) => "span",
            (_, Some(Role::Navigation)) => "nav",
            _ => "div",
        };
        el.base = match (node.kind, node.type_name.as_str()) {
            (NodeKind::Flex, _) => "vx-flex",
            (NodeKind::Grid, _) => "vx-grid",
            (NodeKind::Stack, _) => "vx-stack",
            (NodeKind::Absolute, _) => "vx-absolute",
            (NodeKind::Scroll | NodeKind::VirtualList, _) => "vx-scroll",
            (_, "Text" | "LinkLabel") => "vx-text",
            (_, "Button") => "vx-button",
            (_, "TextInput" | "NumberField") => "vx-input",
            (_, "Toggle" | "CheckBox") => "vx-check",
            (_, "Slider") => "vx-range",
            (_, "ProgressBar") => "vx-progress",
            (_, "ProgressRing") => "vx-ring",
            (_, "Spinner") => "vx-spinner",
            (_, "Badge") => "vx-badge",
            (_, "Avatar") => "vx-avatar",
            (_, "Image" | "Svg") if masked => "vx-mask",
            (_, "Image" | "Svg") => "vx-image",
            _ => "vx-leaf",
        };
        // A disclosure's caption header draws the chevron that turns with it.
        if summary
            && node.kind == NodeKind::Leaf
            && !matches!(node.type_name.as_str(), "Toggle" | "CheckBox")
        {
            el.classes.push("vx-chevron".to_owned());
        }
        // A radio group's caption option draws the circle that fills with it.
        if item.is_some_and(|(role, _)| role == Role::Radio)
            && node.kind == NodeKind::Leaf
            && !matches!(node.type_name.as_str(), "Toggle" | "CheckBox")
        {
            el.classes.push("vx-radio".to_owned());
        }
        match node.type_name.as_str() {
            "LinkLabel" => el.classes.push("vx-link".to_owned()),
            "Modal" | "Sheet" => el.classes.push("vx-layer".to_owned()),
            "Toast" => el
                .classes
                .extend(["vx-layer".to_owned(), "vx-toast".to_owned()]),
            _ => {}
        }
        if node.kind == NodeKind::VirtualList {
            el.classes.push("vx-list".to_owned());
            if node.style.axis == Some(AxisIr::Row) {
                el.classes.push("vx-across".to_owned());
            }
            if node.style.list.is_some_and(|list| list.pages) {
                el.classes.push("vx-pages".to_owned());
            }
            if node.style.list.is_some_and(|list| list.lanes.is_some()) {
                el.classes.push("vx-lanes".to_owned());
            }
        }
        if node
            .style
            .scroll
            .is_some_and(|scroll| scroll.snap.is_some())
        {
            el.classes.push("vx-snap".to_owned());
        }
        match node.type_name.as_str() {
            "Button" => el.attr("type", "button"),
            "TextInput" | "NumberField" if !lines => el.attr("type", "text"),
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
            "ProgressBar" | "ProgressRing" => {
                el.attr("aria-valuemin", "0");
                el.attr("aria-valuemax", "1");
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
            // A status speaks politely of itself.
            Some(LiveRegion::Polite) if role == Some(Role::Status) => {}
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
                let at = self
                    .site(route.handler)
                    .unwrap_or(self.own(node.instance, node.origin));
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
            if kind.responds()
                && routes
                    .iter()
                    .any(|r| r.event.is_control() && kind.reports(r.event))
            {
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
                            self.summary = index == 0 && node.type_name == "Disclosure";
                            self.away =
                                kind == ControlKind::Pages && selected.is_some_and(|s| s != index);
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
                let row = style.axis == Some(AxisIr::Row);
                if row {
                    decls.push(("flex-direction", "row".into()));
                    decls.push(("overflow-y", "hidden".into()));
                } else {
                    decls.push(("overflow-x", "hidden".into()));
                }
                // A snapping viewport rests its items as `--vx-snap` aligns
                // them, its inset kept clear.
                if let Some(scroll) = style.scroll
                    && let Some(snap) = scroll.snap
                {
                    let axis = if row { "x" } else { "y" };
                    decls.push(("scroll-snap-type", format!("{axis} mandatory")));
                    let align = match snap {
                        AotSnap::Start | AotSnap::Pages => "start",
                        AotSnap::Center => "center",
                        AotSnap::End => "end",
                    };
                    decls.push(("--vx-snap", align.into()));
                    if snap == AotSnap::Pages {
                        decls.push(("--vx-snap-stop", "always".into()));
                    }
                    if scroll.snap_inset > 0.0 {
                        let edge = if row {
                            "scroll-padding-inline"
                        } else {
                            "scroll-padding-block"
                        };
                        decls.push((edge, css::px(f64::from(scroll.snap_inset))));
                    }
                }
                // The page mounts every row; the browser skips laying out and
                // painting those off screen, sizing each at the estimate until
                // it has been shown.
                if node.kind == NodeKind::VirtualList
                    && let Some(list) = style.list
                {
                    decls.push(("--vx-row", css::terms(list.estimate, device)));
                    // A grid's lanes are the tracks across its axis; a
                    // masonry packs its items along it where the browser
                    // can.
                    if let Some(lanes) = list.lanes {
                        let track = match lanes.columns {
                            0 => format!(
                                "repeat(auto-fill, minmax(min({}, 100%), 1fr))",
                                css::terms(lanes.min, device)
                            ),
                            n => format!("repeat({n}, minmax(0, 1fr))"),
                        };
                        let (across, along) = if row {
                            ("grid-template-rows", "grid-template-columns")
                        } else {
                            ("grid-template-columns", "grid-template-rows")
                        };
                        decls.push(("display", "grid".into()));
                        decls.push((across, track));
                        if row {
                            decls.push(("grid-auto-flow", "column".into()));
                        }
                        if lanes.masonry {
                            decls.push((along, "masonry".into()));
                        }
                        let (gap, cross) = if row {
                            ("column-gap", "row-gap")
                        } else {
                            ("row-gap", "column-gap")
                        };
                        decls.push((gap, css::terms(lanes.gap, device)));
                        decls.push((cross, css::terms(lanes.cross_gap, device)));
                        decls.push(("align-items", "start".into()));
                        decls.push(("align-content", "start".into()));
                    }
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

    /// Writes a progress indicator's value and bounds as `aria-value*`, the
    /// fraction its fill shows as `--vx-at`, its tint as its `color` and its
    /// label as its `aria-label`. With no value it has no `aria-valuenow`,
    /// which its rule shows running.
    fn progress(
        &mut self,
        control: &Control,
        el: &mut Element,
        decls: &mut Vec<(&'static str, String)>,
        wires: &mut Vec<Wire>,
        at: TextRange,
    ) {
        let mut fixed = [Some(0.0), Some(1.0), None];
        let mut changes = false;
        for (i, (entry, name, attr, what)) in [
            (
                control.min,
                "min",
                "aria-valuemin",
                "the progress's minimum",
            ),
            (
                control.max,
                "max",
                "aria-valuemax",
                "the progress's maximum",
            ),
            (
                control.value,
                "value",
                "aria-valuenow",
                "the progress's value",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            if entry.is_none() {
                continue;
            }
            match self.bind(wires, entry, Prop::Progress(name), what, at) {
                Some(f) => {
                    fixed[i] = f.float();
                    if let Some(v) = fixed[i] {
                        el.attr(attr, &css::number(v));
                    }
                }
                None => changes = true,
            }
        }
        // The script keeps the fraction of any that change.
        if let ([Some(min), Some(max), Some(value)], false) = (fixed, changes) {
            let span = max - min;
            let at = if span > 0.0 {
                ((value - min) / span).clamp(0.0, 1.0)
            } else {
                0.0
            };
            decls.push(("--vx-at", css::number(at)));
        }
        // No tint keeps the theme's accent.
        if let Some(Fixed::Value(tint)) =
            self.bind(wires, control.tint, Prop::Progress("tint"), "the tint", at)
            && tint.as_int().is_some()
        {
            decls.push(("color", Fixed::Value(tint).color()));
        }
        if control.semantics.label.is_none()
            && let Some(fixed) = self.bind(
                wires,
                control.caption,
                Prop::Label { alt: false },
                "the label",
                at,
            )
        {
            el.attr("aria-label", &fixed.text());
        }
    }

    /// Writes a badge's count as its text, `max` and a plus past its `max`,
    /// and with no count makes it a dot (`vx-dot`); a count or most that
    /// changes rides in `data-count`/`data-max` for the script to show
    /// again. Its tint is its background, its label its `aria-label`.
    fn badge(
        &mut self,
        key: u32,
        control: &Control,
        el: &mut Element,
        decls: &mut Vec<(&'static str, String)>,
        wires: &mut Vec<Wire>,
        at: TextRange,
    ) {
        let max = match control.max {
            Some(_) => self
                .bind(
                    wires,
                    control.max,
                    Prop::Badge("max"),
                    "the badge's most",
                    at,
                )
                .map(|fixed| fixed.int()),
            None => Some(Some(99)),
        };
        let count = match control.value {
            Some(_) => self
                .bind(
                    wires,
                    control.value,
                    Prop::Badge("count"),
                    "the badge's count",
                    at,
                )
                .map(|fixed| fixed.int()),
            None => Some(None),
        };
        if let Some(max) = max {
            el.attr("data-max", &max.unwrap_or(99).to_string());
        }
        if let Some(count) = count {
            el.attr(
                "data-count",
                &count.map_or_else(String::new, |c| c.to_string()),
            );
        }
        match (count, max) {
            (Some(Some(count)), Some(max)) => {
                el.text = Some(viso_schema::plate::badge_count(count, max.unwrap_or(99)));
            }
            (Some(None), _) => el.classes.push("vx-dot".to_owned()),
            _ => {}
        }
        self.plate_fill(key, control, decls, wires, at);
        if control.semantics.label.is_none()
            && let Some(fixed) = self.bind(
                wires,
                control.caption,
                Prop::Label { alt: false },
                "the label",
                at,
            )
        {
            el.attr("aria-label", &fixed.text());
        }
    }

    /// Writes an avatar's name's initials as its text, the color the name
    /// picks as its background unless its tint says, and the name as its
    /// `aria-label`; or its image as its background picture. A name or
    /// image that changes the script shows again.
    fn avatar(
        &mut self,
        key: u32,
        control: &Control,
        el: &mut Element,
        decls: &mut Vec<(&'static str, String)>,
        wires: &mut Vec<Wire>,
        at: TextRange,
    ) {
        let tinted = control.tint.is_some();
        if tinted {
            el.attr("data-tinted", "");
        }
        let image = match control.value {
            Some(_) => self
                .bind(
                    wires,
                    control.value,
                    Prop::Avatar("image"),
                    "the picture",
                    at,
                )
                .map(|fixed| fixed.resource()),
            None => Some(None),
        };
        let name = match control.caption {
            Some(_) => self
                .bind(wires, control.caption, Prop::Avatar("name"), "the name", at)
                .map(|fixed| fixed.text()),
            None => Some(String::new()),
        };
        if let Some(Some(id)) = image {
            let url = self.asset(id);
            el.attr("data-image", &url);
            decls.push(("background-image", format!("url(\"{url}\")")));
        }
        if let Some(name) = &name {
            el.attr("data-name", name);
            if image == Some(None) {
                el.text = Some(viso_schema::plate::initials(name));
            }
            if !tinted {
                let rgb = viso_schema::plate::avatar_rgb(name);
                decls.push(("background-color", css::color((rgb << 8) | 0xff)));
            }
            if control.semantics.label.is_none() && !name.is_empty() {
                el.attr("role", "img");
                el.attr("aria-label", name);
            }
        }
        self.plate_fill(key, control, decls, wires, at);
    }

    /// Writes a badge's or avatar's tint as its background.
    fn plate_fill(
        &mut self,
        key: u32,
        control: &Control,
        decls: &mut Vec<(&'static str, String)>,
        wires: &mut Vec<Wire>,
        at: TextRange,
    ) {
        let Some(tint) = control.tint else {
            return;
        };
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
            Fixed::Value(Value::Nil) => {}
            fixed => decls.push(("background-color", fixed.color())),
        }
    }

    /// Writes a number field's number, its bounds, its step and the places
    /// it shows: the keyboard and pattern of what it holds, its bounds as
    /// `aria-valuemin`/`aria-valuemax`, and its step and places as
    /// `data-step`/`data-places` for the page's script to step and settle by.
    fn number(
        &mut self,
        control: &Control,
        el: &mut Element,
        wires: &mut Vec<Wire>,
        at: TextRange,
    ) {
        let constant = |entry: Option<u32>, this: &mut Self| -> Value {
            entry
                .and_then(
                    |entry| match this.fixed(entry, "the number field's format", at) {
                        Fixed::Value(v) => Some(v),
                        _ => None,
                    },
                )
                .unwrap_or(Value::Nil)
        };
        let (min, max, step, precision) = (
            constant(control.min, self),
            constant(control.max, self),
            constant(control.step, self),
            constant(control.precision, self),
        );
        let format = number_format(&min, &max, &step, &precision);
        // The `InputKind` tags of digits, integer and decimal.
        let kind = match (format.whole(), format.signed()) {
            (true, false) => 1,
            (true, true) => 2,
            (false, _) => 3,
        };
        if let Some((mode, pattern)) = input_kind(kind) {
            el.attr("inputmode", mode);
            if let Some(pattern) = pattern {
                el.attr("pattern", pattern);
            }
        }
        if format.min.is_finite() {
            el.attr("aria-valuemin", &format.show(format.min));
        }
        if format.max.is_finite() {
            el.attr("aria-valuemax", &format.show(format.max));
        }
        el.attr("data-step", &format!("{}", format.step));
        el.attr("data-places", &format.precision.to_string());
        // A field given no number shows the one nearest zero in its range.
        let fixed = match control.value {
            Some(_) => self
                .bind(wires, control.value, Prop::Number, "the number", at)
                .map(|fixed| fixed.float()),
            None => Some(None),
        };
        if let Some(number) = fixed {
            let shown = format.show(format.settle(number.unwrap_or(format.origin())));
            el.attr("value", &shown);
            el.attr("aria-valuenow", &shown);
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
        let at = self.own(node.instance, node.origin);
        let mut selected = None;
        let mask = el.tag != "img";
        match control.kind {
            ControlKind::Label | ControlKind::Button => {
                match self.bind(
                    wires,
                    control.text.spans,
                    Prop::Spans,
                    "the text's pieces",
                    at,
                ) {
                    Some(Fixed::Value(pieces)) if spans_markup(&pieces).is_some() => {
                        el.markup = spans_markup(&pieces);
                    }
                    // Pieces the script keeps current replace the text.
                    None if control.text.spans.is_some() => {}
                    _ => {
                        if let Some(fixed) =
                            self.bind(wires, control.value, Prop::Text, "the text", at)
                        {
                            el.text = Some(fixed.text());
                        }
                    }
                }
                if let Some(fixed) =
                    self.bind(wires, control.url, Prop::Href, "where the link leads", at)
                {
                    el.attr("href", &fixed.text());
                }
                if let Some(Fixed::Value(on)) = self.bind(
                    wires,
                    control.text.selectable,
                    Prop::Selectable,
                    "whether the text is selectable",
                    at,
                ) && on.as_int().is_some_and(|v| v != 0)
                {
                    el.classes.push("vx-selectable".to_owned());
                }
            }
            ControlKind::TextInput => {
                if let Some(fixed) =
                    self.bind(wires, control.value, Prop::InputValue, "the text", at)
                {
                    if el.tag == "textarea" {
                        el.text = Some(fixed.text());
                    } else {
                        el.attr("value", &fixed.text());
                    }
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
                if let Some(fixed) = self.bind(
                    wires,
                    control.input_kind,
                    Prop::InputKind,
                    "what it holds",
                    at,
                ) && let Some((mode, pattern)) = input_kind(fixed.int().unwrap_or(0))
                {
                    el.attr("inputmode", mode);
                    if let Some(pattern) = pattern {
                        el.attr("pattern", pattern);
                    }
                }
                if let Some(fixed) =
                    self.bind(wires, control.max_length, Prop::MaxLength, "its length", at)
                    && let Some(max) = fixed.int()
                {
                    el.attr("maxlength", &max.to_string());
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
            ControlKind::Disclosure => {
                if let Some(fixed) = self.bind(
                    wires,
                    control.value,
                    Prop::Open,
                    "whether it is expanded",
                    at,
                ) && fixed.truth()
                {
                    el.attr("open", "");
                }
            }
            ControlKind::Dialog | ControlKind::Toast => {
                let dialog = control.kind == ControlKind::Dialog;
                let shows = self.bind(
                    wires,
                    control.value,
                    Prop::Layer { dialog },
                    "whether it is open",
                    at,
                );
                // A layer its script opens starts closed.
                let open = shows.is_some_and(|fixed| fixed.truth());
                match (dialog, open) {
                    (true, true) => el.attr("open", ""),
                    (false, false) => el.attr("hidden", ""),
                    _ => {}
                }
            }
            // A sheet's script, ahead of what it shows, slides its panel to
            // the detent its `data-detent` picks among its `data-detents`.
            ControlKind::Sheet => {
                wires.push(Wire::Sheet);
                let sheet = node.style.sheet.unwrap_or_default();
                el.attr("data-sheet-edge", &(sheet.edge as u8).to_string());
                let detents: Vec<String> = sheet
                    .detents()
                    .iter()
                    .map(|detent| match *detent {
                        SheetDetent::Full => "full".to_owned(),
                        SheetDetent::Fraction(f) => format!("{}%", f * 100.0),
                        SheetDetent::Fixed(v) => format!("{v}px"),
                    })
                    .collect();
                if !detents.is_empty() {
                    el.attr("data-detents", &detents.join(" "));
                }
                if !sheet.dismissible {
                    el.attr("data-dismissible", "false");
                }
                if let Some(fixed) = self.bind(
                    wires,
                    control.min,
                    Prop::Data("data-detent"),
                    "the detent it rests at",
                    at,
                ) {
                    el.attr("data-detent", &fixed.text());
                }
                if self
                    .bind(
                        wires,
                        control.value,
                        Prop::Layer { dialog: true },
                        "whether it is open",
                        at,
                    )
                    .is_some_and(|fixed| fixed.truth())
                {
                    el.attr("open", "");
                }
            }
            ControlKind::Number => self.number(control, el, wires, at),
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
            ControlKind::Progress | ControlKind::Ring | ControlKind::Spinner => {
                self.progress(control, el, decls, wires, at);
            }
            ControlKind::Badge => self.badge(key, control, el, decls, wires, at),
            ControlKind::Avatar => self.avatar(key, control, el, decls, wires, at),
            ControlKind::Viewer => {
                for (entry, name, what) in [
                    (control.min, "data-min-scale", "the viewer's least scale"),
                    (control.max, "data-max-scale", "the viewer's greatest scale"),
                    (
                        control.pan_margin,
                        "data-pan-margin",
                        "how far the viewer pans past its bounds",
                    ),
                    (
                        control.value,
                        "data-double-tap",
                        "the scale a double tap zooms to",
                    ),
                ] {
                    if let Some(fixed) = self.bind(wires, entry, Prop::Data(name), what, at) {
                        let shown = match &fixed {
                            Fixed::Value(Value::Agg(length)) => length
                                .fields
                                .first()
                                .and_then(Value::as_float)
                                .map_or_else(String::new, css::number),
                            _ => fixed.text(),
                        };
                        el.attr(name, &shown);
                    }
                }
                if control.node.clip.is_none() {
                    el.style("overflow: hidden");
                }
                wires.push(Wire::Viewer);
            }
            ControlKind::SwipeRow => {
                if let Some(fixed) = self.bind(
                    wires,
                    control.value,
                    Prop::Data("data-full-swipe"),
                    "the edges a full swipe runs on",
                    at,
                ) {
                    el.attr("data-full-swipe", &fixed.text());
                }
                if control.node.clip.is_none() {
                    el.style("overflow: hidden");
                }
                wires.push(Wire::SwipeRow);
            }
            // A handle is a button the list's script carries its row by.
            ControlKind::ReorderHandle => {
                el.attr("data-reorder-handle", "");
                el.attr("tabindex", "0");
                if control.node.cursor.is_none() {
                    decls.push(("cursor", "grab".to_owned()));
                }
                decls.push(("touch-action", "none".to_owned()));
            }
            // A preview lies off the page, put away there as the image its
            // parent's drag shows.
            ControlKind::DragPreview => {
                el.attr("data-drag-preview", "");
                el.attr("aria-hidden", "true");
                decls.push(("position", "fixed".to_owned()));
                decls.push(("inset-block-start", "0".to_owned()));
                decls.push(("inset-inline-start", "-100000px".to_owned()));
                decls.push(("pointer-events", "none".to_owned()));
            }
            // A panel lies below its row's content, filling the row.
            ControlKind::SwipeActions => {
                let edge = self
                    .bind(
                        wires,
                        control.value,
                        Prop::Data("data-swipe-edge"),
                        "the edge the actions wait past",
                        at,
                    )
                    .and_then(|fixed| fixed.int())
                    .unwrap_or(1);
                el.attr("data-swipe-edge", &edge.to_string());
                decls.push(("position", "absolute".to_owned()));
                decls.push(("inset", "0".to_owned()));
                decls.push(("z-index", "-1".to_owned()));
            }
            ControlKind::Pages => {
                // Its transition first, so a turn reads the current one.
                if let Some(fixed) = self.bind(
                    wires,
                    control.page_transition,
                    Prop::Turn,
                    "how the pages trade places",
                    at,
                ) {
                    el.attr("data-turn", &fixed.int().unwrap_or(0).to_string());
                }
                selected = match control.value {
                    Some(entry) => self
                        .bind(wires, Some(entry), Prop::Page, "the page", at)
                        .and_then(|fixed| fixed.int()),
                    None => Some(0),
                };
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
            ControlKind::Plain | ControlKind::Shader | ControlKind::Viewport => {}
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
            control.text.font_weight,
            Prop::FontWeight,
            "the weight",
            at,
        ) && let Some(weight) = fixed.int().and_then(css_weight)
        {
            decls.push(("font-weight", weight.to_string()));
        }
        if let Some(fixed) = self.bind(
            wires,
            control.text.font_style,
            Prop::FontStyle,
            "the slant",
            at,
        ) && fixed.int() == Some(1)
        {
            decls.push(("font-style", "italic".into()));
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
        if let Some(fixed) = self.bind(
            wires,
            control.node.sticky,
            Prop::Sticky,
            "whether it sticks",
            at,
        ) && fixed.truth()
        {
            el.style(STICKY);
        }
        if let Some(fixed) = self.bind(wires, control.node.hero, Prop::Hero, "its hero tag", at)
            && matches!(fixed, Fixed::Value(Value::Str(_)))
        {
            el.attr("data-hero", &fixed.text());
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.focus_ring,
            Prop::FocusRing,
            "whether it wears the focus ring",
            at,
        ) && !fixed.truth()
        {
            el.style("outline: none");
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.access_key,
            Prop::AccessKey,
            "its access key",
            at,
        ) {
            el.attr("accesskey", &fixed.text());
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.drag_axis,
            Prop::DragAxis,
            "the axis it drags along",
            at,
        ) && let Some(axis) = fixed.int()
        {
            el.attr("data-drag-axis", if axis == 0 { "row" } else { "column" });
        }
        let carries = [
            control.node.drag,
            control.node.drag_kind,
            control.node.drop_accepts,
            control.node.drop_operation,
        ];
        if carries.iter().any(Option::is_some) {
            wires.push(Wire::DragDrop);
        }
        for (entry, name, what) in [
            (control.node.drag, "data-drag", "the text it carries"),
            (
                control.node.drag_kind,
                "data-drag-kind",
                "the kind it carries",
            ),
            (
                control.node.drop_accepts,
                "data-drop-accepts",
                "the kinds it takes",
            ),
            (
                control.node.drop_operation,
                "data-drop-operation",
                "what a drop on it does",
            ),
        ] {
            match self.bind(wires, entry, Prop::Data(name), what, at) {
                Some(Fixed::Value(Value::Nil)) | None => {}
                Some(Fixed::Value(Value::List(kinds))) => {
                    let kinds: Vec<String> = kinds.iter().map(text_of).collect();
                    el.attr(name, &kinds.join("\n"));
                }
                Some(fixed) => {
                    el.attr(name, &fixed.text());
                    if name == "data-drag" {
                        el.attr("draggable", "true");
                    }
                }
            }
        }
        if let Some(fixed) = self.bind(
            wires,
            control.node.cursor,
            Prop::Cursor,
            "the pointer shape",
            at,
        ) && let Some(cursor) = fixed.int().and_then(css_cursor)
        {
            el.style(&format!("cursor: {cursor}"));
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
            VarToken::Shadow => "the shadow",
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
                VarToken::Shadow => cast(&v),
                VarToken::Translate => offset(&v),
                VarToken::Scale => css::number(v.as_float().unwrap_or(1.0)),
                VarToken::Rotation => format!("{}deg", css::number(v.as_float().unwrap_or(0.0))),
                VarToken::Origin => origin(&v, false),
                VarToken::Transition(_) => String::new(),
            },
            Fixed::Theme(name) => format!("var({name})"),
        }
    }

    /// The node's background, opacity, corner radius, border and shadow, each
    /// its base value and the values its styles' `when` blocks select, as
    /// rules of its own class; and how they move. A border and a shadow are
    /// both a `box-shadow`, so each is a custom property the one
    /// `box-shadow` lists.
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
        // Whether a border or a shadow shows: its `box-shadow` then lists
        // both, the one not given as none.
        let boxed = [
            (look.border, viso_schema::view::ControlInput::Border),
            (look.shadow, viso_schema::view::ControlInput::Shadow),
        ]
        .into_iter()
        .any(|(base, input)| {
            let input = viso_schema::view::Switched::Input(input);
            base.is_some() || control.arms.iter().any(|a| a.target == input)
        });
        for (base, input, property, name, token) in [
            (
                look.background,
                viso_schema::view::ControlInput::Background,
                "background-color",
                "background-color",
                VarToken::Color,
            ),
            (
                look.opacity,
                viso_schema::view::ControlInput::Opacity,
                "opacity",
                "opacity",
                VarToken::Opacity,
            ),
            (
                look.corner_radius,
                viso_schema::view::ControlInput::CornerRadius,
                "border-radius",
                "border-radius",
                VarToken::Radius,
            ),
            (
                look.border,
                viso_schema::view::ControlInput::Border,
                "--vx-border",
                "border",
                VarToken::Border,
            ),
            (
                look.shadow,
                viso_schema::view::ControlInput::Shadow,
                "--vx-shadow",
                "shadow",
                VarToken::Shadow,
            ),
            (
                look.translate,
                viso_schema::view::ControlInput::Translate,
                "translate",
                "translate",
                VarToken::Translate,
            ),
            (
                look.scale,
                viso_schema::view::ControlInput::Scale,
                "scale",
                "scale",
                VarToken::Scale,
            ),
            (
                look.rotation,
                viso_schema::view::ControlInput::Rotation,
                "rotate",
                "rotate",
                VarToken::Rotation,
            ),
        ] {
            let input = viso_schema::view::Switched::Input(input);
            let arms: Vec<_> = control.arms.iter().filter(|a| a.target == input).collect();
            if let Some(entry) = base {
                let var = format!("--v{key}-{name}");
                let value = self.look_value(wires, entry, var, token, at);
                decls.push((property, value));
            } else if boxed && matches!(token, VarToken::Border | VarToken::Shadow) {
                decls.push((property, NO_SHADOW.to_owned()));
            }
            // The first arm that holds wins: the rules are written last first,
            // so an earlier one of equal specificity overrides the later.
            for (index, arm) in arms.iter().enumerate().rev() {
                let Some(selector) = self.selector(&arm.when, key, at, wires) else {
                    continue;
                };
                let var = format!("--v{key}-{name}-{index}");
                let value = self.look_value(wires, arm.entry, var, token, at);
                self.out
                    .sheet
                    .rule(None, format!(".{own}{selector}"), &[(property, value)]);
                el.own_class = true;
            }
        }
        if boxed {
            decls.push((
                "box-shadow",
                "var(--vx-shadow), var(--vx-border)".to_owned(),
            ));
        }
        // A text's color switches as its look does; the arms of a value with
        // no CSS rule of its own here do not export.
        let switched = |input| viso_schema::view::Switched::Input(input);
        let arms: Vec<_> = control
            .arms
            .iter()
            .enumerate()
            .filter(|(_, a)| a.target == switched(viso_schema::view::ControlInput::TextColor))
            .collect();
        for &(index, arm) in arms.iter().rev() {
            let Some(selector) = self.selector(&arm.when, key, at, wires) else {
                continue;
            };
            let var = format!("--v{key}-color-{index}");
            let value = self.look_value(wires, arm.entry, var, VarToken::Color, at);
            self.out
                .sheet
                .rule(None, format!(".{own}{selector}"), &[("color", value)]);
            el.own_class = true;
        }
        const LOOKS: [viso_schema::view::ControlInput; 9] = [
            viso_schema::view::ControlInput::Background,
            viso_schema::view::ControlInput::Opacity,
            viso_schema::view::ControlInput::CornerRadius,
            viso_schema::view::ControlInput::Border,
            viso_schema::view::ControlInput::Shadow,
            viso_schema::view::ControlInput::Translate,
            viso_schema::view::ControlInput::Scale,
            viso_schema::view::ControlInput::Rotation,
            viso_schema::view::ControlInput::TextColor,
        ];
        if control.arms.iter().any(|a| {
            !matches!(a.target, viso_schema::view::Switched::Input(input) if LOOKS.contains(&input))
        }) {
            self.need(
                Need::Unsupported,
                at,
                "a style's `when` switches a value the export does not switch yet: \
                 `background`, `opacity`, `corner_radius`, `border`, `shadow`, `translate`, \
                 `scale`, \
                 `rotation` and a text's `color` do",
                Vec::new(),
            );
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
                        viso_schema::Interaction::Selected => {
                            r#":is([aria-selected="true"],[role="radio"][aria-checked="true"])"#
                        }
                        viso_schema::Interaction::Dragging => "[data-viso-dragging]",
                        viso_schema::Interaction::DropOver => "[data-viso-drop-over]",
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

    fn float(&self) -> Option<f64> {
        match self {
            Fixed::Value(v) => v.as_float(),
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

/// How a number field bounded by `min` and `max`, stepping by `step` and
/// showing `precision` places shows its number; an absent bound is none, an
/// absent step `1`, an absent precision as many places as the step has.
fn number_format(min: &Value, max: &Value, step: &Value, precision: &Value) -> NumberFormat {
    let format = NumberFormat::new(
        min.as_float().unwrap_or(f64::NEG_INFINITY),
        max.as_float().unwrap_or(f64::INFINITY),
        step.as_float().unwrap_or(1.0),
    );
    match precision.as_int().and_then(|p| u8::try_from(p).ok()) {
        Some(places) => format.with_precision(places),
        None => format,
    }
}

/// The `inputmode` and `pattern` of the `InputKind` tagged `tag`; `None`
/// for prose, which needs neither.
fn input_kind(tag: i64) -> Option<(&'static str, Option<&'static str>)> {
    Some(match tag {
        1 => ("numeric", Some("[0-9]*")),
        2 => ("numeric", Some("-?[0-9]*")),
        3 => ("decimal", Some("-?[0-9]*[.]?[0-9]*")),
        4 => ("tel", None),
        5 => ("email", None),
        6 => ("url", None),
        7 => ("search", None),
        _ => return None,
    })
}

/// The CSS `font-weight` of the `FontWeight` of tag `tag`.
fn css_weight(tag: i64) -> Option<u16> {
    [100, 300, 400, 500, 600, 700, 900]
        .get(usize::try_from(tag).ok()?)
        .copied()
}

/// The CSS family list of a `FontFamily`.
fn css_family(family: &Value) -> Option<String> {
    match family {
        Value::Int(0) => Some("system-ui, sans-serif".into()),
        Value::Int(1) => Some("serif".into()),
        Value::Int(2) => Some("monospace".into()),
        Value::Agg(named) => {
            let name = named.fields.first()?.as_str()?;
            let quoted: String = name.chars().filter(|c| !matches!(c, '"' | '\\')).collect();
            Some(format!("\"{quoted}\", system-ui, sans-serif"))
        }
        _ => None,
    }
}

/// A `MixedLength` a span sizes or raises by, its `%` of the font size.
fn css_span_length(length: &Value) -> Option<String> {
    let Value::Agg(terms) = length else {
        return None;
    };
    let term = |at: usize| {
        terms
            .fields
            .get(at)
            .and_then(Value::as_float)
            .unwrap_or(0.0)
    };
    let mut device = false;
    Some(css::terms(
        crate::ir::ui_ir::TermsIr {
            dp: term(0) as f32,
            px: term(1) as f32,
            sp: term(2) as f32,
            em: (term(3) + term(4) / 100.0) as f32,
            pct: 0.0,
        },
        &mut device,
    ))
}

/// The markup of a text's pieces, each a `<span>` its style asks for and
/// plain text where it asks for none; `None` for no pieces.
fn spans_markup(pieces: &Value) -> Option<String> {
    let Value::List(pieces) = pieces else {
        return None;
    };
    if pieces.is_empty() {
        return None;
    }
    let mut out = String::new();
    for piece in pieces.iter() {
        let Value::Agg(span) = piece else { continue };
        let [
            text,
            color,
            background,
            weight,
            slant,
            family,
            size,
            raise,
            underline,
            strikethrough,
            link,
        ] = &span.fields[..]
        else {
            continue;
        };
        let link = link.as_str();
        let mut style: Vec<String> = Vec::new();
        if let Some(c) = color.as_int() {
            style.push(format!("color: {}", css::color(c as u32)));
        }
        if let Some(c) = background.as_int() {
            style.push(format!("background-color: {}", css::color(c as u32)));
        }
        if let Some(w) = weight.as_int().and_then(css_weight) {
            style.push(format!("font-weight: {w}"));
        }
        match slant.as_int() {
            Some(1) => style.push("font-style: italic".into()),
            Some(0) => style.push("font-style: normal".into()),
            _ => {}
        }
        if let Some(f) = css_family(family) {
            style.push(format!("font-family: {f}"));
        }
        if let Some(size) = css_span_length(size) {
            style.push(format!("font-size: {size}"));
        }
        if let Some(raise) = css_span_length(raise) {
            style.push(format!("vertical-align: {raise}"));
        }
        let lines: Vec<&str> = [(underline, "underline"), (strikethrough, "line-through")]
            .into_iter()
            .filter(|(on, _)| on.as_int().is_some_and(|v| v != 0))
            .map(|(_, line)| line)
            .collect();
        if !lines.is_empty() {
            style.push(format!("text-decoration-line: {}", lines.join(" ")));
        }
        let text = text.as_str().unwrap_or_default();
        if style.is_empty() && link.is_none() {
            escape_text(text, &mut out);
            continue;
        }
        let tag = if link.is_some() { "a" } else { "span" };
        out.push('<');
        out.push_str(tag);
        if let Some(href) = link {
            out.push_str(" href=\"");
            escape_attr(href, &mut out);
            out.push('"');
        }
        if !style.is_empty() {
            out.push_str(" style=\"");
            escape_attr(&style.join("; "), &mut out);
            out.push('"');
        }
        out.push('>');
        escape_text(text, &mut out);
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    }
    Some(out)
}

/// The CSS `cursor` of the `Cursor` of tag `tag`.
fn css_cursor(tag: i64) -> Option<&'static str> {
    const CURSORS: [&str; 24] = [
        "default",
        "pointer",
        "text",
        "vertical-text",
        "crosshair",
        "move",
        "grab",
        "grabbing",
        "not-allowed",
        "wait",
        "progress",
        "help",
        "context-menu",
        "copy",
        "alias",
        "zoom-in",
        "zoom-out",
        "ew-resize",
        "ns-resize",
        "nesw-resize",
        "nwse-resize",
        "col-resize",
        "row-resize",
        "none",
    ];
    CURSORS.get(usize::try_from(tag).ok()?).copied()
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
    let easing = spec.fields.get(2).unwrap_or(&Value::Int(2));
    Some((
        format!(
            "{property} {} {} {}",
            css::seconds(css::duration(easing, time(0))),
            css::easing(easing),
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
        | (Role::Dialog, "dialog", _)
        | (Role::Label, _, _) => return None,
        (Role::Group, ..) => "group",
        (Role::Button, ..) => "button",
        (Role::CheckBox, ..) => "checkbox",
        (Role::Slider, ..) => "slider",
        (Role::Radio, ..) => "radio",
        (Role::Image, ..) => "img",
        (Role::TextField | Role::SecureField, ..) => "textbox",
        (Role::SpinButton, ..) => "spinbutton",
        (Role::Menu, ..) => "menu",
        (Role::MenuItem, ..) => "menuitem",
        (Role::Link, "a", _) => return None,
        (Role::Link, ..) => "link",
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
        (Role::Table, "table", _)
        | (Role::Row, "tr", _)
        | (Role::Cell, "td", _)
        | (Role::ColumnHeader, "th", _)
        | (Role::Separator, "hr", _) => return None,
        (Role::MenuItemCheckBox, ..) => "menuitemcheckbox",
        (Role::MenuItemRadio, ..) => "menuitemradio",
        (Role::MenuBar, ..) => "menubar",
        (Role::Separator, ..) => "separator",
        (Role::ListBox, ..) => "listbox",
        (Role::ListOption, ..) => "option",
        (Role::ComboBox | Role::EditableComboBox, ..) => "combobox",
        (Role::Tooltip, ..) => "tooltip",
        (Role::Table, ..) => "table",
        (Role::Row, ..) => "row",
        (Role::Cell, ..) => "cell",
        (Role::ColumnHeader, ..) => "columnheader",
        (Role::ProgressIndicator, ..) => "progressbar",
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
    /// Markup it holds in place of text, already escaped.
    pub(super) markup: Option<String>,
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
                if let Some(markup) = &el.markup {
                    out.push_str(markup);
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

/// The `box-shadow` of no border or no shadow, which a list of them takes
/// where `none` is not allowed.
const NO_SHADOW: &str = "0 0 #0000";

/// An `Option<Border>` as an inset `box-shadow`, [`NO_SHADOW`] for no
/// border.
fn stroke(value: &Value) -> String {
    let Value::Agg(agg) = value else {
        return NO_SHADOW.to_owned();
    };
    let [width, paint] = &agg.fields[..] else {
        return NO_SHADOW.to_owned();
    };
    let color = paint
        .as_int()
        .map_or("transparent".to_owned(), |c| css::color(c as u32));
    format!("inset 0 0 0 {} {color}", css::px(css::dp(width).max(0.0)))
}

/// An `Option<Shadow>` as a `box-shadow`, [`NO_SHADOW`] for no shadow.
fn cast(value: &Value) -> String {
    css::shadow(value).unwrap_or_else(|| NO_SHADOW.to_owned())
}
