//! The iOS accessibility bridge (ADR 0030, ADR 0042): the root view is a
//! UIAccessibility container holding one element per announced node.
//!
//! The bridge folds each [`SemanticsDelta`] the facade publishes into a
//! [`TreeMirror`] and keeps a `UIAccessibilityElement` per node id,
//! which keeps VoiceOver's cursor in place across updates. The first time an
//! assistive technology asks the view for its elements the bridge queues
//! [`AccessRequest::Activated`] and answers with none; the facade's next frame
//! sends the tree, and a layout-changed notification makes the technology ask
//! again. Layout containers are left out: UIKit reads a flat list in document
//! order. Activating or adjusting an element queues the action like any
//! input.

use std::collections::{HashMap, HashSet};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSString};
use objc2_ui_kit::{
    UIAccessibilityElement, UIAccessibilityIsSwitchControlRunning,
    UIAccessibilityIsVoiceOverRunning, UIAccessibilityLayoutChangedNotification,
    UIAccessibilityPostNotification, UIAccessibilityTraitAdjustable, UIAccessibilityTraitButton,
    UIAccessibilityTraitImage, UIAccessibilityTraitSelected, UIAccessibilityTraitStaticText,
    UIAccessibilityTraits, UIResponder,
};

use super::view::VisoView;
use super::{LOOP, WINDOW, drive, guarded, push};
use crate::access_mirror::TreeMirror;
use crate::accessibility::{AccessAction, AccessNode, AccessRequest, SemanticsDelta, SemanticsId};
use crate::backend::ios_translate::{
    ElementKind, announced, element_kind, shows_selected, spoken_value,
};
use crate::event::RawEvent;

/// The bridge's state, in the loop.
pub(super) struct Access {
    /// An assistive technology asked for the elements, so the facade
    /// publishes.
    active: bool,
    mirror: TreeMirror,
    elements: HashMap<SemanticsId, Retained<AccessElement>>,
    /// The announced elements in document order, as handed to UIKit.
    order: Option<Retained<NSArray>>,
}

impl Access {
    pub(super) fn new() -> Self {
        Self {
            active: false,
            mirror: TreeMirror::new(),
            elements: HashMap::new(),
            order: None,
        }
    }

    /// Fold `delta` in and rebuild the element list; returns the focused
    /// element when the focus moved.
    fn apply(
        &mut self,
        view: &VisoView,
        delta: SemanticsDelta,
        scale: f64,
    ) -> Option<Retained<AccessElement>> {
        let changes = self.mirror.apply(delta);
        let changed: HashSet<SemanticsId> = changes.changed.into_iter().collect();
        let Self {
            mirror, elements, ..
        } = self;
        for id in &changes.removed {
            elements.remove(id);
        }
        let mut order = Vec::with_capacity(elements.len());
        mirror.walk(|node, _| {
            let id = node.id;
            if !announced(node.role) {
                elements.remove(&id);
                return;
            }
            let mut fresh = false;
            let element = elements.entry(id).or_insert_with(|| {
                fresh = true;
                AccessElement::new(view, id)
            });
            if fresh || changed.contains(&id) {
                element.describe(node, scale);
            }
            order.push(element.clone());
        });
        let refs: Vec<&AnyObject> = order.iter().map(|e| -> &AnyObject { e }).collect();
        self.order = Some(NSArray::from_slice(&refs));
        changes
            .focus_moved
            .then(|| self.elements.get(&self.mirror.focus()).cloned())
            .flatten()
    }
}

/// The view's elements for UIKit. The first ask activates the bridge.
pub(super) fn elements() -> Option<Retained<NSArray>> {
    let (order, activate) = LOOP.with(|l| {
        let Ok(mut access) = l.access.try_borrow_mut() else {
            return (None, false);
        };
        let activate = !access.active;
        access.active = true;
        (access.order.clone(), activate)
    });
    if activate {
        send(AccessRequest::Activated);
    }
    order
}

/// Publish `delta` if an assistive technology has asked for the elements.
pub(super) fn update(view: &VisoView, delta: SemanticsDelta) {
    let scale = view.contentScaleFactor();
    let Some(focus) = LOOP.with(|l| {
        let mut access = l.access.borrow_mut();
        access.active.then(|| access.apply(view, delta, scale))
    }) else {
        return;
    };
    let focus: Option<&AnyObject> = focus.as_deref().map(|e| -> &AnyObject { e });
    // SAFETY: the layout-changed notification takes the element to move the
    // cursor to, or nil; the constant is an immutable UIKit value.
    unsafe { UIAccessibilityPostNotification(UIAccessibilityLayoutChangedNotification, focus) };
}

