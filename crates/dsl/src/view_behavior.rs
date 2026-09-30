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
use viso_view::{EventRoute, Route, ViewHost, ViewRegions};

use crate::behavior::Site;
use crate::frontend::{Compiled, SourceKind};
use crate::hir::ConstValue;
use crate::ir::binding_ir::NodeKey;
use crate::ir::ui_ir::{UiItem, UiNode};
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
}

impl ViewBehavior {
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

/// The behavior of `compiled`'s view: `None` when no node declares a handler
/// and the view has no region, so a view without behavior mounts no VM.
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
    let mut sites = Vec::new();
    let mut key = 0;
    for item in &compiled.tree.items {
        collect(item, &mut key, &mut sites);
    }
    let regions = has_regions(&compiled.tree);
    if sites.is_empty() && !regions {
        return Ok(None);
    }
    let Some(component) = &compiled.component else {
        let mut errors: Vec<MountError> = sites
            .iter()
            .map(|(_, _, site)| {
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
    for (node, event, site) in sites {
        let at = site.at;
        let Some(route) = EventRoute::of(event) else {
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
        match routes.last_mut() {
            Some((last, list)) if *last == node => list.push((route, index)),
            _ => routes.push((node, vec![(route, index)])),
        }
    }
    let regions = match view_regions(compiled, layout, &routes) {
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
    let bytes = module.encode();
    Ok(Some(ViewBehavior {
        module,
        bytes,
        component: name.clone(),
        slots,
        regions,
        routes,
    }))
}

/// Records every handler under `item` as `(node, event, at)`, numbering nodes
/// in the pre-order the Binding IR keys them by.
fn collect<'a>(item: &'a UiItem, key: &mut u32, sites: &mut Vec<(NodeKey, &'a str, Site)>) {
    let walk = |items: &'a [UiItem], key: &mut u32, sites: &mut Vec<_>| {
        for item in items {
            collect(item, key, sites);
        }
    };
    match item {
        UiItem::Node(node) => node_sites(node, key, sites),
        UiItem::If(region) => {
            for arm in &region.arms {
                walk(&arm.items, key, sites);
            }
        }
        UiItem::For(region) => walk(&region.body, key, sites),
        UiItem::Match(region) => {
            for arm in &region.arms {
                walk(&arm.items, key, sites);
            }
        }
    }
}

fn node_sites<'a>(node: &'a UiNode, key: &mut u32, sites: &mut Vec<(NodeKey, &'a str, Site)>) {
    let own = NodeKey(*key);
    *key += 1;
    for handler in &node.handlers {
        let event = handler.event.trim_start_matches("r#");
        sites.push((
            own,
            event,
            Site {
                instance: handler.instance,
                at: handler.origin,
            },
        ));
    }
    for child in &node.children {
        collect(child, key, sites);
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
