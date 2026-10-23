//! The [`InteractiveViewer`] widget: content that pans and zooms.
//!
//! An `InteractiveViewer` is a clip box over one content child that a drag
//! pans and a pinch zooms — two fingers, a trackpad's pinch, or a wheel turned
//! with Control held — within bounds that keep the content's edges at or past
//! the viewer's and limits on its scale. A wheel or a trackpad's two-finger
//! scroll pans it, a flick coasts on, a pull past the bounds springs back
//! where the platform's viewports bounce, and a double tap zooms in about
//! where it lands and back out. It takes the keyboard's focus: arrows and
//! page keys pan it, `+` and `-` zoom it, `0` and Home put it back. The
//! gestures are `viso-ui`'s viewer ([`viso_ui::viewer`]), not handlers of the
//! widget's own: a pan or a zoom marks only the content's transform.
//!
//! The content is laid out in the viewer's box, at scale 1 as it lays out
//! there: content sized to fill shows whole at rest (a photo), content of a
//! fixed size larger than the viewer shows in part (a diagram). A
//! [`ViewerHandle`] moves it from the app's own handlers.

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Alignment2D, BoxStyle, BuildCx, Component, EventCx, NodeId, Role, Semantics, Size, StackStyle,
    Vec2, ViewerLimits, ViewerMove,
};

/// The look and limits of an [`InteractiveViewer`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InteractiveViewerStyle {
    /// Its own size within its parent: the clip box.
    pub size: Size,
    /// Its own background, border and radius.
    pub background: BoxStyle,
    /// How far the content may scale and move.
    pub limits: ViewerLimits,
    /// The scale a double tap zooms to about where it lands while at rest,
    /// and back from once zoomed in; `None` for no double tap.
    pub double_tap: Option<f32>,
}

impl Default for InteractiveViewerStyle {
    fn default() -> Self {
        InteractiveViewerStyle {
            size: Size::fill(),
            background: BoxStyle::NONE,
            limits: ViewerLimits::default(),
            double_tap: Some(2.5),
        }
    }
}

/// A slot an app passes to [`InteractiveViewer::handle`] and reads after
/// building for the viewer's [`ViewerHandle`].
pub type ViewerHandleSlot = Rc<RefCell<Option<ViewerHandle>>>;

/// Moves a built [`InteractiveViewer`]'s content from an event handler, gliding
/// there. Cheap to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewerHandle {
    node: NodeId,
}

impl ViewerHandle {
    /// The viewer's node.
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// Back as laid out.
    pub fn reset(&self, ev: &mut EventCx<'_>) {
        ev.move_viewer(self.node, ViewerMove::Reset);
    }

    /// To `scale`, keeping what shows at window point `at` there; the
    /// viewer's middle for `None`.
    pub fn zoom_to(&self, ev: &mut EventCx<'_>, scale: f32, at: Option<Vec2>) {
        ev.move_viewer(self.node, ViewerMove::Zoom { scale, at });
    }

    /// By `factor`, about `at` as [`zoom_to`](Self::zoom_to).
    pub fn zoom_by(&self, ev: &mut EventCx<'_>, factor: f32, at: Option<Vec2>) {
        ev.move_viewer(self.node, ViewerMove::ZoomBy { factor, at });
    }

    /// The content moved by `by`.
    pub fn pan_by(&self, ev: &mut EventCx<'_>, by: Vec2) {
        ev.move_viewer(self.node, ViewerMove::Pan(by));
    }
}

/// [`InteractiveViewer`]'s content-declaration closure, run once at build.
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// Content that pans and zooms within a clip box. Build one with
/// [`interactive_viewer`] and give it its content with
/// [`content`](Self::content):
///
/// ```
/// use viso_widgets::{InteractiveViewerStyle, interactive_viewer};
/// use viso_ui::{BoxStyle, BuildCx, Component, LeafStyle, NodeStore, Size};
///
/// let diagram = interactive_viewer(InteractiveViewerStyle::default())
///     .label("Diagram")
///     .content(|cx| {
///         // the one content child, larger than the viewer
///         cx.leaf(LeafStyle {
///             size: Size::fixed(2000.0, 1500.0),
///             style: BoxStyle::NONE,
///         });
///     });
///
/// let mut store = NodeStore::new();
/// let mut cx = BuildCx::new(&mut store);
/// diagram.build(&mut cx);
/// ```
pub struct InteractiveViewer {
    style: InteractiveViewerStyle,
    content: Option<ContentBuilder>,
    label: Option<String>,
    slot: Option<ViewerHandleSlot>,
}

