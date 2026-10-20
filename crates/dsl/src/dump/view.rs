//! The view a file mounts: its retained-tree template (the UI IR) and its
//! binding edges with what each reactive source invalidates (the reactive
//! IR). Nodes are numbered `n0…` in pre-order through every region, the order
//! the binding edges key them by.

use std::collections::HashMap;
use std::fmt::Write as _;

use viso_ende::JsonWriter;
use viso_schema::DirtyClass;

use super::{Dump, FileCx, strings};
use crate::frontend::{Compiled, SourceKind};
use crate::ir::{
    Align, Avoid, AxisIr, BindingKind, Justify, LengthIr, NodeKind, StyleIr, TermsIr, UiItem,
    UiNode, UiTree,
};
use crate::resolve::SymbolId;
use crate::syntax::TextRange;

/// A source range with its start position.
#[derive(Clone, Copy)]
struct At {
    range: TextRange,
    line: u32,
    column: u32,
}

impl At {
    fn new(cx: &FileCx<'_>, range: TextRange) -> At {
        let (line, column) = cx.position(range);
        At {
            range,
            line,
            column,
        }
    }

    fn write(self, w: &mut JsonWriter) {
        super::hir::span(w, self.range, (self.line, self.column));
    }
}

/// A property the node binds to an expression.
struct Bound {
    name: String,
    value: String,
    dirty: DirtyClass,
    instance: u32,
    at: At,
}

/// A handler the node declares.
struct Handler {
    event: String,
    capture: bool,
    instance: u32,
    at: At,
}

/// A value a view-driven control reads.
struct Control {
    property: String,
    value: String,
    instance: u32,
    at: At,
}

