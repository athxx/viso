//! The [`Sheet`] control — a panel that slides in from an edge of the surface
//! over a dimming scrim, rests at its detents, and a drag carries between them
//! or off.
//!
//! A `Sheet` is a layer over the whole surface: its own box is the scrim, and
//! its one child the panel, pinned to an edge ([`SheetEdge`], default
//! [`Bottom`](SheetEdge::Bottom)) at its full extent — a bottom sheet spans the
//! width at a fixed height, a side panel the height at a fixed width. Opened
//! ([`SheetHandle::open`]) it shows, takes the focus and traps Tab in it, and
//! springs in to its detent ([`SheetDetent`]: all of the panel by default, a
//! share of the surface, or a length), the scrim darkening as it comes; closed,
//! it springs back out, and once out it hides and gives the focus back. The
//! motion is `viso-ui`'s ([`viso_ui::sheet`]) and marks only transforms and the
//! scrim's paint.
//!
//! A finger dragging the panel along its axis carries it — from a list within
//! it once the list has nowhere to scroll the way the finger set out — and a
//! mouse drags it by its grabber; let go, it springs to the detent nearest
//! where its speed would carry it ([`Sheet::on_detent`] hears which), or, past
//! its least detent, off: a drag dismisses it as Escape and a press on the
//! scrim do, firing [`Sheet::on_dismiss`]. A sheet that is not
//! [`dismissible`](Sheet::dismissible) eases below its least detent instead.
//!
//! ```
//! use std::cell::RefCell;
//! use std::rc::Rc;
//! use viso_widgets::{SheetDetent, SheetEdge, sheet};
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Sizing};
//!
//! let control = sheet()
//!     .edge(SheetEdge::Bottom)
//!     .extent(640.0)
//!     .detents([SheetDetent::Fraction(0.5), SheetDetent::Full])
//!     .content(|cx| { cx.leaf(LeafStyle { size: Sizing::fill(), style: BoxStyle::NONE }); })
//!     .on_dismiss(|_ev| { /* the sheet was dismissed */ });
//!
//! // Sheet authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! let handle = control.build(&mut cx);
//! // A handler calls the handle's methods; `handle` is `Copy`.
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use viso_ui::ColorToken;

use viso_ui::{
    Align, Alignment2D, Axis, BoxStyle, BuildCx, Component, Controls, DirtyClass, DispatchPhase,
    EventCx, FlexStyle, Inset, Justify, Key, LeafStyle, Length, NodeId, PointerPhase, Rgba, Role,
    Semantics, Sizing, StackStyle, StateId, StateValue,
};
pub use viso_ui::{SheetDetent, SheetEdge};

/// A shared, mutable dismiss callback, fired when the sheet closes (Escape, a
/// press on the scrim, a drag off, or a programmatic [`SheetHandle::close`]).
type SharedDismiss = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>)>>>>;

/// What hears the detent a drag brought the sheet to.
type DetentHandler = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize)>>>>;

/// A build-time content builder: the panel's body.
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

viso_ui::control_handle! {
    /// Opens, closes or moves a built [`Sheet`] from an event handler. An open
    /// when already open, or a close when already closed, is a no-op.
    ///
    /// `Copy`, an id alone: `build` returns it, or mint one first with
    /// [`BuildCx::handle`](viso_ui::BuildCx::handle) for closures declared
    /// before the control. Once the control is gone, every call is a no-op.
    pub struct SheetHandle;
}

impl SheetHandle {
    /// Open the sheet: it shows, takes the focus and springs in.
    pub fn open(self, ev: &mut EventCx<'_>) {
        if let Some(parts) = ev.control::<SheetParts>(self.0) {
            parts.open(ev);
        }
    }

    /// Close the sheet: it springs out, then hides and gives the focus
    /// back. Fires `on_dismiss`.
    pub fn close(self, ev: &mut EventCx<'_>) {
        if let Some(parts) = ev.control::<SheetParts>(self.0) {
            parts.close(ev);
        }
    }