/// An [`InteractiveViewer`] of the given style, with no content yet.
pub fn interactive_viewer(style: InteractiveViewerStyle) -> InteractiveViewer {
    InteractiveViewer {
        style,
        content: None,
        label: None,
        slot: None,
    }
}

impl InteractiveViewer {
    /// Declare its one content child.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Name it to an assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Fill `slot` with its [`ViewerHandle`] as it builds.
    pub fn handle(mut self, slot: &ViewerHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }
}

impl Component for InteractiveViewer {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let handle = cx.stack(
            StackStyle {
                align: Alignment2D::Center,
                size: self.style.size,
                style: self.style.background,
                ..StackStyle::default()
            },
            |cx| {
                if let Some(content) = &self.content {
                    content(cx);
                }
            },
        );
        cx.clip(handle, true);
        cx.viewer(handle, Some(self.style.limits));
        cx.focusable(handle, true);
        let node = handle.id();
        if let Some(scale) = self.style.double_tap {
            cx.count_clicks(handle);
            cx.on_pointer(handle, move |ev| {
                if ev.phase() == viso_ui::DispatchPhase::Capture || ev.clicks() != 2 {
                    return;
                }
                let Some(p) = ev.pointer().copied() else {
                    return;
                };
                if p.phase == viso_ui::PointerPhase::Down {
                    ev.move_viewer(
                        node,
                        ViewerMove::Toggle {
                            scale,
                            at: Vec2::new(p.x, p.y),
                        },
                    );
                }
            });
        }
        let mut semantics = Semantics::role(Role::Group);
        if let Some(label) = &self.label {
            semantics = semantics.with_label(label.clone());
        }
        cx.semantics(handle, semantics);
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(ViewerHandle { node });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::context::StoreRequest;
    use viso_ui::{LeafStyle, NodeStore, Rect};

    fn built(style: InteractiveViewerStyle) -> (NodeStore, NodeId, ViewerHandle) {
        let slot = ViewerHandleSlot::default();
        let viewer = interactive_viewer(style)
            .label("Map")
            .handle(&slot)
            .content(|cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(400.0, 300.0),
                    style: BoxStyle::NONE,
                });
            });
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            viewer.build(&mut cx);
            cx.root().unwrap()
        };
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 100.0), &mut Vec::new());
        let handle = slot.borrow().expect("filled as it builds");
        (store, root, handle)
    }

    #[test]
    fn it_builds_a_focusable_clipping_viewer_named_to_assistive_technology() {
        let (store, root, handle) = built(InteractiveViewerStyle::default());
        assert_eq!(handle.node(), root);
        assert!(store.is_viewer(root));
        assert!(store.clips(root) && store.focusable(root));
        assert!(store.has_handler(root), "it hears double taps");
        let sem = store.semantics(root).unwrap();
        assert_eq!(sem.role, Role::Group);
        assert_eq!(sem.label.as_deref(), Some("Map"));
        // The larger content lays out about the middle, its edges past the
        // viewer's.
        let content = store.arena().children(root).next().unwrap();
        assert_eq!(
            store.bounds(content),
            Rect::new(-100.0, -100.0, 400.0, 300.0)
        );
    }

    #[test]
    fn no_double_tap_leaves_it_no_handler() {
        let (store, root, _) = built(InteractiveViewerStyle {
            double_tap: None,
            ..InteractiveViewerStyle::default()
        });
        assert!(!store.has_handler(root));
    }

    #[test]
    fn the_handle_moves_the_content_from_a_handler() {
        let (mut store, root, handle) = built(InteractiveViewerStyle::default());
        store.set_reduced_motion(true);
        let mut states = viso_ui::StateStore::new();
        let bindings = viso_ui::BindingTable::new();
        let mut ev = EventCx::__new(&mut states, &bindings);
        handle.zoom_to(&mut ev, 2.0, None);
        handle.pan_by(&mut ev, Vec2::new(1000.0, 0.0));
        for request in ev.__take_store_requests() {
            assert!(matches!(request, StoreRequest::Viewer(..)));
            request.__apply(&mut store);
        }
        assert_eq!(store.viewer_scale(root), Some(2.0));
        assert_eq!(store.viewer_pan(root).unwrap().x, 100.0, "to its bound");
    }
}
