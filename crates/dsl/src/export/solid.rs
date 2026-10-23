//! A view as a SolidJS project another web project owns: the lowered tree as
//! the JSX of one component, its states as signals, its computeds as memos,
//! its inputs as typed props and its events as callback props; its Behavior
//! IR translated to TypeScript over a store that commits each handler as one
//! transaction; and a Vite project around it.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use viso_behavior::Value;
use viso_behavior::i18n::Catalog;
use viso_schema::view::EventRoute;

use super::eval::Eval;
use super::html::{
    Content, Element, Lowered, Piece, Prop, RegionKind, VarToken, Wire, escape_attr,
};
use super::script::{self, Dialect};
use super::{Finding, GENERATED, Need, SolidOptions};
use crate::behavior::ir::{FuncId, FunctionKind};
use crate::hir::{HirComponent, Ty};
use crate::syntax::TextRange;

/// The runtime the component imports: the interpreter's value semantics,
/// what DOM elements show and the payloads of their events, the tasks,
/// effects and resources, the browser's environment, and the store and
/// region components.
const RUNTIME: &str = concat!(
    include_str!("values.js"),
    include_str!("easing.js"),
    include_str!("dom.js"),
    include_str!("work.js"),
    include_str!("env.js"),
    include_str!("solid.js")
);

/// The runtime's types.
const RUNTIME_TYPES: &str = include_str!("solid.d.ts");

/// The versions the project's `package.json` asks for.
const SOLID: &str = "^1.9.0";
const VITE: &str = "^8.0.0";
const VITE_SOLID: &str = "^2.11.0";
const TYPESCRIPT: &str = "^5.9.0";

/// What the project is made of.
pub(super) struct Input<'a> {
    pub(super) page: &'a Lowered,
    pub(super) eval: Option<&'a Eval<'a>>,
    pub(super) catalog: Option<&'a Catalog>,
    pub(super) component: Option<&'a HirComponent>,
    pub(super) styles: &'a str,
    pub(super) options: &'a SolidOptions,
    pub(super) origin: TextRange,
}

/// The project's files by path, in path order.
pub(super) fn project(input: &Input<'_>) -> Result<Vec<(String, String)>, Vec<Finding>> {
    let name = input
        .component
        .map_or_else(|| "View".to_owned(), |c| pascal(&c.schema.name));
    let mut files = BTreeMap::new();
    let (component, behavior) = component(input, &name)?;
    files.insert(format!("src/components/{name}.tsx"), component);
    if let Some(behavior) = behavior {
        files.insert(format!("src/components/{name}.behavior.ts"), behavior);
    }
    let mut runtime = String::new();
    let _ = writeln!(runtime, "// {GENERATED}");
    runtime.push_str(
        "import { batch, createMemo, createRenderEffect, createRoot, createSignal, getOwner, mapArray, onCleanup, untrack } from \"solid-js\";\n\n",
    );
    runtime.push_str("const A = new Map();\n");
    runtime.push_str(RUNTIME);
    files.insert("src/viso/runtime.js".to_owned(), runtime);
    files.insert(
        "src/viso/runtime.d.ts".to_owned(),
        format!("// {GENERATED}\n{RUNTIME_TYPES}"),
    );
    files.insert("src/styles.css".to_owned(), input.styles.to_owned());
    files.insert("src/App.tsx".to_owned(), app(&name));
    files.insert("src/index.tsx".to_owned(), entry());
    files.insert("index.html".to_owned(), index(input.options));
    files.insert("package.json".to_owned(), package(input.options));
    files.insert("tsconfig.json".to_owned(), TSCONFIG.to_owned());
    files.insert("vite.config.ts".to_owned(), vite());
    Ok(files.into_iter().collect())
}

