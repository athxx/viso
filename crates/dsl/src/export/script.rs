//! The page script of an export: each Behavior IR function the page
//! runs, translated instruction by instruction to JavaScript with the value
//! representation the interpreter has — integers as `BigInt` checked against
//! their type's range, floats as numbers (an `F32` rounded after every
//! operation), aggregates as `{t, f}`, lists as arrays written copy on write —
//! and the source locale's messages for what it translates, then the mount
//! of each content, wiring its elements to the entries and
//! handlers it names, and a runtime that commits each handler as one
//! transaction and re-evaluates only what reads a state it changed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use viso_behavior::Value;
use viso_behavior::i18n::{CaseKey, Catalog, Category, PatternOp, Table};
use viso_view::ControlKind;

use super::eval::{Deps, Eval};
use super::html::{Content, Lowered, Prop, RegionKind, VarToken, Wire};
use super::{Finding, Need};
use crate::behavior::ir::{
    BinaryOp, Const, DisplayKind, FuncId, Inst, Num, PathStep, Program, UnaryOp,
};
use crate::syntax::TextRange;

/// The runtime every page script carries: the interpreter's value semantics,
/// what DOM elements show and the payloads of their events, then the page's
/// transactions and regions.
const RUNTIME: &str = concat!(
    include_str!("values.js"),
    include_str!("dom.js"),
    include_str!("page.js")
);

/// The page script of `page`, or why one of its functions has no JavaScript
/// form.
pub(super) fn script(
    page: &Lowered,
    eval: &Eval<'_>,
    catalog: Option<&Catalog>,
    header: &str,
    origin: TextRange,
) -> Result<String, Vec<Finding>> {
    let program = eval.program;
    let mut contents: Vec<&Content> = vec![&page.root];
    let mut arms = Vec::with_capacity(page.regions.len());
    for region in &page.regions {
        let first = contents.len();
        contents.extend(region.arms.iter());
        arms.push(first..contents.len());
    }

    let roots = roots(page, eval);
    let entry = |e: u32| eval.func(e);
    let functions = reachable(program, roots);

    let mut out = String::new();
    let _ = writeln!(out, "// {header}");
    out.push_str("\"use strict\";\n(() => {\n");
    let inputs: Result<Vec<String>, String> = eval.inputs().iter().map(literal).collect();
    let states: Result<Vec<String>, String> = eval.states().iter().map(literal).collect();
    let (inputs, states) = match (inputs, states) {
        (Ok(inputs), Ok(states)) => (inputs, states),
        (Err(why), _) | (_, Err(why)) => {
            return Err(vec![Finding {
                need: Need::Unsupported,
                at: origin,
                message: format!("the component starts with {why}, which a page cannot hold"),
                notes: Vec::new(),
            }]);
        }
    };
    let _ = writeln!(out, "const I = [{}];", inputs.join(", "));
    let _ = writeln!(out, "let S = [{}];", states.join(", "));
    out.push_str("let W = S;\n");
    out.push_str("const A = new Map([");
    for (at, (id, path)) in page.assets.iter().enumerate() {
        if at > 0 {
            out.push_str(", ");
        }
        let _ = write!(
            out,
            "[\"{id}\", {}]",
            string(&format!("assets/{}", super::html::url_path(path)))
        );
    }
    out.push_str("]);\n");
    let events: Vec<String> = eval.layout.events.iter().map(|e| string(e)).collect();
    let _ = writeln!(out, "const E = [{}];", events.join(", "));
    let translates = functions.iter().any(|&id| {
        program.function(id).body.as_ref().is_ok_and(|body| {
            body.insts
                .iter()
                .any(|i| matches!(i, Inst::Translate { .. }))
        })
    });
    let messages = messages(catalog.filter(|_| translates)).map_err(|why| {
        vec![Finding {
            need: Need::Unsupported,
            at: origin,
            message: format!("the message catalog does not read: {why}"),
            notes: Vec::new(),
        }]
    })?;
    let _ = writeln!(out, "const T = {messages};");
    out.push_str(RUNTIME);

    let mut findings = Vec::new();
    for id in &functions {
        match function(program, *id, origin, Dialect::Page) {
            Ok(code) => out.push_str(&code),
            Err(mut found) => findings.append(&mut found),
        }
    }
    if !findings.is_empty() {
        return Err(findings);
    }

    out.push_str(if page.regions.is_empty() {
        "const G = ["
    } else {
        "const G = [\n"
    });
    for (index, region) in page.regions.iter().enumerate() {
        let deps = region_entries(region.kind)
            .into_iter()
            .map(|e| eval.deps(e).0)
            .fold(Deps::Slots(Vec::new()), union);
        let mounts: Vec<String> = arms[index].clone().map(|c| format!("m{c}")).collect();
        let keep: Vec<&str> = region
            .preserve
            .iter()
            .map(|k| if *k { "true" } else { "false" })
            .collect();
        let call = |e: u32, extra: bool| {
            let f = entry(e).0;
            if extra {
                format!("(sc, v) => f{f}(...sc, v)")
            } else {
                format!("(sc) => f{f}(...sc)")
            }
        };
        let kind = match region.kind {
            RegionKind::If { select } => format!("k: 0, sel: {}", call(select, false)),
            RegionKind::Match { scrutinee, select } => format!(
                "k: 1, scr: {}, sel: {}",
                call(scrutinee, false),
                call(select, true)
            ),
            RegionKind::For { items, key } => format!(
                "k: 2, items: {}, key: {}",
                call(items, false),
                key.map_or("null".to_owned(), |k| call(k, true))
            ),
        };
        let _ = writeln!(
            out,
            "  {{ {kind}, arms: [{}], keep: [{}], deps: {} }},",
            mounts.join(", "),
            keep.join(", "),
            deps_literal(&deps)
        );
    }
    out.push_str("];\n");

    for (index, content) in contents.iter().enumerate() {
        mount(&mut out, index, content, eval);
    }
    out.push_str("start(m0);\n})();\n");
    Ok(out)
}

