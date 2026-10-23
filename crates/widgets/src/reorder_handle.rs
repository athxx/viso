//! The [`ReorderHandle`] widget: the grip a row of a reorderable list is
//! carried by.
//!
//! A `ReorderHandle` sits anywhere within a row of a list that takes
//! reorders ([`Scroll::on_reorder`](crate::Scroll::on_reorder),
//! [`VirtualList::on_reorder`](crate::VirtualList::on_reorder)). A drag on it
//! lifts the row, which follows the pointer while the others slide aside;
//! let go, it lands in the gap and the list's handler hears where it went
//! from and to. To the keyboard and to assistive technology it is a button:
//! Space or Enter lifts the row, the arrows along the list move it a place
//! at a time, Space or Enter drops it and Escape puts it back. The gesture
//! is `viso-ui`'s ([`viso_ui::row_gesture::reorder`]); a carried row marks
//! only transforms.
//!
//! ```
//! use viso_widgets::{label, reorder_handle, view, ViewStyle};
//! use viso_ui::{BuildCx, Component, NodeStore};
//!
//! let row = view(ViewStyle::default()).children(|cx| {
//!     label("Milk").build(cx);
//!     reorder_handle().build(cx);
//! });
//!
//! let mut store = NodeStore::new();
//! let mut cx = BuildCx::new(&mut store);
//! row.build(&mut cx);
//! ```

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, CursorIcon, FlexStyle, Inset, Justify, LeafStyle,
    Rgba, Role, Semantics, Size,
};

/// How a [`ReorderHandle`] looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReorderHandleStyle {
    /// Its size: the room a finger presses.
    pub size: Size,
    /// The grip's bars' color.
    pub color: Rgba,
    /// How long the bars are, and how thick.
    pub bar: (f32, f32),
    /// The room between the bars.
    pub gap: f32,
}

impl Default for ReorderHandleStyle {
    fn default() -> Self {
        ReorderHandleStyle {
            size: Size::fixed(44.0, 44.0),
            color: Rgba {
                r: 0.56,
                g: 0.56,
                b: 0.58,
                a: 1.0,
            },
            bar: (18.0, 2.0),
            gap: 4.0,
        }
    }
}

/// The grip a reorderable row is carried by. Build one with
/// [`reorder_handle`].
pub struct ReorderHandle {
    style: ReorderHandleStyle,
    label: String,
}

/// A [`ReorderHandle`] of three bars, named "Reorder" to assistive
/// technology.
pub fn reorder_handle() -> ReorderHandle {
    ReorderHandle {
        style: ReorderHandleStyle::default(),
        label: "Reorder".to_owned(),
    }
}

impl ReorderHandle {
    /// Replace the whole [`ReorderHandleStyle`].
    pub fn style(mut self, style: ReorderHandleStyle) -> Self {
        self.style = style;
        self
    }

    /// Name it `label` to assistive technology: what it carries, say.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

impl Component for ReorderHandle {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let s = self.style;
        let handle = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: s.gap,
                padding: Inset::all(0.0),
                align: Align::Center,
                justify: Justify::Center,
                size: s.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                for _ in 0..3 {
                    cx.leaf(LeafStyle {
                        size: Size::fixed(s.bar.0, s.bar.1),
                        style: BoxStyle::solid(s.color),
                    });
                }
            },
        );
        cx.reorder_handle(handle);
        cx.focusable(handle, true);
        cx.cursor(handle, CursorIcon::Grab);
        cx.semantics(
            handle,
            Semantics::role(Role::Button).with_label(self.label.clone()),
        );
    }
}
