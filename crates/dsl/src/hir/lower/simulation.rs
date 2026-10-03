//! The game state layers: `@local` state, the Simulation domain a system's
//! Simulation hooks and every callable they reach form, and the checks that
//! keep gameplay independent of presentation and reproducible.
//!
//! Simulation code may not touch `@local` state or use a Presentation value
//! (`E9103`), nor reach a non-deterministic source (`E9104`): a native below
//! the package's determinism tier, a `task` or `await`, or the adaptive
//! environment. Its state must snapshot (`E9105`). A Presentation command it
//! calls is deferred by the scheduler, so it is allowed.

use std::collections::{HashMap, HashSet, VecDeque};

use viso_behavior::native::{Determinism, HookDomain, NativeId, NativeKind, SchemaTy};

use crate::ast::{AstNode, Item, Member, decl_attributes};
use crate::diag::{Diagnostic, Related};
use crate::hir::component::MemberEnv;
use crate::hir::infer::{TypeEnv, VariantPayload};
use crate::resolve::{Resolution, ResolvedRef, SymbolId, SymbolKind};
use crate::syntax::{SyntaxKind, SyntaxNode, TextRange};

use super::{Declarations, HirComponent, InputDevices, ModuleEnv, Ty, name_of};

/// What the package's targets are and how it is built: what game input and
/// determinism checks hold it to, the fixed step its games run at, and
/// whether debug draw is compiled out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetProfile {
    /// The input devices the targets have.
    pub devices: InputDevices,
    /// The determinism tier the Simulation domain needs (`[game] determinism`).
    pub determinism: Determinism,
    /// The ticks a second of the fixed step (`[game] tick_rate`), at least 1:
    /// tick timers convert their durations with it.
    pub tick_rate: u32,
    /// A release build, which removes debug draw.
    pub release: bool,
}

impl Default for TargetProfile {
    fn default() -> TargetProfile {
        TargetProfile {
            devices: InputDevices::default(),
            determinism: Determinism::SameBinary,
            tick_rate: viso_behavior::DEFAULT_TICK_RATE,
            release: false,
        }
    }
}

/// A system's hook: the action implementing it, the trait hook's name and
/// its domain.
pub(super) type BoundHook = (SymbolId, String, HookDomain);

/// One callable body: what it mentions, calls natively and reads of the
/// environment, by span.
struct Body {
    module: usize,
    name: String,
    mentions: Vec<(SymbolId, TextRange)>,
    natives: Vec<(NativeId, TextRange)>,
    env: Vec<TextRange>,
    awaits: Vec<TextRange>,
}

/// The package's callables and Simulation roots, gathered module by module.
#[derive(Default)]
pub(super) struct Domains {
    bodies: HashMap<SymbolId, Body>,
    /// Each Simulation hook action and the `System.hook` it implements.
    roots: Vec<(SymbolId, String)>,
    /// Every `@local` state.
    locals: HashSet<SymbolId>,
}

/// Whether `node` carries the attribute `name`.
fn has_attribute(node: &SyntaxNode, name: &str) -> Option<SyntaxNode> {
    decl_attributes(node)
        .into_iter()
        .find(|(n, _, _)| n == name)
        .map(|(_, attr, _)| attr)
}

