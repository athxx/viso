//! The Web accessibility bridge (ADR 0030): an ARIA mirror of the published
//! tree, transparent elements laid over the canvas where each node draws.
//!
//! AccessKit has no Web adapter, and a page cannot tell whether a screen
//! reader runs. The mirror therefore starts as a single transparent "Enable
//! accessibility" button; activating it (a screen reader clicks it) queues
//! [`AccessRequest::Activated`], and the facade's next frame sends the tree.
//! From then on each update is folded into a [`TreeMirror`] and only the
//! elements of changed nodes are rewritten. Each element is positioned
//! against its parent's, so a moved node moves its children's elements too.
//! The mirror takes no pointer input: the canvas keeps every mouse and touch
//! event. A screen reader's click on an element queues a click on its node;
//! arrow keys on a focused slider step it.

use std::collections::{HashMap, HashSet};

use wasm_bindgen::JsCast;
use web_sys::{Document, Element, Event, HtmlElement, KeyboardEvent};

use super::{Listener, WINDOW, drive, push};
use crate::access_mirror::TreeMirror;
use crate::accessibility::accesskit::{Action, Node, Rect, Role, TreeUpdate};
use crate::accessibility::{AccessAction, AccessRequest};
use crate::backend::web_translate::{aria_checked, aria_role, named_by_content, slider_step};
use crate::event::RawEvent;

/// The mirror's root: fixed over the canvas, invisible, and transparent to
/// pointer input.
const CONTAINER_STYLE: &str = "position:fixed;left:0;top:0;width:0;height:0;overflow:hidden;\
    opacity:0;color:transparent;pointer-events:none";

const ELEMENT_STYLE: &str = "position:absolute;overflow:hidden;margin:0;padding:0;border:0";

/// The attribute carrying a node's id on its element.
const ID_ATTRIBUTE: &str = "data-viso-node";

/// One window's mirror.
pub(super) struct WebAccess {
    container: HtmlElement,
    /// The activation button, until it is pressed.
    placeholder: Option<HtmlElement>,
    mirror: TreeMirror,
    elements: HashMap<u64, HtmlElement>,
    _listeners: Vec<Listener>,
}

impl WebAccess {
    /// Build the mirror, still inactive, as the last child of `parent`.
    pub(super) fn attach(document: &Document, parent: &Element) -> Option<Self> {
        let container: HtmlElement = document.create_element("div").ok()?.unchecked_into();
        container.set_attribute("style", CONTAINER_STYLE).ok()?;
        let placeholder: HtmlElement = document.create_element("button").ok()?.unchecked_into();
        placeholder.set_attribute("style", ELEMENT_STYLE).ok()?;
        placeholder
            .set_attribute("aria-label", "Enable accessibility")
            .ok()?;
        container.append_child(&placeholder).ok()?;
        parent.append_child(&container).ok()?;
        let listeners = vec![
            Listener::new(&container, "click", on_click),
            Listener::new(&container, "keydown", on_key),
        ];
        Some(Self {
            container,
            placeholder: Some(placeholder),
            mirror: TreeMirror::new(),
            elements: HashMap::new(),
            _listeners: listeners,
        })
    }

    pub(super) fn remove(&self) {
        self.container.remove();
    }

    /// Fold `update` in and patch the elements it changed. `canvas` is the
    /// canvas's client rect, `scale` device pixels per CSS pixel.
    pub(super) fn update(
        &mut self,
        document: &Document,
        update: TreeUpdate,
        canvas: (f64, f64, f64, f64),
        scale: f64,
    ) {
        if self.placeholder.is_some() {
            return;
        }
        let style = self.container.style();
        let (left, top, width, height) = canvas;
        for (name, value) in [
            ("left", left),
            ("top", top),
            ("width", width),
            ("height", height),
        ] {
            let _ = style.set_property(name, &format!("{value}px"));
        }

        let changes = self.mirror.apply(update);
        for id in &changes.removed {
            if let Some(element) = self.elements.remove(id) {
                element.remove();
            }
        }
        let changed: HashSet<u64> = changes.changed.iter().copied().collect();
        let Self {
            container,
            mirror,
            elements,
            ..
        } = self;
        // Rewrite changed nodes, and place each node whose own or parent's
        // bounds may have moved.
        mirror.walk(|id, node, parent| {
            let fresh = !elements.contains_key(&id);
            let Some(element) = element_for(elements, document, id) else {
                return;
            };
            if fresh || changed.contains(&id) {
                describe(&element, node);
            }
            if fresh || changed.contains(&id) || parent.is_some_and(|p| changed.contains(&p)) {
                let origin = parent.and_then(|p| mirror.get(p)).and_then(Node::bounds);
                place(&element, node.bounds(), origin, scale);
            }
        });
        // Put the children of every changed node in order under it; a fresh
        // node's parent changed with it, the root hangs off the container.
        for &id in &changes.changed {
            let Some(node) = mirror.get(id) else { continue };
            let Some(parent) = elements.get(&id) else {
                continue;
            };
            for child in node.children() {
                if let Some(element) = elements.get(&child.0) {
                    let _ = parent.append_child(element);
                }
            }
        }
        if let Some(root) = mirror.root().and_then(|id| elements.get(&id))
            && root.parent_element().as_deref() != Some(container.as_ref())
        {
            let _ = container.append_child(root);
        }
    }