/// The component file, and its behavior file when it has one.
fn component(input: &Input<'_>, name: &str) -> Result<(String, Option<String>), Vec<Finding>> {
    let page = input.page;
    let mut idents = Idents::default();
    for taken in [
        "st",
        "f",
        "props",
        "MESSAGES",
        "show",
        "store",
        "Each",
        "With",
        "Choose",
        "Show",
        "Switch",
        "Match",
        "createSignal",
        "behavior",
        "Value",
        "Messages",
        name,
    ] {
        idents.used.insert(taken.to_owned());
    }
    let marked = std::iter::once(&page.root)
        .chain(page.regions.iter().flat_map(|r| &r.arms))
        .any(|c| !c.marks.is_empty());

    // The functions the view calls, translated over the store.
    let mut names = BTreeMap::new();
    let mut memos = BTreeSet::new();
    let mut behavior = None;
    if let Some(eval) = input.eval {
        let program = eval.program;
        let roots = script::roots(page, eval);
        let functions = script::reachable(program, roots.clone());
        let mut fns = Idents::default();
        for taken in ["st", "$", "fault", "behavior"] {
            fns.used.insert(taken.to_owned());
        }
        for &id in &functions {
            let function = program.function(id);
            let member = function.name.rsplit('.').next().unwrap_or_default();
            let wanted = match function.kind {
                FunctionKind::Fn | FunctionKind::Action | FunctionKind::Computed => ident(member),
                FunctionKind::Handler => format!("handler{}", id.0),
                FunctionKind::RegionEntry => format!("entry{}", id.0),
                FunctionKind::Closure => format!("closure{}", id.0),
                _ => format!("f{}", id.0),
            };
            names.insert(id, fns.take(&wanted, id.0));
            if function.kind == FunctionKind::Computed
                && function.params == 0
                && function.captures.is_empty()
                && !eval.reads_regional(id)
            {
                memos.insert(id);
            }
        }
        let dialect = Dialect::Solid {
            names: &names,
            memos: &memos,
        };
        let mut out = String::new();
        out.push_str("export function behavior(st: Store) {\n");
        let suspends = script::suspending(program, &functions);
        let mut findings = Vec::new();
        for &id in &functions {
            match script::function(program, id, input.origin, dialect, &suspends) {
                Ok(code) => {
                    for line in code.lines() {
                        let _ = writeln!(out, "  {line}");
                    }
                }
                Err(mut found) => findings.append(&mut found),
            }
        }
        if !findings.is_empty() {
            return Err(findings);
        }
        for id in &memos {
            let name = &names[id];
            let _ = writeln!(out, "  const {name}$memo = st.memo({name});");
        }
        let exported: Vec<&str> = roots.iter().map(|id| names[id].as_str()).collect();
        let _ = writeln!(out, "  return {{ {} }};\n}}", exported.join(", "));
        let mut imports = Vec::new();
        if out.contains("$.") {
            imports.push("$");
        }
        if out.contains("fault(") {
            imports.push("fault");
        }
        imports.push("type Store");
        let mut file = String::new();
        let _ = writeln!(file, "// {GENERATED}");
        let _ = writeln!(
            file,
            "import {{ {} }} from \"../viso/runtime\";\n",
            imports.join(", ")
        );
        let _ = writeln!(
            file,
            "/** `{name}`'s functions, translated from its Behavior IR, over its store. */"
        );
        file.push_str(&out);
        behavior = Some(file);
    } else if marked {
        return Err(vec![Finding {
            need: Need::Unsupported,
            at: input.origin,
            message: "internal: the view changes but its component has no behavior".to_owned(),
            notes: Vec::new(),
        }]);
    }

    // The assets it shows, each imported so the bundler places it.
    let mut assets = BTreeMap::new();
    for (index, id) in page.assets.keys().enumerate() {
        assets.insert(*id, format!("asset{index}"));
    }

    let mut jsx = Jsx {
        page,
        eval: input.eval,
        names: &names,
        assets: &assets,
        idents: &mut idents,
        solid: BTreeSet::new(),
        runtime: BTreeSet::new(),
        out: String::new(),
        mount: None,
    };
    jsx.content(&page.root, &[], 2);
    let Jsx {
        out: view,
        solid: solid_uses,
        runtime: runtime_uses,
        ..
    } = jsx;

    let mut out = String::new();
    let _ = writeln!(out, "// {GENERATED}");
    let mut solid: Vec<&str> = solid_uses.into_iter().collect();
    if behavior.is_some() {
        solid.push("createSignal");
    }
    solid.sort_unstable_by_key(|s| s.to_ascii_lowercase());
    if !solid.is_empty() {
        let _ = writeln!(out, "import {{ {} }} from \"solid-js\";", solid.join(", "));
    }
    let mut runtime: Vec<&str> = runtime_uses.into_iter().collect();
    if behavior.is_some() {
        runtime.extend(["settle", "store", "type Messages", "type Value"]);
        if input
            .eval
            .is_some_and(|eval| eval.browser_env().next().is_some())
        {
            runtime.push("envField");
        }
    }
    if marked {
        runtime.push("show");
    }
    runtime.sort_unstable_by_key(|s| s.trim_start_matches("type ").to_ascii_lowercase());
    runtime.dedup();
    if !runtime.is_empty() {
        let _ = writeln!(
            out,
            "import {{ {} }} from \"../viso/runtime\";",
            runtime.join(", ")
        );
    }
    if behavior.is_some() {
        let _ = writeln!(out, "import {{ behavior }} from \"./{name}.behavior\";");
    }
    out.push_str("import \"../styles.css\";\n");
    for (id, import) in &assets {
        let path = &page.assets[id];
        let _ = writeln!(
            out,
            "import {import} from {};",
            script::string(&format!("../assets/{path}"))
        );
    }
    out.push('\n');

    // The props: an input each, then an `on…` callback per event.
    let layout = input.eval.map(|e| e.layout);
    let mut props = Vec::new();
    let mut inputs = Vec::new();
    if let (Some(eval), Some(layout)) = (input.eval, layout) {
        for (slot, input_name) in layout.inputs.iter().enumerate() {
            let ty = input
                .component
                .and_then(|c| c.schema.inputs.iter().find(|i| &i.name == input_name))
                .map(|i| &i.meta.inferred_type);
            let (ts, kind) = ty.map_or(("Value", "value"), prop_type);
            let prop = ident(input_name);
            let fallback = eval
                .inputs()
                .get(slot)
                .map(script::literal)
                .transpose()
                .map_err(|why| unsupported(input.origin, &why))?
                .unwrap_or_else(|| "null".to_owned());
            props.push(format!("  {prop}?: {ts};"));
            inputs.push(format!("[() => props.{prop}, \"{kind}\", {fallback}]"));
        }
        for event in &layout.events {
            props.push(format!(
                "  on{}?: (...args: Value[]) => void;",
                pascal(event)
            ));
        }
    }
    let props_type = format!("{name}Props");
    let _ = writeln!(out, "export interface {props_type} {{");
    for prop in &props {
        let _ = writeln!(out, "{prop}");
    }
    out.push_str("}\n\n");
    if behavior.is_some() {
        let messages = script::messages(input.catalog).map_err(|why| {
            unsupported(
                input.origin,
                &format!("the message catalog does not read: {why}"),
            )
        })?;
        let _ = writeln!(out, "const MESSAGES: Messages = {messages};");
        out.push_str("settle(MESSAGES);\n\n");
    }
    let props_param = if props.is_empty() { "_props" } else { "props" };
    let _ = writeln!(
        out,
        "export default function {name}({props_param}: {props_type}) {{"
    );
    if let (Some(eval), Some(layout)) = (input.eval, layout) {
        let mut signals = Vec::new();
        for (slot, value) in eval.states().iter().enumerate() {
            let state = layout.states.get(slot).map_or("state", String::as_str);
            let getter = idents.take(&ident(state), slot as u32);
            let setter = idents.take(&format!("set{}", pascal(&getter)), slot as u32);
            let browser = eval
                .browser_env()
                .find(|&(at, _, _)| at == slot as u32)
                .map(|(_, tag, _)| tag);
            let value = if eval.locale_slots().contains(&(slot as u32))
                && let Value::Agg(locale) = value
            {
                // The reader's locale, as the page found it.
                format!("{{ t: {}, f: [MESSAGES.R] }}", locale.tag)
            } else if let Some(tag) = browser {
                // The browser's, the window's for a field its reader's place
                // decides until it is laid out.
                format!("envField({tag})")
            } else {
                script::literal(value).map_err(|why| {
                    unsupported(
                        input.origin,
                        &format!("the component starts with {why}, which a page cannot hold"),
                    )
                })?
            };
            let _ = writeln!(
                out,
                "  const [{getter}, {setter}] = createSignal<Value>({value}, {{ equals: false }});"
            );
            signals.push(format!("[{getter}, {setter}]"));
        }
        let events: Vec<String> = layout
            .events
            .iter()
            .map(|e| format!("() => props.on{}", pascal(e)))
            .collect();
        let asset_list: Vec<String> = assets
            .iter()
            .map(|(id, import)| format!("[\"{id}\", {import}]"))
            .collect();
        out.push_str("  const st = store({\n");
        let _ = writeln!(out, "    states: [{}],", signals.join(", "));
        let _ = writeln!(out, "    inputs: [{}],", inputs.join(", "));
        let _ = writeln!(out, "    events: [{}],", events.join(", "));
        out.push_str("    messages: MESSAGES,\n");
        let wide: Vec<String> = eval
            .browser_env()
            .filter(|&(_, tag, _)| {
                !viso_schema::EnvField::from_tag(tag).is_some_and(|f| f.anchored())
            })
            .map(|(slot, tag, _)| format!("[{slot}, {tag}]"))
            .collect();
        if !wide.is_empty() {
            let _ = writeln!(out, "    env: [{}],", wide.join(", "));
        }
        let _ = writeln!(out, "    assets: [{}],", asset_list.join(", "));
        out.push_str("  });\n  const f = behavior(st);\n");
        let effects = effects_literal(eval, &names, &page.root, true, &[]);
        if !effects.is_empty() {
            let _ = writeln!(out, "  st.fx([{}]);", effects.join(", "));
        }
    }
    out.push_str("  return (\n");
    out.push_str(&view);
    out.push_str("  );\n}\n");
    Ok((out, behavior))
}

