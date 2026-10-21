//! A compiled view's behavior as a runtime mounts it: the component's bytecode,
//! the UI cell each of its states is held in, each view node's handler routes
//! and the view's control-flow regions.
//!
//! The macros, the hot reload commit and the release package all mount a view's
//! handlers and regions from this one table, so they run the same under every
//! target.

use std::rc::Rc;

use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Vm};
pub use viso_schema::view::{Control, ControlInput, LookArm, Switched, When};
use viso_schema::view::{ControlKind, EventRoute, LayoutSlot, Route, ViewRegions};
use viso_schema::{EnvField, StateValue};
use viso_shader::program::BrushLayout;

use crate::behavior::Site;
use crate::frontend::{Compiled, SourceKind};
use crate::hir::{CheckedShader, ConstValue};
use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{UiItem, UiNode, UiStyled, UiWhen};
use crate::resolve::SymbolId;
use crate::syntax::TextRange;
use crate::view_regions::{has_regions, structure_errors, view_regions};

/// The diagnostic code of a handler or a region that does not mount.
pub const UNMOUNTED_HANDLER: &str = "E3711";

/// Why a view's behavior does not mount.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountError {
    /// Where the author wrote what does not mount.
    pub at: Option<TextRange>,
    /// What does not mount, and why.
    pub message: String,
}

impl MountError {
    /// The error as a diagnostic at its span, or at `fallback` when it has none.
    pub fn diagnostic(&self, fallback: TextRange) -> crate::diag::Diagnostic {
        crate::diag::Diagnostic::error(
            UNMOUNTED_HANDLER,
            self.at.unwrap_or(fallback),
            self.message.clone(),
        )
    }

    pub(crate) fn new(at: Option<TextRange>, message: impl Into<String>) -> Self {
        Self {
            at,
            message: message.into(),
        }
    }
}

/// A view's behavior.
#[derive(Debug, Clone)]
pub struct ViewBehavior {
    /// The verified module.
    pub module: Rc<Module>,
    /// The module's wire form.
    pub bytes: Vec<u8>,
    /// The component the view mounts.
    pub component: String,
    /// Each state source and its slot in the component.
    pub slots: Vec<(SymbolId, u32)>,
    /// The control-flow regions, empty for a view without any.
    pub regions: ViewRegions,
    /// Each node's routes, by ascending node key; a node without handlers is
    /// absent.
    routes: Vec<(NodeKey, Vec<Route>)>,
    /// Each view-driven native node, by ascending node key: a control's
    /// response and a label's text.
    controls: Vec<(NodeKey, Control)>,
    /// Each `env` field the view reads.
    pub env: Vec<EnvRead>,
    /// Each node its component's actions and handlers reach as a `NodeRef`,
    /// by ascending key: the static ones the view mounts with it, the ones
    /// regions mount carrying theirs in their templates.
    pub named: Vec<(u32, NodeKey)>,
    /// The view's shader table: each shader its shader views paint with, in
    /// the order their controls index it.
    pub shaders: Vec<ViewBrush>,
}

/// A shader a view paints with: its program and its layout as a brush.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewBrush {
    pub program: viso_shader::program::Program,
    pub layout: BrushLayout,
}

/// An `env` field a view reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvRead {
    /// The component's state slot holding it.
    pub slot: u32,
    /// The field.
    pub field: EnvField,
    /// The root node of the component instance that reads it, where its
    /// anchored fields resolve.
    pub anchor: NodeKey,
}

impl ViewBehavior {
    /// The module compiled to Rust, every chunk, for the release build to
    /// install in place of interpreting it (ADR 0041). `krate` is the path the
    /// code names `viso_behavior` by where it is compiled.
    pub fn native_code(&self, krate: &str) -> String {
        viso_behavior::aot::lower_module(&self.module, krate)
    }

    /// The wire form of the module as the build running
    /// [`native_code`](Self::native_code) carries it
    /// ([`Module::compiled`]): no bytecode, only what that code and the VM
    /// read of each body.
    pub fn compiled_bytes(&self) -> Vec<u8> {
        self.module.compiled().encode()
    }