impl Domains {
    /// Records the callables, `@local` states and Simulation hooks of the
    /// module `env` lowered, checking each `@local` (it marks a system
    /// `state`, `E9103`) and each Simulation state's type (`E9105`).
    pub(super) fn collect(
        &mut self,
        items: &[Item],
        refs: &[ResolvedRef],
        env: &ModuleEnv<'_>,
        components: &[HirComponent],
        hooks: &[(SymbolId, Vec<BoundHook>)],
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        let natives = env.native_calls.borrow();
        let mut index: Vec<(TextRange, Resolution)> =
            refs.iter().map(|r| (r.range, r.to)).collect();
        index.sort_by_key(|(range, _)| range.start());
        let body = |name: String, node: &SyntaxNode| {
            let range = node.text_range();
            let start = index.partition_point(|(r, _)| r.start() < range.start());
            let inside = index[start..]
                .iter()
                .take_while(|(r, _)| r.end() <= range.end());
            let mut mentions = Vec::new();
            let mut uses_env = Vec::new();
            for &(at, to) in inside {
                match to {
                    Resolution::Symbol(s) => mentions.push((s, at)),
                    Resolution::Env => uses_env.push(at),
                    Resolution::Local(_) | Resolution::Native(_) => {}
                }
            }
            let calls = natives
                .iter()
                .filter(|(at, _)| range.contains_range(**at))
                .map(|(at, id)| (*id, *at))
                .collect();
            let awaits = node
                .descendants_with_tokens()
                .into_iter()
                .filter_map(|e| e.as_token().cloned())
                .filter(|t| t.kind() == SyntaxKind::AwaitKw)
                .map(|t| t.text_range())
                .collect();
            Body {
                module: env.module,
                name,
                mentions,
                natives: calls,
                env: uses_env,
                awaits,
            }
        };
        for item in items {
            let decl = match item {
                Item::Export(e) => e.declaration(),
                other => Some(other.clone()),
            };
            match decl {
                Some(Item::System(system)) => {
                    let component = system.as_component();
                    env.focus_component(&component);
                    let system_name = name_of(system.name());
                    let system_symbol = env.component_symbol();
                    let states = components
                        .iter()
                        .find(|c| c.source_origin == system.syntax().text_range())
                        .map(|c| &c.schema.states[..])
                        .unwrap_or_default();
                    for member in system.members() {
                        let node = member.syntax().clone();
                        let local = has_attribute(&node, "local");
                        let (name, is_body) = match &member {
                            Member::State(s) => (name_of(s.name()), false),
                            Member::Fn(f) => (name_of(f.name()), true),
                            Member::Action(a) => (name_of(a.name()), true),
                            Member::Computed(c) => (name_of(c.name()), true),
                            Member::Task(t) => (name_of(t.name()), true),
                            _ => (String::new(), false),
                        };
                        let symbol = env.member_symbol(&name);
                        if let Member::State(_) = member {
                            if local.is_some() {
                                self.locals.extend(symbol);
                            } else if let Some(state) = states
                                .iter()
                                .find(|s| symbol.is_some() && s.meta.resolved_symbol == symbol)
                            {
                                let ty = &state.meta.inferred_type;
                                if check_snapshot(ty, &name, &node, env, diagnostics)
                                    && let Some(symbol) = symbol
                                {
                                    env.behavior.borrow_mut().snapshot_state(
                                        system_symbol,
                                        symbol,
                                        schema_hash(ty, env),
                                    );
                                }
                            }
                        } else if let Some(attr) = local {
                            misplaced_local(&attr, diagnostics);
                        }
                        if is_body && let Some(symbol) = symbol {
                            let qualified = format!("{system_name}.{name}");
                            self.bodies.insert(symbol, body(qualified, &node));
                        }
                    }
                    for (owner, bound) in hooks {
                        if *owner != system_symbol {
                            continue;
                        }
                        for (action, hook, domain) in bound {
                            if *domain == HookDomain::Simulation {
                                self.roots.push((*action, format!("{system_name}.{hook}")));
                            }
                        }
                    }
                }
                Some(Item::Component(component)) => {
                    for member in component.members() {
                        if let Some(attr) = has_attribute(member.syntax(), "local") {
                            misplaced_local(&attr, diagnostics);
                        }
                    }
                }
                Some(Item::Fn(f)) => {
                    if let Some(&s) = env.scope.declared.get(&f.syntax().text_range()) {
                        self.bodies.insert(s, body(name_of(f.name()), f.syntax()));
                    }
                }
                Some(Item::Action(a)) => {
                    if let Some(&s) = env.scope.declared.get(&a.syntax().text_range()) {
                        self.bodies.insert(s, body(name_of(a.name()), a.syntax()));
                    }
                }
                _ => {}
            }
        }
    }