fn unsupported(at: TextRange, why: &str) -> Vec<Finding> {
    vec![Finding {
        need: Need::Unsupported,
        at,
        message: why.to_owned(),
        notes: Vec::new(),
    }]
}

/// A prop's TypeScript type and how the store reads it as a value.
fn prop_type(ty: &Ty) -> (&'static str, &'static str) {
    match ty {
        Ty::Bool => ("boolean", "bool"),
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 => {
            ("bigint | number", "int")
        }
        Ty::F32 | Ty::F64 => ("number", "float"),
        Ty::String => ("string", "string"),
        _ => ("Value", "value"),
    }
}

/// Writes the lowered tree as JSX.
struct Jsx<'a> {
    page: &'a Lowered,
    eval: Option<&'a Eval<'a>>,
    names: &'a BTreeMap<FuncId, String>,
    assets: &'a BTreeMap<u64, String>,
    idents: &'a mut Idents,
    /// What it uses of `solid-js` and of the runtime.
    solid: BTreeSet<&'static str>,
    runtime: BTreeSet<&'static str>,
    out: String,
    /// The mount of the region content being written, when a mount around
    /// it mounts a component.
    mount: Option<String>,
}

impl Jsx<'_> {
    fn line(&mut self, depth: usize, text: &str) {
        for _ in 0..depth {
            self.out.push_str("  ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// The call of entry `entry` with the region bindings `scope` and, after
    /// them, `extra`.
    fn call(&self, entry: u32, scope: &[String], extra: Option<&str>) -> String {
        let id = self.eval.map(|e| e.func(entry));
        let name = id
            .and_then(|id| self.names.get(&id))
            .cloned()
            .unwrap_or_else(|| format!("entry{entry}"));
        let mut args: Vec<String> = scope.iter().map(|s| format!("{s}()")).collect();
        args.extend(extra.map(str::to_owned));
        format!("f.{name}({})", args.join(", "))
    }

    /// The value of entry `entry` as [`call`](Self::call) reads it, seeing
    /// the states of the mount it is written in.
    fn value(&self, entry: u32, scope: &[String], extra: Option<&str>) -> String {
        let call = self.call(entry, scope, extra);
        match &self.mount {
            Some(m) => format!("st.at({m}, () => {call})"),
            None => call,
        }
    }

    /// The mount argument of a handler written here.
    fn mount_arg(&self) -> String {
        self.mount
            .as_ref()
            .map_or_else(String::new, |m| format!(", {m}"))
    }

    /// `content` at `depth`: its one node, or a fragment of its nodes.
    fn content(&mut self, content: &Content, scope: &[String], depth: usize) {
        match content.nodes.len() {
            0 => self.line(depth, "<></>"),
            1 => self.pieces(&content.nodes, content, scope, depth),
            _ => {
                self.line(depth, "<>");
                self.pieces(&content.nodes, content, scope, depth + 1);
                self.line(depth, "</>");
            }
        }
    }

    /// `content` at `depth` as a string of its own: when it mounts a
    /// component, inside a mount keeping its states, running its effects and
    /// owning its tasks.
    fn arm(&mut self, content: &Content, scope: &[String], depth: usize) -> String {
        let outer = std::mem::take(&mut self.out);
        match self.eval.filter(|_| !content.instances.is_empty()) {
            Some(eval) => {
                let at = self.idents.take("m", 0);
                let parent = self.mount.clone().unwrap_or_else(|| "null".to_owned());
                let own: Vec<String> = content.instances.iter().map(u32::to_string).collect();
                let mut locals = Vec::new();
                for regional in eval
                    .layout
                    .regional
                    .iter()
                    .filter(|r| content.instances.contains(&r.instance))
                {
                    for (i, init) in regional.inits.iter().enumerate() {
                        let slot = regional.base + i as u32;
                        match eval.layout.env.iter().find(|env| env.slot == slot) {
                            Some(read) if read.field.anchored() => {
                                self.runtime.insert("envField");
                                locals.push(format!(
                                    "[{slot}, () => envField({})]",
                                    read.field.tag()
                                ));
                            }
                            Some(_) => {}
                            None => {
                                let init = init.map_or("null".to_owned(), |e| {
                                    format!("() => {}", self.call(e, scope, None))
                                });
                                locals.push(format!("[{slot}, {init}]"));
                            }
                        }
                    }
                }
                let effects = effects_literal(eval, self.names, content, false, scope);
                let fx = if effects.is_empty() {
                    "null".to_owned()
                } else {
                    format!("({at}) => [{}]", effects.join(", "))
                };
                self.line(
                    depth,
                    &format!(
                        "<>{{st.mount({parent}, [{}], [{}], {fx}, ({at}) => (",
                        own.join(", "),
                        locals.join(", ")
                    ),
                );
                let outer_mount = self.mount.replace(at);
                self.content(content, scope, depth + 1);
                self.mount = outer_mount;
                self.line(depth, "))}</>");
            }
            None => self.content(content, scope, depth),
        }
        std::mem::replace(&mut self.out, outer)
    }

    fn pieces(&mut self, pieces: &[Piece], content: &Content, scope: &[String], depth: usize) {
        for piece in pieces {
            match piece {
                Piece::Element { el, children, mark } => {
                    let wires: &[Wire] = mark.map_or(&[], |m| &content.marks[m].wires);
                    self.element(el, children, wires, content, scope, depth);
                }
                Piece::Region { region, .. } => self.region(*region, scope, depth),
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn element(
        &mut self,
        el: &Element,
        children: &[Piece],
        wires: &[Wire],
        content: &Content,
        scope: &[String],
        depth: usize,
    ) {
        let mut attrs: Vec<String> = Vec::new();
        let mut class = el.base.to_owned();
        for c in &el.classes {
            class.push(' ');
            class.push_str(c);
        }
        attrs.push(format!("class=\"{class}\""));
        for (name, value) in &el.attrs {
            if *name == "checked" && value.is_empty() {
                attrs.push("checked".to_owned());
            } else {
                let mut v = String::new();
                escape_attr(value, &mut v);
                attrs.push(format!("{name}=\"{v}\""));
            }
        }
        // The style: each declaration a literal, or an expression of what
        // it shows.
        let mut style: Vec<(String, String)> = Vec::new();
        for declarations in &el.style {
            for declaration in declarations.split("; ") {
                if let Some((property, value)) = declaration.split_once(": ") {
                    style.push((property.to_owned(), script::string(value)));
                }
            }
        }
        if let Some(shown) = &el.asset {
            let import = self.assets.get(&shown.id).cloned().unwrap_or_default();
            if shown.mask {
                let mask = format!("show.mask({import})");
                self.runtime.insert("show");
                style.insert(0, ("-webkit-mask-image".to_owned(), mask.clone()));
                style.insert(0, ("mask-image".to_owned(), mask));
            } else {
                attrs.push(format!("src={{{import}}}"));
            }
        }
        let mut class_list: Vec<(String, String)> = Vec::new();
        let mut text = el.text.as_ref().map(|t| script::string(t));
        if let Some(markup) = &el.markup {
            attrs.push(format!("innerHTML={{{}}}", script::string(markup)));
        }
        let mut refs: Vec<String> = Vec::new();
        let mut events: Vec<String> = Vec::new();
        let synced = wires.iter().any(|w| matches!(w, Wire::Sync { .. }));
        let args = |scope: &[String]| -> String {
            scope.iter().map(|s| format!(", {s}()")).collect::<String>()
        };
        for wire in wires {
            match wire {
                Wire::Prop { entry, prop } => {
                    let v = self.value(*entry, scope, None);
                    match prop {
                        Prop::Text => text = Some(format!("show.text({v})")),
                        Prop::TextColor => {
                            style.push(("color".to_owned(), format!("show.color({v})")))
                        }
                        Prop::SoftWrap => style.push((
                            "white-space".to_owned(),
                            format!("show.truth({v}) ? \"pre-wrap\" : undefined"),
                        )),
                        Prop::Visible => style.push((
                            "display".to_owned(),
                            format!("show.truth({v}) ? undefined : \"none\""),
                        )),
                        Prop::Focusable => {
                            refs.push(format!("st.effect(() => show.focusable(el, {v}));"));
                        }
                        Prop::Enabled => {
                            refs.push(format!("st.effect(() => show.enabled(el, {v}));"));
                        }
                        Prop::Autofocus => refs.push(format!("show.autofocus(el, {v});")),
                        Prop::Clip => style.push((
                            "overflow".to_owned(),
                            format!("show.truth({v}) ? \"hidden\" : undefined"),
                        )),
                        Prop::Sticky => {
                            for (name, on) in [
                                ("position", "sticky"),
                                ("inset-block-start", "0"),
                                ("inset-inline-start", "0"),
                                ("z-index", "1"),
                            ] {
                                style.push((
                                    name.to_owned(),
                                    format!("show.truth({v}) ? \"{on}\" : undefined"),
                                ));
                            }
                        }
                        Prop::Cursor => {
                            style.push(("cursor".to_owned(), format!("show.cursor({v})")))
                        }
                        Prop::FontWeight => {
                            style.push(("font-weight".to_owned(), format!("show.weight({v})")))
                        }
                        Prop::FontStyle => {
                            style.push(("font-style".to_owned(), format!("show.italic({v})")))
                        }
                        Prop::Spans => text = Some(format!("show.spans({v})")),
                        Prop::Selectable => attrs.push(format!(
                            "classList={{{{ \"vx-selectable\": show.truth({v}) }}}}"
                        )),
                        Prop::Label { alt } => {
                            let name = if *alt { "alt" } else { "aria-label" };
                            attrs.push(format!("{name}={{show.opt({v})}}"));
                        }
                        Prop::Hint => {
                            attrs.push(format!("attr:aria-description={{show.opt({v})}}"))
                        }
                        Prop::Spoken => attrs.push(format!("aria-valuetext={{show.opt({v})}}")),
                        Prop::Hidden => attrs.push(format!(
                            "aria-hidden={{show.truth({v}) ? \"true\" : undefined}}"
                        )),
                        Prop::HeadingLevel => attrs.push(format!("aria-level={{show.opt({v})}}")),
                        Prop::Checked => attrs.push(format!("checked={{show.truth({v})}}")),
                        Prop::Open => attrs.push(format!("open={{show.truth({v})}}")),
                        Prop::Layer { .. } => {
                            refs.push(format!("st.effect(() => show.layer(el, {v}));"));
                        }
                        Prop::Range(name) => attrs.push(format!("{name}={{show.num({v})}}")),
                        Prop::Href => attrs.push(format!("href={{show.opt({v})}}")),
                        Prop::Stepper(name) => refs.push(format!(
                            "st.effect(() => show.stepper(el, {v}, \"{name}\"));"
                        )),
                        Prop::Pressed => attrs.push(format!(
                            "aria-pressed={{show.truth({v}) ? \"true\" : \"false\"}}"
                        )),
                        Prop::Badge(name) => {
                            refs.push(format!("st.effect(() => show.badge(el, {v}, \"{name}\"));"))
                        }
                        Prop::Avatar(name) => refs.push(format!(
                            "st.effect(() => show.avatar(el, {v}, \"{name}\"));"
                        )),
                        Prop::Progress(name) => refs.push(format!(
                            "st.effect(() => show.progress(el, {v}, \"{name}\"));"
                        )),
                        Prop::InputValue => attrs.push(format!("value={{show.text({v})}}")),
                        Prop::Placeholder => attrs.push(format!("placeholder={{show.opt({v})}}")),
                        Prop::Secure => attrs.push(format!(
                            "type={{show.truth({v}) ? \"password\" : \"text\"}}"
                        )),
                        Prop::InputKind => {
                            attrs.push(format!("inputmode={{show.mode({v})}}"));
                            attrs.push(format!("pattern={{show.pattern({v})}}"));
                        }
                        Prop::MaxLength => attrs.push(format!("maxlength={{show.num({v})}}")),
                        Prop::Number => {
                            refs.push(format!("st.effect(() => show.number(el, {v}));"))
                        }
                        Prop::Selected { radio } => {
                            refs.push(format!("st.effect(() => show.select(el, {v}, {radio}));"));
                        }
                        Prop::Page => refs.push(format!("st.effect(() => show.page(el, {v}));")),
                        Prop::Hero => attrs.push(format!("data-hero={{show.opt({v})}}")),
                        Prop::AccessKey => attrs.push(format!("accesskey={{show.opt({v})}}")),
                        Prop::DragAxis => attrs.push(format!("data-drag-axis={{show.axis({v})}}")),
                        Prop::Data(name) => {
                            attrs.push(format!("{name}={{show.data({v}, \"{name}\")}}"))
                        }
                        Prop::FocusRing => style.push((
                            "outline".to_owned(),
                            format!("show.truth({v}) ? undefined : \"none\""),
                        )),
                        Prop::Turn => refs.push(format!("st.effect(() => show.turn(el, {v}));")),
                        Prop::Source { mask: false } => {
                            attrs.push(format!("src={{show.url({v})}}"))
                        }
                        Prop::Source { mask: true } => {
                            let mask = format!("show.mask(show.url({v}))");
                            style.push(("mask-image".to_owned(), mask.clone()));
                            style.push(("-webkit-mask-image".to_owned(), mask));
                        }
                        Prop::Fit { mask: false } => {
                            style.push(("object-fit".to_owned(), format!("show.fit({v})")));
                        }
                        Prop::Fit { mask: true } => {
                            let size = format!("show.maskSize({v})");
                            style.push(("mask-size".to_owned(), size.clone()));
                            style.push(("-webkit-mask-size".to_owned(), size));
                        }
                        Prop::Var {
                            name,
                            token: VarToken::Origin,
                        } => {
                            style.push((name.clone(), format!("show.origin({v}, false)")));
                            style.push((format!("{name}-rtl"), format!("show.origin({v}, true)")));
                        }
                        Prop::Var { name, token } => {
                            let value = match token {
                                VarToken::Color => format!("show.color({v})"),
                                VarToken::Opacity => format!("show.opacity({v})"),
                                VarToken::Radius => format!("show.radius({v})"),
                                VarToken::Border => format!("show.stroke({v})"),
                                VarToken::Shadow => format!("show.shadow({v})"),
                                VarToken::Translate => format!("show.offset({v})"),
                                VarToken::Scale => format!("show.scale({v})"),
                                VarToken::Rotation => format!("show.rotate({v})"),
                                VarToken::Origin => unreachable!("an origin sets two variables"),
                                VarToken::Transition(property) => {
                                    format!("show.move({v}, \"{property}\")")
                                }
                            };
                            style.push((name.clone(), value));
                        }
                        Prop::Class(name) => {
                            class_list.push((name.clone(), format!("show.truth({v})")))
                        }
                    }
                }
                Wire::On {
                    route,
                    handler,
                    capture,
                    kind,
                } => {
                    let h = self.call(*handler, &[], None);
                    let h = h.trim_end_matches("()");
                    let rest = args(scope);
                    let m = self.mount_arg();
                    if *route == EventRoute::Click && !capture && !synced {
                        events.push(format!(
                            "onClick={{(e) => show.reaches(e) && st.run(() => {h}(st.click(e){rest}){m})}}"
                        ));
                    } else {
                        refs.push(format!(
                            "st.on(el, {}, {capture}, {}, (p) => {h}(p{rest}){m});",
                            *route as u8, *kind as u8
                        ));
                    }
                }
                Wire::Sync { kind, entry } => {
                    let value = entry.map_or("null".to_owned(), |e| {
                        format!("() => {}", self.value(e, scope, None))
                    });
                    refs.push(format!("st.sync(el, {}, {value});", *kind as u8));
                }
                Wire::Region(_) => {}
                Wire::Viewer => refs.push("st.viewer(el);".to_owned()),
                Wire::SwipeRow => refs.push("st.swipeRow(el);".to_owned()),
                Wire::Sheet => refs.push("st.sheet(el);".to_owned()),
                Wire::DragDrop => refs.push("st.dragDrop();".to_owned()),
                Wire::Anchor(instance) => {
                    if let Some(eval) = self.eval {
                        let reads = script::anchored_reads(eval, *instance);
                        refs.push(format!(
                            "st.anchor(el, [{}]{});",
                            reads.join(", "),
                            self.mount_arg()
                        ));
                    }
                }
            }
        }
        if !style.is_empty() {
            let dynamic = style.iter().any(|(_, v)| !v.starts_with('"'));
            if dynamic {
                let entries: Vec<String> = style
                    .iter()
                    .map(|(k, v)| format!("{}: {v}", script::string(k)))
                    .collect();
                attrs.push(format!("style={{{{ {} }}}}", entries.join(", ")));
            } else {
                let declarations: Vec<String> = style
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.trim_matches('"')))
                    .collect();
                let mut v = String::new();
                escape_attr(&declarations.join("; "), &mut v);
                attrs.push(format!("style=\"{v}\""));
            }
        }
        if !class_list.is_empty() {
            let entries: Vec<String> = class_list
                .iter()
                .map(|(k, v)| format!("{}: {v}", script::string(k)))
                .collect();
            attrs.push(format!("classList={{{{ {} }}}}", entries.join(", ")));
        }
        match refs.len() {
            0 => {}
            1 => attrs.push(format!("ref={{(el) => {}}}", refs[0].trim_end_matches(';'))),
            _ => attrs.push(format!("ref={{(el) => {{ {} }}}}", refs.join(" "))),
        }
        attrs.extend(events);

        let open = format!("<{} {}", el.tag, attrs.join(" "));
        let empty = el.void() || (text.is_none() && children.is_empty());
        let one_line = open.len() + depth * 2 < 100;
        let head = |this: &mut Self, close: &str| {
            if one_line {
                this.line(depth, &format!("{open}{close}"));
            } else {
                this.line(depth, &format!("<{}", el.tag));
                for attr in &attrs {
                    this.line(depth + 1, attr);
                }
                this.line(depth, close.trim_start());
            }
        };
        if empty {
            head(self, " />");
            return;
        }
        head(self, ">");
        if let Some(text) = &text {
            self.line(depth + 1, &format!("{{{text}}}"));
        }
        self.pieces(children, content, scope, depth + 1);
        self.line(depth, &format!("</{}>", el.tag));
    }

    fn region(&mut self, index: u32, scope: &[String], depth: usize) {
        let page = self.page;
        let region = &page.regions[index as usize];
        let keep = region.preserve.iter().any(|k| *k);
        match region.kind {
            RegionKind::If { select } => {
                let sel = self.value(select, scope, None);
                self.choice(index, &sel, None, keep, scope, depth);
            }
            RegionKind::Match { scrutinee, select } => {
                let value = self.idents.take(&format!("value{index}"), index);
                let scr = self.value(scrutinee, scope, None);
                let mut inner = scope.to_vec();
                inner.push(value.clone());
                let sel = self.value(select, scope, Some(&format!("{value}()")));
                self.runtime.insert("With");
                self.line(depth, &format!("<With value={{() => {scr}}}>"));
                self.line(depth + 1, &format!("{{({value}) => ("));
                self.choice(index, &sel, Some(&inner), keep, scope, depth + 2);
                self.line(depth + 1, ")}");
                self.line(depth, "</With>");
            }
            RegionKind::For { items, key } => {
                let wanted = region
                    .binding
                    .as_deref()
                    .map_or_else(|| format!("item{index}"), ident);
                let item = self.idents.take(&wanted, index);
                let each = self.value(items, scope, None);
                let by = key.map_or("null".to_owned(), |k| {
                    format!("(v) => {}", self.value(k, scope, Some("v")))
                });
                let mut inner = scope.to_vec();
                inner.push(item.clone());
                self.runtime.insert("Each");
                self.line(depth, &format!("<Each each={{() => {each}}} by={{{by}}}>"));
                self.line(depth + 1, &format!("{{({item}) => ("));
                let arm = self.arm(&region.arms[0], &inner, depth + 2);
                self.out.push_str(&arm);
                self.line(depth + 1, ")}");
                self.line(depth, "</Each>");
            }
        }
    }

    /// The arm `sel` chooses of region `index`, each arm seeing `inner` (the
    /// scope by default).
    fn choice(
        &mut self,
        index: u32,
        sel: &str,
        inner: Option<&[String]>,
        keep: bool,
        scope: &[String],
        depth: usize,
    ) {
        let page = self.page;
        let region = &page.regions[index as usize];
        let inner = inner.unwrap_or(scope);
        let arms = &region.arms;
        if keep {
            self.runtime.insert("Choose");
            let kept: Vec<&str> = region
                .preserve
                .iter()
                .map(|k| if *k { "true" } else { "false" })
                .collect();
            self.line(depth, "<Choose");
            self.line(depth + 1, &format!("arm={{() => {sel}}}"));
            self.line(depth + 1, &format!("keep={{[{}]}}", kept.join(", ")));
            self.line(depth + 1, "arms={[");
            for arm in arms {
                self.line(depth + 2, "() => (");
                let arm = self.arm(arm, inner, depth + 3);
                self.out.push_str(&arm);
                self.line(depth + 2, "),");
            }
            self.line(depth + 1, "]}");
            self.line(depth, "/>");
            return;
        }
        match arms.len() {
            1 | 2 => {
                self.solid.insert("Show");
                self.line(depth, "<Show");
                self.line(depth + 1, &format!("when={{{sel} === 0n}}"));
                if let Some(fallback) = arms.get(1) {
                    self.line(depth + 1, "fallback={");
                    let arm = self.arm(fallback, inner, depth + 2);
                    self.out.push_str(&arm);
                    self.line(depth + 1, "}");
                }
                self.line(depth, ">");
                let arm = self.arm(&arms[0], inner, depth + 1);
                self.out.push_str(&arm);
                self.line(depth, "</Show>");
            }
            _ => {
                self.solid.insert("Switch");
                self.solid.insert("Match");
                self.line(depth, "<Switch>");
                for (at, arm) in arms.iter().enumerate() {
                    self.line(depth + 1, &format!("<Match when={{{sel} === {at}n}}>"));
                    let arm = self.arm(arm, inner, depth + 2);
                    self.out.push_str(&arm);
                    self.line(depth + 1, "</Match>");
                }
                self.line(depth, "</Switch>");
            }
        }
    }
}

/// The effects a mount of `content` runs, as the store takes them: those of
/// the instances it mounts, and at the root those of every instance no
/// region mounts, each calling its entries with the region bindings `scope`.
fn effects_literal(
    eval: &Eval<'_>,
    names: &BTreeMap<FuncId, String>,
    content: &Content,
    root: bool,
    scope: &[String],
) -> Vec<String> {
    let call = |entry: u32, extra: Option<&str>| {
        let name = names
            .get(&eval.func(entry))
            .cloned()
            .unwrap_or_else(|| format!("entry{entry}"));
        let mut args: Vec<String> = extra.map(str::to_owned).into_iter().collect();
        args.extend(scope.iter().map(|s| format!("{s}()")));
        format!("f.{name}({})", args.join(", "))
    };
    script::effects_of(eval, content, root)
        .map(|effect| {
            script::effect_literal(
                eval,
                effect,
                &|entry| format!("() => {}", call(entry, None)),
                &|entry| format!("(v: Value) => {}", call(entry, Some("v"))),
            )
        })
        .collect()
}

/// The identifiers a scope has given out.
#[derive(Default)]
struct Idents {
    used: BTreeSet<String>,
}

impl Idents {
    /// `wanted`, or it suffixed with `n` when that is taken or reserved.
    fn take(&mut self, wanted: &str, n: u32) -> String {
        let mut name = wanted.to_owned();
        if RESERVED.contains(&name.as_str()) || self.used.contains(&name) {
            name = format!("{wanted}{n}");
        }
        while self.used.contains(&name) {
            name.push('_');
        }
        self.used.insert(name.clone());
        name
    }
}

/// The words a JavaScript identifier cannot be, and globals it must not
/// shadow.
const RESERVED: &[&str] = &[
    "arguments",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "eval",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "Infinity",
    "instanceof",
    "interface",
    "let",
    "NaN",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// `name` as a JavaScript identifier.
fn ident(name: &str) -> String {
    let mut out: String = name
        .trim_start_matches("r#")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '$' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// `name` with its first letter, and each after an `_`, upper case.
fn pascal(name: &str) -> String {
    let mut out = String::new();
    let mut upper = true;
    for c in ident(name).chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        "View".to_owned()
    } else {
        out
    }
}

fn app(name: &str) -> String {
    format!(
        "// {GENERATED}\nimport View from \"./components/{name}\";\n\nexport default function App() {{\n  return <View />;\n}}\n"
    )
}

fn entry() -> String {
    format!(
        "// {GENERATED}\nimport {{ render }} from \"solid-js/web\";\nimport App from \"./App\";\n\nrender(() => <App />, document.getElementById(\"root\")!);\n"
    )
}

fn index(options: &SolidOptions) -> String {
    let mut lang = String::new();
    escape_attr(&options.lang, &mut lang);
    let mut title = String::new();
    super::html::escape_text(&options.title, &mut title);
    format!(
        "<!DOCTYPE html>\n<!-- {GENERATED} -->\n<html lang=\"{lang}\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n<title>{title}</title>\n</head>\n<body>\n<div id=\"root\" style=\"width: 100%; height: 100%\"></div>\n<script type=\"module\" src=\"/src/index.tsx\"></script>\n</body>\n</html>\n"
    )
}

fn package(options: &SolidOptions) -> String {
    format!(
        "{{\n  \"name\": {},\n  \"version\": {},\n  \"private\": true,\n  \"type\": \"module\",\n  \"scripts\": {{\n    \"dev\": \"vite\",\n    \"build\": \"tsc && vite build\",\n    \"preview\": \"vite preview\"\n  }},\n  \"dependencies\": {{\n    \"solid-js\": \"{SOLID}\"\n  }},\n  \"devDependencies\": {{\n    \"typescript\": \"{TYPESCRIPT}\",\n    \"vite\": \"{VITE}\",\n    \"vite-plugin-solid\": \"{VITE_SOLID}\"\n  }}\n}}\n",
        script::string(&options.package),
        script::string(&options.version)
    )
}

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "bundler",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "jsx": "preserve",
    "jsxImportSource": "solid-js",
    "strict": true,
    "allowJs": true,
    "noEmit": true,
    "isolatedModules": true,
    "skipLibCheck": true,
    "types": ["vite/client"]
  },
  "include": ["src"]
}
"#;

fn vite() -> String {
    format!(
        "// {GENERATED}\nimport {{ defineConfig }} from \"vite\";\nimport solid from \"vite-plugin-solid\";\n\nexport default defineConfig({{\n  base: \"./\",\n  plugins: [solid()],\n}});\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_declares_each_name_once() {
        assert_eq!(super::super::redeclared(RUNTIME), Vec::<&str>::new());
    }
}
