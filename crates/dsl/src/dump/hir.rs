//! The typed HIR of a file: each component's and system's members and each
//! module function, with the node contract of each: type, effect,
//! ownership, capabilities, the reactive sources it reads and its constant.

use std::collections::HashMap;
use std::fmt::Write as _;

use viso_behavior::native::{Natives, SlotCardinality};
use viso_ende::JsonWriter;

use super::{Dump, FileCx, strings};
use crate::ast::{AstNode, CompilationUnit};
use crate::hir::{
    CallableKind, ConstValue, EffectClass, HirComponent, HirMeta, LoweredPackage, OwnershipMode,
    VariantPayload,
};
use crate::resolve::SymbolId;
use crate::syntax::{SyntaxKind, TextRange};

/// One member, or one module function, with its node contract.
struct Member {
    /// `input`, `state`, `computed`, `event`, `slot`, `fn`, `action`, `task`
    /// or `handler`.
    kind: &'static str,
    name: String,
    /// The type, an event's payload, a slot's cardinality.
    ty: String,
    symbol: Option<SymbolId>,
    effect: &'static str,
    ownership: &'static str,
    capabilities: Vec<String>,
    reads: Vec<String>,
    constant: Option<String>,
    range: TextRange,
    at: (u32, u32),
}

/// A component, a system, a template, or the module's own functions.
struct Item {
    /// `component`, `system`, `template` or `module`.
    kind: &'static str,
    name: String,
    symbol: Option<SymbolId>,
    range: TextRange,
    at: (u32, u32),
    members: Vec<Member>,
}

/// The HIR of one file.
pub(super) struct HirFile {
    items: Vec<Item>,
}

pub(super) fn file(cx: &FileCx<'_>) -> HirFile {
    let package = cx.package;
    let hir = &package.hir;
    let natives = package.model.graph.natives();
    let Some(module) = package.model.module_of(cx.index) else {
        return HirFile { items: Vec::new() };
    };
    let own = |symbol: SymbolId| module.decls.iter().any(|d| d.id == symbol);
    let names = names(hir);
    let describe = |meta: &HirMeta| hir.types.describe_in(&meta.inferred_type, natives);
    let member = |kind, name: &str, ty: String, meta: &HirMeta| Member {
        kind,
        name: name.to_owned(),
        ty,
        symbol: meta.resolved_symbol,
        effect: effect(meta.effect_class),
        ownership: ownership(meta.ownership_mode),
        capabilities: meta.capability_set.iter().map(str::to_owned).collect(),
        reads: meta
            .reactive_reads
            .iter()
            .map(|s| names.get(s).cloned().unwrap_or_else(|| s.to_string()))
            .collect(),
        constant: meta.constant_value.as_ref().map(constant),
        range: meta.source_origin,
        at: cx.position(meta.source_origin),
    };
    let mut items = Vec::new();
    for component in hir.components.iter().filter(|c| own(c.schema.symbol)) {
        if !cx.keeps(Some(component.schema.symbol)) {
            continue;
        }
        let schema = &component.schema;
        let mut members = Vec::new();
        for input in &schema.inputs {
            members.push(member(
                "input",
                &input.name,
                describe(&input.meta),
                &input.meta,
            ));
        }
        for state in &schema.states {
            members.push(member(
                "state",
                &state.name,
                describe(&state.meta),
                &state.meta,
            ));
        }
        for computed in &schema.computeds {
            members.push(member(
                "computed",
                &computed.name,
                describe(&computed.meta),
                &computed.meta,
            ));
        }
        for event in &schema.events {
            let payload = payload(hir, natives, event.meta.resolved_symbol);
            members.push(member("event", &event.name, payload, &event.meta));
        }
        for slot in &schema.slots {
            let cardinality = match slot.cardinality {
                SlotCardinality::One => "one",
                SlotCardinality::Optional => "optional",
                SlotCardinality::Many => "many",
            };
            let ty = if slot.default {
                format!("{cardinality} default")
            } else {
                cardinality.to_owned()
            };
            members.push(Member {
                kind: "slot",
                name: slot.name.clone(),
                ty,
                symbol: None,
                effect: "pure",
                ownership: "none",
                capabilities: Vec::new(),
                reads: Vec::new(),
                constant: None,
                range: slot.declared_at,
                at: cx.position(slot.declared_at),
            });
        }
        for callable in &schema.callables {
            members.push(member(
                callable_kind(callable.kind),
                &callable.name,
                signature(cx, callable.meta.source_origin),
                &callable.meta,
            ));
        }
        if let Some(focus) = &cx.focus
            && let Some(name) = &focus.member
        {
            members.retain(|m| &m.name == name);
        }
        items.push(Item {
            kind: declaration_kind(cx, component),
            name: schema.name.clone(),
            symbol: Some(schema.symbol),
            range: component.source_origin,
            at: cx.position(component.source_origin),
            members,
        });
    }
    let plain = |kind, name: &str, ty: String, range: TextRange| Member {
        kind,
        name: name.to_owned(),
        ty,
        symbol: None,
        effect: "pure",
        ownership: "none",
        capabilities: Vec::new(),
        reads: Vec::new(),
        constant: None,
        range,
        at: cx.position(range),
    };
    let ty = |ty: &crate::hir::Ty| hir.types.describe_in(ty, natives);
    let types: Vec<TextRange> = CompilationUnit::cast(cx.root.clone())
        .map(|cu| super::declarations(&cu))
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.kind == "record" || d.kind == "enum")
        .map(|d| d.range)
        .collect();
    let declared = |d: &&crate::resolve::SymbolDecl| {
        types.iter().any(|r| r.contains_range(d.name_range)) && cx.keeps(Some(d.id))
    };
    for decl in module.decls.iter().filter(declared) {
        let (kind, members): (&'static str, Vec<Member>) =
            if let Some(fields) = hir.types.records.get(&decl.id) {
                let fields = fields
                    .iter()
                    .map(|f| plain("field", &f.name, ty(&f.ty), f.declared_at))
                    .collect();
                ("record", fields)
            } else if let Some(variants) = hir.types.enums.get(&decl.id) {
                let variants = variants
                    .iter()
                    .map(|v| {
                        let payload = match &v.payload {
                            VariantPayload::Unit => String::new(),
                            VariantPayload::Tuple(tys) => {
                                let tys: Vec<String> = tys.iter().map(ty).collect();
                                format!("({})", tys.join(", "))
                            }
                            VariantPayload::Record(fields) => {
                                let fields: Vec<String> = fields
                                    .iter()
                                    .map(|f| format!("{}: {}", f.name, ty(&f.ty)))
                                    .collect();
                                format!("{{ {} }}", fields.join("; "))
                            }
                        };
                        plain("variant", &v.name, payload, v.declared_at)
                    })
                    .collect();
                ("enum", variants)
            } else {
                continue;
            };
        items.push(Item {
            kind,
            name: cx.text(decl.name_range).to_owned(),
            symbol: Some(decl.id),
            range: decl.name_range,
            at: cx.position(decl.name_range),
            members,
        });
    }
    let functions: Vec<Member> = hir
        .callables
        .iter()
        .filter(|c| c.meta.resolved_symbol.is_some_and(own))
        .filter(|c| cx.keeps(c.meta.resolved_symbol))
        .map(|c| {
            let ty = signature(cx, c.meta.source_origin);
            member(callable_kind(c.kind), &c.name, ty, &c.meta)
        })
        .collect();
    if !functions.is_empty() {
        items.push(Item {
            kind: "module",
            name: cx.module.clone(),
            symbol: None,
            range: cx.root.text_range(),
            at: (1, 1),
            members: functions,
        });
    }
    HirFile { items }
}