    /// Close it if open, open it if closed.
    pub fn toggle(self, ev: &mut EventCx<'_>) {
        if let Some(parts) = ev.control::<SheetParts>(self.0) {
            parts.toggle(ev);
        }
    }

    /// Bring it to rest at detent `detent`, by its place among its detents
    /// least first.
    pub fn set_detent(self, ev: &mut EventCx<'_>, detent: usize) {
        if let Some(parts) = ev.control::<SheetParts>(self.0) {
            parts.set_detent(ev, detent);
        }
    }

    /// The sheet's node: the layer whose child is the panel.
    pub fn node(self, cx: &impl Controls) -> Option<NodeId> {
        Some(cx.control::<SheetParts>(self.0)?.node())
    }
}

/// What a built control's handlers and its [`SheetHandle`] share.
#[derive(Clone)]
struct SheetParts {
    /// The reactive cell holding whether it is open.
    open: StateId,
    /// The sheet: the layer whose child is the panel.
    sheet: NodeId,
    on_dismiss: SharedDismiss,
}

impl SheetParts {
    /// Open the sheet: it shows, takes the focus and springs in.
    pub fn open(&self, ev: &mut EventCx<'_>) {
        set_open(self, ev, true);
    }

    /// Close the sheet: it springs out, then hides and gives the focus
    /// back. Fires `on_dismiss`.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        set_open(self, ev, false);
    }

    /// Close it if open, open it if closed.
    pub fn toggle(&self, ev: &mut EventCx<'_>) {
        let next = !matches!(ev.get(self.open), Some(StateValue::Bool(true)));
        set_open(self, ev, next);
    }

    /// Bring it to rest at detent `detent`, by its place among its detents
    /// least first.
    pub fn set_detent(&self, ev: &mut EventCx<'_>, detent: usize) {
        ev.set_sheet_detent(self.sheet, detent);
    }

    /// The sheet's node: the layer whose child is the panel.
    pub fn node(&self) -> NodeId {
        self.sheet
    }
}

/// A translucent black wash — the default scrim dimming the scene behind an open
/// sheet so the background reads as inert.
const DEFAULT_SCRIM: Rgba = ColorToken::Scrim.rgba();

/// The default full extent of the panel along the slide axis.
const DEFAULT_EXTENT: f32 = 320.0;

/// The grabber's bar, its color, and the room it takes along the axis.
const GRABBER: (f32, f32) = (36.0, 5.0);
const GRABBER_ROOM: f32 = 17.0;
const GRABBER_COLOR: Rgba = ColorToken::Muted.alpha(0.6);

/// The visual and layout parameters of a [`Sheet`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SheetStyle {
    /// The dimming color painted over the surface at full dim, under the
    /// panel; `None` paints no scrim.
    pub scrim: Option<Rgba>,
    /// The edge the panel lies against and slides in from.
    pub edge: SheetEdge,
    /// The panel's full extent along the slide axis (its height for a
    /// bottom or top sheet, its width for a side panel).
    pub extent: f32,
    /// The panel's own box.
    pub background: BoxStyle,
    /// Whether a grabber bar heads a bottom or top panel: what a mouse drags
    /// it by.
    pub grabber: bool,
    /// Whether a drag past its least detent dismisses it.
    pub dismissible: bool,
}

impl Default for SheetStyle {
    fn default() -> Self {
        SheetStyle {
            scrim: Some(DEFAULT_SCRIM),
            edge: SheetEdge::default(),
            extent: DEFAULT_EXTENT,
            background: BoxStyle::NONE,
            grabber: false,
            dismissible: true,
        }
    }
}

/// A panel that slides in from an edge of the surface and takes focus while
/// open. Build one with [`sheet`]; see the [module docs](self).
pub struct Sheet {
    content: Option<ContentBuilder>,
    style: SheetStyle,
    detents: Vec<SheetDetent>,
    detent: usize,
    on_dismiss: SharedDismiss,
    on_detent: DetentHandler,
    /// The handle `build` binds, when the app minted one up front.
    handle: Option<SheetHandle>,
}