/// The entries that decide a region's content.
pub(super) fn region_entries(kind: RegionKind) -> Vec<u32> {
    match kind {
        RegionKind::If { select } => vec![select],
        RegionKind::Match { scrutinee, select } => vec![scrutinee, select],
        RegionKind::For { items, key } => [Some(items), key].into_iter().flatten().collect(),
    }
}

fn union(a: Deps, b: Deps) -> Deps {
    match (a, b) {
        (Deps::Slots(mut a), Deps::Slots(b)) => {
            a.extend(b);
            a.sort_unstable();
            a.dedup();
            Deps::Slots(a)
        }
        _ => Deps::Any,
    }
}

fn deps_literal(deps: &Deps) -> String {
    match deps {
        Deps::Any => "null".to_owned(),
        Deps::Slots(slots) => {
            let slots: Vec<String> = slots.iter().map(u32::to_string).collect();
            format!("[{}]", slots.join(", "))
        }
    }
}

/// The mount of content `index`: its marked elements wired in document
/// order, with `sc` its region bindings.
fn mount(out: &mut String, index: usize, content: &Content, eval: &Eval<'_>) {
    let _ = writeln!(out, "function m{index}(e, sc) {{\n  const u = [], r = [];");
    let call = |entry: u32| format!("f{}(...sc)", eval.func(entry).0);
    for (at, mark) in content.marks.iter().enumerate() {
        let el = format!("e[{at}]");
        for wire in &mark.wires {
            match wire {
                Wire::Prop { entry, prop } => {
                    let deps = deps_literal(&eval.deps(*entry).0);
                    let value = call(*entry);
                    let set = match prop {
                        Prop::Text => format!("P.text({el}, {value})"),
                        Prop::TextColor => format!("P.color({el}, {value})"),
                        Prop::SoftWrap => format!("P.wrap({el}, {value})"),
                        Prop::Visible => format!("P.visible({el}, {value})"),
                        Prop::Focusable => format!("P.focusable({el}, {value})"),
                        Prop::Label { alt } => format!("P.label({el}, {value}, {alt})"),
                        Prop::Hint => format!("P.hint({el}, {value})"),
                        Prop::Spoken => format!("P.spoken({el}, {value})"),
                        Prop::Hidden => format!("P.hidden({el}, {value})"),
                        Prop::HeadingLevel => format!("P.level({el}, {value})"),
                        Prop::Checked => format!("P.checked({el}, {value})"),
                        Prop::Range(name) => format!("P.range({el}, {value}, \"{name}\")"),
                        Prop::InputValue => format!("P.input({el}, {value})"),
                        Prop::Selected { radio } => format!("P.selected({el}, {value}, {radio})"),
                        Prop::Source { mask } => format!("P.source({el}, {value}, {mask})"),
                        Prop::Fit { mask } => format!("P.fit({el}, {value}, {mask})"),
                        Prop::Var { name, token } => match token {
                            VarToken::Color => format!("P.vcolor({el}, {value}, \"{name}\")"),
                            VarToken::Opacity => {
                                format!("P.vopacity({el}, {value}, \"{name}\")")
                            }
                            VarToken::Transition(property) => {
                                format!("P.vmove({el}, {value}, \"{name}\", \"{property}\")")
                            }
                        },
                        Prop::Class(name) => format!("P.cls({el}, {value}, \"{name}\")"),
                    };
                    let _ = writeln!(out, "  u.push([{deps}, () => {set}]);");
                }
                Wire::On {
                    route,
                    handler,
                    capture,
                    kind,
                } => {
                    let f = eval.func(*handler).0;
                    let _ = writeln!(
                        out,
                        "  on({el}, {}, {capture}, {}, (p) => f{f}(p, ...sc));",
                        *route as u8, *kind as u8
                    );
                }
                Wire::Sync { kind, entry } => {
                    let value = entry.map_or("null".to_owned(), |e| format!("() => {}", call(e)));
                    if *kind == ControlKind::Select {
                        let _ = writeln!(
                            out,
                            "  {el}.$radio = {el}.getAttribute(\"role\") === \"radiogroup\";"
                        );
                    }
                    let _ = writeln!(
                        out,
                        "  {el}.$sync = () => sync({el}, {}, {value});",
                        *kind as u8
                    );
                }
                Wire::Region(region) => {
                    let _ = writeln!(out, "  r.push(region({el}, G[{region}], sc));");
                }
            }
        }
    }
    out.push_str("  return { u, r };\n}\n");
}