/// Stop publishing once no assistive technology that reads the elements
/// runs; the next ask activates the bridge again.
pub(super) fn assistive_technology_changed() {
    if UIAccessibilityIsVoiceOverRunning() || UIAccessibilityIsSwitchControlRunning() {
        return;
    }
    let was_active = LOOP.with(|l| {
        let mut access = l.access.borrow_mut();
        std::mem::replace(&mut *access, Access::new()).active
    });
    if was_active {
        send(AccessRequest::Deactivated);
    }
}

fn send(request: AccessRequest) {
    push(RawEvent::Accessibility {
        window: WINDOW,
        request,
    });
    drive();
}

/// The UIKit traits a node's element announces.
fn traits(node: &AccessNode) -> UIAccessibilityTraits {
    // SAFETY: the trait constants are immutable UIKit values.
    let (button, adjustable, text, image, selected) = unsafe {
        (
            UIAccessibilityTraitButton,
            UIAccessibilityTraitAdjustable,
            UIAccessibilityTraitStaticText,
            UIAccessibilityTraitImage,
            UIAccessibilityTraitSelected,
        )
    };
    let mut traits = match element_kind(node) {
        ElementKind::Button => button,
        ElementKind::Adjustable => adjustable,
        ElementKind::StaticText => text,
        ElementKind::Image => image,
        ElementKind::Plain => 0,
    };
    if shows_selected(node) {
        traits |= selected;
    }
    traits
}

define_class!(
    // SAFETY: UIAccessibilityElement is designed to be subclassed; the
    // element is initialized through its designated initializer in `new` and
    // has no `Drop` impl.
    #[unsafe(super(UIAccessibilityElement, UIResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoAccessElement"]
    #[ivars = SemanticsId]
    pub(super) struct AccessElement;

    unsafe impl NSObjectProtocol for AccessElement {}

    impl AccessElement {
        #[unsafe(method(accessibilityActivate))]
        fn activate(&self) -> bool {
            // Without a click handler UIKit falls back to a tap at the
            // element's center, which reaches the view as a touch.
            self.perform(AccessAction::Click)
        }

        #[unsafe(method(accessibilityIncrement))]
        fn increment(&self) {
            self.perform(AccessAction::Increment);
        }

        #[unsafe(method(accessibilityDecrement))]
        fn decrement(&self) {
            self.perform(AccessAction::Decrement);
        }
    }
);

impl AccessElement {
    fn new(view: &VisoView, target: SemanticsId) -> Retained<Self> {
        let this = Self::alloc(view.mtm()).set_ivars(target);
        // SAFETY: `initWithAccessibilityContainer:` is the designated
        // initializer; the container is the live view, which the element
        // references weakly.
        let element: Retained<Self> =
            unsafe { msg_send![super(this), initWithAccessibilityContainer: view] };
        element.setIsAccessibilityElement(true);
        element
    }

    /// Copy `node`'s label, hint, value, traits and frame; bounds are
    /// physical pixels in the view, the frame points.
    fn describe(&self, node: &AccessNode, scale: f64) {
        let text = |text: Option<&str>| text.map(NSString::from_str);
        self.setAccessibilityLabel(text(node.label.as_deref()).as_deref());
        self.setAccessibilityHint(text(node.description.as_deref()).as_deref());
        self.setAccessibilityValue(text(spoken_value(node).as_deref()).as_deref());
        self.setAccessibilityTraits(traits(node));
        let b = node.bounds;
        let frame = CGRect {
            origin: CGPoint::new(f64::from(b.x) / scale, f64::from(b.y) / scale),
            size: CGSize::new(f64::from(b.width) / scale, f64::from(b.height) / scale),
        };
        self.setAccessibilityFrameInContainerSpace(frame);
    }

    /// Queue `action` on this element's node if the node supports it.
    fn perform(&self, action: AccessAction) -> bool {
        let target = *self.ivars();
        let supports = LOOP.with(|l| {
            l.access
                .try_borrow()
                .ok()
                .and_then(|access| {
                    access
                        .mirror
                        .get(target)
                        .map(|n| n.actions.contains(action))
                })
                .unwrap_or(false)
        });
        if supports {
            guarded(|| send(AccessRequest::Action { target, action }));
        }
        supports
    }
}