struct Node {
    key: u32,
    type_name: String,
    local_name: Option<String>,
    kind: &'static str,
    style: Vec<(&'static str, String)>,
    bound: Vec<Bound>,
    handlers: Vec<Handler>,
    controls: Vec<Control>,
    looks: Vec<String>,
    role: Option<String>,
    live: Option<String>,
    instance: u32,
    at: At,
    children: Vec<Entry>,
}

/// One arm of a region: its head (`if`'s condition, a `match` pattern and
/// guard, empty for `else` and a `for` body) and its items.
struct Arm {
    head: String,
    preserve: Option<String>,
    items: Vec<Entry>,
}

/// An `if`, `for` or `match` region.
struct Region {
    kind: &'static str,
    head: String,
    arms: Vec<Arm>,
    instance: u32,
    at: At,
}

enum Entry {
    Node(Node),
    Region(Region),
}

/// An inlined component instance.
struct Instance {
    index: u32,
    component: String,
    identity: String,
    parent: u32,
    depth: u32,
    regional: bool,
    at: At,
}

/// The UI IR of the view a file mounts.
pub(super) struct UiView {
    view: Option<String>,
    items: Vec<Entry>,
    instances: Vec<Instance>,
}

pub(super) fn ui(cx: &FileCx<'_>, compiled: &Compiled) -> UiView {
    let names = super::hir::names(&cx.package.hir);
    let mut key = 0;
    let items = entries(cx, &compiled.tree.items, &mut key);
    let instances = compiled
        .tree
        .instances
        .iter()
        .enumerate()
        .map(|(i, instance)| Instance {
            index: i as u32 + 1,
            component: name_of(&names, instance.component),
            identity: instance.identity.clone(),
            parent: instance.parent,
            depth: instance.depth,
            regional: instance.regional,
            at: At::new(cx, instance.origin),
        })
        .collect();
    UiView {
        view: view_name(compiled),
        items,
        instances,
    }
}

/// `class` as the runtime names it.
fn classes(class: crate::ir::DirtyClass) -> DirtyClass {
    DirtyClass::from_bits(class.bits())
}

fn view_name(compiled: &Compiled) -> Option<String> {
    compiled.component.as_ref().map(|c| c.schema.name.clone())
}

fn name_of(names: &HashMap<SymbolId, String>, symbol: SymbolId) -> String {
    names
        .get(&symbol)
        .cloned()
        .unwrap_or_else(|| symbol.to_string())
}

fn entries(cx: &FileCx<'_>, items: &[UiItem], key: &mut u32) -> Vec<Entry> {
    items.iter().map(|item| entry(cx, item, key)).collect()
}

fn entry(cx: &FileCx<'_>, item: &UiItem, key: &mut u32) -> Entry {
    let text = |range: Option<TextRange>| range.map(|r| cx.text(r).to_owned()).unwrap_or_default();
    match item {
        UiItem::Node(node) => Entry::Node(self::node(cx, node, key)),
        UiItem::If(region) => Entry::Region(Region {
            kind: "if",
            head: String::new(),
            arms: region
                .arms
                .iter()
                .map(|arm| Arm {
                    head: text(arm.condition),
                    preserve: arm.preserve.clone(),
                    items: entries(cx, &arm.items, key),
                })
                .collect(),
            instance: region.instance,
            at: At::new(cx, region.origin),
        }),
        UiItem::For(region) => {
            let mut head = format!(
                "{} in {}",
                region.binding.as_deref().unwrap_or("_"),
                text(region.iterable)
            );
            if let Some(key) = region.key {
                let _ = write!(head, " key {}", cx.text(key));
            }
            Entry::Region(Region {
                kind: "for",
                head,
                arms: vec![Arm {
                    head: String::new(),
                    preserve: None,
                    items: entries(cx, &region.body, key),
                }],
                instance: region.instance,
                at: At::new(cx, region.origin),
            })
        }
        UiItem::Match(region) => Entry::Region(Region {
            kind: "match",
            head: text(region.scrutinee),
            arms: region
                .arms
                .iter()
                .map(|arm| {
                    let mut head = text(arm.pattern);
                    if let Some(guard) = arm.guard {
                        let _ = write!(head, " if {}", cx.text(guard));
                    }
                    Arm {
                        head,
                        preserve: None,
                        items: entries(cx, &arm.items, key),
                    }
                })
                .collect(),
            instance: region.instance,
            at: At::new(cx, region.origin),
        }),
    }
}

fn node(cx: &FileCx<'_>, node: &UiNode, key: &mut u32) -> Node {
    let own = *key;
    *key += 1;
    Node {
        key: own,
        type_name: node.type_name.clone(),
        local_name: node.local_name.clone(),
        kind: kind_name(node.kind),
        style: style(&node.style),
        bound: node
            .pending
            .iter()
            .map(|p| Bound {
                name: p.name.clone(),
                value: cx.text(p.value).to_owned(),
                dirty: classes(p.dirty),
                instance: p.instance,
                at: At::new(cx, p.value),
            })
            .collect(),
        handlers: node
            .handlers
            .iter()
            .map(|h| Handler {
                event: h.event.clone(),
                capture: h.capture,
                instance: h.instance,
                at: At::new(cx, h.origin),
            })
            .collect(),
        controls: node
            .control_reads
            .iter()
            .map(|(property, range, instance)| Control {
                property: property.clone(),
                value: cx.text(*range).to_owned(),
                instance: *instance,
                at: At::new(cx, *range),
            })
            .collect(),
        looks: node
            .styled
            .iter()
            .flat_map(|s| s.looks.iter().map(|l| l.property.clone()))
            .collect(),
        role: node
            .role
            .map(|r| viso_schema::view::source_name(r).to_owned()),
        live: node.live.map(|l| l.label().to_owned()),
        instance: node.instance,
        at: At::new(cx, node.origin),
        children: entries(cx, &node.children, key),
    }
}

fn kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Flex => "flex",
        NodeKind::Grid => "grid",
        NodeKind::Stack => "stack",
        NodeKind::Absolute => "absolute",
        NodeKind::Scroll => "scroll",
        NodeKind::VirtualList => "virtual_list",
        NodeKind::Leaf => "leaf",
        NodeKind::Component => "component",
    }
}