/// The entries and handlers `page` names, which its script calls.
pub(super) fn roots(page: &Lowered, eval: &Eval<'_>) -> BTreeSet<FuncId> {
    let mut roots = BTreeSet::new();
    let contents = std::iter::once(&page.root).chain(page.regions.iter().flat_map(|r| &r.arms));
    for content in contents {
        for mark in &content.marks {
            for wire in &mark.wires {
                match *wire {
                    Wire::Prop { entry, .. }
                    | Wire::Sync {
                        entry: Some(entry), ..
                    } => {
                        roots.insert(eval.func(entry));
                    }
                    Wire::On { handler, .. } => {
                        roots.insert(eval.func(handler));
                    }
                    Wire::Sync { entry: None, .. } | Wire::Region(_) => {}
                }
            }
        }
    }
    for region in &page.regions {
        for entry in region_entries(region.kind) {
            roots.insert(eval.func(entry));
        }
    }
    roots
}

/// Every function `roots` call, transitively, ascending.
pub(super) fn reachable(program: &Program, roots: BTreeSet<FuncId>) -> BTreeSet<FuncId> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<FuncId> = roots.into_iter().collect();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Ok(body) = &program.function(id).body {
            for inst in &body.insts {
                match inst {
                    Inst::Call { func, .. } | Inst::Closure { func, .. } => stack.push(*func),
                    _ => {}
                }
            }
        }
    }
    seen
}

/// The messages a script translates with, as an object literal: the
/// catalog's source locale `L`, each message's pattern in it `M` by id, and
/// the id of each key `MK`; none without a catalog.
pub(super) fn messages(catalog: Option<&Catalog>) -> Result<String, String> {
    let Some(catalog) = catalog else {
        return Ok("{ L: \"\", M: [], MK: new Map() }".to_owned());
    };
    let mut out = String::new();
    let source = catalog.locales().next().unwrap_or_default();
    let _ = write!(out, "{{ L: {}, M: [", string(source));
    let mut tables: Vec<Option<Table>> = catalog.locales().map(|_| None).collect();
    let own = catalog.table(0).map_err(|e| e.0)?;
    for id in 0..catalog.messages().len() {
        let entry = own.entries[id];
        let origin = usize::from(entry.origin);
        let table = if origin == 0 {
            &own
        } else {
            if tables[origin].is_none() {
                tables[origin] = Some(catalog.table(origin).map_err(|e| e.0)?);
            }
            tables[origin].as_ref().unwrap_or(&own)
        };
        let start = entry.start as usize;
        let ops = &table.ops[start..start + entry.len as usize];
        if id > 0 {
            out.push_str(", ");
        }
        match ops {
            [] => out.push_str("\"\""),
            [PatternOp::Text { start, len }] => out.push_str(&string(slice(table, *start, *len))),
            _ => out.push_str(&pattern(table, ops)),
        }
    }
    out.push_str("], MK: new Map([");
    for (id, message) in catalog.messages().iter().enumerate() {
        if id > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "[{}, {id}]", string(&message.key));
    }
    out.push_str("]) }");
    Ok(out)
}