    /// The activation button was pressed: drop it and start publishing.
    fn activate(&mut self) -> bool {
        let Some(placeholder) = self.placeholder.take() else {
            return false;
        };
        placeholder.remove();
        true
    }

    fn supports(&self, id: u64, action: Action) -> bool {
        self.mirror
            .get(id)
            .is_some_and(|node| node.supports_action(action))
    }
}

/// The element mirroring `id`, created on first use.
fn element_for(
    elements: &mut HashMap<u64, HtmlElement>,
    document: &Document,
    id: u64,
) -> Option<HtmlElement> {
    if let Some(element) = elements.get(&id) {
        return Some(element.clone());
    }
    let element: HtmlElement = document.create_element("div").ok()?.unchecked_into();
    element.set_attribute("style", ELEMENT_STYLE).ok()?;
    element.set_attribute(ID_ATTRIBUTE, &id.to_string()).ok()?;
    elements.insert(id, element.clone());
    Some(element)
}

/// Write `node`'s role, name and state onto its element.
fn describe(element: &HtmlElement, node: &Node) {
    let role = node.role();
    let set = |name: &str, value: Option<&str>| {
        let _ = match value {
            Some(value) => element.set_attribute(name, value),
            None => element.remove_attribute(name),
        };
    };
    set("role", aria_role(role));
    if named_by_content(role) {
        set("aria-label", None);
        element.set_text_content(node.label());
    } else {
        set("aria-label", node.label());
    }
    let number = |value: Option<f64>| value.map(|v| v.to_string());
    set("aria-valuenow", number(node.numeric_value()).as_deref());
    set("aria-valuemin", number(node.min_numeric_value()).as_deref());
    set("aria-valuemax", number(node.max_numeric_value()).as_deref());
    let checkable = matches!(role, Role::CheckBox | Role::RadioButton);
    set(
        "aria-checked",
        node.toggled().filter(|_| checkable).map(aria_checked),
    );
    let flag = |value: Option<bool>| value.map(|v| if v { "true" } else { "false" });
    set("aria-expanded", flag(node.is_expanded()));
    set("aria-selected", flag(node.is_selected()));
    // Focusable by a screen reader, not by Tab: the canvas's own focus
    // order stays the only one.
    set(
        "tabindex",
        node.supports_action(Action::Focus).then_some("-1"),
    );
}

/// Size and place an element in CSS pixels against its parent's bounds.
fn place(element: &HtmlElement, bounds: Option<Rect>, origin: Option<Rect>, scale: f64) {
    let bounds = bounds.unwrap_or_default();
    let (x, y) = origin.map_or((0.0, 0.0), |o| (o.x0, o.y0));
    let style = element.style();
    for (name, value) in [
        ("left", bounds.x0 - x),
        ("top", bounds.y0 - y),
        ("width", bounds.width()),
        ("height", bounds.height()),
    ] {
        let _ = style.set_property(name, &format!("{}px", value / scale));
    }
}

/// The node whose element (or a descendant of it) `event` targets.
fn target_node(event: &Event) -> Option<u64> {
    let target: Element = event.target()?.dyn_into().ok()?;
    let element = target.closest(&format!("[{ID_ATTRIBUTE}]")).ok()??;
    element.get_attribute(ID_ATTRIBUTE)?.parse().ok()
}

fn on_click(event: Event) {
    let activated = with_access(|access| {
        if access.activate() {
            return Some(AccessRequest::Activated);
        }
        let target = target_node(&event)?;
        access
            .supports(target, Action::Click)
            .then_some(AccessRequest::Action {
                target,
                action: AccessAction::Click,
            })
    });
    if let Some(request) = activated {
        event.prevent_default();
        send(request);
    }
}

fn on_key(event: Event) {
    let Some(key) = event.dyn_ref::<KeyboardEvent>().map(KeyboardEvent::key) else {
        return;
    };
    let Some(action) = slider_step(&key) else {
        return;
    };
    let supported = match action {
        AccessAction::Decrement => Action::Decrement,
        _ => Action::Increment,
    };
    let request = with_access(|access| {
        let target = target_node(&event)?;
        access
            .supports(target, supported)
            .then_some(AccessRequest::Action { target, action })
    });
    if let Some(request) = request {
        event.prevent_default();
        send(request);
    }
}

fn with_access<R>(f: impl FnOnce(&mut WebAccess) -> Option<R>) -> Option<R> {
    let page = super::current_page()?;
    let mut access = page.access.try_borrow_mut().ok()?;
    f(access.as_mut()?)
}

fn send(request: AccessRequest) {
    push(RawEvent::Accessibility {
        window: WINDOW,
        request,
    });
    drive();
}