    /// Checks every callable the Simulation roots reach, reporting into the
    /// module that declares each.
    pub(super) fn check(
        &self,
        decls: &Declarations,
        natives: &viso_behavior::native::Natives,
        profile: TargetProfile,
        per_module: &mut [Vec<Diagnostic>],
    ) {
        // Breadth-first from the roots, each body reached once with the root
        // it was first reached from.
        let mut reached: HashMap<SymbolId, &str> = HashMap::new();
        let mut queue: VecDeque<SymbolId> = VecDeque::new();
        for (action, root) in &self.roots {
            if reached.insert(*action, root).is_none() {
                queue.push_back(*action);
            }
        }
        let mut order = Vec::new();
        while let Some(symbol) = queue.pop_front() {
            order.push(symbol);
            let Some(body) = self.bodies.get(&symbol) else {
                continue;
            };
            let root = reached[&symbol];
            for &(mention, _) in &body.mentions {
                if self.bodies.contains_key(&mention) && !reached.contains_key(&mention) {
                    reached.insert(mention, root);
                    queue.push_back(mention);
                }
            }
        }
        let mut reported: HashSet<TextRange> = HashSet::new();
        for symbol in order {
            let Some(body) = self.bodies.get(&symbol) else {
                continue;
            };
            let root = reached[&symbol];
            let mut found = Vec::new();
            for &(mention, at) in &body.mentions {
                if self.locals.contains(&mention) {
                    found.push((
                        "E9103",
                        at,
                        "the Simulation domain does not read or write `@local` state".to_owned(),
                    ));
                } else if decls.facts.get(&mention).map(|f| f.kind) == Some(SymbolKind::Task) {
                    found.push((
                        "E9104",
                        at,
                        "the Simulation domain does not start a `task`".to_owned(),
                    ));
                }
            }
            for &(id, at) in &body.natives {
                let Some(entry) = natives.function_by_id(id) else {
                    continue;
                };
                let f = entry.function;
                let name = entry.path.rsplit("::").next().unwrap_or(&entry.path);
                if f.presentation {
                    if f.ret != SchemaTy::Unit {
                        found.push((
                            "E9103",
                            at,
                            format!(
                                "`{name}` returns a Presentation value, which the Simulation \
                                 domain does not use"
                            ),
                        ));
                    }
                } else if f.kind == NativeKind::Task {
                    found.push((
                        "E9104",
                        at,
                        format!(
                            "`{name}` is a native task, which the Simulation domain does not await"
                        ),
                    ));
                } else if f.determinism < profile.determinism {
                    found.push((
                        "E9104",
                        at,
                        format!(
                            "`{name}` is reproducible at `{}`, below the `{}` the Simulation \
                             domain needs",
                            f.determinism.name(),
                            profile.determinism.name()
                        ),
                    ));
                }
            }
            for &at in &body.env {
                found.push((
                    "E9104",
                    at,
                    "the Simulation domain does not read the adaptive environment".to_owned(),
                ));
            }
            for &at in &body.awaits {
                found.push((
                    "E9104",
                    at,
                    "the Simulation domain does not `await`".to_owned(),
                ));
            }
            for (code, at, message) in found {
                if !reported.insert(at) {
                    continue;
                }
                let mut diagnostic = Diagnostic::error(code, at, message);
                diagnostic.notes.push(if body.name == root {
                    format!("`{root}` is a Simulation hook")
                } else {
                    format!(
                        "`{}` runs in the Simulation domain: `{root}` reaches it",
                        body.name
                    )
                });
                if let Some(module) = per_module.get_mut(body.module) {
                    module.push(diagnostic);
                }
            }
        }
    }
}

fn misplaced_local(attr: &SyntaxNode, diagnostics: &mut Vec<Diagnostic>) {
    diagnostics.push(Diagnostic::error(
        "E9103",
        attr.text_range(),
        "`@local` marks a `state` of a `system`: Presentation state the Simulation never reads",
    ));
}

/// Whether a value of `ty` can be captured in a snapshot; `E9105` if not.
fn check_snapshot(
    ty: &Ty,
    name: &str,
    node: &SyntaxNode,
    env: &ModuleEnv<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let mut seen = HashSet::new();
    let Some(why) = not_snapshot(ty, env, &mut seen) else {
        return true;
    };
    let at = node
        .children_with_tokens()
        .into_iter()
        .filter_map(|e| e.as_token().cloned())
        .find(|t| t.text() == name)
        .map_or(node.text_range(), |t| t.text_range());
    let mut diagnostic = Diagnostic::error(
        "E9105",
        at,
        format!("the Simulation state `{name}` does not implement `Snapshot`: {why}"),
    );
    diagnostic
        .notes
        .push("mark it `@local` if only Presentation uses it".to_owned());
    diagnostic.related.push(Related::new(
        at,
        "a Simulation state is snapshotted every tick",
    ));
    diagnostics.push(diagnostic);
    false
}

