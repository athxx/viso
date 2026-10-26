//! Drag and drop: the [`Draggable`] a drag carries data from and the
//! [`DropZone`] it carries it to.
//!
//! A `Draggable` wraps content and offers [`DragData`] — text, a link,
//! files, or the app's own value ([`DragData::value`]). A mouse drags it
//! past the slop, a finger holds it a long press; then a preview — the same
//! content built again, lifted over the scene — follows the pointer, and a
//! `DropZone` that takes one of the data's representations lights up as it
//! comes over ([`DropZone::on_enter`]), and takes it let go there
//! ([`DropZone::on_drop`]). The `Draggable` hears how it ended
//! ([`Draggable::on_end`]): what the zone did with it, or `None` put back.
//! To the keyboard a `Draggable` is focusable: Space or Enter lifts it over
//! the first zone that takes it, Tab and the arrows step it through them,
//! Space or Enter drops it and Escape puts it back. The carrying is
//! `viso-ui`'s ([`viso_ui::drag_drop`]); a carried preview marks only
//! transforms.
//!
//! ```
//! use viso_widgets::{draggable, drop_zone, label};
//! use viso_ui::{BuildCx, Component, DragAccept, DragData, DragKind, NodeStore};
//!
//! struct Card(u32);
//!
//! let card = draggable(DragData::value(Card(7)).with(viso_ui::DragItem::Text("Card 7".into())))
//!     .content(|cx| label("Card 7").build(cx))
//!     .on_end(|_ev, end| { /* end.operation: what the zone did */ });
//! let done = drop_zone([DragAccept::Value(DragKind::of::<Card>())])
//!     .content(|cx| label("Done").build(cx))
//!     .on_drop(|_ev, drop| { let _card = drop.data.get::<Card>(); });
//!
//! let mut store = NodeStore::new();
//! let mut cx = BuildCx::new(&mut store);
//! card.build(&mut cx);
//! done.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    BoxStyle, BuildCx, Component, CursorIcon, DragAccept, DragData, DragEnd, DropEvent,
    DropOperation, DropPhase, DropTarget, EventCx, Inset, Rgba, Role, Semantics, Shadow, Sizing,
    StackStyle, Vec2,
};

/// A build-time content builder, run for the content and again for its
/// preview.
type ContentBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

type EndHandler = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, DragEnd)>>>>;
type DropHandler = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, &DropEvent)>>>>;

/// How a [`Draggable`]'s preview looks while carried.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DraggableStyle {
    /// The size it asks of its parent.
    pub size: Sizing,
    /// How opaque its preview is.
    pub preview_opacity: f32,
    /// The shadow its preview casts, lifted.
    pub preview_shadow: Option<Shadow>,
}

impl Default for DraggableStyle {
    fn default() -> Self {
        DraggableStyle {
            size: StackStyle::default().size,
            preview_opacity: 0.9,
            preview_shadow: Some(Shadow {
                color: Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.25,
                },
                offset: Vec2::new(0.0, 6.0),
                blur: 18.0,
                spread: 0.0,
                inset: false,
            }),
        }
    }
}

/// Content a drag carries data from. Build one with [`draggable`].
pub struct Draggable {
    data: DragData,
    content: Option<ContentBuilder>,
    preview: Option<ContentBuilder>,
    style: DraggableStyle,
    label: Option<String>,
    on_end: EndHandler,
}

/// A [`Draggable`] offering `data`.
pub fn draggable(data: DragData) -> Draggable {
    Draggable {
        data,
        content: None,
        preview: None,
        style: DraggableStyle::default(),
        label: None,
        on_end: Rc::new(RefCell::new(None)),
    }
}

impl Draggable {
    /// Set its content, built again as its preview unless
    /// [`preview`](Self::preview) says.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Show `preview` under the pointer while carried in place of its
    /// content.
    pub fn preview(mut self, preview: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.preview = Some(Box::new(preview));
        self
    }