fn slice(table: &Table, start: u32, len: u32) -> &str {
    &table.text[start as usize..(start + len) as usize]
}

/// A run of pattern ops as the array `$.tr` formats: text as a string,
/// `[0, a]` an argument, `[1, a]` a number, `[2]` the plural's number,
/// `[3, a, ordinal, offset, [[n, body]], {category: body}, other]` a plural,
/// `[4, a, [[text, body]], other]` a select.
fn pattern(table: &Table, ops: &[PatternOp]) -> String {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < ops.len() {
        match ops[i] {
            PatternOp::Text { start, len } => parts.push(string(slice(table, start, len))),
            PatternOp::Arg(a) => parts.push(format!("[0, {a}]")),
            PatternOp::Number(a) => parts.push(format!("[1, {a}]")),
            PatternOp::Pound => parts.push("[2]".to_owned()),
            PatternOp::Plural {
                arg,
                ordinal,
                offset,
                cases,
            } => {
                let (choices, next) = choices(table, ops, i, cases);
                let mut exact = Vec::new();
                let mut categories = Vec::new();
                let mut other = "[]".to_owned();
                for (key, body) in choices {
                    match key {
                        CaseKey::Exact(n) => exact.push(format!("[{}, {body}]", float(n))),
                        CaseKey::Category(c) => categories.push(format!(
                            "{}: {body}",
                            match c {
                                Category::Zero => "zero",
                                Category::One => "one",
                                Category::Two => "two",
                                Category::Few => "few",
                                Category::Many => "many",
                            }
                        )),
                        CaseKey::Other => other = body,
                        CaseKey::Text { .. } => {}
                    }
                }
                parts.push(format!(
                    "[3, {arg}, {}, {}, [{}], {{ {} }}, {other}]",
                    u8::from(ordinal),
                    float(offset),
                    exact.join(", "),
                    categories.join(", ")
                ));
                i = next;
                continue;
            }
            PatternOp::Select { arg, cases } => {
                let (choices, next) = choices(table, ops, i, cases);
                let mut texts = Vec::new();
                let mut other = "[]".to_owned();
                for (key, body) in choices {
                    match key {
                        CaseKey::Text { start, len } => {
                            texts.push(format!("[{}, {body}]", string(slice(table, start, len))));
                        }
                        CaseKey::Other => other = body,
                        _ => {}
                    }
                }
                parts.push(format!("[4, {arg}, [{}], {other}]", texts.join(", ")));
                i = next;
                continue;
            }
            PatternOp::Case { .. } => {}
        }
        i += 1;
    }
    format!("[{}]", parts.join(", "))
}

/// The cases of the choice at `at` with their bodies as patterns, and the op
/// after the choice.
fn choices(
    table: &Table,
    ops: &[PatternOp],
    at: usize,
    cases: u16,
) -> (Vec<(CaseKey, String)>, usize) {
    let mut out = Vec::with_capacity(usize::from(cases));
    let mut i = at + 1;
    for _ in 0..cases {
        let Some(&PatternOp::Case { key, len }) = ops.get(i) else {
            break;
        };
        let end = (i + 1 + len as usize).min(ops.len());
        out.push((key, pattern(table, &ops[i + 1..end])));
        i = end;
    }
    (out, i)
}

/// A value as a JavaScript literal of its representation; `Err` names what a
/// page cannot hold.
pub(super) fn literal(value: &Value) -> Result<String, String> {
    Ok(match value {
        Value::Nil => "null".to_owned(),
        Value::Int(i) => format!("{i}n"),
        Value::Float(f) => float(*f),
        Value::Str(s) => string(s),
        Value::List(items) => {
            let items: Result<Vec<String>, String> = items.iter().map(literal).collect();
            format!("[{}]", items?.join(", "))
        }
        Value::Agg(agg) => {
            let fields: Result<Vec<String>, String> = agg.fields.iter().map(literal).collect();
            format!("{{ t: {}, f: [{}] }}", agg.tag, fields?.join(", "))
        }
        Value::Closure(_) => return Err("a function value".to_owned()),
        Value::Handle(_) => return Err("a native object".to_owned()),
    })
}