    /// Whether the view mounts anything on its root: an `effect`, the
    /// tasks a `start` runs, or the game its module's systems make, which the
    /// root owns.
    pub fn mounts_on_root(&self) -> bool {
        if !self.module.systems().is_empty() {
            return true;
        }
        let effects = self
            .module
            .component(&self.component)
            .is_some_and(|index| !self.module.layout(index).effects.is_empty());
        effects
            || self
                .module
                .chunks()
                .iter()
                .any(|chunk| chunk.kind == viso_behavior::ChunkKind::Task)
    }

    /// The routes of the node `key`.
    pub fn routes(&self, key: NodeKey) -> &[Route] {
        self.routes
            .binary_search_by_key(&key, |(k, _)| *k)
            .map_or(&[], |i| &self.routes[i].1)
    }

    /// Every node with routes, by ascending node key.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeKey, &[Route])> {
        self.routes
            .iter()
            .map(|(key, routes)| (*key, routes.as_slice()))
    }

    /// The view-driven native node `key`: a control's response or a label's
    /// text.
    pub fn control(&self, key: NodeKey) -> Option<Control> {
        self.controls
            .binary_search_by_key(&key, |(k, _)| *k)
            .ok()
            .map(|i| self.controls[i].1.clone())
    }

    /// Every view-driven native node, by ascending node key.
    pub fn controls(&self) -> impl Iterator<Item = (NodeKey, Control)> + '_ {
        self.controls.iter().cloned()
    }

    /// The wire form of [`regions`](Self::regions), which a macro expansion
    /// embeds.
    pub fn region_bytes(&self) -> Vec<u8> {
        use viso_ende::Encode;
        self.regions.encode_to_vec()
    }

    /// The slot of state source `symbol`.
    pub fn slot(&self, symbol: SymbolId) -> Option<u32> {
        self.slots
            .iter()
            .find(|(s, _)| *s == symbol)
            .map(|(_, slot)| *slot)
    }
}