/// The folded style's set fields, by name, values as source spells them.
fn style(style: &StyleIr) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some(axis) = style.axis {
        let axis = match axis {
            AxisIr::Row => "row",
            AxisIr::Column => "column",
        };
        out.push(("axis", axis.to_owned()));
    }
    if let Some(justify) = style.justify {
        let justify = match justify {
            Justify::Start => "start",
            Justify::Center => "center",
            Justify::End => "end",
        };
        out.push(("justify", justify.to_owned()));
    }
    if let Some(align) = style.align {
        let align = match align {
            Align::Start => "start",
            Align::Center => "center",
            Align::End => "end",
            Align::Stretch => "stretch",
        };
        out.push(("align", align.to_owned()));
    }
    if let Some(width) = style.width {
        out.push(("width", length(width)));
    }
    if let Some(height) = style.height {
        out.push(("height", length(height)));
    }
    if let Some(gap) = style.gap {
        out.push(("gap", format!("{gap}dp")));
    }
    if let Some(p) = style.padding {
        out.push((
            "padding",
            format!("{}dp {}dp {}dp {}dp", p.top, p.right, p.bottom, p.left),
        ));
    }
    if let Some(lengths) = &style.lengths {
        let terms = [
            ("width", lengths.width),
            ("height", lengths.height),
            ("gap", lengths.gap),
            ("font_size", lengths.font_size),
        ];
        for (name, value) in terms {
            if let Some(value) = value {
                out.push((name, self::terms(value)));
            }
        }
        if let Some(p) = lengths.padding {
            let edges = [p.top, p.end, p.bottom, p.start].map(self::terms);
            out.push(("padding", edges.join(" ")));
        }
    }
    if let Some(scope) = style.scope {
        let basis = scope
            .basis
            .map_or_else(|| "parent".to_owned(), |b| format!("{b}dp"));
        out.push(("scope", basis));
    }
    if let Some(avoid) = style.avoid {
        let avoid = match avoid {
            Avoid::SafeArea => "safe_area",
            Avoid::Keyboard => "keyboard",
        };
        out.push(("avoid", avoid.to_owned()));
    }
    out
}

/// A length's terms, `0dp` when it has none: `100% - 16dp` as `100% + -16dp`.
fn terms(terms: TermsIr) -> String {
    let parts: Vec<String> = [
        (terms.pct * 100.0, "%"),
        (terms.dp, "dp"),
        (terms.px, "px"),
        (terms.sp, "sp"),
        (terms.em, "em"),
    ]
    .into_iter()
    .filter(|(value, _)| *value != 0.0)
    .map(|(value, unit)| format!("{value}{unit}"))
    .collect();
    if parts.is_empty() {
        "0dp".to_owned()
    } else {
        parts.join(" + ")
    }
}

fn length(length: LengthIr) -> String {
    match length {
        LengthIr::Fixed(dp) => format!("{dp}dp"),
        LengthIr::Relative { fixed, pct } => format!("{}% + {fixed}dp", pct * 100.0),
        LengthIr::Fill { weight } => format!("fill({weight})"),
        LengthIr::Fit => "fit".to_owned(),
    }
}

/// ` (in #i)` for an item of an inlined instance's view.
fn within(instance: u32) -> String {
    if instance == 0 {
        String::new()
    } else {
        format!(" (in #{instance})")
    }
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
}

fn write_entries(out: &mut String, entries: &[Entry], depth: usize) {
    for entry in entries {
        match entry {
            Entry::Node(node) => write_node(out, node, depth),
            Entry::Region(region) => write_region(out, region, depth),
        }
    }
}