pub(super) fn float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f == f64::INFINITY {
        "Infinity".to_owned()
    } else if f == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else {
        format!("{f:?}")
    }
}

/// `s` as a JavaScript string literal.
pub(super) fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\x3c"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn num(n: Num) -> &'static str {
    n.name()
}

/// An integer operand at type `n`: its pattern, or its unsigned value for a
/// `U64`, whose pattern is stored signed.
fn wide(n: Num, r: &str) -> String {
    if n == Num::U64 {
        format!("$.u({r})")
    } else {
        r.to_owned()
    }
}

/// Where a translated function runs, which decides how it reaches the
/// component.
#[derive(Clone, Copy)]
pub(super) enum Dialect<'a> {
    /// The page script: the states in `W`, the inputs in `I`, functions
    /// named `f<id>`.
    Page,
    /// A Solid component's behavior: everything through its store `st`,
    /// functions by `names`, and each `memos` computed read through its memo
    /// outside a transaction.
    Solid {
        names: &'a BTreeMap<FuncId, String>,
        memos: &'a BTreeSet<FuncId>,
    },
}

impl Dialect<'_> {
    pub(super) fn name(self, id: FuncId) -> String {
        match self {
            Dialect::Page => format!("f{}", id.0),
            Dialect::Solid { names, .. } => names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("f{}", id.0)),
        }
    }

    fn solid(self) -> bool {
        matches!(self, Dialect::Solid { .. })
    }
}

/// Function `id` as a JavaScript function, typed as TypeScript's `any` in
/// the Solid dialect.
pub(super) fn function(
    program: &Program,
    id: FuncId,
    origin: TextRange,
    dialect: Dialect<'_>,
) -> Result<String, Vec<Finding>> {
    let function = program.function(id);
    let body = function.body.as_ref().map_err(|unsupported| {
        vec![Finding {
            need: Need::Unsupported,
            at: unsupported.at,
            message: format!("`{}` {}", function.name, unsupported.reason),
            notes: Vec::new(),
        }]
    })?;
    let mut out = String::new();
    let any = if dialect.solid() { ": any" } else { "" };
    let params: Vec<String> = (0..function.params).map(|r| format!("r{r}{any}")).collect();
    let captures: Vec<String> = (0..function.captures.len())
        .map(|c| format!("c{c}{any}"))
        .collect();
    let _ = writeln!(
        out,
        "function {}({}){any} {{",
        dialect.name(id),
        params
            .iter()
            .chain(captures.iter())
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    );
    let locals: Vec<String> = (function.params..body.regs.max(function.params))
        .map(|r| format!("r{r}{any}"))
        .collect();
    if !locals.is_empty() {
        let _ = writeln!(out, "  let {};", locals.join(", "));
    }
    for (at, reg) in function.captures.iter().enumerate() {
        let _ = writeln!(out, "  r{} = c{at};", reg.0);
    }
    let mut targets = BTreeSet::new();
    for inst in &body.insts {
        match inst {
            Inst::Jump { target } | Inst::JumpIf { target, .. } => {
                targets.insert(*target);
            }
            Inst::Switch {
                targets: table,
                default,
                ..
            } => {
                targets.extend(table.iter().copied());
                targets.insert(*default);
            }
            _ => {}
        }
    }
    let branches = !targets.is_empty();
    let indent = if branches { "    " } else { "  " };
    if branches {
        out.push_str("  let pc = 0;\n  for (;;) switch (pc) {\n  case 0:\n");
    }
    let mut findings = Vec::new();
    for (at, inst) in body.insts.iter().enumerate() {
        if branches && at > 0 && targets.contains(&(at as u32)) {
            let _ = writeln!(out, "  case {at}:");
        }
        match inst_js(program, inst, dialect) {
            Ok(code) => {
                let _ = writeln!(out, "{indent}{code}");
            }
            Err(why) => findings.push(Finding {
                need: Need::Unsupported,
                at: body.spans.get(at).copied().unwrap_or(origin),
                message: why,
                notes: vec![
                    "the page script runs what HTML can: state, values, text and the standard text and math functions".to_owned(),
                ],
            }),
        }
    }
    if branches {
        out.push_str("  default:\n    throw fault(\"a jump out of the function\");\n  }\n");
    } else if !matches!(body.insts.last(), Some(Inst::Return { .. })) {
        out.push_str("  throw fault(\"the end of a function that returns\");\n");
    }
    out.push_str("}\n");
    if findings.is_empty() {
        Ok(out)
    } else {
        Err(findings)
    }
}