/// The source name of every member and declaration of `hir`, by symbol.
pub(super) fn names(hir: &LoweredPackage) -> HashMap<SymbolId, String> {
    let mut names: HashMap<SymbolId, String> = hir.types.names.clone();
    for component in &hir.components {
        let schema = &component.schema;
        names.insert(schema.symbol, schema.name.clone());
        let members = schema
            .inputs
            .iter()
            .map(|m| (&m.name, &m.meta))
            .chain(schema.states.iter().map(|m| (&m.name, &m.meta)))
            .chain(schema.computeds.iter().map(|m| (&m.name, &m.meta)))
            .chain(schema.callables.iter().map(|m| (&m.name, &m.meta)));
        for (name, meta) in members {
            if let Some(symbol) = meta.resolved_symbol {
                names.insert(symbol, name.clone());
            }
        }
    }
    names
}

/// `(name: Type, …)` of the event `symbol`, `()` when it carries nothing.
fn payload(hir: &LoweredPackage, natives: &Natives, symbol: Option<SymbolId>) -> String {
    let fields = symbol.and_then(|s| hir.types.records.get(&s));
    let fields: Vec<String> = fields
        .into_iter()
        .flatten()
        .map(|f| format!("{}: {}", f.name, hir.types.describe_in(&f.ty, natives)))
        .collect();
    format!("({})", fields.join(", "))
}