/// The behavior of `compiled`'s view: `None` when no node declares a handler,
/// the view has no region, no effect and no view-driven native node of a
/// component, so a view without behavior mounts no VM.
///
/// # Errors
///
/// Every handler and region that does not mount: a region at the root or
/// content under a `VirtualList`, a handler for an event the runtime does not
/// deliver, and a handler or region entry whose body the behavior lowering
/// does not represent.
pub fn view_behavior(compiled: &Compiled) -> Result<Option<ViewBehavior>, Vec<MountError>> {
    let mut errors = structure_errors(&compiled.tree);
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut walk = Walk::default();
    for item in &compiled.tree.items {
        walk.item(item);
    }
    let Walk {
        sites,
        nodes,
        roots,
        mut named,
        ..
    } = walk;
    named.sort_unstable();
    let regions = has_regions(&compiled.tree);
    let effects = compiled.component.as_ref().is_some_and(|c| {
        compiled
            .behavior
            .component(&c.schema.name)
            .is_some_and(|layout| !layout.effects.is_empty())
    });
    // A module declaring systems runs a game on the view's root.
    let game = compiled.component.is_some() && !compiled.behavior.systems.is_empty();
    if sites.is_empty()
        && named.is_empty()
        && !regions
        && !effects
        && !game
        && (nodes.is_empty() || compiled.component.is_none())
    {
        return Ok(None);
    }
    // Only a source that failed to compile declares no component.
    let Some(component) = &compiled.component else {
        return Ok(None);
    };
    let name = &component.schema.name;
    let Some(layout) = compiled.behavior.component(name) else {
        return Err(vec![MountError::new(
            Some(component.source_origin),
            format!("internal: component `{name}` has no behavior layout"),
        )]);
    };
    let mut routes: Vec<(NodeKey, Vec<Route>)> = Vec::new();
    for (node, kind, event, capture, site) in sites {
        let at = site.at;
        let route = EventRoute::of(event).or_else(|| kind.route(event));
        let Some(route) = route else {
            errors.push(MountError::new(
                Some(at),
                format!("the runtime does not deliver `{event}` events yet"),
            ));
            continue;
        };
        let Some(index) = layout.handler(site) else {
            errors.push(MountError::new(
                Some(at),
                "internal: the handler was not lowered",
            ));
            continue;
        };
        let function = compiled
            .behavior
            .function(layout.handlers[index as usize].1);
        if let Err(unsupported) = &function.body {
            errors.push(MountError::new(
                Some(unsupported.at),
                format!("the handler {}", unsupported.reason),
            ));
            continue;
        }
        let route = Route {
            event: route,
            handler: index,
            capture,
        };
        match routes.last_mut() {
            Some((last, list)) if *last == node => list.push(route),
            _ => routes.push((node, vec![route])),
        }
    }
    let mut controls = Vec::with_capacity(nodes.len());
    let mut shaders: Vec<(SymbolId, ViewBrush)> = Vec::new();
    for (key, kind, node, item) in nodes {
        let mut control = Control::new(kind);
        control.semantics.role = node_role(node, item);
        control.semantics.live = node.live.unwrap_or_default();
        for (property, at, instance) in &node.control_reads {
            let input = kind.input(property);
            let slot = LayoutSlot::of(property).filter(|_| input.is_none());
            if input.is_none() && slot.is_none() {
                continue;
            }
            if kind == ControlKind::Shader && input == Some(ControlInput::Value) {
                match painted(compiled, *instance, *at) {
                    Some((symbol, brush)) => {
                        let at = match shaders.iter().position(|(s, _)| *s == symbol) {
                            Some(at) => at,
                            None => {
                                shaders.push((symbol, brush));
                                shaders.len() - 1
                            }
                        };
                        control.shader = Some(at as u32);
                    }
                    None => errors.push(MountError::new(
                        Some(*at),
                        "internal: the shader the view paints with was not lowered",
                    )),
                }
            }
            let site = Site {
                instance: *instance,
                at: *at,
                part: 0,
            };
            let Some(index) = layout.handler(site) else {
                errors.push(MountError::new(
                    Some(*at),
                    match slot {
                        Some(_) => format!(
                            "a `{property}` the compiler cannot fold does not lay out yet; a constant or one reading a value does"
                        ),
                        None => "internal: the control's value was not lowered".to_string(),
                    },
                ));
                continue;
            };
            let function = compiled
                .behavior
                .function(layout.handlers[index as usize].1);
            if let Err(unsupported) = &function.body {
                errors.push(MountError::new(
                    Some(unsupported.at),
                    format!("the control's `{property}` {}", unsupported.reason),
                ));
                continue;
            }
            match (input, slot) {
                (Some(input), _) => control.set_entry(input, index),
                (None, Some(slot)) => control.set_layout(slot, index),
                (None, None) => {}
            }
        }
        if let Some(styled) = &node.styled {
            styled_look(
                compiled,
                layout,
                kind,
                node.instance,
                styled,
                &mut control,
                &mut errors,
            );
        }
        controls.push((key, control));
    }
    let regions = match view_regions(compiled, layout, &routes, &controls) {
        Ok(regions) => regions,
        Err(region_errors) => {
            errors.extend(region_errors);
            ViewRegions::default()
        }
    };
    let module = match compiled.behavior.bytecode() {
        Ok(module) => module,
        Err(error) => {
            errors.push(MountError::new(
                Some(component.source_origin),
                format!("internal: the behavior does not verify: {error:?}"),
            ));
            return Err(errors);
        }
    };
    if !errors.is_empty() {
        return Err(errors);
    }
    let module = Rc::new(module);
    let Some(index) = module.component(name) else {
        return Err(vec![MountError::new(
            Some(component.source_origin),
            format!("the behavior does not mount: the behavior module has no component `{name}`"),
        )]);
    };
    // The runtime links the module against the standard natives under the
    // grant it asks for when it mounts the view; a conflict there is the
    // compiler's to report.
    let grant: Vec<&str> = module.capabilities().iter().map(|c| &**c).collect();
    let mut vm = Vm::new(Rc::clone(&module), Budget::default());
    if let Err(conflict) = vm.link(&Natives::standard(), &grant) {
        return Err(vec![MountError::new(
            Some(component.source_origin),
            format!("the behavior does not mount: the behavior module does not link: {conflict}"),
        )]);
    }
    let states = module.layout(index);
    let slots = compiled
        .sources
        .iter()
        .filter(|source| matches!(source.kind, SourceKind::State { .. }))
        .filter_map(|source| Some((source.symbol, states.state(&source.name)? as u32)))
        .collect();
    let env = layout
        .env
        .iter()
        .filter_map(|read| {
            let &(_, anchor) = roots.iter().find(|(i, _)| *i == read.instance)?;
            Some(EnvRead {
                slot: read.slot,
                field: read.field,
                anchor,
            })
        })
        .collect();
    let bytes = module.encode();
    Ok(Some(ViewBehavior {
        module,
        bytes,
        component: name.clone(),
        slots,
        regions,
        routes,
        controls,
        env,
        named,
        shaders: shaders.into_iter().map(|(_, brush)| brush).collect(),
    }))
}

