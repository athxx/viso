//! The iOS accessibility bridge (ADR 0030): the root view is a
//! UIAccessibility container holding one element per announced node.
//!
//! AccessKit has no UIKit adapter, so the bridge folds the facade's updates
//! into a [`TreeMirror`] and keeps a `UIAccessibilityElement` per node id,
//! which keeps VoiceOver's cursor in place across updates. The first time an
//! assistive technology asks the view for its elements the bridge queues
//! [`AccessRequest::Activated`] and answers with none; the facade's next frame
//! sends the tree, and a layout-changed notification makes the technology ask
//! again. Layout containers are left out: UIKit reads a flat list in document
//! order. Activating or adjusting an element queues the action like any
//! input.

use std::collections::{HashMap, HashSet};

use accesskit::{Action, Node, Role, Toggled, TreeUpdate};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSString};
use objc2_ui_kit::{
    UIAccessibilityElement, UIAccessibilityIsSwitchControlRunning,
    UIAccessibilityIsVoiceOverRunning, UIAccessibilityLayoutChangedNotification,
    UIAccessibilityPostNotification, UIAccessibilityTraitAdjustable, UIAccessibilityTraitButton,
    UIAccessibilityTraitSelected, UIAccessibilityTraitStaticText, UIAccessibilityTraits,
    UIResponder,
};

use super::view::VisoView;
use super::{LOOP, WINDOW, drive, guarded, push};
use crate::access_mirror::TreeMirror;
use crate::accessibility::{AccessAction, AccessRequest};
use crate::event::RawEvent;

/// The bridge's state, in the loop.
pub(super) struct Access {
    /// An assistive technology asked for the elements, so the facade
    /// publishes.
    active: bool,
    mirror: TreeMirror,
    elements: HashMap<u64, Retained<AccessElement>>,
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

    /// Fold `update` in and rebuild the element list; returns the focused
    /// element when the focus moved.
    fn apply(
        &mut self,
        view: &VisoView,
        update: TreeUpdate,
        scale: f64,
    ) -> Option<Retained<AccessElement>> {
        let changes = self.mirror.apply(update);
        let changed: HashSet<u64> = changes.changed.into_iter().collect();
        let Self {
            mirror, elements, ..
        } = self;
        for id in &changes.removed {
            elements.remove(id);
        }
        let mut order = Vec::with_capacity(elements.len());
        mirror.walk(|id, node, _| {
            if !announced(node) {
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

/// Publish `update` if an assistive technology has asked for the elements.
pub(super) fn update(view: &VisoView, update: TreeUpdate) {
    let scale = view.contentScaleFactor();
    let Some(focus) = LOOP.with(|l| {
        let mut access = l.access.borrow_mut();
        access.active.then(|| access.apply(view, update, scale))
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

/// Whether a node becomes an element. Containers only structure the tree,
/// which UIKit reads flat.
fn announced(node: &Node) -> bool {
    !matches!(
        node.role(),
        Role::Window
            | Role::GenericContainer
            | Role::Group
            | Role::Region
            | Role::Navigation
            | Role::TabList
            | Role::Tree
            | Role::Dialog
    )
}

/// The traits UIKit announces for a node, following AccessKit's role
/// mapping on the other targets.
fn traits(node: &Node) -> UIAccessibilityTraits {
    // SAFETY: the trait constants are immutable UIKit values.
    let (button, adjustable, text, selected) = unsafe {
        (
            UIAccessibilityTraitButton,
            UIAccessibilityTraitAdjustable,
            UIAccessibilityTraitStaticText,
            UIAccessibilityTraitSelected,
        )
    };
    let mut traits = match node.role() {
        Role::Button | Role::CheckBox | Role::RadioButton | Role::Tab => button,
        Role::Slider => adjustable,
        Role::Label | Role::Status => text,
        _ if node.supports_action(Action::Click) => button,
        _ => 0,
    };
    if node.toggled() == Some(Toggled::True) || node.is_selected() == Some(true) {
        traits |= selected;
    }
    traits
}

/// The spoken value: a slider's position as a percentage of its range, or
/// its bare value without one.
fn value(node: &Node) -> Option<String> {
    let value = node.numeric_value()?;
    Some(match (node.min_numeric_value(), node.max_numeric_value()) {
        (Some(min), Some(max)) if max > min => {
            format!("{}%", ((value - min) / (max - min) * 100.0).round())
        }
        _ => format!("{value}"),
    })
}

define_class!(
    // SAFETY: UIAccessibilityElement is designed to be subclassed; the
    // element is initialized through its designated initializer in `new` and
    // has no `Drop` impl.
    #[unsafe(super(UIAccessibilityElement, UIResponder, objc2::runtime::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoAccessElement"]
    #[ivars = u64]
    pub(super) struct AccessElement;

    unsafe impl NSObjectProtocol for AccessElement {}

    impl AccessElement {
        #[unsafe(method(accessibilityActivate))]
        fn activate(&self) -> bool {
            // Without a click handler UIKit falls back to a tap at the
            // element's center, which reaches the view as a touch.
            self.perform(Action::Click, AccessAction::Click)
        }

        #[unsafe(method(accessibilityIncrement))]
        fn increment(&self) {
            self.perform(Action::Increment, AccessAction::Increment);
        }

        #[unsafe(method(accessibilityDecrement))]
        fn decrement(&self) {
            self.perform(Action::Decrement, AccessAction::Decrement);
        }
    }
);

impl AccessElement {
    fn new(view: &VisoView, target: u64) -> Retained<Self> {
        let this = Self::alloc(view.mtm()).set_ivars(target);
        // SAFETY: `initWithAccessibilityContainer:` is the designated
        // initializer; the container is the live view, which the element
        // references weakly.
        let element: Retained<Self> =
            unsafe { msg_send![super(this), initWithAccessibilityContainer: view] };
        element.setIsAccessibilityElement(true);
        element
    }

    /// Copy `node`'s label, value, traits and frame; bounds are physical
    /// pixels in the view, the frame points.
    fn describe(&self, node: &Node, scale: f64) {
        self.setAccessibilityLabel(node.label().map(NSString::from_str).as_deref());
        self.setAccessibilityValue(value(node).as_deref().map(NSString::from_str).as_deref());
        self.setAccessibilityTraits(traits(node));
        let frame = node.bounds().map_or(CGRect::ZERO, |b| CGRect {
            origin: CGPoint::new(b.x0 / scale, b.y0 / scale),
            size: CGSize::new(b.width() / scale, b.height() / scale),
        });
        self.setAccessibilityFrameInContainerSpace(frame);
    }

    /// Queue `action` on this element's node if the node supports it.
    fn perform(&self, supported: Action, action: AccessAction) -> bool {
        let target = *self.ivars();
        let supports = LOOP.with(|l| {
            l.access
                .try_borrow()
                .ok()
                .and_then(|access| {
                    access
                        .mirror
                        .get(target)
                        .map(|n| n.supports_action(supported))
                })
                .unwrap_or(false)
        });
        if supports {
            guarded(|| send(AccessRequest::Action { target, action }));
        }
        supports
    }
}