fn r(reg: crate::behavior::ir::Reg) -> String {
    format!("r{}", reg.0)
}

fn regs(list: &[crate::behavior::ir::Reg]) -> String {
    list.iter().map(|x| r(*x)).collect::<Vec<_>>().join(", ")
}

/// One instruction as a JavaScript statement.
fn inst_js(program: &Program, inst: &Inst, dialect: Dialect<'_>) -> Result<String, String> {
    let solid = dialect.solid();
    Ok(match inst {
        Inst::Const { dst, value } => {
            let value = match value {
                Const::Unit => "0n".to_owned(),
                Const::Bool(b) => if *b { "1n" } else { "0n" }.to_owned(),
                Const::Int(v) => format!("{}n", *v as i64),
                Const::Float(f) => float(*f),
                Const::Char(c) => format!("{}n", u32::from(*c)),
                Const::Str(s) => string(s),
                Const::Color(c) => format!("{c}n"),
                Const::Nil => "null".to_owned(),
                Const::Tag(t) => format!("{t}n"),
            };
            format!("{} = {value};", r(*dst))
        }
        Inst::Move { dst, src } => format!("{} = {};", r(*dst), r(*src)),
        Inst::LoadState { dst, slot } if solid => format!("{} = st.get({slot});", r(*dst)),
        Inst::StoreState { slot, src } if solid => format!("st.set({slot}, {});", r(*src)),
        Inst::LoadInput { dst, slot } if solid => format!("{} = st.input({slot});", r(*dst)),
        Inst::LoadState { dst, slot } => format!("{} = W[{slot}];", r(*dst)),
        Inst::StoreState { slot, src } => format!("W[{slot}] = {};", r(*src)),
        Inst::LoadInput { dst, slot } => format!("{} = I[{slot}];", r(*dst)),
        Inst::Unary { dst, op, src } => {
            let s = r(*src);
            let value = match *op {
                UnaryOp::Neg(n) if n.is_float() => format!("-{s}"),
                UnaryOp::Neg(n) => format!("$.ck(\"{}\", -{})", num(n), wide(n, &s)),
                UnaryOp::Not => format!("({s} === 0n ? 1n : 0n)"),
                UnaryOp::BitNot(n) => format!("$.wr(\"{}\", ~{})", num(n), wide(n, &s)),
            };
            format!("{} = {value};", r(*dst))
        }
        Inst::Binary { dst, op, lhs, rhs } => {
            let (a, b) = (r(*lhs), r(*rhs));
            format!("{} = {};", r(*dst), binary(*op, &a, &b))
        }
        Inst::Cast { dst, src, from, to } => {
            let s = r(*src);
            let value = match (from.is_float(), to.is_float()) {
                (false, false) => format!("$.wr(\"{}\", {})", num(*to), wide(*from, &s)),
                (false, true) if *to == Num::F32 => {
                    format!("Math.fround(Number({}))", wide(*from, &s))
                }
                (false, true) => format!("Number({})", wide(*from, &s)),
                (true, false) => format!("$.sat(\"{}\", {s})", num(*to)),
                (true, true) if *to == Num::F32 => format!("Math.fround({s})"),
                (true, true) => s,
            };
            format!("{} = {value};", r(*dst))
        }
        Inst::Call { dst, func, args } => match dialect {
            Dialect::Solid { memos, .. } if memos.contains(func) => {
                let name = dialect.name(*func);
                format!("{} = st.work ? {name}() : {name}$memo();", r(*dst))
            }
            _ => format!("{} = {}({});", r(*dst), dialect.name(*func), regs(args)),
        },
        Inst::CallValue { dst, callee, args } => {
            format!("{} = {}({});", r(*dst), r(*callee), regs(args))
        }
        Inst::Native { dst, import, args } => {
            let path = program
                .natives
                .get(*import as usize)
                .map(|n| n.path.as_ref())
                .unwrap_or_default();
            let a: Vec<String> = args.iter().map(|x| r(*x)).collect();
            let value = native(path, &a).ok_or_else(|| {
                format!("it calls `{path}`, which has no JavaScript form in an export")
            })?;
            format!("{} = {value};", r(*dst))
        }
        Inst::Closure {
            dst,
            func,
            captures,
        } => format!(
            "{} = $.bind({}, [{}]);",
            r(*dst),
            dialect.name(*func),
            regs(captures)
        ),
        Inst::Make { dst, tag, fields } => {
            format!("{} = {{ t: {tag}, f: [{}] }};", r(*dst), regs(fields))
        }
        Inst::List { dst, items } => format!("{} = [{}];", r(*dst), regs(items)),
        Inst::Field { dst, src, index } => format!("{} = {}.f[{index}];", r(*dst), r(*src)),
        Inst::Index { dst, list, index } => {
            format!("{} = $.at({}, {});", r(*dst), r(*list), r(*index))
        }
        Inst::SetPath { root, path, src } => {
            let steps: Vec<String> = path
                .iter()
                .map(|step| match step {
                    PathStep::Field(n) => format!("[0, {n}]"),
                    PathStep::Index(i) => format!("[1, {}]", r(*i)),
                })
                .collect();
            format!(
                "{0} = $.set({0}, [{1}], {2});",
                r(*root),
                steps.join(", "),
                r(*src)
            )
        }
        Inst::Len { dst, src } => format!("{} = BigInt({}.length);", r(*dst), r(*src)),
        Inst::Push { list, item } => format!("{0} = $.push({0}, {1});", r(*list), r(*item)),
        Inst::Insert { list, index, item } => format!(
            "{0} = $.insert({0}, {1}, {2});",
            r(*list),
            r(*index),
            r(*item)
        ),
        Inst::Remove { dst, list, index } => format!(
            "[{1}, {0}] = $.remove({1}, {2});",
            r(*dst),
            r(*list),
            r(*index)
        ),
        Inst::Truncate { list, len } => format!("{0} = $.trunc({0}, {1});", r(*list), r(*len)),
        Inst::Tag { dst, src } => format!("{} = $.tag({});", r(*dst), r(*src)),
        Inst::IsNil { dst, src } => format!("{} = ({} === null ? 1n : 0n);", r(*dst), r(*src)),
        Inst::Jump { target } => format!("pc = {target}; continue;"),
        Inst::JumpIf { cond, when, target } => {
            let test = if *when { "!==" } else { "===" };
            format!("if ({} {test} 0n) {{ pc = {target}; continue; }}", r(*cond))
        }
        Inst::Switch {
            src,
            base,
            targets,
            default,
        } => {
            let table: Vec<String> = targets.iter().map(u32::to_string).collect();
            format!(
                "{{ const k = {} - {base}n; pc = k >= 0n && k < {}n ? [{}][Number(k)] : {default}; continue; }}",
                r(*src),
                targets.len(),
                table.join(", ")
            )
        }
        Inst::Return { src } => format!("return {};", r(*src)),
        Inst::Emit { event, args } if solid => format!("st.emit({event}, [{}]);", regs(args)),
        Inst::Emit { event, args } => format!("$.emit({event}, [{}]);", regs(args)),
        Inst::Start { .. } => {
            return Err("it starts a task, which an export does not run".to_owned());
        }
        Inst::Display { dst, src, kind } => {
            let s = r(*src);
            let value = match kind {
                DisplayKind::Bool => format!("$.show(\"bool\", {s})"),
                DisplayKind::Signed => format!("$.show(\"signed\", {s})"),
                DisplayKind::Unsigned => format!("$.show(\"unsigned\", {s})"),
                DisplayKind::F32 => format!("$.f32({s})"),
                DisplayKind::F64 => format!("$.f64({s})"),
                DisplayKind::Char => format!("$.show(\"char\", {s})"),
                DisplayKind::Str => s,
                DisplayKind::Dimension(suffix) => format!("$.f64({s}) + {}", string(suffix)),
            };
            format!("{} = {value};", r(*dst))
        }
        Inst::Concat { dst, parts } => {
            if parts.is_empty() {
                format!("{} = \"\";", r(*dst))
            } else {
                format!("{} = \"\".concat({});", r(*dst), regs(parts))
            }
        }
        // The locale is the page's, which the catalog's source table is.
        Inst::Translate {
            dst, message, args, ..
        } => format!(
            "{} = {}.tr({}, [{}]);",
            r(*dst),
            if solid { "st" } else { "$" },
            r(*message),
            regs(args)
        ),
        Inst::Unreachable => "throw fault(\"an unreachable point\");".to_owned(),
    })
}