/// An empty [`Sheet`] against the bottom edge, all of its panel showing when
/// open, over a dark scrim.
pub fn sheet() -> Sheet {
    Sheet {
        content: None,
        style: SheetStyle::default(),
        detents: Vec::new(),
        detent: 0,
        on_dismiss: Rc::new(RefCell::new(None)),
        on_detent: Rc::new(RefCell::new(None)),
        handle: None,
    }
}

impl Sheet {
    /// Set the panel's body.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Set the edge the panel lies against and slides in from.
    pub fn edge(mut self, edge: SheetEdge) -> Self {
        self.style.edge = edge;
        self
    }

    /// Set the panel's full extent along the slide axis.
    pub fn extent(mut self, extent: f32) -> Self {
        self.style.extent = extent;
        self
    }

    /// Set the dimming scrim color.
    pub fn scrim(mut self, scrim: Rgba) -> Self {
        self.style.scrim = Some(scrim);
        self
    }

    /// Let it rest at `detents`, a drag carrying it between them; it opens
    /// at the first unless [`detent`](Self::detent) says.
    pub fn detents(mut self, detents: impl IntoIterator<Item = SheetDetent>) -> Self {
        self.detents = detents.into_iter().collect();
        self
    }

    /// Open it at detent `detent`, by its place among its detents least
    /// first.
    pub fn detent(mut self, detent: usize) -> Self {
        self.detent = detent;
        self
    }

    /// Whether a drag past its least detent dismisses it (the default).
    pub fn dismissible(mut self, dismissible: bool) -> Self {
        self.style.dismissible = dismissible;
        self
    }

    /// Head a bottom or top panel with a grabber bar.
    pub fn grabber(mut self, grabber: bool) -> Self {
        self.style.grabber = grabber;
        self
    }

    /// Set the dismiss callback, fired when the sheet closes.
    pub fn on_dismiss(self, handler: impl FnMut(&mut EventCx<'_>) + 'static) -> Self {
        *self.on_dismiss.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Hear the detent a drag brought it to, by its place among its detents
    /// least first.
    pub fn on_detent(self, handler: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        *self.on_detent.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`SheetStyle`].
    pub fn style(mut self, style: SheetStyle) -> Self {
        self.style = style;
        self
    }

    /// Bind `handle`, minted with [`BuildCx::handle`](viso_ui::BuildCx::handle)
    /// before the control is built, so closures declared earlier can drive it.
    pub fn handle(mut self, handle: SheetHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Build the control into `cx` and return its handle.
    pub fn build(mut self, cx: &mut BuildCx<'_>) -> SheetHandle {
        let handle = *self.handle.get_or_insert_with(|| cx.handle());
        Component::build(&self, cx);
        handle
    }
}

/// Opens or closes the sheet `h` names, if that changes it; a close fires
/// `on_dismiss`.
fn set_open(h: &SheetParts, ev: &mut EventCx<'_>, next: bool) {
    let cur = matches!(ev.get(h.open), Some(StateValue::Bool(true)));
    if cur == next {
        return;
    }
    ev.set(h.open, StateValue::Bool(next));
    ev.present_sheet(h.sheet, next);
    if !next && let Some(f) = h.on_dismiss.borrow_mut().as_mut() {
        f(ev);
    }
}

impl Component for Sheet {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let open = cx.state(StateValue::Bool(false));
        let s = self.style;
        let axis = s.edge.axis();
        let align = match s.edge {
            SheetEdge::Bottom => Alignment2D::Bottom,
            SheetEdge::Top => Alignment2D::Top,
            SheetEdge::Leading => Alignment2D::Start,
            SheetEdge::Trailing => Alignment2D::End,
        };
        let panel_size = match axis {
            Axis::Column => Sizing {
                width: Length::fill(),
                height: Length::Fixed(s.extent),
            },
            Axis::Row => Sizing {
                width: Length::Fixed(s.extent),
                height: Length::fill(),
            },
        };
        let grabber = s.grabber && axis == Axis::Column;
        let root = cx.stack(
            StackStyle {
                align,
                padding: Inset::all(0.0),
                size: Sizing::fill(),
                style: BoxStyle::NONE,
            },
            |cx| {
                let panel = cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        gap: 0.0,
                        padding: Inset::all(0.0),
                        align: Align::Stretch,
                        justify: Justify::Start,
                        size: panel_size,
                        style: s.background,
                    },
                    |cx| {
                        let bar = |cx: &mut BuildCx<'_>| {
                            cx.flex(
                                FlexStyle {
                                    axis: Axis::Row,
                                    gap: 0.0,
                                    padding: Inset::all(0.0),
                                    align: Align::Center,
                                    justify: Justify::Center,
                                    size: Sizing {
                                        width: Length::fill(),
                                        height: Length::Fixed(GRABBER_ROOM),
                                    },
                                    style: BoxStyle::NONE,
                                },
                                |cx| {
                                    cx.leaf(LeafStyle {
                                        size: Sizing::fixed(GRABBER.0, GRABBER.1),
                                        style: BoxStyle::solid(GRABBER_COLOR)
                                            .with_radius(GRABBER.1 / 2.0),
                                    });
                                },
                            );
                        };
                        if grabber && s.edge == SheetEdge::Bottom {
                            bar(cx);
                        }
                        if let Some(content) = &self.content {
                            (content)(cx);
                        }
                        if grabber && s.edge == SheetEdge::Top {
                            bar(cx);
                        }
                    },
                );
                cx.focusable(panel, true);
                cx.semantics(panel, Semantics::role(Role::Dialog));
            },
        );
        cx.sheet(
            root,
            viso_ui::SheetSpec {
                edge: s.edge,
                detents: self.detents.clone(),
                dismissible: s.dismissible,
            },
            s.scrim,
        );
        cx.sheet_detent(root, self.detent);
        cx.bind(open, root, DirtyClass::PAINT);
        cx.semantics(root, Semantics::role(Role::Group));