    /// Name it `label` to assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Hear how its drag ended.
    pub fn on_end(self, handler: impl FnMut(&mut EventCx<'_>, DragEnd) + 'static) -> Self {
        *self.on_end.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`DraggableStyle`].
    pub fn style(mut self, style: DraggableStyle) -> Self {
        self.style = style;
        self
    }
}

impl Component for Draggable {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let s = self.style;
        let content = self.content.as_deref();
        let preview = self.preview.as_deref().or(content);
        let mut lifted = None;
        let source = cx.stack(
            StackStyle {
                size: s.size,
                ..StackStyle::default()
            },
            |cx| {
                if let Some(content) = content {
                    content(cx);
                }
                // The preview lies over the surface, its content at its top
                // start corner.
                let layer = cx.stack(
                    StackStyle {
                        size: Sizing::fill(),
                        padding: Inset::all(0.0),
                        ..StackStyle::default()
                    },
                    |cx| {
                        let body = cx.stack(StackStyle::default(), |cx| {
                            if let Some(preview) = preview {
                                preview(cx);
                            }
                        });
                        if let Some(shadow) = s.preview_shadow {
                            cx.shadow(body, shadow);
                        }
                    },
                );
                cx.opacity(layer, s.preview_opacity);
                lifted = Some(layer);
            },
        );
        cx.drag_source(source, self.data.clone());
        if let Some(layer) = lifted {
            cx.drag_preview(source, layer);
        }
        cx.focusable(source, true);
        cx.cursor(source, CursorIcon::Grab);
        let mut semantics = Semantics::role(Role::Group);
        if let Some(label) = &self.label {
            semantics = semantics.with_label(label.clone());
        }
        cx.semantics(source, semantics);
        let on_end = self.on_end.clone();
        cx.on_key(source, move |ev| {
            if let Some(end) = ev.drag_end()
                && let Some(f) = on_end.borrow_mut().as_mut()
            {
                f(ev, end);
            }
        });
    }
}

/// How a [`DropZone`] looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DropZoneStyle {
    /// The size it asks of its parent.
    pub size: Sizing,
    /// Its own box.
    pub background: BoxStyle,
}

impl Default for DropZoneStyle {
    fn default() -> Self {
        DropZoneStyle {
            size: StackStyle::default().size,
            background: BoxStyle::NONE,
        }
    }
}

/// Where a drag carries data to. Build one with [`drop_zone`].
pub struct DropZone {
    target: DropTarget,
    content: Option<ContentBuilder>,
    style: DropZoneStyle,
    on_enter: DropHandler,
    on_leave: DropHandler,
    on_drop: DropHandler,
}

/// A [`DropZone`] taking data that offers one of `accepts`; none for any.
pub fn drop_zone(accepts: impl IntoIterator<Item = DragAccept>) -> DropZone {
    DropZone {
        target: DropTarget {
            accepts: accepts.into_iter().collect(),
            operation: DropOperation::Move,
        },
        content: None,
        style: DropZoneStyle::default(),
        on_enter: Rc::new(RefCell::new(None)),
        on_leave: Rc::new(RefCell::new(None)),
        on_drop: Rc::new(RefCell::new(None)),
    }
}

impl DropZone {
    /// Set its content.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// What a drop on it does with the data ([`DropOperation::Move`] unless
    /// set).
    pub fn operation(mut self, operation: DropOperation) -> Self {
        self.target.operation = operation;
        self
    }

    /// Hear a drag it takes come over it.
    pub fn on_enter(self, handler: impl FnMut(&mut EventCx<'_>, &DropEvent) + 'static) -> Self {
        *self.on_enter.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Hear the drag leave it, or be put back over it.
    pub fn on_leave(self, handler: impl FnMut(&mut EventCx<'_>, &DropEvent) + 'static) -> Self {
        *self.on_leave.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Take the data a drag let go over it.
    pub fn on_drop(self, handler: impl FnMut(&mut EventCx<'_>, &DropEvent) + 'static) -> Self {
        *self.on_drop.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`DropZoneStyle`].
    pub fn style(mut self, style: DropZoneStyle) -> Self {
        self.style = style;
        self
    }
}

impl Component for DropZone {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let zone = cx.stack(
            StackStyle {
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
        cx.drop_target(zone, self.target.clone());
        let handlers = [
            self.on_enter.clone(),
            self.on_leave.clone(),
            self.on_drop.clone(),
        ];
        cx.on_key(zone, move |ev| {
            let Some(drop) = ev.drop_event().cloned() else {
                return;
            };
            let at = match drop.phase {
                DropPhase::Enter => 0,
                DropPhase::Leave => 1,
                DropPhase::Drop => 2,
            };
            if let Some(f) = handlers[at].borrow_mut().as_mut() {
                f(ev, &drop);
            }
        });
    }
}