/// A binary operator over the operands `a` and `b`.
fn binary(op: BinaryOp, a: &str, b: &str) -> String {
    let cmp = |n: Num, op: &str| {
        if n.is_float() {
            format!("({a} {op} {b} ? 1n : 0n)")
        } else {
            format!("({} {op} {} ? 1n : 0n)", wide(n, a), wide(n, b))
        }
    };
    let arith = |n: Num, op: &str| {
        if n == Num::F32 {
            format!("Math.fround({a} {op} {b})")
        } else if n.is_float() {
            format!("{a} {op} {b}")
        } else {
            format!("$.ck(\"{}\", {} {op} {})", num(n), wide(n, a), wide(n, b))
        }
    };
    match op {
        BinaryOp::Add(n) => arith(n, "+"),
        BinaryOp::Sub(n) => arith(n, "-"),
        BinaryOp::Mul(n) => arith(n, "*"),
        BinaryOp::Div(n) if n.is_float() => arith(n, "/"),
        BinaryOp::Div(n) => format!(
            "$.ck(\"{}\", $.div({}, {}))",
            num(n),
            wide(n, a),
            wide(n, b)
        ),
        BinaryOp::Rem(n) if n.is_float() => arith(n, "%"),
        BinaryOp::Rem(n) => format!(
            "$.ck(\"{}\", $.rem(\"{}\", {}, {}))",
            num(n),
            num(n),
            wide(n, a),
            wide(n, b)
        ),
        BinaryOp::BitAnd(_) => format!("{a} & {b}"),
        BinaryOp::BitOr(_) => format!("{a} | {b}"),
        BinaryOp::BitXor(_) => format!("{a} ^ {b}"),
        BinaryOp::Shl(n) => format!(
            "$.wr(\"{}\", {} << $.sh(\"{}\", {b}))",
            num(n),
            wide(n, a),
            num(n)
        ),
        BinaryOp::Shr(n) => format!(
            "$.wr(\"{}\", {} >> $.sh(\"{}\", {b}))",
            num(n),
            wide(n, a),
            num(n)
        ),
        BinaryOp::Lt(n) => cmp(n, "<"),
        BinaryOp::Le(n) => cmp(n, "<="),
        BinaryOp::Gt(n) => cmp(n, ">"),
        BinaryOp::Ge(n) => cmp(n, ">="),
        BinaryOp::Eq => format!("($.eq({a}, {b}) ? 1n : 0n)"),
        BinaryOp::Ne => format!("($.eq({a}, {b}) ? 0n : 1n)"),
        BinaryOp::Concat => format!("{a} + {b}"),
    }
}