/// `(params) -> Return` of the callable declared at `range`, as source
/// spells it, whitespace collapsed; `-> Unit` when it declares no return.
fn signature(cx: &FileCx<'_>, range: TextRange) -> String {
    let Some(node) = cx
        .root
        .descendants()
        .into_iter()
        .find(|node| node.text_range() == range)
    else {
        return String::new();
    };
    let child = |kind| {
        node.children()
            .into_iter()
            .find(|c| c.kind() == kind)
            .map(|c| {
                cx.text(c.text_range())
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            })
    };
    let params = child(SyntaxKind::ParamList).unwrap_or_else(|| "()".to_owned());
    let ret = child(SyntaxKind::ReturnType).unwrap_or_else(|| "-> Unit".to_owned());
    format!("{params} {ret}")
}

/// `component`, `system` or `template`, as the declaration is written.
fn declaration_kind(cx: &FileCx<'_>, component: &HirComponent) -> &'static str {
    let kind = cx
        .root
        .descendants()
        .into_iter()
        .find(|node| node.text_range() == component.source_origin)
        .map(|node| node.kind());
    match kind {
        Some(SyntaxKind::SystemDecl) => "system",
        Some(SyntaxKind::TemplateDecl) => "template",
        _ => "component",
    }
}

fn effect(effect: EffectClass) -> &'static str {
    match effect {
        EffectClass::Pure => "pure",
        EffectClass::Read => "read",
        EffectClass::Action => "action",
        EffectClass::Task => "task",
    }
}

fn ownership(mode: OwnershipMode) -> &'static str {
    match mode {
        OwnershipMode::Owned => "owned",
        OwnershipMode::Derived => "derived",
        OwnershipMode::Borrowed => "borrowed",
        OwnershipMode::None => "none",
    }
}

fn callable_kind(kind: CallableKind) -> &'static str {
    match kind {
        CallableKind::Fn => "fn",
        CallableKind::Action => "action",
        CallableKind::Task => "task",
        CallableKind::EventHandler => "handler",
    }
}

fn constant(value: &ConstValue) -> String {
    match value {
        ConstValue::Bool(b) => b.to_string(),
        ConstValue::Int(n, _) => n.to_string(),
        ConstValue::Float(x, _) => x.to_string(),
        ConstValue::Str(s) => super::quote(s),
    }
}

impl Dump for HirFile {
    fn write_text(&self, out: &mut String) {
        for item in &self.items {
            let _ = write!(
                out,
                "{} {} @{}:{}",
                item.kind, item.name, item.at.0, item.at.1
            );
            if let Some(symbol) = item.symbol {
                let _ = write!(out, " #{symbol}");
            }
            out.push('\n');
            for m in &item.members {
                let name = if m.name.is_empty() { "_" } else { &m.name };
                let _ = write!(
                    out,
                    "  {} {name}: {}  {} {}",
                    m.kind, m.ty, m.effect, m.ownership
                );
                if !m.capabilities.is_empty() {
                    let _ = write!(out, "  requires [{}]", m.capabilities.join(", "));
                }
                if !m.reads.is_empty() {
                    let _ = write!(out, "  reads [{}]", m.reads.join(", "));
                }
                if let Some(constant) = &m.constant {
                    let _ = write!(out, "  = {constant}");
                }
                let _ = writeln!(out, "  @{}:{}", m.at.0, m.at.1);
            }
        }
    }

    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("items");
        w.begin_array();
        for item in &self.items {
            w.begin_object();
            w.name("kind");
            w.string(item.kind);
            w.name("name");
            w.string(&item.name);
            w.name("symbol_id");
            match item.symbol {
                Some(symbol) => w.string(&symbol.to_string()),
                None => w.null(),
            }
            span(w, item.range, item.at);
            w.name("members");
            w.begin_array();
            for m in &item.members {
                w.begin_object();
                w.name("kind");
                w.string(m.kind);
                w.name("name");
                w.string(&m.name);
                w.name("type");
                w.string(&m.ty);
                w.name("symbol_id");
                match m.symbol {
                    Some(symbol) => w.string(&symbol.to_string()),
                    None => w.null(),
                }
                w.name("effect");
                w.string(m.effect);
                w.name("ownership");
                w.string(m.ownership);
                w.name("capabilities");
                strings(w, m.capabilities.iter().map(String::as_str));
                w.name("reads");
                strings(w, m.reads.iter().map(String::as_str));
                w.name("constant");
                match &m.constant {
                    Some(constant) => w.string(constant),
                    None => w.null(),
                }
                span(w, m.range, m.at);
                w.end_object();
            }
            w.end_array();
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
}

/// A `span` member from a range and the position of its start.
pub(super) fn span(w: &mut JsonWriter, range: TextRange, at: (u32, u32)) {
    w.name("span");
    w.begin_object();
    w.name("start");
    w.uint(u64::from(range.start().to_u32()));
    w.name("end");
    w.uint(u64::from(range.end().to_u32()));
    w.name("line");
    w.uint(u64::from(at.0));
    w.name("column_utf16");
    w.uint(u64::from(at.1));
    w.end_object();
}