/// The shader the `shader` value at `at`, in the view of instance `instance`,
/// paints with, and it as a brush; `None` when it did not check as one.
fn painted(compiled: &Compiled, instance: u32, at: TextRange) -> Option<(SymbolId, ViewBrush)> {
    let component = match compiled.tree.instance(instance) {
        Some(inlined) => Some(inlined.component),
        None => compiled.component.as_ref().map(|c| c.schema.symbol),
    };
    let paint = compiled
        .paints
        .iter()
        .find(|paint| paint.component == component && paint.at == at)?;
    let CheckedShader {
        program: Some(program),
        brush: Some(layout),
        ..
    } = compiled
        .shaders
        .iter()
        .find(|shader| shader.symbol == Some(paint.shader))?
    else {
        return None;
    };
    Some((
        paint.shader,
        ViewBrush {
            program: program.clone(),
            layout: layout.clone(),
        },
    ))
}

/// Fills `control` (of a node of kind `kind` in instance `instance`) with the
/// look its styles `styled` give it: each base value as the property's entry,
/// each arm as a [`LookArm`] in priority order.
fn styled_look(
    compiled: &Compiled,
    layout: &crate::behavior::ComponentLayout,
    kind: ControlKind,
    instance: u32,
    styled: &UiStyled,
    control: &mut Control,
    errors: &mut Vec<MountError>,
) {
    let entry = |part: u32, errors: &mut Vec<MountError>| -> Option<u32> {
        let site = Site {
            instance,
            at: styled.at,
            part,
        };
        let Some(index) = layout.handler(site) else {
            errors.push(MountError::new(
                Some(styled.at),
                "internal: the style's value was not lowered",
            ));
            return None;
        };
        let function = compiled
            .behavior
            .function(layout.handlers[index as usize].1);
        if let Err(unsupported) = &function.body {
            errors.push(MountError::new(
                Some(unsupported.at),
                format!("the style's value {}", unsupported.reason),
            ));
            return None;
        }
        Some(index)
    };
    let mut arms = Vec::new();
    for look in &styled.looks {
        let target = match (kind.input(&look.property), LayoutSlot::of(&look.property)) {
            (Some(input), _) => Switched::Input(input),
            (None, Some(slot)) => Switched::Layout(slot),
            (None, None) => continue,
        };
        if let Some(index) = look.base.and_then(|part| entry(part, errors)) {
            match target {
                Switched::Input(input) => control.set_entry(input, index),
                Switched::Layout(slot) => control.set_layout(slot, index),
            }
        }
        for arm in &look.arms {
            let mut when = Vec::with_capacity(arm.when.len());
            for step in &arm.when {
                when.push(match *step {
                    UiWhen::State(state) => When::State(state),
                    UiWhen::Part(part) => match entry(part, errors) {
                        Some(index) => When::Entry(index),
                        None => return,
                    },
                    UiWhen::Not => When::Not,
                    UiWhen::And => When::And,
                    UiWhen::Or => When::Or,
                });
            }
            let Some(index) = entry(arm.part, errors) else {
                return;
            };
            arms.push(LookArm {
                target,
                when: when.into(),
                entry: index,
            });
        }
    }
    control.arms = arms.into();
}