/// A standard native function's call in JavaScript, `None` for one without
/// a JavaScript form.
fn native(path: &str, a: &[String]) -> Option<String> {
    let arg = |i: usize| a.get(i).cloned().unwrap_or_else(|| "null".to_owned());
    if let Some(name) = path.strip_prefix("viso::math::") {
        let known = [
            "sqrt", "abs", "floor", "round", "min", "max", "clamp", "sin", "cos", "tan", "atan",
            "atan2", "exp", "log", "pow", "hypot",
        ];
        return known
            .contains(&name)
            .then(|| format!("$.m.{name}({})", a.join(", ")));
    }
    Some(match path {
        "viso::text::upper" => format!("{}.toUpperCase()", arg(0)),
        "viso::text::lower" => format!("{}.toLowerCase()", arg(0)),
        "viso::text::trim" => format!("{}.trim()", arg(0)),
        "viso::text::len" => format!("BigInt([...{}].length)", arg(0)),
        "viso::text::contains" => format!("({}.includes({}) ? 1n : 0n)", arg(0), arg(1)),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_keep_their_representation() {
        assert_eq!(literal(&Value::Int(-3)).unwrap(), "-3n");
        assert_eq!(literal(&Value::Float(1.0)).unwrap(), "1.0");
        assert_eq!(literal(&Value::Float(f64::NAN)).unwrap(), "NaN");
        assert_eq!(string("a\"</b>\n"), "\"a\\\"\\x3c/b>\\n\"");
    }

    #[test]
    fn integer_operators_check_their_type() {
        assert_eq!(
            binary(BinaryOp::Add(Num::I64), "r0", "r1"),
            "$.ck(\"i64\", r0 + r1)"
        );
        assert_eq!(
            binary(BinaryOp::Lt(Num::U64), "r0", "r1"),
            "($.u(r0) < $.u(r1) ? 1n : 0n)"
        );
        assert_eq!(
            binary(BinaryOp::Mul(Num::F32), "r0", "r1"),
            "Math.fround(r0 * r1)"
        );
    }
}