        let handle = SheetParts {
            open,
            sheet: root.id(),
            on_dismiss: self.on_dismiss.clone(),
        };
        // Escape, and a drag off, close it; the detent a drag settles on is
        // heard.
        let keys = handle.clone();
        let on_detent = self.on_detent.clone();
        cx.on_key(root, move |ev| {
            if let Some(detent) = ev.sheet_detent() {
                if let Some(f) = on_detent.borrow_mut().as_mut() {
                    f(ev, detent);
                }
                return;
            }
            let escape = ev.key().is_some_and(|k| {
                k.pressed && !k.repeat && matches!(k.key, Key::Escape | Key::Back)
            });
            if escape || ev.dismissed() {
                ev.stop_propagation();
                set_open(&keys, ev, false);
            }
        });
        // A press on the scrim, the sheet's own box, closes it and keeps the
        // rest of the press, so its release reaches nothing it uncovers.
        let presses = handle.clone();
        let sheet_node = root.id();
        cx.on_pointer(root, move |ev| {
            if ev.phase() != DispatchPhase::Target {
                return;
            }
            ev.stop_propagation();
            if ev.pointer().is_some_and(|p| p.phase == PointerPhase::Down) {
                ev.capture_pointer(sheet_node);
                set_open(&presses, ev, false);
            }
        });
        if let Some(bound) = self.handle {
            cx.bind_control(bound.0, root.id(), Rc::new(handle));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, NodeStore, Rect, SemanticProjector, StateStore, TextEdits, VirtualLists,
    };

    fn built(control: Sheet) -> (NodeStore, NodeId) {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            control.build(&mut cx);
            cx.root().unwrap()
        };
        store.layout(root, Rect::new(0.0, 0.0, 300.0, 600.0), &mut Vec::new());
        (store, root)
    }

    #[test]
    fn it_builds_a_hidden_sheet_over_a_focusable_dialog_panel() {
        let (store, root) = built(sheet().grabber(true));
        assert!(store.is_sheet(root) && store.hidden(root));
        let panel = store.arena().children(root).next().unwrap();
        assert!(store.focusable(panel));
        assert_eq!(store.semantics(panel).map(|s| s.role), Some(Role::Dialog));
        assert_eq!(
            store.arena().children(panel).count(),
            1,
            "the grabber heads the panel"
        );
    }
}