/// A hash of the schema of `ty`: its structure with every record's field
/// names and every enum's variants spelled out, so a snapshot value restores
/// only into a state whose type would read it the same way.
fn schema_hash(ty: &Ty, env: &ModuleEnv<'_>) -> u64 {
    let mut text = String::new();
    schema_text(ty, env, &mut Vec::new(), &mut text);
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn schema_text(ty: &Ty, env: &ModuleEnv<'_>, open: &mut Vec<SymbolId>, out: &mut String) {
    use std::fmt::Write;
    let each = |items: &[Ty], open: &mut Vec<SymbolId>, out: &mut String| {
        for item in items {
            schema_text(item, env, open, out);
            out.push(',');
        }
    };
    match ty {
        Ty::Named(id) => {
            let _ = write!(out, "{:x}{:x}", id.hi, id.lo);
            if open.contains(id) {
                return;
            }
            open.push(*id);
            if let Some(fields) = env.record_fields(*id) {
                out.push('{');
                for field in fields {
                    out.push_str(&field.name);
                    out.push(':');
                    schema_text(&field.ty, env, open, out);
                    out.push(',');
                }
                out.push('}');
            } else if let Some(variants) = env.enum_variants(*id) {
                out.push('[');
                for variant in variants {
                    out.push_str(&variant.name);
                    match &variant.payload {
                        VariantPayload::Unit => {}
                        VariantPayload::Tuple(items) => {
                            out.push('(');
                            each(items, open, out);
                            out.push(')');
                        }
                        VariantPayload::Record(fields) => {
                            out.push('{');
                            for field in fields {
                                out.push_str(&field.name);
                                out.push(':');
                                schema_text(&field.ty, env, open, out);
                                out.push(',');
                            }
                            out.push('}');
                        }
                    }
                    out.push(',');
                }
                out.push(']');
            }
            open.pop();
        }
        Ty::Tuple(items) => {
            out.push('(');
            each(items, open, out);
            out.push(')');
        }
        Ty::List(t) | Ty::Option(t) | Ty::Range(t) | Ty::RangeInclusive(t) => {
            let head = match ty {
                Ty::List(_) => "List<",
                Ty::Option(_) => "Option<",
                Ty::Range(_) => "Range<",
                _ => "RangeInclusive<",
            };
            out.push_str(head);
            schema_text(t, env, open, out);
            out.push('>');
        }
        Ty::Result(a, b) => {
            out.push_str("Result<");
            schema_text(a, env, open, out);
            out.push(',');
            schema_text(b, env, open, out);
            out.push('>');
        }
        other => {
            let _ = write!(out, "{other:?}");
        }
    }
}

/// Why a value of `ty` cannot be snapshotted, or `None` when it can: value
/// types derive `Snapshot`, a closure cannot, a native handle only when its
/// type declares it.
fn not_snapshot(ty: &Ty, env: &ModuleEnv<'_>, seen: &mut HashSet<SymbolId>) -> Option<String> {
    match ty {
        Ty::Fn(..) => Some("a closure has no snapshot".to_owned()),
        Ty::Native(id) => {
            let entry = env.natives.ty_by_id(*id)?;
            (!entry.ty.snapshots()).then(|| {
                let name = entry.path.rsplit("::").next().unwrap_or(&entry.path);
                format!("the native handle `{name}` declares no snapshot")
            })
        }
        Ty::Tuple(items) => items.iter().find_map(|t| not_snapshot(t, env, seen)),
        Ty::List(t) | Ty::Option(t) | Ty::Range(t) | Ty::RangeInclusive(t) => {
            not_snapshot(t, env, seen)
        }
        Ty::Result(a, b) => not_snapshot(a, env, seen).or_else(|| not_snapshot(b, env, seen)),
        Ty::Named(id) => {
            if !seen.insert(*id) {
                return None;
            }
            if let Some(fields) = env.record_fields(*id) {
                return fields.iter().find_map(|f| not_snapshot(&f.ty, env, seen));
            }
            let variants = env.enum_variants(*id)?;
            variants.iter().find_map(|v| match &v.payload {
                VariantPayload::Unit => None,
                VariantPayload::Tuple(items) => {
                    items.iter().find_map(|t| not_snapshot(t, env, seen))
                }
                VariantPayload::Record(fields) => {
                    fields.iter().find_map(|f| not_snapshot(&f.ty, env, seen))
                }
            })
        }
        _ => None,
    }
}

/// Each hook action of a system with its trait hook's name and domain.
pub(super) fn hook_domains(env: &ModuleEnv<'_>, hooks: &[(NativeId, SymbolId)]) -> Vec<BoundHook> {
    hooks
        .iter()
        .filter_map(|&(id, action)| {
            let (name, domain) = env.natives.traits().iter().find_map(|t| {
                t.native_trait
                    .hooks
                    .iter()
                    .find(|h| t.hook_id(h.name) == id)
                    .map(|h| (h.name.to_owned(), h.domain))
            })?;
            Some((action, name, domain))
        })
        .collect()
}
