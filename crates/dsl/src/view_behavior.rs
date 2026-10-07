//! A compiled view's behavior as a runtime mounts it: the component's bytecode,
//! the UI cell each of its states is held in, each view node's handler routes
//! and the view's control-flow regions.
//!
//! The macros, the hot reload commit and the release package all mount a view's
//! handlers and regions from this one table, so they run the same under every
//! target.

use std::rc::Rc;

use viso_behavior::Module;
use viso_ui::StateValue;
use viso_ui::adaptive::EnvField;
pub use viso_view::{Control, ControlInput, LookArm, When};
use viso_view::{ControlKind, EventRoute, Route, ViewHost, ViewRegions};

use crate::behavior::Site;
use crate::frontend::{Compiled, SourceKind};
use crate::hir::ConstValue;
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
    /// Whether the view mounts anything on its root: an `effect`, or the
    /// tasks a `start` runs, which the root owns.
    pub fn mounts_on_root(&self) -> bool {
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
/// content under a `VirtualList`, a handler or a region in a fragment (which
/// has no component to run against), a handler for an event the runtime does
/// not deliver, and a handler or region entry whose body the behavior lowering
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
        ..
    } = walk;
    let regions = has_regions(&compiled.tree);
    let effects = compiled.component.as_ref().is_some_and(|c| {
        compiled
            .behavior
            .component(&c.schema.name)
            .is_some_and(|layout| !layout.effects.is_empty())
    });
    if sites.is_empty()
        && !regions
        && !effects
        && (nodes.is_empty() || compiled.component.is_none())
    {
        return Ok(None);
    }
    let Some(component) = &compiled.component else {
        let mut errors: Vec<MountError> = sites
            .iter()
            .map(|(_, _, _, _, site)| {
                MountError::new(
                    Some(site.at),
                    "a `ui!` fragment has no component state for a handler to run \
                     against; declare it with `component!`",
                )
            })
            .collect();
        if regions {
            errors.push(MountError::new(
                compiled.tree.items.first().map(|item| match item {
                    UiItem::Node(node) => node.origin,
                    UiItem::If(region) => region.origin,
                    UiItem::For(region) => region.origin,
                    UiItem::Match(region) => region.origin,
                }),
                "a `ui!` fragment has no component state for a control-flow region \
                 to run against; declare it with `component!`",
            ));
        }
        return Err(errors);
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
    for (key, kind, node) in nodes {
        let mut control = Control::new(kind);
        for (property, at, instance) in &node.control_reads {
            let Some(input) = kind.input(property) else {
                continue;
            };
            let site = Site {
                instance: *instance,
                at: *at,
                part: 0,
            };
            let Some(index) = layout.handler(site) else {
                errors.push(MountError::new(
                    Some(*at),
                    "internal: the control's value was not lowered",
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
            control.set_entry(input, index);
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
    if let Err(error) = ViewHost::new(Rc::clone(&module), name) {
        return Err(vec![MountError::new(
            Some(component.source_origin),
            format!("the behavior does not mount: {error}"),
        )]);
    }
    let index = module
        .component(name)
        .expect("the module keeps the program's components");
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
    }))
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
        let Some(input) = kind.input(&look.property) else {
            continue;
        };
        if let Some(index) = look.base.and_then(|part| entry(part, errors)) {
            control.set_entry(input, index);
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
                input,
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
    /// Each view-driven native node: a control, or a label showing a text.
    nodes: Vec<(NodeKey, ControlKind, &'a UiNode)>,
    /// Each component instance's root node, the first of its view's nodes.
    roots: Vec<(u32, NodeKey)>,
}

impl<'a> Walk<'a> {
    fn item(&mut self, item: &'a UiItem) {
        match item {
            UiItem::Node(node) => self.node(node),
            UiItem::If(region) => {
                for arm in &region.arms {
                    self.items(&arm.items);
                }
            }
            UiItem::For(region) => self.items(&region.body),
            UiItem::Match(region) => {
                for arm in &region.arms {
                    self.items(&arm.items);
                }
            }
        }
    }

    fn items(&mut self, items: &'a [UiItem]) {
        for item in items {
            self.item(item);
        }
    }

    fn node(&mut self, node: &'a UiNode) {
        let own = NodeKey(self.key);
        self.key += 1;
        if !self.roots.iter().any(|(i, _)| *i == node.instance) {
            self.roots.push((node.instance, own));
        }
        let kind = ControlKind::of(&node.type_name);
        if kind.responds() || !node.control_reads.is_empty() || node.styled.is_some() {
            self.nodes.push((own, kind, node));
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
        self.items(&node.children);
    }
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