/// The handler sites and view-driven native nodes of a view, numbered in the
/// pre-order the Binding IR keys nodes by.
#[derive(Default)]
struct Walk<'a> {
    /// The next node key.
    key: u32,
    /// Each handler: its node, the node's control kind, its event and its site.
    sites: Vec<(NodeKey, ControlKind, &'a str, bool, Site)>,
    /// Each view-driven native node: a control, or a label showing a text,
    /// with the role it takes as an item of the selection it is in.
    nodes: Vec<(NodeKey, ControlKind, &'a UiNode, Option<viso_schema::Role>)>,
    /// Each component instance's root node, the first of its view's nodes.
    roots: Vec<(u32, NodeKey)>,
    /// Each node its component reaches as a `NodeRef`, by its key.
    named: Vec<(u32, NodeKey)>,
    /// The role the next node takes as an item: a tab of a `Tabs`, a radio
    /// button of a `RadioGroup`, whose direct children (in or out of a
    /// control-flow region) are its items.
    item: Option<viso_schema::Role>,
}

impl<'a> Walk<'a> {
    fn item(&mut self, item: &'a UiItem) {
        let role = self.item;
        match item {
            UiItem::Node(node) => self.node(node),
            UiItem::If(region) => {
                for arm in &region.arms {
                    self.item = role;
                    self.items(&arm.items);
                }
            }
            UiItem::For(region) => self.items(&region.body),
            UiItem::Match(region) => {
                for arm in &region.arms {
                    self.item = role;
                    self.items(&arm.items);
                }
            }
        }
    }

    /// The items of a region, each an item of the selection the region is in
    /// when it is one's child.
    fn items(&mut self, items: &'a [UiItem]) {
        let item = self.item;
        for each in items {
            self.item = item;
            self.item(each);
        }
        self.item = None;
    }

    fn node(&mut self, node: &'a UiNode) {
        let own = NodeKey(self.key);
        self.key += 1;
        if !self.roots.iter().any(|(i, _)| *i == node.instance) {
            self.roots.push((node.instance, own));
        }
        if let Some(key) = node.node_ref {
            self.named.push((key, own));
        }
        let kind = ControlKind::of(&node.type_name);
        let item = self.item.take();
        if kind.responds()
            || !node.control_reads.is_empty()
            || node.styled.is_some()
            || node.role.is_some()
            || node.live.is_some()
            || item.is_some()
        {
            self.nodes.push((own, kind, node, item));
        }
        for handler in &node.handlers {
            let event = handler.event.trim_start_matches("r#");
            self.sites.push((
                own,
                kind,
                event,
                handler.capture,
                Site {
                    instance: handler.instance,
                    at: handler.origin,
                    part: 0,
                },
            ));
        }
        let items = match node.type_name.as_str() {
            "Tabs" => Some(viso_schema::Role::Tab),
            "RadioGroup" => Some(viso_schema::Role::Radio),
            _ => None,
        };
        self.item = items;
        self.items(&node.children);
    }
}

/// The role of `node`: the one its `semantics.role` names, else the one it
/// takes as an item of a selection, else its widget's.
fn node_role(node: &UiNode, item: Option<viso_schema::Role>) -> Option<viso_schema::Role> {
    node.role
        .or(item)
        .or_else(|| viso_schema::view::widget_role(&node.type_name))
}

/// A state initializer as the UI cell value it starts from: `None` for a value
/// no cell holds.
pub fn state_value(initial: &ConstValue) -> Option<StateValue> {
    Some(match initial {
        ConstValue::Bool(value) => StateValue::Bool(*value),
        ConstValue::Int(value, _) => StateValue::Int(i32::try_from(*value).ok()?),
        ConstValue::Float(value, _) => {
            let value = *value as f32;
            if !value.is_finite() {
                return None;
            }
            StateValue::Float(value)
        }
        ConstValue::Str(_) => return None,
    })
}
