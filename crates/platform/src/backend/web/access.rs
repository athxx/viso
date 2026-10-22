//! The Web accessibility bridge (ADR 0030, ADR 0042): an ARIA mirror of the
//! published tree, transparent elements laid over the canvas where each node
//! draws.
//!
//! A page cannot tell whether a screen reader runs. The mirror therefore starts as a single transparent "Enable
//! accessibility" button; activating it (a screen reader clicks it) queues
//! [`AccessRequest::Activated`], and the facade's next frame sends the tree.
//! From then on each [`SemanticsDelta`] is folded into a [`TreeMirror`] and only the
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
use crate::accessibility::{
    AccessAction, AccessNode, AccessRequest, SemanticsDelta, SemanticsId, SemanticsRect,
};
use crate::backend::web_translate::{aria_attributes, element_id, named_by_content, slider_step};
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
    elements: HashMap<SemanticsId, HtmlElement>,
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

    /// Fold `delta` in and patch the elements it changed. `canvas` is the
    /// canvas's client rect, `scale` device pixels per CSS pixel.
    pub(super) fn update(
        &mut self,
        document: &Document,
        delta: SemanticsDelta,
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

        let changes = self.mirror.apply(delta);
        for id in &changes.removed {
            if let Some(element) = self.elements.remove(id) {
                element.remove();
            }
        }
        let changed: HashSet<SemanticsId> = changes.changed.iter().copied().collect();
        let Self {
            container,
            mirror,
            elements,
            ..
        } = self;
        // Rewrite changed nodes, and place each node whose own or parent's
        // bounds may have moved.
        mirror.walk(|node, parent| {
            let id = node.id;
            let fresh = !elements.contains_key(&id);
            let Some(element) = element_for(elements, document, id) else {
                return;
            };
            if fresh || changed.contains(&id) {
                describe(&element, node);
            }
            if fresh || changed.contains(&id) || parent.is_some_and(|p| changed.contains(&p)) {
                let origin = parent.and_then(|p| mirror.get(p)).map(|p| p.bounds);
                place(&element, node.bounds, origin, scale);
            }
        });
        // Put the children of every changed node in order under it; a fresh
        // node's parent changed with it, the root hangs off the container.
        for &id in &changes.changed {
            let Some(node) = mirror.get(id) else { continue };
            let Some(parent) = elements.get(&id) else {
                continue;
            };
            for child in &node.children {
                if let Some(element) = elements.get(child) {
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

    fn supports(&self, id: SemanticsId, action: AccessAction) -> bool {
        self.mirror
            .get(id)
            .is_some_and(|node| node.actions.contains(action))
    }
}

/// The element mirroring `id`, created on first use.
fn element_for(
    elements: &mut HashMap<SemanticsId, HtmlElement>,
    document: &Document,
    id: SemanticsId,
) -> Option<HtmlElement> {
    if let Some(element) = elements.get(&id) {
        return Some(element.clone());
    }
    let element: HtmlElement = document.create_element("div").ok()?.unchecked_into();
    element.set_attribute("style", ELEMENT_STYLE).ok()?;
    element
        .set_attribute(ID_ATTRIBUTE, &id.0.to_string())
        .ok()?;
    element.set_id(&element_id(id));
    elements.insert(id, element.clone());
    Some(element)
}

/// Write `node`'s role, name and state onto its element.
fn describe(element: &HtmlElement, node: &AccessNode) {
    aria_attributes(node, |name, value| {
        let _ = match value {
            Some(value) => element.set_attribute(name, value),
            None => element.remove_attribute(name),
        };
    });
    if named_by_content(node.role) {
        element.set_text_content(node.label.as_deref());
    }
}

/// Size and place an element in CSS pixels against its parent's bounds.
fn place(element: &HtmlElement, bounds: SemanticsRect, origin: Option<SemanticsRect>, scale: f64) {
    let (x, y) = origin.map_or((0.0, 0.0), |o| (o.x, o.y));
    let style = element.style();
    for (name, value) in [
        ("left", bounds.x - x),
        ("top", bounds.y - y),
        ("width", bounds.width),
        ("height", bounds.height),
    ] {
        let value = f64::from(value);
        let _ = style.set_property(name, &format!("{}px", value / scale));
    }
}

/// The node whose element (or a descendant of it) `event` targets.
fn target_node(event: &Event) -> Option<SemanticsId> {
    let target: Element = event.target()?.dyn_into().ok()?;
    let element = target.closest(&format!("[{ID_ATTRIBUTE}]")).ok()??;
    element
        .get_attribute(ID_ATTRIBUTE)?
        .parse()
        .ok()
        .map(SemanticsId)
}

fn on_click(event: Event) {
    let activated = with_access(|access| {
        if access.activate() {
            return Some(AccessRequest::Activated);
        }
        let target = target_node(&event)?;
        access
            .supports(target, AccessAction::Click)
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
    let request = with_access(|access| {
        let target = target_node(&event)?;
        access
            .supports(target, action)
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