fn write_node(out: &mut String, node: &Node, depth: usize) {
    indent(out, depth);
    let _ = write!(out, "n{} {}", node.key, node.type_name);
    if let Some(name) = &node.local_name {
        let _ = write!(out, " {name}");
    }
    let _ = writeln!(
        out,
        " {} @{}:{}{}",
        node.kind,
        node.at.line,
        node.at.column,
        within(node.instance)
    );
    if !node.style.is_empty() {
        indent(out, depth + 1);
        let fields: Vec<String> = node.style.iter().map(|(k, v)| format!("{k}={v}")).collect();
        let _ = writeln!(out, "style {}", fields.join(" "));
    }
    for bound in &node.bound {
        indent(out, depth + 1);
        let _ = writeln!(
            out,
            "bind {}: {}  {}{}",
            bound.name,
            bound.value,
            bound.dirty,
            within(bound.instance)
        );
    }
    for control in &node.controls {
        indent(out, depth + 1);
        let _ = writeln!(
            out,
            "control {}: {}{}",
            control.property,
            control.value,
            within(control.instance)
        );
    }
    for handler in &node.handlers {
        indent(out, depth + 1);
        let capture = if handler.capture { " capture" } else { "" };
        let _ = writeln!(
            out,
            "on {}{capture} @{}:{}{}",
            handler.event,
            handler.at.line,
            handler.at.column,
            within(handler.instance)
        );
    }
    if !node.looks.is_empty() {
        indent(out, depth + 1);
        let _ = writeln!(out, "looks [{}]", node.looks.join(", "));
    }
    if let Some(role) = &node.role {
        indent(out, depth + 1);
        let _ = writeln!(out, "role {role}");
    }
    if let Some(live) = &node.live {
        indent(out, depth + 1);
        let _ = writeln!(out, "live {live}");
    }
    write_entries(out, &node.children, depth + 1);
}

fn write_region(out: &mut String, region: &Region, depth: usize) {
    indent(out, depth);
    let head = if region.head.is_empty() {
        String::new()
    } else {
        format!(" {}", region.head)
    };
    let _ = writeln!(
        out,
        "{}{head} @{}:{}{}",
        region.kind,
        region.at.line,
        region.at.column,
        within(region.instance)
    );
    for arm in &region.arms {
        if region.kind == "for" {
            write_entries(out, &arm.items, depth + 1);
            continue;
        }
        indent(out, depth + 1);
        let head = match (region.kind, arm.head.is_empty()) {
            ("if", true) => "else".to_owned(),
            ("if", false) => format!("when {}", arm.head),
            _ => format!("arm {}", arm.head),
        };
        match &arm.preserve {
            Some(preserve) => {
                let _ = writeln!(out, "{head} preserve {preserve}");
            }
            None => {
                let _ = writeln!(out, "{head}");
            }
        }
        write_entries(out, &arm.items, depth + 2);
    }
}

fn json_entries(w: &mut JsonWriter, entries: &[Entry]) {
    w.begin_array();
    for entry in entries {
        match entry {
            Entry::Node(node) => json_node(w, node),
            Entry::Region(region) => json_region(w, region),
        }
    }
    w.end_array();
}

fn json_node(w: &mut JsonWriter, node: &Node) {
    w.begin_object();
    w.name("node");
    w.uint(u64::from(node.key));
    w.name("type");
    w.string(&node.type_name);
    w.name("local_name");
    match &node.local_name {
        Some(name) => w.string(name),
        None => w.null(),
    }
    w.name("kind");
    w.string(node.kind);
    w.name("instance");
    w.uint(u64::from(node.instance));
    node.at.write(w);
    w.name("style");
    w.begin_object();
    for (key, value) in &node.style {
        w.name(key);
        w.string(value);
    }
    w.end_object();
    w.name("bindings");
    w.begin_array();
    for bound in &node.bound {
        w.begin_object();
        w.name("property");
        w.string(&bound.name);
        w.name("value");
        w.string(&bound.value);
        w.name("invalidates");
        strings(w, bound.dirty.iter_names());
        w.name("instance");
        w.uint(u64::from(bound.instance));
        bound.at.write(w);
        w.end_object();
    }
    w.end_array();
    w.name("controls");
    w.begin_array();
    for control in &node.controls {
        w.begin_object();
        w.name("property");
        w.string(&control.property);
        w.name("value");
        w.string(&control.value);
        w.name("instance");
        w.uint(u64::from(control.instance));
        control.at.write(w);
        w.end_object();
    }
    w.end_array();
    w.name("handlers");
    w.begin_array();
    for handler in &node.handlers {
        w.begin_object();
        w.name("event");
        w.string(&handler.event);
        w.name("capture");
        w.bool(handler.capture);
        w.name("instance");
        w.uint(u64::from(handler.instance));
        handler.at.write(w);
        w.end_object();
    }
    w.end_array();
    w.name("looks");
    strings(w, node.looks.iter().map(String::as_str));
    w.name("role");
    match &node.role {
        Some(role) => w.string(role),
        None => w.null(),
    }
    w.name("live");
    match &node.live {
        Some(live) => w.string(live),
        None => w.null(),
    }
    w.name("children");
    json_entries(w, &node.children);
    w.end_object();
}

fn json_region(w: &mut JsonWriter, region: &Region) {
    w.begin_object();
    w.name("region");
    w.string(region.kind);
    w.name("head");
    w.string(&region.head);
    w.name("instance");
    w.uint(u64::from(region.instance));
    region.at.write(w);
    w.name("arms");
    w.begin_array();
    for arm in &region.arms {
        w.begin_object();
        w.name("head");
        w.string(&arm.head);
        w.name("preserve");
        match &arm.preserve {
            Some(preserve) => w.string(preserve),
            None => w.null(),
        }
        w.name("items");
        json_entries(w, &arm.items);
        w.end_object();
    }
    w.end_array();
    w.end_object();
}

fn write_view_name(out: &mut String, view: Option<&str>) {
    match view {
        Some(view) => {
            let _ = writeln!(out, "view {view}");
        }
        None => out.push_str("no view\n"),
    }
}

fn json_view_name(w: &mut JsonWriter, view: Option<&str>) {
    w.name("view");
    match view {
        Some(view) => w.string(view),
        None => w.null(),
    }
}

impl Dump for UiView {
    fn write_text(&self, out: &mut String) {
        write_view_name(out, self.view.as_deref());
        write_entries(out, &self.items, 1);
        for instance in &self.instances {
            let regional = if instance.regional { " regional" } else { "" };
            let _ = writeln!(
                out,
                "instance #{} {} {} parent=#{} depth={}{regional} @{}:{}",
                instance.index,
                instance.component,
                instance.identity,
                instance.parent,
                instance.depth,
                instance.at.line,
                instance.at.column
            );
        }
    }

    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        json_view_name(w, self.view.as_deref());
        w.name("items");
        json_entries(w, &self.items);
        w.name("instances");
        w.begin_array();
        for instance in &self.instances {
            w.begin_object();
            w.name("instance");
            w.uint(u64::from(instance.index));
            w.name("component");
            w.string(&instance.component);
            w.name("identity");
            w.string(&instance.identity);
            w.name("parent");
            w.uint(u64::from(instance.parent));
            w.name("depth");
            w.uint(u64::from(instance.depth));
            w.name("regional");
            w.bool(instance.regional);
            instance.at.write(w);
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
}

/// A reactive source of the view.
struct ReactiveSource {
    name: String,
    kind: &'static str,
    symbol: SymbolId,
    /// What a write to it invalidates; empty when the view never reads it.
    invalidates: DirtyClass,
}

/// One binding edge.
struct Edge {
    source: String,
    node: u32,
    node_type: String,
    property: String,
    class: DirtyClass,
    dynamic: bool,
}

/// The reactive IR of the view a file mounts.
pub(super) struct ReactiveView {
    view: Option<String>,
    sources: Vec<ReactiveSource>,
    edges: Vec<Edge>,
    dynamic_fallback_nodes: u32,
}

pub(super) fn reactive(cx: &FileCx<'_>, compiled: &Compiled) -> ReactiveView {
    let mut names = super::hir::names(&cx.package.hir);
    let declared = compiled.sources.iter().chain(&compiled.regional);
    for source in declared.clone() {
        names.insert(source.symbol, source.name.clone());
    }
    let invalidations: HashMap<SymbolId, DirtyClass> = compiled
        .invalidations
        .iter()
        .map(|&(symbol, class)| (symbol, classes(class)))
        .collect();
    let mut sources: Vec<ReactiveSource> = declared
        .map(|source| ReactiveSource {
            name: source.name.clone(),
            kind: match source.kind {
                SourceKind::State { .. } => "state",
                SourceKind::Input => "input",
                SourceKind::Computed => "computed",
            },
            symbol: source.symbol,
            invalidates: invalidations
                .get(&source.symbol)
                .copied()
                .unwrap_or(DirtyClass::EMPTY),
        })
        .collect();
    for &(symbol, class) in &compiled.invalidations {
        if !sources.iter().any(|s| s.symbol == symbol) {
            sources.push(ReactiveSource {
                name: name_of(&names, symbol),
                kind: "instance",
                symbol,
                invalidates: classes(class),
            });
        }
    }
    let types = node_types(&compiled.tree);
    let edges = compiled
        .bindings
        .edges
        .iter()
        .map(|edge| Edge {
            source: name_of(&names, edge.source),
            node: edge.node.0,
            node_type: types.get(edge.node.0 as usize).cloned().unwrap_or_default(),
            property: edge.property.clone(),
            class: classes(edge.class),
            dynamic: edge.kind == BindingKind::Dynamic,
        })
        .collect();
    ReactiveView {
        view: view_name(compiled),
        sources,
        edges,
        dynamic_fallback_nodes: compiled.bindings.dynamic_fallback_nodes,
    }
}

/// The type of each node of `tree`, by pre-order key.
fn node_types(tree: &UiTree) -> Vec<String> {
    fn walk(items: &[UiItem], out: &mut Vec<String>) {
        for item in items {
            match item {
                UiItem::Node(node) => {
                    out.push(node.type_name.clone());
                    walk(&node.children, out);
                }
                UiItem::If(region) => region.arms.iter().for_each(|a| walk(&a.items, out)),
                UiItem::For(region) => walk(&region.body, out),
                UiItem::Match(region) => region.arms.iter().for_each(|a| walk(&a.items, out)),
            }
        }
    }
    let mut out = Vec::new();
    walk(&tree.items, &mut out);
    out
}

impl Dump for ReactiveView {
    fn write_text(&self, out: &mut String) {
        write_view_name(out, self.view.as_deref());
        if self.view.is_none() {
            return;
        }
        for source in &self.sources {
            let _ = write!(out, "  {} {} #{}", source.kind, source.name, source.symbol);
            if source.invalidates.is_empty() {
                out.push_str("  unread\n");
            } else {
                let _ = writeln!(out, "  invalidates {}", source.invalidates);
            }
        }
        for edge in &self.edges {
            let kind = if edge.dynamic { "  dynamic" } else { "" };
            let _ = writeln!(
                out,
                "  {} -> n{} {}.{}  {}{kind}",
                edge.source, edge.node, edge.node_type, edge.property, edge.class
            );
        }
        let _ = writeln!(
            out,
            "  dynamic_fallback_nodes {}",
            self.dynamic_fallback_nodes
        );
    }

    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        json_view_name(w, self.view.as_deref());
        w.name("sources");
        w.begin_array();
        for source in &self.sources {
            w.begin_object();
            w.name("name");
            w.string(&source.name);
            w.name("kind");
            w.string(source.kind);
            w.name("symbol_id");
            w.string(&source.symbol.to_string());
            w.name("invalidates");
            strings(w, source.invalidates.iter_names());
            w.end_object();
        }
        w.end_array();
        w.name("edges");
        w.begin_array();
        for edge in &self.edges {
            w.begin_object();
            w.name("source");
            w.string(&edge.source);
            w.name("node");
            w.uint(u64::from(edge.node));
            w.name("node_type");
            w.string(&edge.node_type);
            w.name("property");
            w.string(&edge.property);
            w.name("invalidates");
            strings(w, edge.class.iter_names());
            w.name("kind");
            w.string(if edge.dynamic { "dynamic" } else { "static" });
            w.end_object();
        }
        w.end_array();
        w.name("dynamic_fallback_nodes");
        w.uint(u64::from(self.dynamic_fallback_nodes));
        w.end_object();
    }
}
